//! Offline speech recognition with sherpa-onnx. Models are downloaded on demand from the
//! sherpa-onnx GitHub releases and kept loaded after first use (loading takes 0.1–2 s).

use super::Request;
use crate::audio::SAMPLE_RATE;
use anyhow::{anyhow, Context, Result};
use futures_util::StreamExt;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

#[derive(Clone, Copy)]
enum Family {
    Moonshine,
    Whisper(&'static str),
    SenseVoice,
    Parakeet,
}

pub struct ModelInfo {
    pub id: &'static str,
    pub name: &'static str,
    pub archive: &'static str,
    pub size_mb: u32,
    pub languages: &'static str,
    pub description: &'static str,
    /// ISO 639-1 codes (Cantonese is "yue"), matched against the spoken-language setting.
    pub langs: &'static [&'static str],
    /// A good default for the languages it covers.
    pub recommended: bool,
    family: Family,
}

const EN: &[&str] = &["en"];
const EUROPEAN: &[&str] = &["bg", "hr", "cs", "da", "nl", "en", "et", "fi", "fr", "de", "el", "hu", "it", "lv", "lt", "mt", "pl", "pt", "ro", "sk", "sl", "es", "sv", "ru", "uk"];
const WHISPER: &[&str] = &[
    "en", "zh", "de", "es", "ru", "ko", "fr", "ja", "pt", "tr", "pl", "ca", "nl", "ar", "sv", "it", "id", "hi", "fi", "vi", "he", "uk", "el", "ms", "cs",
    "ro", "da", "hu", "ta", "no", "th", "ur", "hr", "bg", "lt", "la", "mi", "ml", "cy", "sk", "te", "fa", "lv", "bn", "sr", "az", "sl", "kn", "et", "mk",
    "br", "eu", "is", "hy", "ne", "mn", "bs", "kk", "sq", "sw", "gl", "mr", "pa", "si", "km", "sn", "yo", "so", "af", "oc", "ka", "be", "tg", "sd", "gu",
    "am", "yi", "lo", "uz", "fo", "ht", "ps", "tk", "nn", "mt", "sa", "lb", "my", "bo", "tl", "mg", "as", "tt", "haw", "ln", "ha", "ba", "jw", "su",
];
const EAST_ASIAN: &[&str] = &["zh", "en", "ja", "ko", "yue"];

const RELEASES: &str = "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models";

