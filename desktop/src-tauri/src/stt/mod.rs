//! Speech-to-text providers. Each provider is a small module with `transcribe` (batch, required)
//! and optionally `stream` (realtime). Dispatch is a plain `match` on `ProviderKind`; adding a
//! provider = one module + one match arm + a variant in `settings::ProviderKind`.

mod assemblyai;
mod azure;
mod deepgram;
mod elevenlabs;
mod google;
pub mod local;
mod openai;
mod ws;

use crate::settings::{ProviderKind, SttEntry};
use anyhow::{anyhow, Result};
use std::sync::{Arc, LazyLock};
use std::time::Duration;
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use tokio::task::JoinHandle;

/// For API calls. Redirects are not followed: keys travel in headers such as x-api-key, which
/// reqwest would keep sending to whatever host a redirect points at.
pub static HTTP: LazyLock<reqwest::Client> = LazyLock::new(|| {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(8))
        .timeout(Duration::from_secs(60))
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(concat!("OpsTrellis/", env!("CARGO_PKG_VERSION")))
        .build()
        .expect("http client")
});

/// For model downloads, which carry no credentials: GitHub release assets redirect to another host.
pub static DOWNLOAD: LazyLock<reqwest::Client> = LazyLock::new(|| {
    reqwest::Client::builder()
        .https_only(true)
        .connect_timeout(Duration::from_secs(8))
        .user_agent(concat!("OpsTrellis/", env!("CARGO_PKG_VERSION")))
        .build()
        .expect("http client")
});

/// A provider entry with its secret resolved, ready to call.
#[derive(Clone)]
pub struct Provider {
    pub entry: SttEntry,
    pub key: Option<String>,
    /// Directory holding downloaded local models.
    pub models_dir: std::path::PathBuf,
}

pub struct Request<'a> {
    pub audio: &'a [i16],
    /// ISO-639-1 code, or None for auto-detect.
    pub language: Option<&'a str>,
    /// Custom vocabulary passed as provider-native hints.
    pub vocabulary: &'a [String],
}

pub type PartialFn = Arc<dyn Fn(String) + Send + Sync>;

pub struct Stream {
    pub audio: UnboundedSender<Vec<i16>>,
    pub result: JoinHandle<Result<String>>,
}

impl Stream {
    /// Closes the audio channel (signalling end of speech) and waits for the final transcript.
    pub async fn finish(self, timeout: Duration) -> Result<String> {
        drop(self.audio);
        match tokio::time::timeout(timeout, self.result).await {
            Ok(Ok(r)) => r,
            Ok(Err(e)) => Err(anyhow!("stream task failed: {e}")),
            Err(_) => Err(anyhow!("timed out waiting for the final transcript")),
        }
    }
}

impl Provider {
    pub fn label(&self) -> String {
        format!("{}/{}", serde_json::to_value(self.entry.provider).ok().and_then(|v| v.as_str().map(String::from)).unwrap_or_default(), self.entry.model)
    }

    fn key(&self) -> Result<&str> {
        self.key.as_deref().filter(|k| !k.is_empty()).ok_or_else(|| anyhow!("No API key set for {}", self.label()))
    }

    pub fn supports_streaming(&self) -> bool {
        matches!(self.entry.provider, ProviderKind::Deepgram | ProviderKind::Assemblyai | ProviderKind::Openai)
            && self.entry.streaming
    }

