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
    family: Family,
    /// SHA-256 of the archive, from GitHub's release asset digest. None: GitHub has no digest for
    /// that (older) upload, so it can't be checked.
    sha256: Option<&'static str>,
}

const RELEASES: &str = "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models";

pub const MODELS: &[ModelInfo] = &[
    ModelInfo { id: "moonshine-tiny-en", name: "Moonshine Tiny", archive: "sherpa-onnx-moonshine-tiny-en-int8", size_mb: 103, languages: "English", description: "Fastest. Good for short English dictation on any CPU.", family: Family::Moonshine, sha256: None },
    ModelInfo { id: "moonshine-base-en", name: "Moonshine Base", archive: "sherpa-onnx-moonshine-base-en-int8", size_mb: 251, languages: "English", description: "More accurate English, still faster than real time on laptops.", family: Family::Moonshine, sha256: None },
    ModelInfo { id: "parakeet-v2-en", name: "Parakeet TDT 0.6B v2", archive: "sherpa-onnx-nemo-parakeet-tdt-0.6b-v2-int8", size_mb: 482, languages: "English", description: "Very accurate English with punctuation and capitalization.", family: Family::Parakeet, sha256: Some("6fc4426b54aa549a0019b8a75b8dea59164e71790a37ac56a045ad57f915f714") },
    ModelInfo { id: "parakeet-unified-en", name: "Parakeet Unified 0.6B", archive: "sherpa-onnx-nemo-parakeet-unified-en-0.6b-int8-non-streaming", size_mb: 501, languages: "English", description: "Newest NVIDIA English model (2026): best English accuracy, with punctuation and capitalization.", family: Family::Parakeet, sha256: Some("99f63605b3a85a54c250c0869670a687b7d6598a47bf2421515e1f839a76e150") },
    ModelInfo { id: "parakeet-v3", name: "Parakeet TDT 0.6B v3", archive: "sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8", size_mb: 487, languages: "25 European languages", description: "Multilingual European model with automatic language detection.", family: Family::Parakeet, sha256: Some("1495e91b59d7fc53ddca91f3431758ec99f8fa253fdb0e1e851be0f72b14b0f9") },
    ModelInfo { id: "whisper-base", name: "Whisper Base", archive: "sherpa-onnx-whisper-base", size_mb: 208, languages: "99 languages", description: "Multilingual Whisper, small and quick.", family: Family::Whisper("base"), sha256: None },
    ModelInfo { id: "whisper-small", name: "Whisper Small", archive: "sherpa-onnx-whisper-small", size_mb: 639, languages: "99 languages", description: "Multilingual Whisper with better accuracy.", family: Family::Whisper("small"), sha256: None },
    ModelInfo { id: "whisper-turbo", name: "Whisper Large v3 Turbo", archive: "sherpa-onnx-whisper-turbo", size_mb: 564, languages: "99 languages", description: "Highest multilingual accuracy; needs a fast CPU.", family: Family::Whisper("turbo"), sha256: None },
    ModelInfo { id: "sense-voice", name: "SenseVoice Small", archive: "sherpa-onnx-sense-voice-zh-en-ja-ko-yue-int8-2024-07-17", size_mb: 163, languages: "Chinese, English, Japanese, Korean, Cantonese", description: "Very fast multilingual model for East Asian languages.", family: Family::SenseVoice, sha256: Some("7d1efa2138a65b0b488df37f8b89e3d91a60676e416f515b952358d83dfd347e") },
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
            downloading: DOWNLOADING.lock().unwrap_or_else(|e| e.into_inner()).contains(m.id),
            description: m.description,
        })
        .collect()
}

/// Downloads and unpacks a model; `progress(downloaded, total)` is called as bytes arrive.
/// Models currently downloading: two downloads of one model would write the same `.part` file.
static DOWNLOADING: Mutex<std::collections::BTreeSet<&'static str>> = Mutex::new(std::collections::BTreeSet::new());

