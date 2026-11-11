//! Offline speech recognition with sherpa-onnx. Models are downloaded on demand, mostly from the
//! sherpa-onnx GitHub releases, and kept loaded after first use (loading takes 0.1–10 s).

use super::Request;
use crate::audio::SAMPLE_RATE;
use anyhow::{anyhow, Context, Result};
use futures_util::StreamExt;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// How a model's files map onto sherpa-onnx's recognizer settings.
#[derive(Clone, Copy)]
enum Family {
    /// sherpa-onnx's four-file int8 export of Moonshine.
    Moonshine,
    /// File prefix, e.g. "turbo" for turbo-encoder.int8.onnx.
    Whisper(&'static str),
    SenseVoice,
    /// NVIDIA NeMo transducers: Parakeet TDT, FastConformer, GigaAM.
    NemoTransducer,
    /// icefall Zipformer transducers; the files carry a checkpoint suffix like "epoch-99-avg-1".
    Zipformer(&'static str),
    NemoCtc,
    Dolphin,
    Omnilingual,
    Canary,
    Qwen3,
    Cohere,
}

pub struct ModelInfo {
    pub id: &'static str,
    pub name: &'static str,
    pub archive: &'static str,
    /// Download size.
    pub size_mb: u32,
    pub languages: &'static str,
    pub description: &'static str,
    /// ISO 639-1 codes (639-3 when there is none; Cantonese is "yue"), matched against the spoken-language setting.
    pub langs: &'static [&'static str],
    /// Languages this model is a top pick for.
    pub recommended: &'static [&'static str],
    /// Where the `<archive>.tar.bz2` is hosted, when not on sherpa-onnx's GitHub releases.
    pub host: Option<&'static str>,
    family: Family,
}

const NONE: &[&str] = &[];
const EN: &[&str] = &["en"];
const EUROPEAN: &[&str] = &["bg", "hr", "cs", "da", "nl", "en", "et", "fi", "fr", "de", "el", "hu", "it", "lv", "lt", "mt", "pl", "pt", "ro", "sk", "sl", "es", "sv", "ru", "uk"];
const EUROPEAN_NOT_EN_RU: &[&str] = &["bg", "hr", "cs", "da", "nl", "et", "fi", "fr", "de", "el", "hu", "it", "lv", "lt", "mt", "pl", "pt", "ro", "sk", "sl", "es", "sv", "uk"];
const WHISPER: &[&str] = &[
    "en", "zh", "de", "es", "ru", "ko", "fr", "ja", "pt", "tr", "pl", "ca", "nl", "ar", "sv", "it", "id", "hi", "fi", "vi", "he", "uk", "el", "ms", "cs",
    "ro", "da", "hu", "ta", "no", "th", "ur", "hr", "bg", "lt", "la", "mi", "ml", "cy", "sk", "te", "fa", "lv", "bn", "sr", "az", "sl", "kn", "et", "mk",
    "br", "eu", "is", "hy", "ne", "mn", "bs", "kk", "sq", "sw", "gl", "mr", "pa", "si", "km", "sn", "yo", "so", "af", "oc", "ka", "be", "tg", "sd", "gu",
    "am", "yi", "lo", "uz", "fo", "ht", "ps", "tk", "nn", "mt", "sa", "lb", "my", "bo", "tl", "mg", "as", "tt", "haw", "ln", "ha", "ba", "jv", "su",
];
const EAST_ASIAN: &[&str] = &["zh", "en", "ja", "ko", "yue"];
const CANARY: &[&str] = &["en", "de", "es", "fr"];
const COHERE: &[&str] = &["en", "fr", "de", "it", "es", "pt", "el", "nl", "pl", "zh", "ja", "ko", "vi", "ar"];
const QWEN3: &[&str] = &[
    "zh", "en", "yue", "ar", "de", "fr", "es", "pt", "id", "it", "ko", "ru", "th", "vi", "ja", "tr", "hi", "ms", "nl", "sv", "da", "fi", "pl", "cs", "tl",
    "fa", "el", "hu", "mk", "ro",
];
const DOLPHIN: &[&str] = &[
    "zh", "ja", "th", "ru", "ko", "id", "vi", "yue", "hi", "ur", "ms", "uz", "ar", "fa", "bn", "ta", "te", "ug", "gu", "my", "tl", "kk", "or", "ne", "mn",
    "km", "jv", "lo", "si", "ps", "pa", "kab", "ba", "ks", "tg", "su", "mr", "ky", "az",
];
/// The best-known of Omnilingual's 1,600+ languages.
const OMNILINGUAL: &[&str] = &[
    "en", "zh", "yue", "ja", "ko", "vi", "th", "id", "ms", "tl", "ar", "fa", "ur", "hi", "bn", "ta", "te", "mr", "gu", "kn", "ml", "pa", "or", "as", "ne",
    "sd", "ks", "ps", "tr", "az", "uz", "kk", "ky", "tg", "tk", "mn", "ug", "ru", "uk", "be", "bg", "sr", "hr", "sl", "sk", "cs", "pl", "de", "fr", "es",
    "pt", "it", "nl", "sv", "da", "no", "fi", "et", "lv", "lt", "hu", "ro", "el", "mt", "is", "ga", "cy", "eu", "ca", "gl", "he", "am", "ha", "yo", "ig",
    "zu", "xh", "so", "sw", "my", "km", "lo", "si", "jv", "su", "ceb", "tt", "ba", "ka", "hy", "mai", "bho", "brx", "kok", "doi", "mni",
];
/// Where Whisper Turbo is the pick: languages with no stronger model here.
const TURBO_PICKS: &[&str] = &["ko", "ar", "tr", "id", "he", "fa", "th", "ms", "tl", "ca"];
/// Indian languages where Omnilingual 1B scored best (Hindi has dedicated picks).
const INDIAN_NOT_HI: &[&str] = &["bn", "ta", "te", "kn", "ml", "mr", "gu", "pa", "ur", "or", "as", "ne"];