pub const MODELS: &[ModelInfo] = &[
    ModelInfo { id: "moonshine-tiny-en", name: "Moonshine Tiny", archive: "sherpa-onnx-moonshine-tiny-en-int8", size_mb: 103, languages: "English", description: "Fastest. Good for short English dictation on any CPU.", langs: EN, recommended: false, family: Family::Moonshine },
    ModelInfo { id: "moonshine-base-en", name: "Moonshine Base", archive: "sherpa-onnx-moonshine-base-en-int8", size_mb: 251, languages: "English", description: "More accurate English, still faster than real time on laptops.", langs: EN, recommended: false, family: Family::Moonshine },
    ModelInfo { id: "parakeet-v2-en", name: "Parakeet TDT 0.6B v2", archive: "sherpa-onnx-nemo-parakeet-tdt-0.6b-v2-int8", size_mb: 482, languages: "English", description: "Very accurate English with punctuation and capitalization.", langs: EN, recommended: false, family: Family::Parakeet },
    ModelInfo { id: "parakeet-unified-en", name: "Parakeet Unified 0.6B", archive: "sherpa-onnx-nemo-parakeet-unified-en-0.6b-int8-non-streaming", size_mb: 501, languages: "English", description: "Newest NVIDIA English model (2026): best English accuracy, with punctuation and capitalization.", langs: EN, recommended: true, family: Family::Parakeet },
    ModelInfo { id: "parakeet-v3", name: "Parakeet TDT 0.6B v3", archive: "sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8", size_mb: 487, languages: "25 European languages", description: "Multilingual European model with automatic language detection.", langs: EUROPEAN, recommended: false, family: Family::Parakeet },
    ModelInfo { id: "parakeet-ultra", name: "Parakeet Ultra 0.6B", archive: "sherpa-onnx-nemo-parakeet-tdt-0.6b-ultra-int8", size_mb: 487, languages: "25 European languages", description: "Moondream's retrained Parakeet v3 (2026): more accurate than v3 in every language tested.", langs: EUROPEAN, recommended: true, family: Family::Parakeet },
    ModelInfo { id: "whisper-base", name: "Whisper Base", archive: "sherpa-onnx-whisper-base", size_mb: 208, languages: "99 languages", description: "Multilingual Whisper, small and quick.", langs: WHISPER, recommended: false, family: Family::Whisper("base") },
    ModelInfo { id: "whisper-small", name: "Whisper Small", archive: "sherpa-onnx-whisper-small", size_mb: 639, languages: "99 languages", description: "Multilingual Whisper with better accuracy.", langs: WHISPER, recommended: false, family: Family::Whisper("small") },
    ModelInfo { id: "whisper-turbo", name: "Whisper Large v3 Turbo", archive: "sherpa-onnx-whisper-turbo", size_mb: 564, languages: "99 languages", description: "Highest multilingual accuracy; needs a fast CPU.", langs: WHISPER, recommended: true, family: Family::Whisper("turbo") },
    ModelInfo { id: "sense-voice", name: "SenseVoice Small", archive: "sherpa-onnx-sense-voice-zh-en-ja-ko-yue-int8-2024-07-17", size_mb: 163, languages: "Chinese, English, Japanese, Korean, Cantonese", description: "Very fast multilingual model for East Asian languages.", langs: EAST_ASIAN, recommended: true, family: Family::SenseVoice },
];

pub fn info(id: &str) -> Option<&'static ModelInfo> {
    MODELS.iter().find(|m| m.id == id)
}

/// Whisper models always encode a 30 s window: a short clip costs as much as a long one, so
/// cutting a dictation into phrases only adds passes and loses context at every cut.
pub fn fixed_window(id: &str) -> bool {
    info(id).is_some_and(|m| matches!(m.family, Family::Whisper(_)))
}

pub fn model_dir(models_dir: &Path, m: &ModelInfo) -> PathBuf {
    models_dir.join(m.archive)
}

pub fn is_installed(models_dir: &Path, m: &ModelInfo) -> bool {
    model_dir(models_dir, m).join(".complete").exists()
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalModel {
    pub id: &'static str,
    pub name: &'static str,
    pub languages: &'static str,
    pub size_mb: u32,
    pub installed: bool,
    pub downloading: bool,
    pub description: &'static str,
    pub langs: &'static [&'static str],
    pub recommended: bool,
}

pub fn list(models_dir: &Path) -> Vec<LocalModel> {
    MODELS
        .iter()
        .map(|m| LocalModel {
            id: m.id,
            name: m.name,
            languages: m.languages,
            size_mb: m.size_mb,
            installed: is_installed(models_dir, m),
            downloading: downloading().contains_key(m.id),
            description: m.description,
            langs: m.langs,
            recommended: m.recommended,
        })
        .collect()
}

/// Models downloading now, each with whether the user asked to cancel it. Two downloads of one
/// model would write the same `.part` file, so a model downloads at most once at a time.
static DOWNLOADING: Mutex<std::collections::BTreeMap<&'static str, bool>> = Mutex::new(std::collections::BTreeMap::new());

fn downloading() -> std::sync::MutexGuard<'static, std::collections::BTreeMap<&'static str, bool>> {
    DOWNLOADING.lock().unwrap_or_else(|e| e.into_inner())
}

/// The error a download returns when the user cancelled it.
#[derive(Debug)]
pub struct Cancelled;

impl std::fmt::Display for Cancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("download cancelled")
    }
}