    pub async fn transcribe(&self, req: &Request<'_>) -> Result<String> {
        if req.audio.is_empty() {
            return Ok(String::new());
        }
        let text = match self.entry.provider {
            ProviderKind::Local => local::transcribe(&self.models_dir, &self.entry.model, req).await?,
            ProviderKind::Openai => openai::transcribe(self.entry.base_url.as_deref(), self.key.as_deref(), &self.entry.model, req).await?,
            ProviderKind::Cloud => openai::transcribe(Some(&cloud_v1(self)), Some(self.key()?), &self.entry.model, req).await?,
            ProviderKind::Deepgram => deepgram::transcribe(self.key()?, &self.entry.model, req).await?,
            ProviderKind::Assemblyai => assemblyai::transcribe(self.key()?, &self.entry.model, req).await?,
            ProviderKind::Google => google::transcribe(self.key()?, &self.entry.model, req).await?,
            ProviderKind::Azure => azure::transcribe(self.key()?, self.entry.region.as_deref().unwrap_or("eastus"), req).await?,
            ProviderKind::Elevenlabs => elevenlabs::transcribe(self.key()?, &self.entry.model, req).await?,
        };
        Ok(text.trim().to_string())
    }

    /// Opens a realtime session. Audio chunks (16 kHz PCM16) are sent on `Stream::audio`.
    pub async fn stream(&self, language: Option<String>, vocabulary: Vec<String>, on_partial: PartialFn) -> Result<Stream> {
        let (tx, rx): (UnboundedSender<Vec<i16>>, UnboundedReceiver<Vec<i16>>) = unbounded_channel();
        let key = self.key()?.to_string();
        let model = self.entry.model.clone();
        let base = self.entry.base_url.clone();
        // Connect before returning so connection errors trigger the batch fallback immediately.
        let result = match self.entry.provider {
            ProviderKind::Deepgram => {
                let ws = deepgram::connect(&key, &model, language.as_deref(), &vocabulary).await?;
                tokio::spawn(deepgram::run(ws, rx, on_partial))
            }
            ProviderKind::Assemblyai => {
                let ws = assemblyai::connect(&key, &model, &vocabulary).await?;
                tokio::spawn(assemblyai::run(ws, rx, on_partial))
            }
            ProviderKind::Openai => {
                let ws = openai::connect_realtime(base.as_deref(), &key, &model, language.as_deref(), &vocabulary).await?;
                tokio::spawn(openai::run_realtime(ws, rx, on_partial))
            }
            _ => return Err(anyhow!("{} does not support streaming", self.label())),
        };
        Ok(Stream { audio: tx, result })
    }
}

fn cloud_v1(p: &Provider) -> String {
    format!("{}/v1", p.entry.base_url.as_deref().unwrap_or("http://localhost:8787").trim_end_matches('/'))
}

/// Sends a request. Its error never names the URL: a query string can carry the user's vocabulary
/// (Deepgram `keyterm`), and request errors end up in the log.
pub async fn send(rb: reqwest::RequestBuilder) -> Result<reqwest::Response> {
    rb.send().await.map_err(|e| e.without_url().into())
}

/// Turns a non-2xx response into a readable error ("deepgram 401: Invalid credentials").
pub async fn check(name: &str, resp: reqwest::Response) -> Result<reqwest::Response> {
    if resp.status().is_success() {
        return Ok(resp);
    }
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    let msg = serde_json::from_str::<serde_json::Value>(&body)
        .ok()
        .and_then(|v| {
            ["error.message", "error", "message", "err_msg", "detail.message", "detail"]
                .iter()
                .find_map(|path| path.split('.').try_fold(&v, |acc, k| acc.get(k)).and_then(|x| x.as_str().map(String::from)))
        })
        .unwrap_or(body);
    Err(anyhow!("{name} {}: {}", status.as_u16(), one_line(&msg, 200)))
}

/// A provider's message as it goes into logs, history and toasts: one line, at most `max` chars.
fn one_line(msg: &str, max: usize) -> String {
    msg.split(|c: char| c.is_control()).filter(|s| !s.trim().is_empty()).map(str::trim).collect::<Vec<_>>().join(" ").chars().take(max).collect()
}

pub fn pcm_bytes(samples: &[i16]) -> Vec<u8> {
    samples.iter().flat_map(|s| s.to_le_bytes()).collect()
}