/// Scripts Whisper spells with many tokens per second of speech; see `transcribe_blocking`.
const TOKEN_HEAVY: &[&str] = &["hi", "bn", "ta", "te", "kn", "ml", "mr", "gu", "pa", "ur", "ne", "as", "or", "sa", "si", "my", "km", "lo", "th", "am", "ka", "hy", "bo"];

/// Our int8 ONNX export of Trelis/tara, pinned to the uploaded revision.
const TARA_HOST: &str = "https://huggingface.co/mtr1920/sherpa-onnx-whisper-tara-int8/resolve/fac80907a471779e9012c121df5a2464635f0da8";

const RELEASES: &str = "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models";

/// Only models whose licences allow commercial use: Moonshine's non-English models and some
/// NVIDIA ones (e.g. Portuguese FastConformer) are non-commercial and must not be added.
pub const MODELS: &[ModelInfo] = &[
    // English
    ModelInfo { id: "moonshine-tiny-en", name: "Moonshine Tiny", archive: "sherpa-onnx-moonshine-tiny-en-int8", size_mb: 103, languages: "English", description: "Fastest. Good for short English dictation on any CPU.", langs: EN, recommended: NONE, host: None, family: Family::Moonshine },
    ModelInfo { id: "moonshine-base-en", name: "Moonshine Base", archive: "sherpa-onnx-moonshine-base-en-int8", size_mb: 251, languages: "English", description: "More accurate English, still faster than real time on laptops.", langs: EN, recommended: NONE, host: None, family: Family::Moonshine },
    ModelInfo { id: "parakeet-110m-en", name: "Parakeet TDT 110M", archive: "sherpa-onnx-nemo-parakeet_tdt_transducer_110m-en-36000-int8", size_mb: 108, languages: "English", description: "Small, very fast English with punctuation and capitalization.", langs: EN, recommended: NONE, host: None, family: Family::NemoTransducer },
    ModelInfo { id: "parakeet-v2-en", name: "Parakeet TDT 0.6B v2", archive: "sherpa-onnx-nemo-parakeet-tdt-0.6b-v2-int8", size_mb: 482, languages: "English", description: "Very accurate English with punctuation and capitalization.", langs: EN, recommended: NONE, host: None, family: Family::NemoTransducer },
    ModelInfo { id: "parakeet-unified-en", name: "Parakeet Unified 0.6B", archive: "sherpa-onnx-nemo-parakeet-unified-en-0.6b-int8-non-streaming", size_mb: 501, languages: "English", description: "Newest NVIDIA English model (2026): best English accuracy, with punctuation and capitalization.", langs: EN, recommended: EN, host: None, family: Family::NemoTransducer },
    ModelInfo { id: "distil-whisper-v3.5", name: "Distil-Whisper Large v3.5", archive: "sherpa-onnx-whisper-distil-large-v3.5", size_mb: 528, languages: "English", description: "Whisper Large accuracy on English at a fraction of the cost; robust to accents and noise.", langs: EN, recommended: NONE, host: None, family: Family::Whisper("distil-large-v3.5") },
    ModelInfo { id: "canary-180m", name: "Canary 180M Flash", archive: "sherpa-onnx-nemo-canary-180m-flash-en-es-de-fr-int8", size_mb: 153, languages: "English, German, Spanish, French", description: "Small NVIDIA model with punctuation. Set your spoken language; it uses English otherwise.", langs: CANARY, recommended: NONE, host: None, family: Family::Canary },
    ModelInfo { id: "cohere-transcribe", name: "Cohere Transcribe 2B", archive: "sherpa-onnx-cohere-transcribe-14-lang-int8-2026-04-01", size_mb: 1699, languages: "14 languages", description: "Top of the 2026 Open ASR leaderboard. Large and slow on CPU; set your spoken language (English otherwise).", langs: COHERE, recommended: NONE, host: None, family: Family::Cohere },
    // European
    ModelInfo { id: "parakeet-v3", name: "Parakeet TDT 0.6B v3", archive: "sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8", size_mb: 487, languages: "25 European languages", description: "Multilingual European model with automatic language detection.", langs: EUROPEAN, recommended: NONE, host: None, family: Family::NemoTransducer },
    ModelInfo { id: "parakeet-ultra", name: "Parakeet Ultra 0.6B", archive: "sherpa-onnx-nemo-parakeet-tdt-0.6b-ultra-int8", size_mb: 487, languages: "25 European languages", description: "Moondream's retrained Parakeet v3 (2026): more accurate than v3 in every language tested.", langs: EUROPEAN, recommended: EUROPEAN_NOT_EN_RU, host: None, family: Family::NemoTransducer },
    ModelInfo { id: "fastconformer-de", name: "FastConformer German", archive: "sherpa-onnx-nemo-transducer-stt_de_fastconformer_hybrid_large_pc-int8", size_mb: 108, languages: "German", description: "Small dedicated German model with punctuation.", langs: &["de"], recommended: NONE, host: None, family: Family::NemoTransducer },
    ModelInfo { id: "fastconformer-es", name: "FastConformer Spanish", archive: "sherpa-onnx-nemo-fast-conformer-transducer-es-1424-int8", size_mb: 107, languages: "Spanish", description: "Small dedicated Spanish model.", langs: &["es"], recommended: NONE, host: None, family: Family::NemoTransducer },
    ModelInfo { id: "gigaam-v3-ru", name: "GigaAM v3", archive: "sherpa-onnx-nemo-transducer-punct-giga-am-v3-russian-2025-12-16", size_mb: 170, languages: "Russian", description: "Sber's Russian model with punctuation; preferred over Whisper Large v3 in blind tests.", langs: &["ru"], recommended: &["ru"], host: None, family: Family::NemoTransducer },
    // Many languages
    ModelInfo { id: "whisper-tiny", name: "Whisper Tiny", archive: "sherpa-onnx-whisper-tiny", size_mb: 116, languages: "99 languages", description: "Smallest multilingual Whisper: a quick way to try any language.", langs: WHISPER, recommended: NONE, host: None, family: Family::Whisper("tiny") },
    ModelInfo { id: "whisper-base", name: "Whisper Base", archive: "sherpa-onnx-whisper-base", size_mb: 208, languages: "99 languages", description: "Multilingual Whisper, small and quick.", langs: WHISPER, recommended: NONE, host: None, family: Family::Whisper("base") },
    ModelInfo { id: "whisper-small", name: "Whisper Small", archive: "sherpa-onnx-whisper-small", size_mb: 639, languages: "99 languages", description: "Multilingual Whisper with better accuracy.", langs: WHISPER, recommended: NONE, host: None, family: Family::Whisper("small") },
    ModelInfo { id: "whisper-turbo", name: "Whisper Large v3 Turbo", archive: "sherpa-onnx-whisper-turbo", size_mb: 564, languages: "99 languages", description: "Best speed-to-accuracy of the Whisper models; needs a fast CPU.", langs: WHISPER, recommended: TURBO_PICKS, host: None, family: Family::Whisper("turbo") },
    ModelInfo { id: "whisper-large-v3", name: "Whisper Large v3", archive: "sherpa-onnx-whisper-large-v3", size_mb: 1068, languages: "99 languages", description: "The most accurate Whisper, and the slowest: best on a fast desktop CPU.", langs: WHISPER, recommended: NONE, host: None, family: Family::Whisper("large-v3") },
    ModelInfo { id: "qwen3-asr", name: "Qwen3-ASR 0.6B", archive: "sherpa-onnx-qwen3-asr-0.6B-int8-2026-03-25", size_mb: 878, languages: "30 languages", description: "Alibaba's 2026 model: detects the language and adds punctuation. Slower on CPU.", langs: QWEN3, recommended: &["hi", "zh"], host: None, family: Family::Qwen3 },
    ModelInfo { id: "omnilingual-300m", name: "Omnilingual ASR 300M", archive: "sherpa-onnx-omnilingual-asr-1600-languages-300M-ctc-v2-int8-2026-02-05", size_mb: 292, languages: "1,600+ languages", description: "Meta's model for over 1,600 languages, including Indian and African languages most models miss.", langs: OMNILINGUAL, recommended: NONE, host: None, family: Family::Omnilingual },
    ModelInfo { id: "omnilingual-1b", name: "Omnilingual ASR 1B", archive: "sherpa-onnx-omnilingual-asr-1600-languages-1B-ctc-v2-int8-2026-02-05", size_mb: 787, languages: "1,600+ languages", description: "The larger, more accurate Omnilingual: about 1% character errors on Tamil, Telugu and Kannada in our tests.", langs: OMNILINGUAL, recommended: INDIAN_NOT_HI, host: None, family: Family::Omnilingual },
    // Asian languages
    ModelInfo { id: "tara", name: "Tara by Trelis", archive: "sherpa-onnx-whisper-tara-int8", size_mb: 1043, languages: "Hindi, English", description: "Trelis Research's 2026 Hindi model built on Whisper Large v3: state of the art on Hindi benchmarks and strong on English. Hinglish comes out in Devanagari. Slow on CPU.", langs: &["hi", "en"], recommended: &["hi"], host: Some(TARA_HOST), family: Family::Whisper("tara") },
    ModelInfo { id: "dolphin-small", name: "Dolphin Small", archive: "sherpa-onnx-dolphin-small-ctc-multi-lang-int8-2025-04-02", size_mb: 191, languages: "40 Asian languages", description: "Built for Asian languages: Hindi, Bengali, Tamil, Telugu, Marathi, Urdu, Thai and more. No English.", langs: DOLPHIN, recommended: NONE, host: None, family: Family::Dolphin },
    ModelInfo { id: "dolphin-base", name: "Dolphin Base", archive: "sherpa-onnx-dolphin-base-ctc-multi-lang-int8-2025-04-02", size_mb: 80, languages: "40 Asian languages", description: "Smaller, faster Dolphin.", langs: DOLPHIN, recommended: NONE, host: None, family: Family::Dolphin },
    ModelInfo { id: "sense-voice", name: "SenseVoice Small", archive: "sherpa-onnx-sense-voice-zh-en-ja-ko-yue-int8-2024-07-17", size_mb: 163, languages: "Chinese, English, Japanese, Korean, Cantonese", description: "Very fast multilingual model for East Asian languages.", langs: EAST_ASIAN, recommended: &["yue"], host: None, family: Family::SenseVoice },
    ModelInfo { id: "parakeet-ja", name: "Parakeet TDT-CTC 0.6B Japanese", archive: "sherpa-onnx-nemo-parakeet-tdt_ctc-0.6b-ja-35000-int8", size_mb: 489, languages: "Japanese", description: "NVIDIA's dedicated Japanese model with punctuation.", langs: &["ja"], recommended: &["ja"], host: None, family: Family::NemoCtc },
    ModelInfo { id: "zipformer-vi", name: "Zipformer Vietnamese", archive: "sherpa-onnx-zipformer-vi-int8-2025-04-20", size_mb: 60, languages: "Vietnamese", description: "Small dedicated Vietnamese model trained on 70,000 hours.", langs: &["vi"], recommended: &["vi"], host: None, family: Family::Zipformer("epoch-12-avg-8") },
];

