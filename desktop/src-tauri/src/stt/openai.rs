//! OpenAI and every OpenAI-compatible endpoint (Groq, Fireworks, self-hosted speaches /
//! whisper.cpp server / vLLM, and OpsTrellis Cloud): batch `POST {base}/audio/transcriptions` and
//! OpenAI Realtime transcription sessions.

use super::ws::{self, Message, Ws};
use super::{check, send, PartialFn, Request, HTTP};
use crate::audio::{to_wav, Resampler, SAMPLE_RATE};
use anyhow::{anyhow, Result};
use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::sync::mpsc::UnboundedReceiver;

pub const DEFAULT_BASE: &str = "https://api.openai.com/v1";

/// Hints as a prompt, most important first. Whisper reads only the last 224 tokens of a prompt,
/// so for Whisper-style models (anything but OpenAI's own gpt-* models) the prompt stays within
/// 220 bytes: its byte-level tokens are at least a byte each, so that is at most 220 tokens.
fn hint_prompt(vocabulary: &[String], model: &str) -> String {
    let budget = if model.starts_with("gpt-") { 600 } else { 220 };
    let mut out = String::new();
    for w in vocabulary {
        if out.len() + w.len() + 2 > budget {
            break;
        }
        if !out.is_empty() {
            out.push_str(", ");
        }
        out.push_str(w);
    }
    out
}

pub async fn transcribe(base: Option<&str>, key: Option<&str>, model: &str, req: &Request<'_>) -> Result<String> {
    let base = base.filter(|b| !b.trim().is_empty()).unwrap_or(DEFAULT_BASE).trim_end_matches('/');
    crate::endpoint::check(base)?;
    let file = reqwest::multipart::Part::bytes(to_wav(req.audio)).file_name("audio.wav").mime_str("audio/wav")?;
    let mut form = reqwest::multipart::Form::new().part("file", file).text("model", model.to_string()).text("response_format", "json");
    if let Some(l) = req.language {
        form = form.text("language", l.to_string());
    }
    if !req.vocabulary.is_empty() {
        // Whisper-style models use the prompt as spelling context.
        form = form.text("prompt", hint_prompt(req.vocabulary, model));
    }
    let mut rb = HTTP.post(format!("{base}/audio/transcriptions")).multipart(form);
    if let Some(k) = key.filter(|k| !k.is_empty()) {
        rb = rb.bearer_auth(k);
    }
    let v: Value = check(&host(base), send(rb).await?).await?.json().await?;
    v["text"].as_str().map(String::from).ok_or_else(|| anyhow!("{}: response has no text", host(base)))
}

fn host(base: &str) -> String {
    base.split("://").nth(1).and_then(|r| r.split('/').next()).unwrap_or(base).to_string()
}

const REALTIME_RATE: u32 = 24_000;

pub async fn connect_realtime(base: Option<&str>, key: &str, model: &str, language: Option<&str>, vocabulary: &[String]) -> Result<Ws> {
    let http_base = base.filter(|b| !b.trim().is_empty()).unwrap_or(DEFAULT_BASE).trim_end_matches('/');
    let mut ws_base = crate::endpoint::check(http_base)?;
    let _ = ws_base.set_scheme(if ws_base.scheme() == "https" { "wss" } else { "ws" });
    let url = format!("{}/realtime?intent=transcription", ws_base.as_str().trim_end_matches('/'));
    let mut ws = ws::connect(&url, &[("Authorization", &format!("Bearer {key}"))]).await?;
    let mut transcription = json!({ "model": model });
    if let Some(l) = language {
        transcription["language"] = json!(l);
    }
    if !vocabulary.is_empty() {
        transcription["prompt"] = json!(hint_prompt(vocabulary, model));
    }
    // Manual turn handling: we commit once when the user releases the hotkey.
    let update = json!({
        "type": "session.update",
        "session": {
            "type": "transcription",
            "audio": { "input": {
                "format": { "type": "audio/pcm", "rate": REALTIME_RATE },
                "transcription": transcription,
                "turn_detection": null
            }}
        }
    });
    ws.send(Message::Text(update.to_string().into())).await?;
    Ok(ws)
}