/// The bundled sample (LibriSpeech: "After early nightfall the yellow lamps would light up…") used by `test_stt` and live tests.
#[cfg(test)]
pub fn sample_audio() -> Vec<i16> {
    crate::audio::read_wav(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("resources/sample.wav")).unwrap()
}

/// Tags some models write for noise or silence instead of words.
static ARTIFACTS: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"(?i)\[\s*(?:blank[_ ]audio|music|noise|silence|inaudible|applause|laughter|no speech)\s*\]|\(\s*(?:silence|(?:upbeat |soft )?music|noise|inaudible|applause|laughter|blank audio|no speech)\s*\)").unwrap()
});
/// What Whisper-style models invent from noise, when it is all there is.
const INVENTED: &[&str] = &["thanks for watching", "thank you for watching", "please subscribe", "subscribe to my channel"];

/// A transcript without non-speech tags; empty when all that is left is an invented sign-off.
pub fn clean_transcript(text: &str) -> String {
    let mut t = text.trim().to_string();
    if ARTIFACTS.is_match(&t) {
        t = ARTIFACTS.replace_all(&t, "").split(' ').filter(|w| !w.is_empty()).collect::<Vec<_>>().join(" ").trim().to_string();
    }
    let bare = t.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase();
    if INVENTED.contains(&bare.as_str()) {
        return String::new();
    }
    t
}

#[cfg(test)]
mod tests {
    #[test]
    fn transcript_artifacts() {
        use super::clean_transcript;
        assert_eq!(clean_transcript(" [BLANK_AUDIO] "), "");
        assert_eq!(clean_transcript("Thanks for watching!"), "");
        assert_eq!(clean_transcript("(upbeat music) Thank you for watching."), "");
        assert_eq!(clean_transcript("Ship it [BLANK_AUDIO] today."), "Ship it today.");
        // Real dictation stays: the user's own brackets, "thank you", a longer sentence.
        assert_eq!(clean_transcript("Thank you."), "Thank you.");
        assert_eq!(clean_transcript("see the note (music section) [draft]"), "see the note (music section) [draft]");
        assert_eq!(clean_transcript("Thanks for watching the kids yesterday!"), "Thanks for watching the kids yesterday!");
        assert_eq!(clean_transcript("Para one.\n\nPara two."), "Para one.\n\nPara two.", "providers' paragraphs stay");
    }

    #[test]
    fn provider_messages_are_one_short_line() {
        assert_eq!(super::one_line("Invalid\r\ncredentials\n\n\x1b[31mx", 200), "Invalid credentials [31mx");
        assert_eq!(super::one_line(&"é".repeat(500), 200).chars().count(), 200);
    }

    #[tokio::test]
    async fn provider_errors_are_capped_and_request_errors_omit_the_url() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let _ = s.read(&mut [0u8; 2048]);
            let body = format!(r#"{{"error":{{"message":"bad key\n{}"}}}}"#, "x".repeat(5000));
            let _ = write!(s, "HTTP/1.1 401 Unauthorized\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", body.len());
        });
        let resp = super::send(super::HTTP.get(format!("http://{addr}/"))).await.unwrap();
        let e = super::check("deepgram", resp).await.unwrap_err().to_string();
        assert!(e.starts_with("deepgram 401: bad key x") && !e.contains('\n') && e.len() < 260, "{e}");

        let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap();
        let e = super::send(super::HTTP.get(format!("http://{closed}/v1/listen?keyterm=Project%20Falcon"))).await.unwrap_err();
        assert!(!format!("{e:#} {e:?}").contains("Falcon"), "{e:#}");
    }

    #[tokio::test]
    async fn api_client_does_not_follow_redirects() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let _ = s.read(&mut [0u8; 2048]);
            let _ = s.write_all(b"HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:9/steal\r\nContent-Length: 0\r\n\r\n");
        });
        let resp = super::HTTP.get(&url).header("x-api-key", "secret").send().await.unwrap();
        assert_eq!(resp.status(), 302);
    }
}