pub fn info(id: &str) -> Option<&'static ModelInfo> {
    MODELS.iter().find(|m| m.id == id)
}

/// sherpa-onnx pads every Whisper clip with 10 s of silence, so a short clip costs nearly as much
/// as a long one: cutting a dictation into phrases only adds passes and loses context at every cut.
pub fn fixed_window(id: &str) -> bool {
    info(id).is_some_and(|m| matches!(m.family, Family::Whisper(_)))
}

/// Whether Whisper will spell this dictation with many tokens per second (see `transcribe_blocking`).
/// Tara is a Hindi model, so it counts unless the language is English.
fn token_heavy(m: &ModelInfo, language: &str) -> bool {
    TOKEN_HEAVY.contains(&language) || (m.id == "tara" && language != "en")
}

/// The longest audio, in seconds, a model gets in one pass. Whisper and the LLM-style decoders
/// see about 30 s, Omnilingual rejects clips over 40 s, Parakeet degrades past 20 s, and Whisper
/// writing a token-heavy script must stay short enough for its padded token budget.
pub fn max_segment_secs(id: &str, language: Option<&str>) -> usize {
    match info(id) {
        Some(m) if matches!(m.family, Family::Whisper(_)) && token_heavy(m, language.unwrap_or("")) => 10,
        Some(m) if matches!(m.family, Family::NemoTransducer) => 20,
        _ => 30,
    }
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
    pub recommended: &'static [&'static str],
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
    let (source, base) = match m.host {
        Some(host) => ("huggingface", host),
        None => ("github", RELEASES),
    };
    let url = format!("{base}/{}.tar.bz2", m.archive);
    let tmp = models_dir.join(format!("{}.tar.bz2.part", m.archive));
    let cancelled = || downloading().get(m.id) == Some(&true);
    fetch(source, &url, &tmp, m.size_mb, cancelled, progress).await?;
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

