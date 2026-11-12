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
    /// SHA-256 of the archive, from GitHub's release asset digest. None: GitHub has no digest for
    /// that (older) upload, so it can't be checked.
    sha256: Option<&'static str>,
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
    ModelInfo { id: "moonshine-tiny-en", name: "Moonshine Tiny", archive: "sherpa-onnx-moonshine-tiny-en-int8", size_mb: 103, languages: "English", description: "Fastest. Good for short English dictation on any CPU.", langs: EN, recommended: NONE, host: None, family: Family::Moonshine, sha256: None },
    ModelInfo { id: "moonshine-base-en", name: "Moonshine Base", archive: "sherpa-onnx-moonshine-base-en-int8", size_mb: 251, languages: "English", description: "More accurate English, still faster than real time on laptops.", langs: EN, recommended: NONE, host: None, family: Family::Moonshine, sha256: None },
    ModelInfo { id: "parakeet-110m-en", name: "Parakeet TDT 110M", archive: "sherpa-onnx-nemo-parakeet_tdt_transducer_110m-en-36000-int8", size_mb: 108, languages: "English", description: "Small, very fast English with punctuation and capitalization.", langs: EN, recommended: NONE, host: None, family: Family::NemoTransducer, sha256: Some("f628312e9fdf8686374cb01a69425c41732529d540860311f16f37cbc32cfe9b") },
    ModelInfo { id: "parakeet-v2-en", name: "Parakeet TDT 0.6B v2", archive: "sherpa-onnx-nemo-parakeet-tdt-0.6b-v2-int8", size_mb: 482, languages: "English", description: "Very accurate English with punctuation and capitalization.", langs: EN, recommended: NONE, host: None, family: Family::NemoTransducer, sha256: Some("6fc4426b54aa549a0019b8a75b8dea59164e71790a37ac56a045ad57f915f714") },
    ModelInfo { id: "parakeet-unified-en", name: "Parakeet Unified 0.6B", archive: "sherpa-onnx-nemo-parakeet-unified-en-0.6b-int8-non-streaming", size_mb: 501, languages: "English", description: "Newest NVIDIA English model (2026): best English accuracy, with punctuation and capitalization.", langs: EN, recommended: EN, host: None, family: Family::NemoTransducer, sha256: Some("99f63605b3a85a54c250c0869670a687b7d6598a47bf2421515e1f839a76e150") },
    ModelInfo { id: "distil-whisper-v3.5", name: "Distil-Whisper Large v3.5", archive: "sherpa-onnx-whisper-distil-large-v3.5", size_mb: 528, languages: "English", description: "Whisper Large accuracy on English at a fraction of the cost; robust to accents and noise.", langs: EN, recommended: NONE, host: None, family: Family::Whisper("distil-large-v3.5"), sha256: Some("ec874c7346d24ef8063e05430ede616d66d80a410360283099d0bdf659187b1d") },
    ModelInfo { id: "canary-180m", name: "Canary 180M Flash", archive: "sherpa-onnx-nemo-canary-180m-flash-en-es-de-fr-int8", size_mb: 153, languages: "English, German, Spanish, French", description: "Small NVIDIA model with punctuation. Set your spoken language; it uses English otherwise.", langs: CANARY, recommended: NONE, host: None, family: Family::Canary, sha256: Some("7a38ed8b13f014ad632b09ff8d22e0c6f1359dd046af9235d281dfae841b9ab9") },
    ModelInfo { id: "cohere-transcribe", name: "Cohere Transcribe 2B", archive: "sherpa-onnx-cohere-transcribe-14-lang-int8-2026-04-01", size_mb: 1699, languages: "14 languages", description: "Top of the 2026 Open ASR leaderboard. Large and slow on CPU; set your spoken language (English otherwise).", langs: COHERE, recommended: NONE, host: None, family: Family::Cohere, sha256: Some("bd582588d50685a795dcd2807ab77e11361b8312d96c53884682def45ab4206d") },
    // European
    ModelInfo { id: "parakeet-v3", name: "Parakeet TDT 0.6B v3", archive: "sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8", size_mb: 487, languages: "25 European languages", description: "Multilingual European model with automatic language detection.", langs: EUROPEAN, recommended: NONE, host: None, family: Family::NemoTransducer, sha256: Some("1495e91b59d7fc53ddca91f3431758ec99f8fa253fdb0e1e851be0f72b14b0f9") },
    ModelInfo { id: "parakeet-ultra", name: "Parakeet Ultra 0.6B", archive: "sherpa-onnx-nemo-parakeet-tdt-0.6b-ultra-int8", size_mb: 487, languages: "25 European languages", description: "Moondream's retrained Parakeet v3 (2026): more accurate than v3 in every language tested.", langs: EUROPEAN, recommended: EUROPEAN_NOT_EN_RU, host: None, family: Family::NemoTransducer, sha256: Some("d216b71d7f628b3f2f2a20ee7ead39ff0b075984959eb4bf6ddf8dc8da737c09") },
    ModelInfo { id: "fastconformer-de", name: "FastConformer German", archive: "sherpa-onnx-nemo-transducer-stt_de_fastconformer_hybrid_large_pc-int8", size_mb: 108, languages: "German", description: "Small dedicated German model with punctuation.", langs: &["de"], recommended: NONE, host: None, family: Family::NemoTransducer, sha256: Some("73b3cce9f94a19fc04bc54b55abd576b622e3925a84ddd554a95e5984bc11677") },
    ModelInfo { id: "fastconformer-es", name: "FastConformer Spanish", archive: "sherpa-onnx-nemo-fast-conformer-transducer-es-1424-int8", size_mb: 107, languages: "Spanish", description: "Small dedicated Spanish model.", langs: &["es"], recommended: NONE, host: None, family: Family::NemoTransducer, sha256: Some("d6d29ffd560c366da1b177b71e79d8733a9a9910dd1ace282d9bfae94e056210") },
    ModelInfo { id: "gigaam-v3-ru", name: "GigaAM v3", archive: "sherpa-onnx-nemo-transducer-punct-giga-am-v3-russian-2025-12-16", size_mb: 170, languages: "Russian", description: "Sber's Russian model with punctuation; preferred over Whisper Large v3 in blind tests.", langs: &["ru"], recommended: &["ru"], host: None, family: Family::NemoTransducer, sha256: Some("f9620a0099019c6afcee26525ef9ed3297fa50dd5691c1902af0c948fc1a470b") },
    // Many languages
    ModelInfo { id: "whisper-tiny", name: "Whisper Tiny", archive: "sherpa-onnx-whisper-tiny", size_mb: 116, languages: "99 languages", description: "Smallest multilingual Whisper: a quick way to try any language.", langs: WHISPER, recommended: NONE, host: None, family: Family::Whisper("tiny"), sha256: None },
    ModelInfo { id: "whisper-base", name: "Whisper Base", archive: "sherpa-onnx-whisper-base", size_mb: 208, languages: "99 languages", description: "Multilingual Whisper, small and quick.", langs: WHISPER, recommended: NONE, host: None, family: Family::Whisper("base"), sha256: None },
    ModelInfo { id: "whisper-small", name: "Whisper Small", archive: "sherpa-onnx-whisper-small", size_mb: 639, languages: "99 languages", description: "Multilingual Whisper with better accuracy.", langs: WHISPER, recommended: NONE, host: None, family: Family::Whisper("small"), sha256: None },
    ModelInfo { id: "whisper-turbo", name: "Whisper Large v3 Turbo", archive: "sherpa-onnx-whisper-turbo", size_mb: 564, languages: "99 languages", description: "Best speed-to-accuracy of the Whisper models; needs a fast CPU.", langs: WHISPER, recommended: TURBO_PICKS, host: None, family: Family::Whisper("turbo"), sha256: None },
    ModelInfo { id: "whisper-large-v3", name: "Whisper Large v3", archive: "sherpa-onnx-whisper-large-v3", size_mb: 1068, languages: "99 languages", description: "The most accurate Whisper, and the slowest: best on a fast desktop CPU.", langs: WHISPER, recommended: NONE, host: None, family: Family::Whisper("large-v3"), sha256: None },
    ModelInfo { id: "qwen3-asr", name: "Qwen3-ASR 0.6B", archive: "sherpa-onnx-qwen3-asr-0.6B-int8-2026-03-25", size_mb: 878, languages: "30 languages", description: "Alibaba's 2026 model: detects the language and adds punctuation. Slower on CPU.", langs: QWEN3, recommended: &["hi", "zh"], host: None, family: Family::Qwen3, sha256: Some("393f8a14e2f5fb96746aaab342997a40641001fbd5bf9592a080a8329178ee96") },
    ModelInfo { id: "omnilingual-300m", name: "Omnilingual ASR 300M", archive: "sherpa-onnx-omnilingual-asr-1600-languages-300M-ctc-v2-int8-2026-02-05", size_mb: 292, languages: "1,600+ languages", description: "Meta's model for over 1,600 languages, including Indian and African languages most models miss.", langs: OMNILINGUAL, recommended: NONE, host: None, family: Family::Omnilingual, sha256: Some("951b32409aade32bd525310bb39e9666773ba3fc611a39e817f620936d76c631") },
    ModelInfo { id: "omnilingual-1b", name: "Omnilingual ASR 1B", archive: "sherpa-onnx-omnilingual-asr-1600-languages-1B-ctc-v2-int8-2026-02-05", size_mb: 787, languages: "1,600+ languages", description: "The larger, more accurate Omnilingual: about 1% character errors on Tamil, Telugu and Kannada in our tests.", langs: OMNILINGUAL, recommended: INDIAN_NOT_HI, host: None, family: Family::Omnilingual, sha256: Some("f4deae6e6cbf4ca785b89eaa3836156581208bf977ea2e6d7ae84d7efcfc3a40") },
    // Asian languages
    ModelInfo { id: "tara", name: "Tara by Trelis", archive: "sherpa-onnx-whisper-tara-int8", size_mb: 1043, languages: "Hindi, English", description: "Trelis Research's 2026 Hindi model built on Whisper Large v3: state of the art on Hindi benchmarks and strong on English. Hinglish comes out in Devanagari. Slow on CPU.", langs: &["hi", "en"], recommended: &["hi"], host: Some(TARA_HOST), family: Family::Whisper("tara"), sha256: Some("222520da0687a5847ff5157cf7cc7a9d218efdf44b3119e4c22732b6833f6efb") },
    ModelInfo { id: "dolphin-small", name: "Dolphin Small", archive: "sherpa-onnx-dolphin-small-ctc-multi-lang-int8-2025-04-02", size_mb: 191, languages: "40 Asian languages", description: "Built for Asian languages: Hindi, Bengali, Tamil, Telugu, Marathi, Urdu, Thai and more. No English.", langs: DOLPHIN, recommended: NONE, host: None, family: Family::Dolphin, sha256: None },
    ModelInfo { id: "dolphin-base", name: "Dolphin Base", archive: "sherpa-onnx-dolphin-base-ctc-multi-lang-int8-2025-04-02", size_mb: 80, languages: "40 Asian languages", description: "Smaller, faster Dolphin.", langs: DOLPHIN, recommended: NONE, host: None, family: Family::Dolphin, sha256: None },
    ModelInfo { id: "sense-voice", name: "SenseVoice Small", archive: "sherpa-onnx-sense-voice-zh-en-ja-ko-yue-int8-2024-07-17", size_mb: 163, languages: "Chinese, English, Japanese, Korean, Cantonese", description: "Very fast multilingual model for East Asian languages.", langs: EAST_ASIAN, recommended: &["yue"], host: None, family: Family::SenseVoice, sha256: Some("7d1efa2138a65b0b488df37f8b89e3d91a60676e416f515b952358d83dfd347e") },
    ModelInfo { id: "parakeet-ja", name: "Parakeet TDT-CTC 0.6B Japanese", archive: "sherpa-onnx-nemo-parakeet-tdt_ctc-0.6b-ja-35000-int8", size_mb: 489, languages: "Japanese", description: "NVIDIA's dedicated Japanese model with punctuation.", langs: &["ja"], recommended: &["ja"], host: None, family: Family::NemoCtc, sha256: Some("4b0a800ef29f4f4c8667339bf6f60d5bfdc2852ddc9dc5741aea65b6f8d1306b") },
    ModelInfo { id: "zipformer-vi", name: "Zipformer Vietnamese", archive: "sherpa-onnx-zipformer-vi-int8-2025-04-20", size_mb: 60, languages: "Vietnamese", description: "Small dedicated Vietnamese model trained on 70,000 hours.", langs: &["vi"], recommended: &["vi"], host: None, family: Family::Zipformer("epoch-12-avg-8"), sha256: None },
];