pub async fn run_realtime(ws: Ws, mut audio: UnboundedReceiver<Vec<i16>>, on_partial: PartialFn) -> Result<String> {
    let (mut sink, mut source) = ws.split();
    let sender = tokio::spawn(async move {
        let mut up = Resampler::new(SAMPLE_RATE, REALTIME_RATE);
        let b64 = base64::engine::general_purpose::STANDARD;
        while let Some(chunk) = audio.recv().await {
            let f: Vec<f32> = chunk.iter().map(|s| *s as f32 / i16::MAX as f32).collect();
            let pcm: Vec<u8> = up.process(&f).iter().flat_map(|s| ((s * i16::MAX as f32) as i16).to_le_bytes()).collect();
            let msg = json!({ "type": "input_audio_buffer.append", "audio": b64.encode(pcm) });
            sink.send(Message::Text(msg.to_string().into())).await?;
        }
        sink.send(Message::Text(json!({ "type": "input_audio_buffer.commit" }).to_string().into())).await?;
        anyhow::Ok(sink)
    });
    let mut partial = String::new();
    while let Some(msg) = source.next().await {
        let Message::Text(t) = msg? else { continue };
        let v: Value = serde_json::from_str(&t)?;
        match v["type"].as_str().unwrap_or("") {
            "conversation.item.input_audio_transcription.delta" => {
                partial.push_str(v["delta"].as_str().unwrap_or(""));
                on_partial(partial.clone());
            }
            "conversation.item.input_audio_transcription.completed" => {
                let _ = sender.await;
                return Ok(v["transcript"].as_str().unwrap_or("").to_string());
            }
            "conversation.item.input_audio_transcription.failed" | "error" => {
                let msg = v.pointer("/error/message").and_then(Value::as_str).unwrap_or("realtime error");
                return Err(anyhow!("openai realtime: {msg}"));
            }
            _ => {}
        }
    }
    Err(anyhow!("openai realtime: connection closed before the transcript completed"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hint_prompt_keeps_the_top_entries() {
        let v: Vec<String> = (0..200).map(|i| format!("Term{i}")).collect();
        let whisper = hint_prompt(&v, "whisper-1");
        assert!(whisper.starts_with("Term0, Term1, ") && whisper.len() <= 220, "{} bytes", whisper.len());
        let gpt = hint_prompt(&v, "gpt-4o-mini-transcribe");
        assert!(gpt.starts_with("Term0, ") && gpt.len() <= 600 && gpt.len() > 220 && !gpt.contains("Term199"));
    }

    #[test]
    fn host_names() {
        assert_eq!(host("https://api.groq.com/openai/v1"), "api.groq.com");
        assert_eq!(host("http://localhost:8000/v1"), "localhost:8000");
    }

    #[tokio::test]
    async fn insecure_base_url_is_refused_before_sending() {
        let audio = [1i16; 1600];
        let req = Request { audio: &audio, language: None, vocabulary: &[] };
        let e = transcribe(Some("http://stt.example.com/v1"), Some("k"), "whisper-1", &req).await.unwrap_err();
        assert!(e.to_string().contains("isn't encrypted"), "{e}");
        let e = connect_realtime(Some("http://stt.example.com/v1"), "k", "gpt-4o-transcribe", None, &[]).await.unwrap_err();
        assert!(e.to_string().contains("isn't encrypted"), "{e}");
    }

    #[tokio::test]
    async fn realtime_protocol() {
        let script = vec![
            r#"{"type":"conversation.item.input_audio_transcription.delta","delta":"Hello"}"#.to_string(),
            r#"{"type":"conversation.item.input_audio_transcription.delta","delta":" world"}"#.to_string(),
            r#"{"type":"conversation.item.input_audio_transcription.completed","transcript":"Hello world."}"#.to_string(),
        ];
        let (url, peer) = super::ws::scripted_peer("input_audio_buffer.commit", script).await;
        let socket = connect_realtime(Some(&url.replacen("ws://", "http://", 1)), "k", "gpt-4o-transcribe", Some("en"), &["OpsTrellis".into()]).await.unwrap();
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let task = tokio::spawn(run_realtime(socket, rx, std::sync::Arc::new(|_| {})));
        tx.send(vec![0i16; 1600]).unwrap();
        drop(tx);
        assert_eq!(task.await.unwrap().unwrap(), "Hello world.");
        let (_, texts) = peer.await.unwrap();
        let update: Value = serde_json::from_str(&texts[0]).unwrap();
        assert_eq!(update["session"]["audio"]["input"]["transcription"]["model"], "gpt-4o-transcribe");
        assert_eq!(update["session"]["audio"]["input"]["format"]["rate"], 24000);
        // 100 ms at 16 kHz -> 2400 samples at 24 kHz, base64 in an append event.
        let append: Value = serde_json::from_str(&texts[1]).unwrap();
        let audio = base64::engine::general_purpose::STANDARD.decode(append["audio"].as_str().unwrap()).unwrap();
        assert_eq!(append["type"], "input_audio_buffer.append");
        assert_eq!(audio.len(), 4800);
    }

    #[tokio::test]
    #[ignore]
    async fn openai_live() {
        let key = std::env::var("OPENAI_API_KEY").expect("OPENAI_API_KEY");
        let audio = super::super::sample_audio();
        let text = transcribe(None, Some(&key), "gpt-4o-mini-transcribe", &Request { audio: &audio, language: Some("en"), vocabulary: &[] })
            .await
            .unwrap();
        assert!(text.to_lowercase().contains("yellow lamps"), "{text}");
    }
}