struct DownloadGuard(&'static str);

impl Drop for DownloadGuard {
    fn drop(&mut self) {
        DOWNLOADING.lock().unwrap_or_else(|e| e.into_inner()).remove(self.0);
    }
}

/// `allow_unverified`: install even when the archive is no longer the one this version pinned.
pub async fn download(models_dir: &Path, id: &str, allow_unverified: bool, progress: impl Fn(u64, u64)) -> Result<()> {
    let m = info(id).ok_or_else(|| anyhow!("Unknown model {id}"))?;
    if !DOWNLOADING.lock().unwrap_or_else(|e| e.into_inner()).insert(m.id) {
        return Err(anyhow!("{} is already downloading", m.name));
    }
    let _guard = DownloadGuard(m.id);
    std::fs::create_dir_all(models_dir)?;
    let url = format!("{RELEASES}/{}.tar.bz2", m.archive);
    let tmp = models_dir.join(format!("{}.tar.bz2.part", m.archive));
    fetch("github", &url, &tmp, m.size_mb, progress).await?;
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
    let busy: Vec<&str> = DOWNLOADING.lock().unwrap_or_else(|e| e.into_inner()).iter().filter_map(|id| info(id)).map(|m| m.archive).collect();
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

/// Streams `url` into `tmp`, reporting progress about every megabyte. A failed or interrupted
/// download leaves nothing behind.
pub(crate) async fn fetch(source: &str, url: &str, tmp: &Path, size_mb: u32, progress: impl Fn(u64, u64)) -> Result<()> {
    let result = async {
        let resp = super::check(source, super::DOWNLOAD.get(url).timeout(std::time::Duration::from_secs(3600)).send().await?).await?;
        let known = resp.content_length();
        let total = known.unwrap_or(size_mb as u64 * 1_000_000);
        // Twice the listed size: room for a re-upload, not for filling the disk.
        let cap = size_mb as u64 * 2_000_000;
        anyhow::ensure!(known.is_none_or(|t| t <= cap), "the download is much larger than expected ({total} bytes)");
        let mut file = tokio::fs::File::create(tmp).await?;
        let mut stream = resp.bytes_stream();
        let (mut done, mut last_report) = (0u64, 0u64);
        while let Some(chunk) = stream.next().await {
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
mod install_tests {
    use super::*;

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
        let ids = ["moonshine-tiny-en", "moonshine-base-en", "parakeet-v2-en", "parakeet-unified-en", "whisper-base", "sense-voice"];
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
    /// cargo test --release bench_dev_speech -- --ignored --nocapture
    #[test]
    #[ignore]
    fn bench_dev_speech() {
        let models = PathBuf::from(std::env::var("OPSTRELLIS_TEST_MODELS").unwrap_or_else(|_| "/tmp/opstrellis-models".into()));
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

        // The options clean_up builds for a terminal or editor with AI off and the default cleanup settings.
        let opts = crate::text::Options {
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
        };

        println!("\n{} clips ({} sentences) from {}\n", clips.len(), rows.len(), wavs.display());
        println!("| model | clips | WER raw | WER after rules | terms raw | terms after rules | real-time factor |\n|---|---|---|---|---|---|---|");
        let mut details = Vec::new();
        for m in MODELS {
            if !is_installed(&models, m) {
                continue;
            }
            unload();
            transcribe_blocking(&models, m.id, Some("en"), &vec![0; 8000]).unwrap();
            let (mut audio_s, mut cpu_s) = (0.0, 0.0);
            let (mut ref_words, mut err_raw, mut err_proc) = (0usize, 0.0, 0.0);
            let (mut terms, mut hit_raw, mut hit_proc) = (0usize, 0usize, 0usize);
            let mut scored: Vec<Clip> = Vec::new();
            for (name, wav, i) in &clips {
                let (expected, wanted) = &rows[*i];
                let audio = crate::audio::read_wav(wav).unwrap();
                let t = std::time::Instant::now();
                let raw = transcribe_blocking(&models, m.id, Some("en"), &audio).unwrap();
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
}