pub fn info(id: &str) -> Option<&'static ModelInfo> {
    MODELS.iter().find(|m| m.id == id)
}

/// sherpa-onnx pads every Whisper clip with 10 s of silence, so a short clip costs nearly as much
/// as a long one: cutting a dictation into phrases only adds passes and loses context at every cut.
fn fixed_window(id: &str) -> bool {
    info(id).is_some_and(|m| matches!(m.family, Family::Whisper(_)))
}

/// NVIDIA transducers give a timestamp per token and words that start with a space, and take
/// hotwords (with beam search).
fn has_words(m: &ModelInfo) -> bool {
    matches!(m.family, Family::NemoTransducer)
}

/// How a dictation with this model is cut into pieces (see `live`).
pub fn plan(id: &str, language: Option<&str>) -> super::live::Plan {
    const RATE: usize = SAMPLE_RATE as usize;
    let max = RATE * max_segment_secs(id, language);
    // Whisper: one pass on release unless the recording outgrows its window, except for scripts
    // that need short pieces to fit its token budget.
    // Moonshine decodes fast and loses words at every cut (it has no context), so its pieces are
    // long: 8 s scored best of 1–16 s in bench_live_dictation.
    let moonshine = info(id).is_some_and(|m| matches!(m.family, Family::Moonshine));
    let min_commit = if fixed_window(id) && max >= RATE * 30 {
        RATE * 20
    } else if moonshine {
        RATE * 8
    } else {
        super::live::MIN_COMMIT
    };
    let words = info(id).is_some_and(has_words);
    let (context, lookahead) = if words { (super::live::CONTEXT, super::live::LOOKAHEAD) } else { (0, 0) };
    let min_pause = if words { super::live::MIN_PAUSE } else { super::live::MIN_PAUSE_PLAIN };
    #[cfg(test)]
    let (min_commit, min_pause, context, lookahead) = {
        let ms = |var: &str, v: usize| std::env::var(var).ok().and_then(|s| s.parse::<usize>().ok()).map_or(v, |ms| ms * RATE / 1000);
        let w = |var: &str, v: usize| if words { ms(var, v) } else { 0 };
        (ms("OPSTRELLIS_MIN_COMMIT_MS", min_commit), ms("OPSTRELLIS_MIN_PAUSE_MS", min_pause), w("OPSTRELLIS_CONTEXT_MS", context), w("OPSTRELLIS_LOOKAHEAD_MS", lookahead))
    };
    super::live::Plan { min_commit, min_pause, max, context, lookahead }
}

/// Whether Whisper will spell this dictation with many tokens per second (see `transcribe_blocking`).
/// Tara is a Hindi model, so it counts unless the language is English.
fn token_heavy(m: &ModelInfo, language: &str) -> bool {
    TOKEN_HEAVY.contains(&language) || (m.id == "tara" && language != "en")
}