/// One loaded model at a time: (model id, language it was loaded for, recognizer).
static LOADED: Mutex<Option<(String, String, sherpa_onnx::OfflineRecognizer)>> = Mutex::new(None);

pub fn unload() {
    *LOADED.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

fn threads() -> i32 {
    std::thread::available_parallelism().map(|n| n.get().min(8) as i32).unwrap_or(4)
}

/// The recognizer settings for a model unpacked in `dir`. Fails when a file it needs is missing,
/// because the C library only reports "could not create" for a bad path.
fn config(dir: &Path, m: &ModelInfo, language: &str) -> Result<sherpa_onnx::OfflineRecognizerConfig> {
    use sherpa_onnx::*;
    let mut files = Vec::new();
    let mut p = |f: &str| {
        let path = dir.join(f);
        let s = path.to_string_lossy().into_owned();
        files.push(path);
        Some(s)
    };
    // Prefer the int8 file when a package ships both precisions.
    let int8 = |base: &str| if dir.join(format!("{base}.int8.onnx")).exists() { format!("{base}.int8.onnx") } else { format!("{base}.onnx") };
    let mut model = OfflineModelConfig { num_threads: threads(), provider: Some("cpu".into()), ..Default::default() };
    match m.family {
        Family::Moonshine => {
            model.moonshine = OfflineMoonshineModelConfig {
                preprocessor: p("preprocess.onnx"),
                encoder: p("encode.int8.onnx"),
                uncached_decoder: p("uncached_decode.int8.onnx"),
                cached_decoder: p("cached_decode.int8.onnx"),
                ..Default::default()
            };
            model.tokens = p("tokens.txt");
        }
        Family::Whisper(size) => {
            model.whisper = OfflineWhisperModelConfig {
                encoder: p(&int8(&format!("{size}-encoder"))),
                decoder: p(&int8(&format!("{size}-decoder"))),
                language: Some(language.to_string()),
                task: Some("transcribe".into()),
                ..Default::default()
            };
            model.tokens = p(&format!("{size}-tokens.txt"));
        }
        Family::SenseVoice => {
            model.sense_voice = OfflineSenseVoiceModelConfig {
                model: p("model.int8.onnx"),
                language: Some(if language.is_empty() { "auto".into() } else { language.to_string() }),
                use_itn: true,
            };
            model.tokens = p("tokens.txt");
        }
        Family::NemoTransducer => {
            model.transducer = OfflineTransducerModelConfig {
                encoder: p(&int8("encoder")),
                decoder: p(&int8("decoder")),
                joiner: p(&int8("joiner")),
            };
            model.tokens = p("tokens.txt");
            model.model_type = Some("nemo_transducer".into());
        }
        Family::Zipformer(suffix) => {
            // icefall ships an int8 decoder that loses accuracy; its examples use the float one.
            model.transducer = OfflineTransducerModelConfig {
                encoder: p(&int8(&format!("encoder-{suffix}"))),
                decoder: p(&format!("decoder-{suffix}.onnx")),
                joiner: p(&int8(&format!("joiner-{suffix}"))),
            };
            model.tokens = p("tokens.txt");
            model.model_type = Some("transducer".into());
        }
        Family::NemoCtc => {
            model.nemo_ctc = OfflineNemoEncDecCtcModelConfig { model: p(&int8("model")) };
            model.tokens = p("tokens.txt");
        }
        Family::Dolphin => {
            model.dolphin = OfflineDolphinModelConfig { model: p(&int8("model")) };
            model.tokens = p("tokens.txt");
        }
        Family::Omnilingual => {
            model.omnilingual = OfflineOmnilingualAsrCtcModelConfig { model: p(&int8("model")) };
            model.tokens = p("tokens.txt");
        }
        Family::Canary => {
            let lang = Some(if CANARY.contains(&language) { language } else { "en" }.to_string());
            model.canary = OfflineCanaryModelConfig {
                encoder: p(&int8("encoder")),
                decoder: p(&int8("decoder")),
                src_lang: lang.clone(),
                tgt_lang: lang,
                use_pnc: true,
            };
            model.tokens = p("tokens.txt");
        }
        Family::Qwen3 => {
            model.qwen3_asr = OfflineQwen3ASRModelConfig {
                conv_frontend: p("conv_frontend.onnx"),
                encoder: p(&int8("encoder")),
                decoder: p(&int8("decoder")),
                tokenizer: p("tokenizer"),
                // Room for a 30 s phrase of fast speech.
                max_total_len: 1024,
                max_new_tokens: 512,
                ..Default::default()
            };
        }
        Family::Cohere => {
            model.cohere_transcribe = OfflineCohereTranscribeModelConfig {
                encoder: p(&int8("encoder")),
                decoder: p(&int8("decoder")),
                language: Some(if COHERE.contains(&language) { language } else { "en" }.to_string()),
                use_punct: true,
                use_itn: true,
            };
            model.tokens = p("tokens.txt");
        }
    }
    if let Some(missing) = files.iter().find(|f| !f.exists()) {
        return Err(anyhow!("{} is incomplete: {} is missing. Delete it and download it again.", m.name, missing.display()));
    }
    Ok(OfflineRecognizerConfig { model_config: model, decoding_method: Some("greedy_search".into()), ..Default::default() })
}

fn load(dir: &Path, m: &ModelInfo, language: &str) -> Result<sherpa_onnx::OfflineRecognizer> {
    sherpa_onnx::OfflineRecognizer::create(&config(dir, m, language)?).ok_or_else(|| anyhow!("could not load {}", m.name))
}

/// Models whose output depends on the language they were loaded for.
fn language_bound(m: &ModelInfo) -> bool {
    matches!(m.family, Family::Whisper(_) | Family::SenseVoice | Family::Canary | Family::Cohere)
}

/// Blocking transcription with the given model (loads it if needed). Audio longer than the model
/// takes in one pass is cut at pauses and transcribed piece by piece.
pub fn transcribe_blocking(models_dir: &Path, id: &str, language: Option<&str>, audio: &[i16]) -> Result<String> {
    let m = info(id).ok_or_else(|| anyhow!("Unknown local model {id}"))?;
    if !is_installed(models_dir, m) {
        return Err(anyhow!("Local model \"{}\" is not downloaded yet (Settings → Local models)", m.name));
    }
    let lang = language.unwrap_or("").to_string();
    let mut guard = LOADED.lock().unwrap_or_else(|e| e.into_inner());
    let reload = match &*guard {
        Some((loaded_id, loaded_lang, _)) => loaded_id != id || (language_bound(m) && *loaded_lang != lang),
        None => true,
    };
    if reload {
        *guard = None;
        let started = std::time::Instant::now();
        *guard = Some((id.to_string(), lang.clone(), load(&model_dir(models_dir, m), m, &lang)?));
        log::info!("loaded local model {id} in {:?}", started.elapsed());
    }
    let recognizer = &guard.as_ref().expect("loaded").2;
    const RATE: usize = SAMPLE_RATE as usize;
    let max = RATE * max_segment_secs(id, language);
    let (mut rest, mut texts) = (audio, Vec::new());
    while rest.len() > max {
        let cut = crate::audio::split_point(&rest[..max], RATE * 6 / 10, max - RATE).filter(|&c| c > 0).unwrap_or(max);
        if !crate::audio::is_silent(&rest[..cut]) {
            texts.push(decode(recognizer, m, &lang, &rest[..cut]));
        }
        rest = &rest[cut..];
    }
    if texts.is_empty() || !crate::audio::is_silent(rest) {
        texts.push(decode(recognizer, m, &lang, rest));
    }
    Ok(texts.into_iter().filter(|t| !t.is_empty()).collect::<Vec<_>>().join(" "))
}

/// One pass of the recognizer over a clip the model can take whole.
fn decode(recognizer: &sherpa_onnx::OfflineRecognizer, m: &ModelInfo, lang: &str, audio: &[i16]) -> String {
    let mut samples: Vec<f32> = audio.iter().map(|s| *s as f32 / 32768.0).collect();
    // Cohere drops the last words of a clip that ends right after speech.
    if matches!(m.family, Family::Cohere) {
        samples.resize(samples.len() + SAMPLE_RATE as usize / 2, 0.0);
    }
    let run = |samples: &[f32]| {
        let stream = recognizer.create_stream();
        stream.accept_waveform(SAMPLE_RATE as i32, samples);
        recognizer.decode(&stream);
        stream.get_result()
    };
    // sherpa-onnx stops Whisper after 6 tokens per second of audio, which cuts off scripts that
    // need more (Hindi, Tamil, Telugu…). Trailing silence raises that budget: such languages start
    // with 3×, and any decode that used up its budget runs again with twice the audio.
    let whisper = matches!(m.family, Family::Whisper(_));
    let window = SAMPLE_RATE as usize * 30;
    if whisper && token_heavy(m, lang) {
        samples.resize((samples.len() * 3).min(window).max(samples.len()), 0.0);
    }
    let mut result = run(&samples);
    while whisper && samples.len() < window && result.as_ref().is_some_and(|r| r.tokens.len() + 1 >= samples.len() * 6 / SAMPLE_RATE as usize) {
        samples.resize((samples.len() * 2).min(window), 0.0);
        result = run(&samples);
    }
    result.map(|r| r.text.trim().to_string()).unwrap_or_default()
}

pub async fn transcribe(models_dir: &Path, id: &str, req: &Request<'_>) -> Result<String> {
    let (dir, id, lang, audio) = (models_dir.to_path_buf(), id.to_string(), req.language.map(String::from), req.audio.to_vec());
    tokio::task::spawn_blocking(move || transcribe_blocking(&dir, &id, lang.as_deref(), &audio)).await?
}

/// Preloads a model so the first dictation isn't delayed by model loading.
/// `language` must match what dictation will pass, or language-bound models load twice.
pub fn warm_up(models_dir: &Path, id: &str, language: Option<&str>) {
    if let Some(m) = info(id) {
        if is_installed(models_dir, m) {
            let _ = transcribe_blocking(models_dir, id, language, &vec![0i16; SAMPLE_RATE as usize / 2]);
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
            assert!(m.recommended.iter().all(|l| m.langs.contains(l)), "{} is recommended for a language it doesn't support", m.id);
        }
    }

    /// Character error rate, ignoring case, spaces and punctuation: fair across scripts without spaces.
    pub fn cer(reference: &str, hypothesis: &str) -> f64 {
        let keep = |s: &str| -> Vec<char> {
            s.to_lowercase().chars().filter(|c| !c.is_whitespace() && !c.is_ascii_punctuation() && !"।॥，。、？！：；「」『』“”‘’…–—¿¡«»".contains(*c)).collect()
        };
        let (r, h) = (keep(reference), keep(hypothesis));
        let mut prev: Vec<usize> = (0..=h.len()).collect();
        for (i, rc) in r.iter().enumerate() {
            let mut cur = vec![i + 1; h.len() + 1];
            for (j, hc) in h.iter().enumerate() {
                cur[j + 1] = (prev[j] + (rc != hc) as usize).min(prev[j + 1] + 1).min(cur[j] + 1);
            }
            prev = cur;
        }
        prev[h.len()] as f64 / r.len().max(1) as f64
    }

    /// Loads every downloaded model and transcribes its own sample clip, then scores every model on
    /// per-language clips (`<lang>.wav` with the reference text in `<lang>.txt`) for the languages it claims.
    /// OPSTRELLIS_TEST_MODELS=<models> OPSTRELLIS_LANG_WAVS=<clips> cargo test --release compare_models -- --ignored --nocapture
    #[test]
    #[ignore]
    fn compare_models() {
        let dir = PathBuf::from(std::env::var("OPSTRELLIS_TEST_MODELS").expect("OPSTRELLIS_TEST_MODELS"));
        // OPSTRELLIS_COMPARE=whisper-turbo,dolphin-small limits the run to those models.
        let only = std::env::var("OPSTRELLIS_COMPARE").ok();
        let installed: Vec<_> = MODELS
            .iter()
            .filter(|m| is_installed(&dir, m) && only.as_deref().is_none_or(|o| o.split(',').any(|id| id == m.id)))
            .collect();
        println!("\n| model | sample | text |\n|---|---|---|");
        for m in &installed {
            let wavs = model_dir(&dir, m).join("test_wavs");
            let mut clips: Vec<_> = std::fs::read_dir(&wavs).map(|d| d.filter_map(|e| e.ok().map(|e| e.path())).collect()).unwrap_or_default();
            clips.retain(|p| p.extension().is_some_and(|e| e == "wav") && !p.to_string_lossy().contains("8k"));
            clips.sort();
            let Some(clip) = clips.first() else { continue };
            let text = transcribe_blocking(&dir, m.id, None, &crate::audio::read_wav(clip).unwrap()).unwrap_or_else(|e| format!("ERROR {e:#}"));
            println!("| {} | {} | {} |", m.id, clip.file_name().unwrap().to_string_lossy(), text);
        }
        let Ok(clips) = std::env::var("OPSTRELLIS_LANG_WAVS") else { return };
        let clips = PathBuf::from(clips);
        let mut langs: Vec<String> = std::fs::read_dir(&clips).unwrap().filter_map(|e| e.ok()?.path().file_stem()?.to_str().map(String::from)).collect();
        langs.sort();
        langs.dedup();
        println!("\n| lang | model | CER | real-time factor | text |\n|---|---|---|---|---|");
        for lang in langs {
            let (Ok(audio), Ok(reference)) = (crate::audio::read_wav(&clips.join(format!("{lang}.wav"))), std::fs::read_to_string(clips.join(format!("{lang}.txt")))) else { continue };
            for m in installed.iter().filter(|m| m.langs.contains(&lang.as_str())) {
                let t = std::time::Instant::now();
                let r = transcribe_blocking(&dir, m.id, Some(&lang), &audio);
                let rtf = t.elapsed().as_secs_f64() / (audio.len() as f64 / SAMPLE_RATE as f64);
                match r {
                    Ok(text) => println!("| {lang} | {} | {:.1}% | {rtf:.3} | {text} |", m.id, 100.0 * cer(&reference, &text)),
                    Err(e) => println!("| {lang} | {} | ERROR | | {e:#} |", m.id),
                }
            }
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