impl std::error::Error for Cancelled {}

struct DownloadGuard(&'static str);

impl Drop for DownloadGuard {
    fn drop(&mut self) {
        downloading().remove(self.0);
    }
}

/// Stops a running download before it unpacks; it then fails with [`Cancelled`] and leaves nothing behind.
pub fn cancel_download(id: &str) {
    if let Some(flag) = downloading().get_mut(id) {
        *flag = true;
    }
}

/// Downloads and unpacks a model; `progress(downloaded, total)` is called as bytes arrive.
pub async fn download(models_dir: &Path, id: &str, progress: impl Fn(u64, u64)) -> Result<()> {
    let m = info(id).ok_or_else(|| anyhow!("Unknown model {id}"))?;
    if downloading().insert(m.id, false).is_some() {
        return Err(anyhow!("{} is already downloading", m.name));
    }
    let _guard = DownloadGuard(m.id);
    std::fs::create_dir_all(models_dir)?;
    let url = format!("{RELEASES}/{}.tar.bz2", m.archive);
    let tmp = models_dir.join(format!("{}.tar.bz2.part", m.archive));
    let cancelled = || downloading().get(m.id) == Some(&true);
    fetch("github", &url, &tmp, m.size_mb, cancelled, progress).await?;
    if cancelled() {
        let _ = tokio::fs::remove_file(&tmp).await;
        return Err(Cancelled.into());
    }
    let dest = models_dir.to_path_buf();
    let dir = model_dir(models_dir, m);
    tokio::task::spawn_blocking(move || -> Result<()> {
        let f = std::fs::File::open(&tmp)?;
        tar::Archive::new(bzip2::read::BzDecoder::new(std::io::BufReader::new(f))).unpack(&dest).context("unpacking model")?;
        std::fs::remove_file(&tmp)?;
        std::fs::write(dir.join(".complete"), b"")?;
        Ok(())
    })
    .await??;
    Ok(())
}

/// Awaits `fut`, giving up with [`Cancelled`] within 200 ms of `cancelled()` turning true, even
/// while the network is stalled.
async fn unless_cancelled<T>(cancelled: &impl Fn() -> bool, fut: impl std::future::Future<Output = T>) -> Result<T> {
    tokio::pin!(fut);
    loop {
        if cancelled() {
            return Err(Cancelled.into());
        }
        if let Ok(v) = tokio::time::timeout(std::time::Duration::from_millis(200), &mut fut).await {
            return Ok(v);
        }
    }
}

/// Streams `url` into `tmp`, reporting progress about every megabyte. A failed, cancelled or
/// interrupted download leaves nothing behind.
pub(crate) async fn fetch(source: &str, url: &str, tmp: &Path, size_mb: u32, cancelled: impl Fn() -> bool, progress: impl Fn(u64, u64)) -> Result<()> {
    let result = async {
        let resp = unless_cancelled(&cancelled, super::DOWNLOAD.get(url).timeout(std::time::Duration::from_secs(3600)).send()).await??;
        let resp = super::check(source, resp).await?;
        let known = resp.content_length();
        let total = known.unwrap_or(size_mb as u64 * 1_000_000);
        let mut file = tokio::fs::File::create(tmp).await?;
        let mut stream = resp.bytes_stream();
        let (mut done, mut last_report) = (0u64, 0u64);
        while let Some(chunk) = unless_cancelled(&cancelled, stream.next()).await? {
            let chunk = chunk?;
            tokio::io::AsyncWriteExt::write_all(&mut file, &chunk).await?;
            done += chunk.len() as u64;
            if done - last_report > 1_000_000 || done == total {
                progress(done, total);
                last_report = done;
            }
        }
        tokio::io::AsyncWriteExt::flush(&mut file).await?;
        anyhow::ensure!(known.is_none_or(|t| t == done), "the download was cut off ({done} of {total} bytes)");
        Ok(())
    }
    .await;
    if result.is_err() {
        let _ = tokio::fs::remove_file(tmp).await;
    }
    result
}

pub fn delete(models_dir: &Path, id: &str) -> Result<()> {
    let m = info(id).ok_or_else(|| anyhow!("Unknown model {id}"))?;
    unload();
    let dir = model_dir(models_dir, m);
    if dir.exists() {
        std::fs::remove_dir_all(dir)?;
    }
    Ok(())
}

enum Engine {
    Moonshine(sherpa_rs::moonshine::MoonshineRecognizer),
    Whisper(sherpa_rs::whisper::WhisperRecognizer),
    SenseVoice(sherpa_rs::sense_voice::SenseVoiceRecognizer),
    Parakeet(sherpa_rs::transducer::TransducerRecognizer),
}

/// One loaded model at a time: (model id, language it was loaded for, engine).
static LOADED: Mutex<Option<(String, String, Engine)>> = Mutex::new(None);

pub fn unload() {
    *LOADED.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

fn threads() -> i32 {
    std::thread::available_parallelism().map(|n| n.get().min(8) as i32).unwrap_or(4)
}

fn load(dir: &Path, m: &ModelInfo, language: &str) -> Result<Engine> {
    let p = |f: &str| dir.join(f).to_string_lossy().into_owned();
    let n = Some(threads());
    Ok(match m.family {
        Family::Moonshine => Engine::Moonshine(
            sherpa_rs::moonshine::MoonshineRecognizer::new(sherpa_rs::moonshine::MoonshineConfig {
                preprocessor: p("preprocess.onnx"),
                encoder: p("encode.int8.onnx"),
                uncached_decoder: p("uncached_decode.int8.onnx"),
                cached_decoder: p("cached_decode.int8.onnx"),
                tokens: p("tokens.txt"),
                num_threads: n,
                ..Default::default()
            })
            .map_err(|e| anyhow!("could not load {}: {e}", m.name))?,
        ),
        Family::Whisper(size) => {
            let pick = |base: &str| if dir.join(format!("{base}.int8.onnx")).exists() { p(&format!("{base}.int8.onnx")) } else { p(&format!("{base}.onnx")) };
            Engine::Whisper(
                sherpa_rs::whisper::WhisperRecognizer::new(sherpa_rs::whisper::WhisperConfig {
                    encoder: pick(&format!("{size}-encoder")),
                    decoder: pick(&format!("{size}-decoder")),
                    tokens: p(&format!("{size}-tokens.txt")),
                    language: language.to_string(),
                    num_threads: n,
                    ..Default::default()
                })
                .map_err(|e| anyhow!("could not load {}: {e}", m.name))?,
            )
        }
        Family::SenseVoice => Engine::SenseVoice(
            sherpa_rs::sense_voice::SenseVoiceRecognizer::new(sherpa_rs::sense_voice::SenseVoiceConfig {
                model: p("model.int8.onnx"),
                tokens: p("tokens.txt"),
                language: if language.is_empty() { "auto".into() } else { language.to_string() },
                use_itn: true,
                num_threads: n,
                ..Default::default()
            })
            .map_err(|e| anyhow!("could not load {}: {e}", m.name))?,
        ),
        Family::Parakeet => Engine::Parakeet(
            sherpa_rs::transducer::TransducerRecognizer::new(sherpa_rs::transducer::TransducerConfig {
                encoder: p("encoder.int8.onnx"),
                decoder: p("decoder.int8.onnx"),
                joiner: p("joiner.int8.onnx"),
                tokens: p("tokens.txt"),
                model_type: "nemo_transducer".into(),
                decoding_method: "greedy_search".into(),
                sample_rate: SAMPLE_RATE as i32,
                feature_dim: 80,
                num_threads: threads(),
                ..Default::default()
            })
            .map_err(|e| anyhow!("could not load {}: {e}", m.name))?,
        ),
    })
}

/// Blocking transcription with the given model (loads it if needed).
pub fn transcribe_blocking(models_dir: &Path, id: &str, language: Option<&str>, audio: &[i16]) -> Result<String> {
    let m = info(id).ok_or_else(|| anyhow!("Unknown local model {id}"))?;
    if !is_installed(models_dir, m) {
        return Err(anyhow!("Local model \"{}\" is not downloaded yet (Settings → Local models)", m.name));
    }
    let lang = language.unwrap_or("").to_string();
    let mut guard = LOADED.lock().unwrap_or_else(|e| e.into_inner());
    let reload = match &*guard {
        Some((loaded_id, loaded_lang, _)) => loaded_id != id || (matches!(m.family, Family::Whisper(_) | Family::SenseVoice) && *loaded_lang != lang),
        None => true,
    };
    if reload {
        *guard = None;
        let started = std::time::Instant::now();
        *guard = Some((id.to_string(), lang.clone(), load(&model_dir(models_dir, m), m, &lang)?));
        log::info!("loaded local model {id} in {:?}", started.elapsed());
    }
    let engine = &mut guard.as_mut().expect("loaded").2;
    let samples: Vec<f32> = audio.iter().map(|s| *s as f32 / 32768.0).collect();
    let text = match engine {
        Engine::Moonshine(r) => r.transcribe(SAMPLE_RATE, &samples).text,
        Engine::Whisper(r) => r.transcribe(SAMPLE_RATE, &samples).text,
        Engine::SenseVoice(r) => r.transcribe(SAMPLE_RATE, &samples).text,
        Engine::Parakeet(r) => r.transcribe(SAMPLE_RATE, &samples),
    };
    Ok(text.trim().to_string())
}

pub async fn transcribe(models_dir: &Path, id: &str, req: &Request<'_>) -> Result<String> {
    let (dir, id, lang, audio) = (models_dir.to_path_buf(), id.to_string(), req.language.map(String::from), req.audio.to_vec());
    tokio::task::spawn_blocking(move || transcribe_blocking(&dir, &id, lang.as_deref(), &audio)).await?
}

/// Preloads a model so the first dictation isn't delayed by model loading.
pub fn warm_up(models_dir: &Path, id: &str) {
    if let Some(m) = info(id) {
        if is_installed(models_dir, m) {
            let _ = transcribe_blocking(models_dir, id, None, &vec![0i16; SAMPLE_RATE as usize / 2]);
        }
    }
}

#[cfg(test)]
mod bench {
    use super::*;

    fn words(s: &str) -> Vec<String> {
        s.to_lowercase()
            .replace(|c: char| !c.is_alphanumeric() && c != '\'' && !c.is_whitespace(), " ")
            .split_whitespace()
            .map(String::from)
            .collect()
    }

    /// Word error rate via edit distance on words.
    pub fn wer(reference: &str, hypothesis: &str) -> f64 {
        let (r, h) = (words(reference), words(hypothesis));
        let mut prev: Vec<usize> = (0..=h.len()).collect();
        for (i, rw) in r.iter().enumerate() {
            let mut cur = vec![i + 1; h.len() + 1];
            for (j, hw) in h.iter().enumerate() {
                cur[j + 1] = (prev[j] + (rw != hw) as usize).min(prev[j + 1] + 1).min(cur[j] + 1);
            }
            prev = cur;
        }
        prev[h.len()] as f64 / r.len().max(1) as f64
    }

    /// The model picker filters by these codes and finds models by id.
    #[test]
    fn catalog_is_consistent() {
        let ids: std::collections::BTreeSet<_> = MODELS.iter().map(|m| m.id).collect();
        assert_eq!(ids.len(), MODELS.len(), "duplicate model id");
        for m in MODELS {
            assert!(!m.langs.is_empty(), "{} has no languages", m.id);
            let unique: std::collections::BTreeSet<_> = m.langs.iter().collect();
            assert_eq!(unique.len(), m.langs.len(), "{} repeats a language", m.id);
            assert!(m.langs.iter().all(|l| (2..=3).contains(&l.len()) && l.chars().all(|c| c.is_ascii_lowercase())), "{} has a bad language code", m.id);
        }
    }

    /// Cancel must work while the network is stalled, not only when the next chunk arrives.
    #[tokio::test]
    async fn cancel_interrupts_a_stalled_wait() {
        let flag = std::sync::atomic::AtomicBool::new(false);
        let cancelled = || flag.load(std::sync::atomic::Ordering::Relaxed);
        assert_eq!(unless_cancelled(&cancelled, async { 7 }).await.unwrap(), 7);
        let started = std::time::Instant::now();
        let stalled = unless_cancelled(&cancelled, std::future::pending::<()>());
        let cancel = async {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            flag.store(true, std::sync::atomic::Ordering::Relaxed);
        };
        let (r, ()) = tokio::join!(stalled, cancel);
        assert!(r.unwrap_err().is::<Cancelled>());
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
    }

    #[test]
    fn wer_metric() {
        assert_eq!(wer("the cat sat", "The cat, sat."), 0.0);
        assert!((wer("the cat sat on the mat", "the cat sat on mat") - 1.0 / 6.0).abs() < 1e-9);
    }

    /// Accuracy/latency benchmark on LibriSpeech clips (with reference transcripts) for every local
    /// English-capable model. OPSTRELLIS_BENCH_DOWNLOAD=1 downloads missing models with the app's downloader.
    /// cargo test --release bench_local_models -- --ignored --nocapture
    #[tokio::test(flavor = "multi_thread")]
    #[ignore]
    async fn bench_local_models() {
        let dir = PathBuf::from(std::env::var("OPSTRELLIS_TEST_MODELS").unwrap_or_else(|_| "/tmp/opstrellis-models".into()));
        let clips_dir = std::env::var("OPSTRELLIS_BENCH_CLIPS").map(PathBuf::from).unwrap_or_else(|_| model_dir(&dir, info("moonshine-tiny-en").unwrap()).join("test_wavs"));
        let refs: Vec<(PathBuf, String)> = std::fs::read_to_string(clips_dir.join("trans.txt"))
            .expect("moonshine-tiny-en with test_wavs required")
            .lines()
            .filter_map(|l| l.split_once(' '))
            .filter(|(f, _)| *f != "8k.wav")
            .map(|(f, t)| (clips_dir.join(f), t.to_string()))
            .collect();
        let ids = ["moonshine-tiny-en", "moonshine-base-en", "parakeet-v2-en", "parakeet-unified-en", "parakeet-v3", "parakeet-ultra", "whisper-base", "sense-voice"];
        println!("\n| model | WER | real-time factor | load |\n|---|---|---|---|");
        for id in ids {
            let m = info(id).unwrap();
            if !is_installed(&dir, m) {
                if std::env::var("OPSTRELLIS_BENCH_DOWNLOAD").is_ok() {
                    download(&dir, id, |_, _| {}).await.unwrap();
                } else {
                    continue;
                }
            }
            unload();
            let t = std::time::Instant::now();
            transcribe_blocking(&dir, id, Some("en"), &vec![0; 8000]).unwrap();
            let load = t.elapsed();
            let (mut errs, mut audio_s, mut cpu_s) = (0.0, 0.0, 0.0);
            for (wav, reference) in &refs {
                let audio = crate::audio::read_wav(wav).unwrap();
                let t = std::time::Instant::now();
                let hyp = transcribe_blocking(&dir, id, Some("en"), &audio).unwrap();
                cpu_s += t.elapsed().as_secs_f64();
                audio_s += audio.len() as f64 / SAMPLE_RATE as f64;
                errs += wer(reference, &hyp);
            }
            println!("| {} | {:.1}% | {:.3} | {:.1}s |", m.name, 100.0 * errs / refs.len() as f64, cpu_s / audio_s, load.as_secs_f64());
        }
    }
}