/// The longest audio, in seconds, a model gets in one pass. Whisper and the LLM-style decoders
/// see about 30 s, Omnilingual rejects clips over 40 s, Parakeet degrades past 20 s, and Whisper
/// writing a token-heavy script must stay short enough for its padded token budget.
fn max_segment_secs(id: &str, language: Option<&str>) -> usize {
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
/// `allow_unverified`: install even when the archive is no longer the one this version pinned.
pub async fn download(models_dir: &Path, id: &str, allow_unverified: bool, progress: impl Fn(u64, u64)) -> Result<()> {
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
    tokio::task::spawn_blocking(move || -> Result<()> {
        let installed = check_digest(&tmp, m, allow_unverified).and_then(|()| install(&tmp, &dest, m.archive, unpacked_limit(m)));
        let _ = std::fs::remove_file(&tmp);
        installed
    })
    .await??;
    Ok(())
}

/// The archive isn't the one this version of OpsTrellis pinned: it was replaced upstream since.
#[derive(Debug)]
pub struct Changed(pub &'static str);

impl std::fmt::Display for Changed {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "{} has changed on GitHub since this version of OpsTrellis was released, so it can't be verified. Download it anyway, or update OpsTrellis.", self.0)
    }
}

impl std::error::Error for Changed {}

fn check_digest(archive: &Path, m: &ModelInfo, allow_unverified: bool) -> Result<()> {
    let Some(expected) = m.sha256 else { return Ok(()) };
    if crate::polish::sha256_hex(archive)? == expected {
        return Ok(());
    }
    anyhow::ensure!(allow_unverified, Changed(m.name));
    log::warn!("installing {} although it doesn't match the pinned checksum (the user chose to)", m.id);
    Ok(())
}

/// Removes what an interrupted install left behind (unpack folders, partial downloads), except
/// for models downloading right now.
pub fn clean_staging(models_dir: &Path) {
    let busy: Vec<&str> = downloading().keys().filter_map(|id| info(id)).map(|m| m.archive).collect();
    for entry in std::fs::read_dir(models_dir).into_iter().flatten().flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(stem) = name.strip_prefix(".unpack-").or_else(|| name.strip_suffix(".tar.bz2.part")) else { continue };
        if busy.iter().any(|a| stem.starts_with(a)) {
            continue;
        }
        let path = entry.path();
        let _ = if path.is_dir() { std::fs::remove_dir_all(&path) } else { std::fs::remove_file(&path) };
    }
}

/// Room for what an archive may unpack to: model weights barely compress, so a few times the
/// download is plenty, and anything bigger is not a model.
fn unpacked_limit(m: &ModelInfo) -> u64 {
    m.size_mb as u64 * 4_000_000
}

/// Unpacks `archive` into a fresh folder in `models_dir` and moves only its `name` folder into
/// place, marked complete. Nothing else in `models_dir` (other models, the polish model) can be
/// touched, and on any failure nothing is left behind.
fn install(archive: &Path, models_dir: &Path, name: &str, limit: u64) -> Result<()> {
    // One install per model at a time (`DOWNLOADING`), so any folder of this model's is left from a killed one.
    let prefix = format!(".unpack-{name}-");
    for entry in std::fs::read_dir(models_dir)?.flatten() {
        if entry.file_name().to_string_lossy().starts_with(&prefix) {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
    let staging = models_dir.join(format!("{prefix}{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&staging)?;
    let result = (|| -> Result<()> {
        unpack(archive, &staging, name, limit).context("unpacking model")?;
        let unpacked = staging.join(name);
        anyhow::ensure!(std::fs::symlink_metadata(&unpacked).is_ok_and(|m| m.is_dir()), "the download doesn't contain the model folder {name}");
        std::fs::write(unpacked.join(".complete"), b"")?;
        let dest = models_dir.join(name);
        if dest.exists() {
            std::fs::remove_dir_all(&dest)?;
        }
        std::fs::rename(&unpacked, &dest)?;
        Ok(())
    })();
    let _ = std::fs::remove_dir_all(&staging);
    result
}

/// Everything in the archive, inside `dest` and `limit` bytes in all; links only within the `model`
/// folder. Anything else fails the install: a model with a missing part is never marked complete.
fn unpack(archive: &Path, dest: &Path, model: &str, limit: u64) -> Result<()> {
    const MAX_ENTRIES: usize = 10_000;
    let file = std::io::BufReader::new(std::fs::File::open(archive)?);
    let mut tar = tar::Archive::new(bzip2::read::BzDecoder::new(file));
    let (mut total, mut count) = (0u64, 0usize);
    for entry in tar.entries()? {
        let mut entry = entry?;
        let kind = entry.header().entry_type();
        // Archive-wide metadata, not a file.
        if kind.is_pax_global_extensions() {
            continue;
        }
        let path = entry.path()?.into_owned();
        if kind.is_symlink() || kind.is_hard_link() {
            let target = entry.link_name()?.map(|t| t.into_owned()).unwrap_or_default();
            anyhow::ensure!(link_inside(model, &path, &target, kind.is_symlink()), "the archive has a link out of the model folder ({})", path.display());
        } else {
            anyhow::ensure!(kind.is_file() || kind.is_dir(), "the archive has something that isn't a file or folder ({})", path.display());
        }
        count += 1;
        total = total.saturating_add(entry.size());
        anyhow::ensure!(total <= limit && count <= MAX_ENTRIES, "the archive is larger than a model can be");
        anyhow::ensure!(entry.unpack_in(dest)?, "the archive has a path outside its folder ({})", path.display());
    }
    Ok(())
}

/// A link at `path` (archive path) is kept only when it and what it points to stay inside the
/// `model` folder. A symlink's target is relative to its own folder, a hard link's to the archive.
fn link_inside(model: &str, path: &Path, target: &Path, symbolic: bool) -> bool {
    use std::path::Component;
    let normalize = |p: &Path| -> Option<Vec<String>> {
        let mut out = Vec::new();
        for c in p.components() {
            match c {
                Component::Normal(s) => out.push(s.to_string_lossy().into_owned()),
                Component::CurDir => {}
                Component::ParentDir => {
                    out.pop()?;
                }
                Component::RootDir | Component::Prefix(_) => return None,
            }
        }
        Some(out)
    };
    let inside = |p: Option<Vec<String>>| p.is_some_and(|p| p.len() > 1 && p[0] == model);
    let resolved = if symbolic { path.parent().map(|dir| dir.join(target)) } else { Some(target.to_path_buf()) };
    !target.as_os_str().is_empty() && inside(normalize(path)) && inside(resolved.as_deref().and_then(normalize))
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
        // Twice the listed size: room for a re-upload, not for filling the disk.
        let cap = size_mb as u64 * 2_000_000;
        anyhow::ensure!(known.is_none_or(|t| t <= cap), "the download is much larger than expected ({total} bytes)");
        let mut file = tokio::fs::File::create(tmp).await?;
        let mut stream = resp.bytes_stream();
        let (mut done, mut last_report) = (0u64, 0u64);
        while let Some(chunk) = unless_cancelled(&cancelled, stream.next()).await? {
            let chunk = chunk?;
            tokio::io::AsyncWriteExt::write_all(&mut file, &chunk).await?;
            done += chunk.len() as u64;
            anyhow::ensure!(done <= cap, "the download is much larger than expected (over {cap} bytes)");
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
            // Hotwords are optional: a model folder that can't take the file still loads.
            match bpe_vocab(dir) {
                Ok(vocab) => {
                    model.modeling_unit = Some("bpe".into());
                    model.bpe_vocab = Some(vocab.to_string_lossy().into_owned());
                }
                Err(e) => log::warn!("no hotwords for {}: {e:#}", m.id),
            }
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
    // Hotwords need beam search, which costs about 10% more than greedy with Parakeet.
    let beam = has_words(m);
    Ok(OfflineRecognizerConfig {
        model_config: model,
        decoding_method: Some(if beam { "modified_beam_search" } else { "greedy_search" }.into()),
        max_active_paths: 4,
        hotwords_score: hotwords_score(),
        ..Default::default()
    })
}

fn hotwords_score() -> f32 {
    #[cfg(test)]
    if let Some(score) = std::env::var("OPSTRELLIS_HOTWORDS_SCORE").ok().and_then(|s| s.parse().ok()) {
        return score;
    }
    HOTWORDS_SCORE
}

/// How strongly hotwords are preferred, per token. Stronger made Parakeet repeat a hotword
/// ("Claude Claude Claude"), write one for a similar word, or drop the rest of a sentence.
const HOTWORDS_SCORE: f32 = 1.0;

/// sherpa-onnx encodes hotwords with a SentencePiece vocabulary, which NeMo exports don't ship.
/// Their tokens.txt lists the BPE pieces in merge order, which is what the scores encode.
fn bpe_vocab(dir: &Path) -> Result<PathBuf> {
    let path = dir.join("bpe.vocab");
    if !path.exists() {
        let tokens = std::fs::read_to_string(dir.join("tokens.txt")).context("reading tokens.txt")?;
        let vocab: String = tokens.lines().filter_map(|l| l.rsplit_once(' ')).map(|(piece, id)| format!("{piece}\t-{id}\n")).collect();
        let tmp = dir.join("bpe.vocab.part");
        std::fs::write(&tmp, vocab)?;
        std::fs::rename(&tmp, &path)?;
    }
    Ok(path)
}

/// The vocabulary entries a transducer can be biased toward: words and names as they are spoken
/// and written alike. Code identifiers (`getUserById`, `snake_case`), paths and flags are left to
/// the text rules, which map their spoken words.
pub fn hotwords_for(models_dir: &Path, id: &str, vocabulary: &[String]) -> String {
    info(id).map(|m| hotwords(&model_dir(models_dir, m), m, vocabulary)).unwrap_or_default()
}

/// sherpa-onnx drops every hotword of a stream when one has a character no token spells
/// (`C#`, an accented name), so those are left out.
fn hotwords(dir: &Path, m: &ModelInfo, vocabulary: &[String]) -> String {
    const MAX: usize = 200;
    if !has_words(m) || bpe_vocab(dir).is_err() {
        return String::new();
    }
    let tokens = std::fs::read_to_string(dir.join("tokens.txt")).unwrap_or_default();
    let spelled: std::collections::HashSet<char> = tokens.lines().filter_map(|l| l.rsplit_once(' ')).flat_map(|(piece, _)| piece.chars()).collect();
    let word = |w: &str| {
        let first = w.chars().next().is_some_and(char::is_alphanumeric);
        let camel = w.starts_with(char::is_lowercase) && w.chars().any(char::is_uppercase);
        first && !camel && w.chars().any(char::is_alphabetic) && w.chars().all(|c| c.is_alphanumeric() || ".'+#&-".contains(c))
    };
    let speakable = |t: &&str| t.len() <= 40 && (1..=4).contains(&t.split_whitespace().count()) && t.split_whitespace().all(word) && t.chars().all(|c| c == ' ' || spelled.contains(&c));
    vocabulary.iter().map(|t| t.trim()).filter(speakable).take(MAX).collect::<Vec<_>>().join("\n")
}

fn load(dir: &Path, m: &ModelInfo, language: &str) -> Result<sherpa_onnx::OfflineRecognizer> {
    sherpa_onnx::OfflineRecognizer::create(&config(dir, m, language)?).ok_or_else(|| anyhow!("could not load {}", m.name))
}

/// Models whose output depends on the language they were loaded for.
fn language_bound(m: &ModelInfo) -> bool {
    matches!(m.family, Family::Whisper(_) | Family::SenseVoice | Family::Canary | Family::Cohere)
}

/// A word of a decoded clip.
pub struct Word {
    /// With its leading space; punctuation stays with the word before it.
    pub text: String,
    /// Seconds from the clip's start.
    pub start: f32,
}

#[derive(Default)]
pub struct Decoded {
    pub text: String,
    /// For models with word timestamps (`has_words`).
    pub words: Option<Vec<Word>>,
}

/// Groups tokens into words: a token that starts with a space starts a word.
fn group_words(tokens: &[String], timestamps: &[f32]) -> Vec<Word> {
    let mut out: Vec<Word> = Vec::new();
    for (t, &start) in tokens.iter().zip(timestamps) {
        match out.last_mut() {
            Some(w) if !t.starts_with(' ') => w.text.push_str(t),
            _ => out.push(Word { text: if t.starts_with(' ') { t.clone() } else { format!(" {t}") }, start }),
        }
    }
    out
}

/// Blocking transcription with the given model (loads it if needed), biased toward `vocabulary`
/// where the model can be. Audio longer than the model takes in one pass is cut at pauses and
/// transcribed piece by piece, with the same cuts as a live dictation.
pub fn transcribe_blocking(models_dir: &Path, id: &str, language: Option<&str>, vocabulary: &[String], audio: &[i16]) -> Result<String> {
    let m = info(id).ok_or_else(|| anyhow!("Unknown local model {id}"))?;
    let hot = hotwords(&model_dir(models_dir, m), m, vocabulary);
    let mut live = super::live::Live::new(plan(id, language));
    live.push(audio);
    live.finish(&mut |a: &[i16]| decode_blocking(models_dir, id, language, &hot, a))
}

/// One pass over a clip the model can take whole (loads the model if needed). `hotwords`: one
/// phrase per line, from `hotwords`.
pub fn decode_blocking(models_dir: &Path, id: &str, language: Option<&str>, hotwords: &str, audio: &[i16]) -> Result<Decoded> {
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
    Ok(decode(recognizer, m, &lang, hotwords, audio))
}

/// One pass of the recognizer over a clip the model can take whole.
fn decode(recognizer: &sherpa_onnx::OfflineRecognizer, m: &ModelInfo, lang: &str, hotwords: &str, audio: &[i16]) -> Decoded {
    let mut samples: Vec<f32> = audio.iter().map(|s| *s as f32 / 32768.0).collect();
    // Cohere drops the last words of a clip that ends right after speech.
    if matches!(m.family, Family::Cohere) {
        samples.resize(samples.len() + SAMPLE_RATE as usize / 2, 0.0);
    }
    let run = |samples: &[f32]| {
        let stream = if hotwords.is_empty() { recognizer.create_stream() } else { recognizer.create_stream_with_hotwords(hotwords) };
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
    let Some(r) = result else { return Decoded::default() };
    let words = r.timestamps.as_ref().filter(|t| has_words(m) && t.len() == r.tokens.len()).map(|t| group_words(&r.tokens, t));
    Decoded { text: r.text.trim().to_string(), words }
}

pub async fn transcribe(models_dir: &Path, id: &str, req: &Request<'_>) -> Result<String> {
    let (dir, id, lang, vocabulary, audio) = (models_dir.to_path_buf(), id.to_string(), req.language.map(String::from), req.vocabulary.to_vec(), req.audio.to_vec());
    tokio::task::spawn_blocking(move || transcribe_blocking(&dir, &id, lang.as_deref(), &vocabulary, &audio)).await?
}

/// Preloads a model so the first dictation isn't delayed by model loading.
/// `language` must match what dictation will pass, or language-bound models load twice.
pub fn warm_up(models_dir: &Path, id: &str, language: Option<&str>) {
    if let Some(m) = info(id) {
        if is_installed(models_dir, m) {
            let _ = decode_blocking(models_dir, id, language, "", &vec![0i16; SAMPLE_RATE as usize / 2]);
        }
    }
}

#[cfg(test)]
mod install_tests {
    use super::*;

    #[test]
    fn hotwords_are_names_the_model_can_spell() {
        let dir = temp("hotwords");
        std::fs::write(dir.join("tokens.txt"), "<unk> 0\n▁S 1\n▁Supa 2\nbase 3\nupabse 4\nio 5\nhan 6\n▁Vercel 7\n▁Next 8\n.js 9\n▁C 10\n").unwrap();
        let m = info("parakeet-unified-en").unwrap();
        let vocab: Vec<String> = ["Supabase", "C#", "getUserById", "--force", "~/projects", "Siobhán", "Next.js", "user_id", "Vercel"].map(String::from).to_vec();
        assert_eq!(hotwords(&dir, m, &vocab), "Supabase\nNext.js\nVercel");
        assert!(dir.join("bpe.vocab").exists());
        assert_eq!(hotwords(&dir, info("whisper-base").unwrap(), &vocab), "", "only transducers take hotwords");
    }

    fn temp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("opstrellis-models-{tag}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    enum E<'a> {
        File(&'a str, &'a [u8]),
        Symlink(&'a str, &'a str),
        /// A path the tar builder itself would refuse.
        Raw(&'a str),
    }

    fn archive(dir: &Path, entries: &[E]) -> PathBuf {
        let path = dir.join(format!("{}.tar.bz2", uuid::Uuid::new_v4()));
        let mut b = tar::Builder::new(bzip2::write::BzEncoder::new(std::fs::File::create(&path).unwrap(), bzip2::Compression::fast()));
        for e in entries {
            let mut h = tar::Header::new_gnu();
            h.set_mode(0o644);
            match e {
                E::File(name, data) => {
                    h.set_size(data.len() as u64);
                    b.append_data(&mut h, name, *data).unwrap();
                }
                E::Symlink(name, target) => {
                    h.set_entry_type(tar::EntryType::Symlink);
                    h.set_size(0);
                    b.append_link(&mut h, name, target).unwrap();
                }
                E::Raw(name) => {
                    h.as_old_mut().name[..name.len()].copy_from_slice(name.as_bytes());
                    h.set_size(3);
                    h.set_cksum();
                    b.append(&h, &b"bad"[..]).unwrap();
                }
            }
        }
        b.into_inner().unwrap().finish().unwrap();
        path
    }

    fn leftovers(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        names.sort();
        names
    }

    #[test]
    fn installs_only_the_model_folder() {
        let (work, models) = (temp("work"), temp("dir"));
        std::fs::create_dir_all(models.join("polish")).unwrap();
        std::fs::write(models.join("polish/s1-mini-q4_k_m.gguf"), "pinned").unwrap();
        std::fs::create_dir_all(models.join(".unpack-model-killed/model")).unwrap();
        let a = archive(&work, &[
            E::File("model/tokens.txt", b"tok"),
            E::File("model/sub/a.onnx", b"w"),
            E::Symlink("model/alias.onnx", "sub/a.onnx"),
            E::File("polish/s1-mini-q4_k_m.gguf", b"swapped"),
            E::File("stray.txt", b"x"),
        ]);
        install(&a, &models, "model", 1_000_000).unwrap();
        assert_eq!(std::fs::read(models.join("model/tokens.txt")).unwrap(), b"tok");
        assert_eq!(std::fs::read(models.join("model/alias.onnx")).unwrap(), b"w", "a link inside the model folder is kept");
        assert!(models.join("model/.complete").exists());
        assert_eq!(std::fs::read_to_string(models.join("polish/s1-mini-q4_k_m.gguf")).unwrap(), "pinned", "other files can't be overwritten");
        assert_eq!(leftovers(&models), ["model", "polish"], "no staging folders (also a killed install's) or stray files");
        install(&a, &models, "model", 1_000_000).unwrap();
        assert_eq!(leftovers(&models), ["model", "polish"], "installing again replaces the folder");
    }

    #[test]
    fn a_bad_archive_fails_and_leaves_nothing() {
        let (work, models) = (temp("work"), temp("dir"));
        let ok = E::File("model/tokens.txt", b"tok");
        for (entries, why) in [
            (vec![E::File("other/tokens.txt", b"tok")], "doesn't contain the model folder"),
            (vec![ok, E::Raw("../escape.txt")], "outside its folder"),
            (vec![E::File("model/tokens.txt", b"tok"), E::Symlink("model/passwd", "/etc/passwd")], "link out of the model folder"),
            (vec![E::File("model/tokens.txt", b"tok"), E::Symlink("model/up", "../../secret")], "link out of the model folder"),
            (vec![E::File("model/a.onnx", &[7u8; 5000])], "larger than a model"),
        ] {
            let a = archive(&work, &entries);
            let e = install(&a, &models, "model", 4000).unwrap_err();
            assert!(format!("{e:#}").contains(why), "{why}: {e:#}");
            assert!(leftovers(&models).is_empty(), "{why}");
        }
        assert!(!work.parent().unwrap().join("escape.txt").exists());
        let junk = work.join("junk.tar.bz2");
        std::fs::write(&junk, b"<!doctype html>not an archive").unwrap();
        assert!(install(&junk, &models, "model", 4000).is_err());
        assert!(leftovers(&models).is_empty());
    }

    #[test]
    fn links_stay_inside_the_model_folder() {
        let ok = |path: &str, target: &str, sym| link_inside("m", Path::new(path), Path::new(target), sym);
        assert!(ok("m/a", "b", true) && ok("m/sub/a", "../b", true) && ok("./m/a", "./sub/b", true));
        assert!(ok("m/a", "m/b", false), "a hard link names an archive path");
        assert!(!ok("m/a", "/etc/passwd", true) && !ok("m/a", "../x", true) && !ok("m/a", "..", true) && !ok("m/a", "", true));
        assert!(!ok("m/a", "b", false), "a hard link to b at the archive root");
        assert!(!ok("x/a", "../m/b", true), "the link itself must be in the model folder");
        assert!(!ok("m/a", "../../m/b", true));
    }

    #[test]
    fn a_changed_archive_needs_the_users_ok() {
        let dir = temp("sum");
        let file = dir.join("a");
        std::fs::write(&file, b"abc").unwrap();
        let mut m = ModelInfo { sha256: Some("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"), ..*info("parakeet-v2-en").unwrap() };
        assert!(check_digest(&file, &m, false).is_ok());
        m.sha256 = info("parakeet-v2-en").unwrap().sha256;
        let e = check_digest(&file, &m, false).unwrap_err();
        assert!(e.downcast_ref::<Changed>().is_some() && e.to_string().contains("has changed on GitHub") && !e.to_string().contains("try again"), "{e}");
        assert!(check_digest(&file, &m, true).is_ok(), "Download anyway");
        m.sha256 = None;
        assert!(check_digest(&file, &m, false).is_ok(), "unpinned");
        for m in MODELS {
            assert!(m.sha256.is_none_or(|d| d.len() == 64 && d.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))), "{}", m.id);
        }
    }

    #[test]
    fn leftovers_of_killed_installs_are_removed() {
        let models = temp("dir");
        std::fs::create_dir_all(models.join(".unpack-sherpa-onnx-whisper-base-1234/x")).unwrap();
        std::fs::write(models.join("sherpa-onnx-whisper-base.tar.bz2.part"), b"half").unwrap();
        std::fs::create_dir_all(models.join("sherpa-onnx-whisper-base")).unwrap();
        std::fs::create_dir_all(models.join("polish")).unwrap();
        clean_staging(&models);
        assert_eq!(leftovers(&models), ["polish", "sherpa-onnx-whisper-base"]);
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
        edits(&words(reference), &words(hypothesis))
    }

    /// Word error rate that also counts casing and punctuation (words as written, split at spaces).
    fn wer_written(reference: &str, hypothesis: &str) -> f64 {
        let split = |s: &str| s.split_whitespace().map(String::from).collect::<Vec<_>>();
        edits(&split(reference), &split(hypothesis))
    }

    fn edits(r: &[String], h: &[String]) -> f64 {
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
            let text = transcribe_blocking(&dir, m.id, None, &[], &crate::audio::read_wav(clip).unwrap()).unwrap_or_else(|e| format!("ERROR {e:#}"));
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
                let r = transcribe_blocking(&dir, m.id, Some(&lang), &[], &audio);
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
                    download(&dir, id, false, |_, _| {}).await.unwrap();
                } else {
                    continue;
                }
            }
            unload();
            let t = std::time::Instant::now();
            transcribe_blocking(&dir, id, Some("en"), &[], &vec![0; 8000]).unwrap();
            let load = t.elapsed();
            let (mut errs, mut audio_s, mut cpu_s) = (0.0, 0.0, 0.0);
            for (wav, reference) in &refs {
                let audio = crate::audio::read_wav(wav).unwrap();
                let t = std::time::Instant::now();
                let hyp = transcribe_blocking(&dir, id, Some("en"), &[], &audio).unwrap();
                cpu_s += t.elapsed().as_secs_f64();
                audio_s += audio.len() as f64 / SAMPLE_RATE as f64;
                errs += wer(reference, &hyp);
            }
            println!("| {} | {:.1}% | {:.3} | {:.1}s |", m.name, 100.0 * errs / refs.len() as f64, cpu_s / audio_s, load.as_secs_f64());
        }
    }

    fn terms_in(s: &str) -> Vec<String> {
        s.split_whitespace().map(|t| t.trim_start_matches(['"', '\'', '“', '‘', '(', '`']).trim_end_matches(['.', ',', ';', ':', '!', '?', '"', '\'', '”', '’', ')', '`']).to_string()).collect()
    }

    /// Whether `term` (one or more words) appears in `text` as whole tokens, case-sensitively.
    fn has_term(text: &[String], term: &str) -> bool {
        let t = terms_in(term);
        !t.is_empty() && text.windows(t.len()).any(|w| w == t.as_slice())
    }

    #[test]
    fn term_matching_is_whole_token_and_case_sensitive() {
        let raw = terms_in("Run kubectl get pods -n staging, then open `main.rs`.");
        assert!(has_term(&raw, "kubectl") && has_term(&raw, "-n") && has_term(&raw, "main.rs") && has_term(&raw, "get pods"));
        assert!(!has_term(&raw, "Kubectl") && !has_term(&raw, "main") && !has_term(&raw, "pod") && !has_term(&raw, "-"));
    }

    struct Clip {
        name: String,
        expected: String,
        terms: Vec<String>,
        raw: String,
        processed: String,
    }

    /// Developer-speech benchmark: for every installed local model, transcribes the WAVs made by
    /// desktop/bench/dev-speech/synthesize.sh and scores the raw transcript and the rule-based
    /// cleanup (docs/BENCHMARK.md).
    /// OPSTRELLIS_BENCH_DEV_WAVS=<dir> [OPSTRELLIS_BENCH_DEV_TSV=<sentences.tsv>] [OPSTRELLIS_TEST_MODELS=<dir>]
    /// [OPSTRELLIS_BENCH_VOCAB=terms] [OPSTRELLIS_HOTWORDS_SCORE=<score>]
    /// cargo test --release bench_dev_speech -- --ignored --nocapture
    fn test_models() -> PathBuf {
        PathBuf::from(std::env::var("OPSTRELLIS_TEST_MODELS").unwrap_or_else(|_| "/tmp/opstrellis-models".into()))
    }

    /// The sentences (expected text, terms) and the WAVs synthesize.sh rendered of them
    /// (name, path, sentence index), sorted by name.
    #[allow(clippy::type_complexity)]
    fn dev_clips() -> (Vec<(String, Vec<String>)>, Vec<(String, PathBuf, usize)>, PathBuf) {
        let wavs = PathBuf::from(std::env::var("OPSTRELLIS_BENCH_DEV_WAVS").expect("OPSTRELLIS_BENCH_DEV_WAVS: the directory written by synthesize.sh"));
        let tsv = std::env::var("OPSTRELLIS_BENCH_DEV_TSV").unwrap_or_else(|_| concat!(env!("CARGO_MANIFEST_DIR"), "/../bench/dev-speech/sentences.tsv").into());
        let rows: Vec<(String, Vec<String>)> = std::fs::read_to_string(&tsv)
            .unwrap_or_else(|e| panic!("{tsv}: {e}"))
            .lines()
            .skip(1)
            .map(|l| {
                let mut c = l.split('\t');
                let (_spoken, expected, terms) = (c.next(), c.next().expect("expected column"), c.next().unwrap_or(""));
                (expected.to_string(), terms.split('|').filter(|t| !t.is_empty()).map(String::from).collect())
            })
            .collect();
        let mut clips: Vec<(String, PathBuf, usize)> = std::fs::read_dir(&wavs)
            .unwrap_or_else(|e| panic!("{}: {e}", wavs.display()))
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "wav"))
            .map(|p| {
                let name = p.file_stem().unwrap().to_string_lossy().into_owned();
                let nn: usize = name.rsplit('-').next().and_then(|n| n.parse().ok()).unwrap_or_else(|| panic!("{name}: expected <voice>-<nn>.wav"));
                assert!((1..=rows.len()).contains(&nn), "{name}: no sentence {nn} in {tsv}");
                (name, p, nn - 1)
            })
            .collect();
        clips.sort();
        assert!(!clips.is_empty(), "no WAVs in {}", wavs.display());
        (rows, clips, wavs)
    }

    /// OPSTRELLIS_BENCH_VOCAB: "terms" (every term of the sentence file, like a dictionary of
    /// that size) or entries separated by `|`.
    fn bench_vocabulary(rows: &[(String, Vec<String>)]) -> Vec<String> {
        match std::env::var("OPSTRELLIS_BENCH_VOCAB").as_deref() {
            Ok("terms") => {
                let mut seen = std::collections::HashSet::new();
                rows.iter().flat_map(|r| r.1.clone()).filter(|t| seen.insert(t.clone())).collect()
            }
            Ok(list) => list.split('|').filter(|t| !t.is_empty()).map(String::from).collect(),
            Err(_) => Vec::new(),
        }
    }

    /// The options clean_up builds for a terminal or editor with AI off and the default cleanup settings.
    fn rules() -> crate::text::Options<'static> {
        crate::text::Options {
            remove_fillers: true,
            voice_commands: true,
            dictionary: &[],
            snippets: &[],
            tone: "none",
            english: true,
            technical: true,
            code: true,
            backticks: false,
            terms: true,
            preserve_formatting: false,
            clipboard: None,
            workspace: None,
            workspace_names: false,
            mentions: false,
        }
    }

    #[test]
    #[ignore]
    fn bench_dev_speech() {
        let models = test_models();
        let (rows, clips, wavs) = dev_clips();
        let opts = rules();
        let vocabulary = bench_vocabulary(&rows);

        println!("\n{} clips ({} sentences) from {}\n", clips.len(), rows.len(), wavs.display());
        println!("| model | clips | WER raw | WER after rules | terms raw | terms after rules | real-time factor |\n|---|---|---|---|---|---|---|");
        let mut details = Vec::new();
        for m in MODELS {
            if !is_installed(&models, m) {
                continue;
            }
            unload();
            transcribe_blocking(&models, m.id, Some("en"), &[], &vec![0; 8000]).unwrap();
            let (mut audio_s, mut cpu_s) = (0.0, 0.0);
            let (mut ref_words, mut err_raw, mut err_proc) = (0usize, 0.0, 0.0);
            let (mut terms, mut hit_raw, mut hit_proc) = (0usize, 0usize, 0usize);
            let mut scored: Vec<Clip> = Vec::new();
            for (name, wav, i) in &clips {
                let (expected, wanted) = &rows[*i];
                let audio = crate::audio::read_wav(wav).unwrap();
                let t = std::time::Instant::now();
                let raw = transcribe_blocking(&models, m.id, Some("en"), &vocabulary, &audio).unwrap();
                cpu_s += t.elapsed().as_secs_f64();
                audio_s += audio.len() as f64 / SAMPLE_RATE as f64;
                let processed = crate::text::process(&raw, &opts).text;
                let n = words(expected).len();
                ref_words += n;
                err_raw += wer(expected, &raw) * n as f64;
                err_proc += wer(expected, &processed) * n as f64;
                let (rt, pt) = (terms_in(&raw), terms_in(&processed));
                terms += wanted.len();
                hit_raw += wanted.iter().filter(|w| has_term(&rt, w)).count();
                hit_proc += wanted.iter().filter(|w| has_term(&pt, w)).count();
                scored.push(Clip { name: name.clone(), expected: expected.clone(), terms: wanted.clone(), raw, processed });
            }
            let pct = |a: f64, b: f64| 100.0 * a / b.max(1.0);
            println!(
                "| {} | {} | {:.1}% | {:.1}% | {:.1}% ({}/{}) | {:.1}% ({}/{}) | {:.3} |",
                m.name,
                clips.len(),
                pct(err_raw, ref_words as f64),
                pct(err_proc, ref_words as f64),
                pct(hit_raw as f64, terms as f64),
                hit_raw,
                terms,
                pct(hit_proc as f64, terms as f64),
                hit_proc,
                terms,
                cpu_s / audio_s
            );
            details.push((m.name, scored));
        }
        for (model, mut scored) in details {
            scored.sort_by(|a, b| wer(&b.expected, &b.processed).total_cmp(&wer(&a.expected, &a.processed)));
            println!("\nWorst 10 after rules, {model}:");
            for c in scored.iter().take(10) {
                println!("  {} (WER {:.0}%, terms {:?})\n    expected:  {}\n    raw:       {}\n    processed: {}", c.name, 100.0 * wer(&c.expected, &c.processed), c.terms, c.expected, c.raw.trim(), c.processed.trim());
            }
        }
    }

    fn level(frame: &[i16]) -> f64 {
        (frame.iter().map(|s| (*s as f64).powi(2)).sum::<f64>() / frame.len().max(1) as f64).sqrt()
    }

    /// `audio` with low-frequency-weighted noise added at `snr_db` below the speech level.
    fn with_noise(audio: &[i16], snr_db: f64) -> Vec<i16> {
        let loud: Vec<f64> = audio.chunks(800).map(level).filter(|l| *l >= 150.0).collect();
        let speech = (loud.iter().map(|l| l * l).sum::<f64>() / loud.len().max(1) as f64).sqrt();
        let (mut x, mut lp) = (0x2545_F491_4F6C_DD1Du64, 0.0f64);
        let noise: Vec<f64> = (0..audio.len())
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                lp = 0.85 * lp + 0.15 * ((x >> 11) as f64 / (1u64 << 53) as f64 * 2.0 - 1.0);
                lp
            })
            .collect();
        let rms = (noise.iter().map(|n| n * n).sum::<f64>() / noise.len().max(1) as f64).sqrt().max(1e-9);
        let gain = speech / 10f64.powf(snr_db / 20.0) / rms;
        audio.iter().zip(&noise).map(|(s, n)| (*s as f64 + n * gain).clamp(-32768.0, 32767.0) as i16).collect()
    }

    /// Words of the previous preview that the next one no longer starts with.
    fn retracted(previews: &[String]) -> usize {
        previews
            .windows(2)
            .map(|p| {
                let (a, b): (Vec<&str>, Vec<&str>) = (p[0].split_whitespace().collect(), p[1].split_whitespace().collect());
                a.len() - a.iter().zip(&b).take_while(|(x, y)| x == y).count()
            })
            .sum()
    }

    /// Runs `step(recorded)` as the recording thread does: it wakes `live::TICK` after its last step ended
    /// and handles what was recorded by then, until the key is released at the end of the audio.
    /// Returns the time spent in steps and how long the release waited for a step still running.
    fn live_clock(samples: usize, mut step: impl FnMut(usize)) -> (std::time::Duration, std::time::Duration) {
        const RATE: f64 = SAMPLE_RATE as f64;
        let (total, mut clock, mut busy) = (samples as f64 / RATE, 0.0, 0.0);
        loop {
            clock += super::super::live::TICK.as_secs_f64();
            if clock >= total {
                return (std::time::Duration::from_secs_f64(busy), std::time::Duration::ZERO);
            }
            let t = std::time::Instant::now();
            step((clock * RATE) as usize);
            let spent = t.elapsed().as_secs_f64();
            (clock, busy) = (clock + spent, busy + spent);
            if clock >= total {
                return (std::time::Duration::from_secs_f64(busy), std::time::Duration::from_secs_f64(clock - total));
            }
        }
    }

    /// The phrase-by-phrase loop `live` replaced, for comparison: a cut at the last pause of at
    /// least 0.6 s by loudness, a preview of the last 5 s, and the rest cut the same way on release.
    fn old_dictation(id: &str, audio: &[i16], preview: bool, decode: &mut dyn FnMut(&[i16]) -> String) -> (String, Vec<String>, std::time::Duration, std::time::Duration) {
        use crate::audio::{is_silent, split_point};
        const RATE: usize = SAMPLE_RATE as usize;
        let longest = RATE * max_segment_secs(id, Some("en"));
        let (mut committed, mut texts, mut previews, mut last_preview) = (0usize, Vec::new(), Vec::new(), 0usize);
        let (busy, wait) = live_clock(audio.len(), |end| {
            let pending = &audio[committed..end];
            let cut = split_point(&pending[..pending.len().min(longest)], RATE * 6 / 10, longest - RATE);
            let shown = if let Some(cut) = cut {
                if !is_silent(&pending[..cut]) {
                    texts.push(decode(&pending[..cut]));
                }
                committed += cut;
                texts.join(" ")
            } else if preview && end - last_preview >= RATE * 9 / 10 {
                last_preview = end;
                let tail = &pending[pending.len().saturating_sub(RATE * 5)..];
                if is_silent(tail) {
                    return;
                }
                let t = decode(tail);
                texts.iter().map(String::as_str).chain([t.as_str()]).collect::<Vec<_>>().join(" ")
            } else {
                return;
            };
            if !shown.trim().is_empty() {
                previews.push(shown.trim().to_string());
            }
        });
        let released = std::time::Instant::now();
        let mut rest = &audio[committed..];
        if !is_silent(rest) {
            let mut parts = Vec::new();
            while rest.len() > longest {
                let cut = split_point(&rest[..longest], RATE * 6 / 10, longest - RATE).filter(|&c| c > 0).unwrap_or(longest);
                if !is_silent(&rest[..cut]) {
                    parts.push(decode(&rest[..cut]));
                }
                rest = &rest[cut..];
            }
            if parts.is_empty() || !is_silent(rest) {
                parts.push(decode(rest));
            }
            texts.push(parts.join(" "));
        }
        (texts.join(" ").trim().to_string(), previews, busy, wait + released.elapsed())
    }

    fn new_dictation(id: &str, audio: &[i16], preview: bool, decode: &mut dyn FnMut(&[i16]) -> Result<Decoded>) -> (String, Vec<String>, std::time::Duration, std::time::Duration) {
        let mut live = super::super::live::Live::new(plan(id, Some("en")));
        let mut previews = Vec::new();
        let (busy, wait) = live_clock(audio.len(), |end| {
            live.push(&audio[live.len()..end]);
            if let Some(s) = live.tick(decode, preview).unwrap() {
                previews.push(s);
            }
        });
        let released = std::time::Instant::now();
        live.push(&audio[live.len()..]);
        let text = live.finish(decode).unwrap();
        (text, previews, busy, wait + released.elapsed())
    }

    /// Long dictations: five consecutive sentences of one voice from the developer-speech set,
    /// joined with pauses of 0.25–1.2 s, clean, quiet (-24 dB) and with noise at 10 dB SNR.
    /// Each is recorded live (as `live_clock` times it) through the old phrase-by-phrase
    /// loop and through `live::Live`, and scored on the final transcript, the words the preview
    /// took back, and the time from release to text. The preview runs for the first voice's clean
    /// dictations (OPSTRELLIS_BENCH_PREVIEW=all: every one, none: no previews).
    /// OPSTRELLIS_BENCH_DEV_WAVS=<dir> [OPSTRELLIS_BENCH_DEV_TSV=<sentences.tsv>] [OPSTRELLIS_BENCH_MODEL=parakeet-unified-en]
    /// [OPSTRELLIS_BENCH_VOCAB=terms|a|b] [OPSTRELLIS_BENCH_LOOPS=old,new,whole] [OPSTRELLIS_BENCH_VARIANTS=clean,quiet,noisy]
    /// [OPSTRELLIS_BENCH_PREFIX=<start of a dictation's expected text>] [OPSTRELLIS_BENCH_DETAIL=1: previews and regressions]
    /// cargo test --release bench_live_dictation -- --ignored --nocapture
    #[test]
    #[ignore]
    fn bench_live_dictation() {
        const RATE: usize = SAMPLE_RATE as usize;
        let models = test_models();
        let id = std::env::var("OPSTRELLIS_BENCH_MODEL").unwrap_or_else(|_| "parakeet-unified-en".into());
        let previews_for = std::env::var("OPSTRELLIS_BENCH_PREVIEW").unwrap_or_default();
        let (rows, clips, _) = dev_clips();
        let hot = hotwords_for(&models, &id, &bench_vocabulary(&rows));
        let opts = rules();
        let gaps = [0.3, 0.8, 0.45, 1.2, 0.25];
        let mut voices: std::collections::BTreeMap<String, Vec<(PathBuf, usize)>> = Default::default();
        for (name, path, i) in &clips {
            voices.entry(name.rsplit_once('-').unwrap().0.to_string()).or_default().push((path.clone(), *i));
        }
        let first = voices.keys().next().cloned().unwrap();
        let prefix = std::env::var("OPSTRELLIS_BENCH_PREFIX").unwrap_or_default();
        // (voice, expected, terms, audio)
        let mut dictations = Vec::new();
        for (voice, list) in &voices {
            for group in list.chunks(5) {
                let mut audio = Vec::new();
                for (k, (path, _)) in group.iter().enumerate() {
                    audio.extend(crate::audio::read_wav(path).unwrap());
                    // The key is released 0.4 s after the last word.
                    let gap = if k + 1 == group.len() { 0.4 } else { gaps[k % gaps.len()] };
                    audio.extend(vec![0i16; (gap * RATE as f64) as usize]);
                }
                let expected = group.iter().map(|(_, i)| rows[*i].0.as_str()).collect::<Vec<_>>().join(" ");
                let terms: Vec<String> = group.iter().flat_map(|(_, i)| rows[*i].1.clone()).collect();
                if expected.starts_with(&prefix) {
                    dictations.push((voice.clone(), expected, terms, audio));
                }
            }
        }
        assert!(!dictations.is_empty(), "no dictation starts with OPSTRELLIS_BENCH_PREFIX={prefix:?}");
        let audio_s: f64 = dictations.iter().map(|d| d.3.len() as f64 / RATE as f64).sum();
        println!("\n{} dictations ({:.0} s of audio, {:.1} s each on average), model {id}, {} hotwords\n", dictations.len(), audio_s, audio_s / dictations.len() as f64, hot.lines().count());
        println!("| loop | audio | WER raw | WER as written | WER after rules | terms after rules | preview words taken back | release → text (median / p95) | CPU while recording |\n|---|---|---|---|---|---|---|---|---|");
        decode_blocking(&models, &id, Some("en"), "", &vec![0; 8000]).unwrap();
        let variants: [(&str, fn(&[i16]) -> Vec<i16>); 3] = [("clean", |a| a.to_vec()), ("quiet", |a| a.iter().map(|s| s / 16).collect()), ("noisy", |a| with_noise(a, 10.0))];
        let only = |var: &str, all: &str| std::env::var(var).map_or(true, |v| v.split(',').any(|x| x == all));
        let detail = std::env::var("OPSTRELLIS_BENCH_DETAIL").is_ok();
        for (variant, transform) in variants.into_iter().filter(|(v, _)| only("OPSTRELLIS_BENCH_VARIANTS", v)) {
            let mut texts: std::collections::BTreeMap<&str, Vec<String>> = Default::default();
            for strategy in ["old", "new", "whole"].into_iter().filter(|s| only("OPSTRELLIS_BENCH_LOOPS", s)) {
                let mut err_written = 0.0;
                let (mut ref_words, mut err_raw, mut err_proc, mut terms, mut hits, mut back, mut previewed) = (0usize, 0.0, 0.0, 0usize, 0usize, 0usize, 0usize);
                let (mut releases, mut cpu) = (Vec::new(), 0.0);
                for (voice, expected, wanted, clean) in &dictations {
                    let audio = transform(clean);
                    let preview = match previews_for.as_str() {
                        "all" => true,
                        "none" => false,
                        _ => variant == "clean" && *voice == first,
                    };
                    let (text, previews, recording, release) = match strategy {
                        "old" => old_dictation(&id, &audio, preview, &mut |a| decode_blocking(&models, &id, Some("en"), "", a).unwrap().text),
                        "new" => new_dictation(&id, &audio, preview, &mut |a| decode_blocking(&models, &id, Some("en"), &hot, a)),
                        // One pass over the whole dictation on release: the most context, the longest wait.
                        _ => {
                            let t = std::time::Instant::now();
                            let text = decode_blocking(&models, &id, Some("en"), &hot, &audio).unwrap().text;
                            (text, Vec::new(), std::time::Duration::ZERO, t.elapsed())
                        }
                    };
                    if detail && preview && *voice == first && texts.get(strategy).is_none_or(|t| t.is_empty()) {
                        println!("\npreviews, {strategy}, first dictation:");
                        for p in &previews {
                            println!("  {p}");
                        }
                    }
                    texts.entry(strategy).or_default().push(text.clone());
                    let processed = crate::text::process(&text, &opts).text;
                    let n = words(expected).len();
                    ref_words += n;
                    err_raw += wer(expected, &text) * n as f64;
                    err_written += wer_written(expected, &text) * n as f64;
                    err_proc += wer(expected, &processed) * n as f64;
                    let pt = terms_in(&processed);
                    terms += wanted.len();
                    hits += wanted.iter().filter(|w| has_term(&pt, w)).count();
                    if preview {
                        back += retracted(&previews);
                        previewed += 1;
                    }
                    releases.push(release.as_secs_f64() * 1000.0);
                    cpu += recording.as_secs_f64();
                }
                releases.sort_by(f64::total_cmp);
                let pct = |a: f64, b: f64| 100.0 * a / b.max(1.0);
                let back = if previewed > 0 { format!("{:.1} per dictation", back as f64 / previewed as f64) } else { "-".into() };
                println!(
                    "| {strategy} | {variant} | {:.1}% | {:.1}% | {:.1}% | {:.1}% | {back} | {:.0} / {:.0} ms | {:.2} s per s |",
                    pct(err_raw, ref_words as f64),
                    pct(err_written, ref_words as f64),
                    pct(err_proc, ref_words as f64),
                    pct(hits as f64, terms as f64),
                    releases[releases.len() / 2],
                    releases[(releases.len() * 95 / 100).min(releases.len() - 1)],
                    cpu / audio_s
                );
            }
            if let (true, Some(old), Some(new)) = (detail, texts.get("old"), texts.get("new")) {
                let mut worst: Vec<usize> = (0..old.len()).collect();
                worst.sort_by(|&a, &b| (wer(&dictations[b].1, &new[b]) - wer(&dictations[b].1, &old[b])).total_cmp(&(wer(&dictations[a].1, &new[a]) - wer(&dictations[a].1, &old[a]))));
                println!("\nNew worse than old most, {variant}:");
                for &i in worst.iter().take(3) {
                    println!("  expected: {}\n  old ({:.0}%): {}\n  new ({:.0}%): {}", dictations[i].1, 100.0 * wer(&dictations[i].1, &old[i]), old[i], 100.0 * wer(&dictations[i].1, &new[i]), new[i]);
                }
            }
        }
    }
}
