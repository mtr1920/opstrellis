//! Never lose a dictation. A recording whose transcription failed is kept in memory until it is
//! retried or the app quits. With `privacy.audioRetention` ("failed" or "all", History on) it is
//! also written to `<app data>/recordings/<history id>.wav` and kept for 24 hours; "all" keeps every
//! dictation's recording, and one interrupted by a quit or crash becomes a failed row at the next launch.

use crate::audio;
use crate::context::{self, AppContext};
use crate::db::HistoryItem;
use crate::pipeline::Mode;
use crate::settings::Settings;
use crate::App;
use anyhow::{anyhow, Result};
use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};
use tauri::Emitter;

pub const TTL: Duration = Duration::from_secs(24 * 60 * 60);
const MAX_KEPT: usize = 3;
const MAX_KEPT_SAMPLES: usize = 20 * 60 * audio::SAMPLE_RATE as usize;
const NOTHING_TO_RETRY: &str = "No failed dictation to retry.";
const BUSY: &str = "Finish the current dictation first.";
const INTERRUPTED: &str = "Interrupted before it was transcribed.";
const RATE: usize = audio::SAMPLE_RATE as usize;
/// Whisper hears only its first 30 s, other local models repeat themselves on long audio, and cloud
/// uploads are capped: a kept recording is sent in parts no longer than this.
const SEGMENT: usize = 30 * RATE;
const ENFORCE_EVERY: Duration = Duration::from_secs(60 * 60);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Retention {
    Off,
    Failed,
    All,
}

/// What may be written to disk. Nothing while History is off: a kept file belongs to its row.
pub fn disk_retention(s: &Settings) -> Retention {
    if s.privacy.history_retention_days == Some(0) {
        return Retention::Off;
    }
    match s.privacy.audio_retention.as_str() {
        "failed" => Retention::Failed,
        "all" => Retention::All,
        _ => Retention::Off,
    }
}

#[derive(Clone)]
struct Kept {
    seq: u64,
    history_id: Option<i64>,
    audio: Arc<Vec<i16>>,
    app_name: Option<String>,
    category: String,
    created_at: String,
}

/// The parts of a recording to transcribe one after another: at most 30 s each, cut at the last
/// pause (as while recording with a local model), silent parts left out.
fn segments(samples: &[i16]) -> Vec<std::ops::Range<usize>> {
    let (mut parts, mut start) = (Vec::new(), 0);
    while samples.len() - start > SEGMENT {
        let cut = audio::split_point(&samples[start..start + SEGMENT], RATE * 6 / 10, RATE * 29).filter(|c| *c > 0).unwrap_or(SEGMENT);
        parts.push(start..start + cut);
        start += cut;
    }
    parts.push(start..samples.len());
    parts.retain(|r| !audio::is_silent(&samples[r.clone()]));
    parts
}

/// Oldest first; never drops the only recording, however long.
fn evict(kept: &mut Vec<Kept>) {
    while kept.len() > 1 && (kept.len() > MAX_KEPT || kept.iter().map(|k| k.audio.len()).sum::<usize>() > MAX_KEPT_SAMPLES) {
        kept.remove(0);
    }
}

#[derive(Debug, PartialEq)]
enum Name {
    Saved(i64),
    Pending,
    Partial,
}

/// Only names this module creates: `<id>.wav`, `pending-<uuid>.wav`, and either plus `.tmp`.
fn parse_name(name: &str) -> Option<Name> {
    if let Some(inner) = name.strip_suffix(".tmp") {
        return parse_name(inner).filter(|n| *n != Name::Partial).map(|_| Name::Partial);
    }
    let stem = name.strip_suffix(".wav")?;
    if let Some(id) = stem.strip_prefix("pending-") {
        return uuid::Uuid::parse_str(id).ok().map(|_| Name::Pending);
    }
    if stem.is_empty() || !stem.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    stem.parse().ok().map(Name::Saved)
}

fn fresh(modified: SystemTime, now: SystemTime) -> bool {
    now.duration_since(modified).map_or(true, |age| age < TTL)
}

fn private_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

pub struct Recovery {
    dir: PathBuf,
    /// Pending files older than this were left by an earlier run.
    started: SystemTime,
    kept: Mutex<Vec<Kept>>,
    seq: AtomicU64,
}

/// The `all` mode recording being written while the dictation is processed.
pub struct PendingWrite(tauri::async_runtime::JoinHandle<Option<PathBuf>>);

pub enum Target {
    Row(i64),
    LastFailed,
}

impl Recovery {
    pub fn new(dir: PathBuf) -> Recovery {
        Recovery { dir, started: SystemTime::now(), kept: Mutex::new(Vec::new()), seq: AtomicU64::new(1) }
    }

    fn kept(&self) -> std::sync::MutexGuard<'_, Vec<Kept>> {
        self.kept.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn keep(&self, history_id: Option<i64>, audio: Arc<Vec<i16>>, ctx: &AppContext, created_at: &str) {
        let seq = self.seq.fetch_add(1, Ordering::SeqCst);
        let mut kept = self.kept();
        kept.push(Kept { seq, history_id, audio, app_name: ctx.app_name.clone(), category: ctx.category.to_string(), created_at: created_at.into() });
        evict(&mut kept);
    }

    fn path(&self, id: i64) -> PathBuf {
        self.dir.join(format!("{id}.wav"))
    }

    fn entries(&self) -> Vec<(PathBuf, Name, SystemTime)> {
        let Ok(dir) = std::fs::read_dir(&self.dir) else { return Vec::new() };
        dir.flatten()
            .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
            .filter_map(|e| {
                let name = parse_name(e.file_name().to_str()?)?;
                Some((e.path(), name, e.metadata().ok()?.modified().ok()?))
            })
            .collect()
    }

    fn on_disk(&self, id: i64, now: SystemTime) -> bool {
        std::fs::metadata(self.path(id)).and_then(|m| m.modified()).is_ok_and(|t| fresh(t, now))
    }

    /// History ids with a recording, in memory or on disk.
    pub fn available(&self) -> HashSet<i64> {
        let now = SystemTime::now();
        let mut ids: HashSet<i64> = self.kept().iter().filter_map(|k| k.history_id).collect();
        ids.extend(self.entries().into_iter().filter_map(|(_, name, t)| match name {
            Name::Saved(id) if fresh(t, now) => Some(id),
            _ => None,
        }));
        ids
    }

    /// Reads from disk if it isn't in memory: call off the async runtime.
    fn load(&self, id: i64) -> Option<Arc<Vec<i16>>> {
        if let Some(k) = self.kept().iter().rev().find(|k| k.history_id == Some(id)) {
            return Some(k.audio.clone());
        }
        if !self.on_disk(id, SystemTime::now()) {
            return None;
        }
        audio::read_wav(&self.path(id)).map_err(|e| log::warn!("reading a kept recording failed: {e:#}")).ok().map(Arc::new)
    }

    fn newest_unsaved(&self) -> Option<Kept> {
        self.kept().iter().rev().find(|k| k.history_id.is_none()).cloned()
    }

    fn drop_memory(&self, keep: impl Fn(&Kept) -> bool) {
        self.kept().retain(keep);
    }

    /// Its history row was deleted.
    pub fn forget(&self, id: i64) {
        self.drop_memory(|k| k.history_id != Some(id));
        self.delete_file(id);
    }

    fn delete_file(&self, id: i64) {
        let _ = std::fs::remove_file(self.path(id));
    }

    /// History was cleared.
    pub fn forget_all(&self) {
        self.kept().clear();
        self.clear_disk();
    }

    pub fn clear_disk(&self) {
        for (path, ..) in self.entries() {
            if let Err(e) = std::fs::remove_file(&path) {
                log::warn!("deleting a kept recording failed: {e}");
            }
        }
    }

    /// History was turned off: what is in memory stays retryable, without a row.
    fn detach_rows(&self) {
        for k in self.kept().iter_mut() {
            k.history_id = None;
        }
    }

    /// Drops recordings past their 24 hours and those of rows `keep_row` rejects (gone, or not
    /// failed while only failed ones are kept).
    fn purge(&self, now: SystemTime, keep_row: impl Fn(i64) -> bool) {
        let ids: Vec<i64> = self.kept().iter().filter_map(|k| k.history_id).collect();
        let gone: Vec<i64> = ids.into_iter().filter(|id| !keep_row(*id)).collect();
        if !gone.is_empty() {
            self.drop_memory(|k| !k.history_id.is_some_and(|id| gone.contains(&id)));
        }
        for (path, name, modified) in self.entries() {
            let remove = match name {
                Name::Saved(id) => !fresh(modified, now) || !keep_row(id),
                Name::Pending | Name::Partial => !fresh(modified, now),
            };
            if remove {
                let _ = std::fs::remove_file(path);
            }
        }
    }

    /// Written to a `.tmp` first, so a crash never leaves a truncated recording under a real name.
    fn write(&self, name: &str, samples: &[i16]) -> std::io::Result<PathBuf> {
        private_dir(&self.dir)?;
        let (path, tmp) = (self.dir.join(name), self.dir.join(format!("{name}.tmp")));
        let _ = std::fs::remove_file(&tmp);
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let written = options.open(&tmp).and_then(|mut f| {
            f.write_all(&audio::to_wav(samples))?;
            f.sync_all()
        });
        let result = written.and_then(|_| std::fs::rename(&tmp, &path));
        if result.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        result.map(|_| path)
    }

    fn save(&self, id: i64, samples: &[i16]) -> std::io::Result<PathBuf> {
        self.write(&format!("{id}.wav"), samples)
    }

    fn write_pending(&self, samples: &[i16]) -> std::io::Result<PathBuf> {
        self.write(&format!("pending-{}.wav", uuid::Uuid::new_v4()), samples)
    }

    /// Names a pending recording after its history row, or deletes it when there is none.
    fn attach(&self, pending: &Path, id: Option<i64>) -> bool {
        match id {
            Some(id) => std::fs::rename(pending, self.path(id)).map_err(|e| log::warn!("keeping the recording failed: {e}")).is_ok(),
            None => {
                let _ = std::fs::remove_file(pending);
                false
            }
        }
    }

    /// Pending recordings an earlier run never finished become failed rows (`add_row` → its id).
    fn recover_interrupted(&self, now: SystemTime, mut add_row: impl FnMut(SystemTime, i64) -> Option<i64>) -> usize {
        let mut recovered = 0;
        for (path, name, modified) in self.entries() {
            if modified >= self.started {
                continue;
            }
            match name {
                Name::Pending if fresh(modified, now) => {
                    let audio_ms = hound::WavReader::open(&path).map(|r| r.duration() as i64 * 1000 / audio::SAMPLE_RATE as i64);
                    let id = audio_ms.ok().and_then(|ms| add_row(modified, ms));
                    recovered += self.attach(&path, id) as usize;
                }
                Name::Partial => {
                    let _ = std::fs::remove_file(path);
                }
                _ => {}
            }
        }
        recovered
    }
}

/// Whether a recording of a row with `status` (None: the row is gone) is kept under `mode`.
fn keeps(mode: Retention, status: Option<&str>) -> bool {
    status.is_some_and(|s| mode == Retention::All || s == "error")
}

/// Claims the pipeline for a retry the way a finishing dictation does, so a press can't start a
/// recording in between; false while recording, processing or retrying.
fn reserve<T>(session: &Mutex<Option<T>>, busy: &AtomicBool) -> bool {
    let session = session.lock().unwrap_or_else(|e| e.into_inner());
    session.is_none() && busy.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst).is_ok()
}

impl App {
    /// "all": the recording is written while the dictation is processed, never delaying it.
    pub(crate) fn start_pending(self: &Arc<Self>, samples: &Arc<Vec<i16>>, settings: &Settings) -> Option<PendingWrite> {
        if disk_retention(settings) != Retention::All {
            return None;
        }
        let (app, samples) = (self.clone(), samples.clone());
        Some(PendingWrite(tauri::async_runtime::spawn_blocking(move || {
            app.recovery.write_pending(&samples).map_err(|e| log::warn!("keeping the recording failed: {e}")).ok()
        })))
    }

    /// Names the pending recording after the dictation's history row (deleted when none was saved).
    pub(crate) async fn attach_pending(&self, pending: Option<PendingWrite>, id: Option<i64>) {
        let Some(PendingWrite(task)) = pending else { return };
        let Ok(Some(path)) = task.await else { return };
        // The settings may have changed while it was written: decide with the current ones.
        let id = id.filter(|id| self.may_keep_on_disk(*id));
        if self.recovery.attach(&path, id) {
            if let Some(id) = id.filter(|id| !self.may_keep_on_disk(*id)) {
                self.recovery.delete_file(id);
            }
            let _ = self.handle.emit("history-updated", ());
        }
    }

    /// Whether row `id`'s recording may stay on disk under the current settings: "all" keeps every
    /// row's, "failed" only those of failed rows. A database error keeps it.
    fn may_keep_on_disk(&self, id: i64) -> bool {
        match disk_retention(&self.settings()) {
            Retention::Off => false,
            mode => self.keeps_row(mode, id),
        }
    }

    /// For memory as well as disk: a failed row's recording is always worth keeping, others only in "all".
    fn keeps_row(&self, mode: Retention, id: i64) -> bool {
        match self.db.get_history(id) {
            Err(_) => true,
            Ok(row) => keeps(mode, row.as_ref().map(|r| r.status.as_str())),
        }
    }

    async fn off_runtime<T: Send + 'static>(self: &Arc<Self>, f: impl FnOnce(&App) -> T + Send + 'static) -> Result<T> {
        let app = self.clone();
        Ok(tokio::task::spawn_blocking(move || f(&app)).await?)
    }

    /// Every provider failed: keep the recording for a retry and save the failure to History.
    pub(crate) async fn keep_failed(
        self: &Arc<Self>,
        error: anyhow::Error,
        samples: Arc<Vec<i16>>,
        pending: Option<PendingWrite>,
        ctx: &AppContext,
        audio_ms: i64,
        settings: &Settings,
    ) -> Result<()> {
        let message = format!("{error:#}");
        let item = HistoryItem {
            created_at: crate::db::now_rfc3339(),
            app_name: ctx.app_name.clone(),
            category: ctx.category.to_string(),
            audio_ms,
            status: "error".into(),
            error: Some(message.clone()),
            mode: "dictate".into(),
            ..Default::default()
        };
        let id = self.save_history(&item, settings);
        self.recovery.keep(id, samples.clone(), ctx, &item.created_at);
        match (pending, id) {
            (Some(p), _) => self.attach_pending(Some(p), id).await,
            // The current setting, not the one processing started with.
            (None, Some(id)) if disk_retention(&self.settings()) == Retention::Failed => {
                let saved = self.off_runtime(move |app| {
                    let saved = app.recovery.save(id, &samples);
                    // Switched to "Never" while it was written.
                    if !app.may_keep_on_disk(id) {
                        app.recovery.delete_file(id);
                    }
                    saved
                });
                if let Ok(Err(e)) = saved.await {
                    log::warn!("keeping the recording on disk failed: {e}");
                }
            }
            _ => {}
        }
        let _ = self.handle.emit("history-updated", ());
        self.toast_error(&message);
        let place = if id.is_some() { "History or the tray" } else { "the tray" };
        self.outcome("warn", format!("Couldn't transcribe. The recording is kept: retry from {place}."));
        Ok(())
    }

    /// Recordings past 24 hours, of deleted rows, or no longer allowed by the current settings are
    /// deleted. Blocking file work.
    pub(crate) fn enforce_recordings(&self) {
        let settings = self.settings();
        if settings.privacy.history_retention_days == Some(0) {
            self.recovery.detach_rows();
        }
        let mode = disk_retention(&settings);
        if mode == Retention::Off {
            self.recovery.clear_disk();
        }
        self.recovery.purge(SystemTime::now(), |id| self.keeps_row(mode, id));
    }

    /// Recordings also expire while the app sits idle in the tray.
    pub(crate) fn spawn_recordings_loop(self: &Arc<Self>) {
        let app = self.clone();
        tauri::async_runtime::spawn(async move {
            loop {
                tokio::time::sleep(ENFORCE_EVERY).await;
                let _ = app.off_runtime(|app| app.enforce_recordings()).await;
            }
        });
    }

    /// At launch: dictations a quit or crash interrupted become failed rows that can be retried.
    pub(crate) fn recover_interrupted(&self) {
        let settings = self.settings();
        if disk_retention(&settings) == Retention::Off {
            return;
        }
        let recovered = self.recovery.recover_interrupted(SystemTime::now(), |modified, audio_ms| {
            let item = HistoryItem {
                created_at: chrono::DateTime::<chrono::Local>::from(modified).to_rfc3339(),
                category: "other".into(),
                audio_ms,
                status: "error".into(),
                error: Some(INTERRUPTED.into()),
                mode: "dictate".into(),
                ..Default::default()
            };
            self.db.add_history(&item, true).unwrap_or_else(|e| {
                log::error!("saving an interrupted dictation failed: {e:#}");
                None
            })
        });
        if recovered > 0 {
            log::info!("recovered {recovered} interrupted dictation(s)");
        }
    }

    /// Tray: retry the newest failed dictation.
    pub fn retry_from_tray(self: &Arc<Self>) {
        if self.is_recording() || self.busy.load(Ordering::SeqCst) {
            self.outcome("warn", BUSY);
            return;
        }
        let app = self.clone();
        tauri::async_runtime::spawn(async move {
            if let Err(e) = app.retry(Target::LastFailed).await {
                log::warn!("retry from the tray: {e:#}");
            }
        });
    }

    /// Transcribes a kept recording again with the current providers and cleanup, saves it, and
    /// puts the text on the clipboard and in paste-last. The pill shows how it went.
    pub async fn retry(self: &Arc<Self>, target: Target) -> Result<String> {
        if !reserve(&self.session, &self.busy) {
            return Err(anyhow!(BUSY));
        }
        self.emit_stage(Mode::Dictate, "transcribing");
        let worker = self.clone();
        // Its own task, so a panic still ends here and clears `busy`.
        let result = match tauri::async_runtime::spawn(async move { worker.retry_inner(target).await }).await {
            Ok(r) => r,
            Err(e) => Err(anyhow!("Retry failed unexpectedly: {e}")),
        };
        if let Err(e) = &result {
            let message = format!("{e:#}");
            self.outcome(if message == NOTHING_TO_RETRY { "warn" } else { "error" }, message);
        }
        {
            let _session = self.session.lock().unwrap_or_else(|e| e.into_inner());
            self.busy.store(false, Ordering::SeqCst);
            self.emit_state("idle", Mode::Dictate, false);
        }
        result
    }

    fn last_failed_row(&self, settings: &Settings) -> Result<Option<i64>> {
        if settings.privacy.history_retention_days == Some(0) {
            return Ok(None);
        }
        let available = self.recovery.available();
        Ok(self.db.failed_dictation_ids(20)?.into_iter().find(|id| available.contains(id)))
    }

    async fn retry_inner(self: &Arc<Self>, target: Target) -> Result<String> {
        let settings = self.settings();
        let row = match target {
            Target::Row(id) => Some(id),
            Target::LastFailed => {
                let s = settings.clone();
                self.off_runtime(move |app| app.last_failed_row(&s)).await??
            }
        };
        let (old, samples, unsaved) = match row {
            Some(id) => {
                let Some(old) = self.db.get_history(id)? else {
                    self.off_runtime(move |app| app.recovery.forget(id)).await?;
                    return Err(anyhow!("This dictation was deleted."));
                };
                if old.mode != "dictate" {
                    return Err(anyhow!("Only dictations can be transcribed again."));
                }
                let samples = self.off_runtime(move |app| app.recovery.load(id)).await?.ok_or_else(|| anyhow!("The recording is no longer available."))?;
                (Some(old), samples, None)
            }
            None => {
                let k = self.recovery.newest_unsaved().ok_or_else(|| anyhow!(NOTHING_TO_RETRY))?;
                (None, k.audio.clone(), Some(k))
            }
        };
        let (app_name, category, created_at) = match (&old, &unsaved) {
            (Some(o), _) => (o.app_name.clone(), o.category.clone(), o.created_at.clone()),
            (None, Some(k)) => (k.app_name.clone(), k.category.clone(), k.created_at.clone()),
            (None, None) => (None, "other".into(), crate::db::now_rfc3339()),
        };
        let providers = self.providers(&settings);
        if providers.is_empty() {
            return Err(anyhow!("No transcription provider is enabled. Add one in Settings → Transcription."));
        }
        let vocabulary = self.vocabulary();
        let language = crate::pipeline::lang(&settings);
        // One part after another; any failure fails the whole retry and the recording stays kept.
        let (mut texts, mut used) = (Vec::new(), None);
        for part in segments(&samples) {
            let (text, p) = crate::pipeline::transcribe_with_fallback(&providers, None, &samples[part], language.as_deref(), &vocabulary).await?;
            used.get_or_insert(p);
            let text = crate::stt::clean_transcript(&text);
            if !text.trim().is_empty() {
                texts.push(text.trim().to_string());
            }
        }
        let raw = texts.join(" ");
        let Some(used) = used.filter(|_| !raw.is_empty()) else { return Err(anyhow!("Didn't catch any words.")) };
        let ctx = context::from_history(app_name.clone(), &category, &settings.app_categories);
        if settings.llm.enabled {
            self.emit_stage(Mode::Dictate, "polishing");
        }
        let cleaned = self.clean_up(&raw, &ctx, &settings, &vocabulary).await?;
        let text = cleaned.text;
        let item = HistoryItem {
            created_at,
            raw_text: raw,
            text: text.clone(),
            app_name,
            category,
            provider: Some(crate::pipeline::provider_name(used.entry.provider).into()),
            model: Some(used.entry.model.clone()),
            audio_ms: audio::duration_ms(&samples),
            words: crate::text::word_count(&text) as i64,
            fixes: cleaned.fixes as i64,
            status: "ok".into(),
            mode: "dictate".into(),
            ..Default::default()
        };
        let history_id = match &old {
            Some(old) => {
                if !self.db.retranscribed_history(old, &item)? {
                    let _ = self.handle.emit("history-updated", ());
                    return Err(anyhow!(if self.db.get_history(old.id)?.is_none() {
                        "This dictation was deleted."
                    } else {
                        "The dictation changed while it was transcribed, so it was left as it is."
                    }));
                }
                Some(old.id)
            }
            None => {
                // History is off: only the aggregates, as for any dictation.
                if let Err(e) = self.db.add_history(&item, false) {
                    log::warn!("recording insights for a retry failed: {e:#}");
                }
                None
            }
        };
        // A dictation already counted ("transcribe again" on an ok row) isn't counted twice.
        if !old.as_ref().is_some_and(|o| o.status == "ok") {
            let terms = if settings.cleanup.terms_on() { crate::terms::find(&text) } else { Vec::new() };
            if let Err(e) = self.db.record_usage(&cleaned.used, &terms) {
                log::warn!("recording dictionary usage failed: {e:#}");
            }
            self.queue_usage(&item, used.entry.provider, &settings);
        }
        let (mode, seq) = (disk_retention(&settings), unsaved.as_ref().map(|k| k.seq));
        self.off_runtime(move |app| match (mode, history_id) {
            (Retention::All, Some(id)) => {
                let on_disk = app.recovery.on_disk(id, SystemTime::now())
                    || app.recovery.save(id, &samples).map_err(|e| log::warn!("keeping the recording on disk failed: {e}")).is_ok();
                if on_disk {
                    app.recovery.drop_memory(|k| k.history_id != Some(id));
                }
            }
            (_, Some(id)) => app.recovery.forget(id),
            (_, None) => app.recovery.drop_memory(|k| Some(k.seq) != seq),
        })
        .await?;
        *self.last_text.lock().unwrap() = Some(crate::LastTranscript { text: text.clone(), history_id });
        let _ = self.handle.emit("history-updated", ());
        let done = if old.as_ref().is_some_and(|o| o.status == "ok") { "Transcribed again" } else { "Recovered" };
        match self.inserter.set_clipboard(&text) {
            Ok(()) => self.outcome("ok", format!("{done} · copied to clipboard")),
            Err(e) => {
                log::warn!("clipboard unavailable after a retry: {e:#}");
                self.outcome("warn", format!("{done}. Use Paste last transcript to insert it."));
            }
        }
        Ok(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("opstrellis-recovery-{tag}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn ctx() -> AppContext {
        context::from_history(Some("Slack".into()), "work_messages", &Default::default())
    }

    fn secs(n: usize) -> Arc<Vec<i16>> {
        Arc::new(vec![1; n * audio::SAMPLE_RATE as usize])
    }

    fn ids(r: &Recovery) -> Vec<Option<i64>> {
        r.kept().iter().map(|k| k.history_id).collect()
    }

    fn age(path: &Path, by: Duration) {
        std::fs::File::options().write(true).open(path).unwrap().set_modified(SystemTime::now() - by).unwrap();
    }

    fn tone(secs: f64) -> Vec<i16> {
        (0..(secs * RATE as f64) as usize).map(|i| ((i as f64 * 0.05).sin() * 3000.0) as i16).collect()
    }

    fn quiet(secs: f64) -> Vec<i16> {
        vec![0; (secs * RATE as f64) as usize]
    }

    fn assert_parts(parts: &[std::ops::Range<usize>], len: usize) {
        assert!(parts.iter().all(|p| !p.is_empty() && p.len() <= SEGMENT), "{parts:?}");
        assert!(parts.windows(2).all(|w| w[0].end <= w[1].start), "in order, no overlap: {parts:?}");
        assert!(parts.last().is_some_and(|p| p.end <= len));
    }

    #[test]
    fn long_recordings_are_sent_in_parts_cut_at_pauses() {
        // 3 minutes: 9 s of speech, then a 1 s pause, 18 times.
        let audio: Vec<i16> = (0..18).flat_map(|_| [tone(9.0), quiet(1.0)].concat()).collect();
        let parts = segments(&audio);
        assert_parts(&parts, audio.len());
        assert_eq!(parts.first().map(|p| p.start), Some(0));
        assert!(parts.len() >= 6, "{} parts", parts.len());
        for p in &parts[..parts.len() - 1] {
            let in_pause = (p.end % (10 * RATE)) as f64 / RATE as f64;
            assert!((9.0..10.0).contains(&in_pause), "cut at {in_pause} s into a 10 s phrase, not in its pause");
        }
        assert!(parts.windows(2).all(|w| w[0].end == w[1].start), "nothing between parts is lost");
        assert_eq!(parts.last().unwrap().end, audio.len());

        // No pause at all: still at most 30 s each, back to back over the whole recording.
        let audio = tone(95.0);
        let parts = segments(&audio);
        assert_parts(&parts, audio.len());
        assert_eq!((parts.len(), parts[0].start, parts.last().unwrap().end), (4, 0, audio.len()));
        assert!(parts.windows(2).all(|w| w[0].end == w[1].start));

        // Up to 30 s: one call, as before.
        let audio = tone(30.0);
        assert_eq!(segments(&audio), vec![0..audio.len()]);
        assert_eq!(segments(&tone(4.0)), vec![0..4 * RATE]);

        // A trailing silent part isn't sent; all silence sends nothing.
        let audio = [tone(29.0), quiet(41.0)].concat();
        let parts = segments(&audio);
        assert_eq!(parts.len(), 1, "{parts:?}");
        assert!(parts[0].start == 0 && (29 * RATE..30 * RATE).contains(&parts[0].end));
        assert!(segments(&quiet(70.0)).is_empty());

        // The 30-minute cap of a hands-free recording.
        let audio = tone(30.0 * 60.0);
        let parts = segments(&audio);
        assert_parts(&parts, audio.len());
        assert_eq!(parts.last().unwrap().end, audio.len());
    }

    #[test]
    fn memory_keeps_three_and_twenty_minutes_but_never_drops_the_only_one() {
        let r = Recovery::new(temp_dir("caps"));
        for id in 1..=4 {
            r.keep(Some(id), secs(5), &ctx(), "2026-11-11T10:00:00+00:00");
        }
        assert_eq!(ids(&r), vec![Some(2), Some(3), Some(4)], "at most three, oldest dropped");
        r.keep(Some(5), secs(20 * 60 - 5), &ctx(), "2026-11-11T10:00:00+00:00");
        assert_eq!(ids(&r), vec![Some(4), Some(5)], "at most 20 minutes in total");
        r.keep(Some(6), secs(30 * 60), &ctx(), "2026-11-11T10:00:00+00:00");
        assert_eq!(ids(&r), vec![Some(6)], "a 30-minute recording alone is kept");
        assert_eq!(r.load(6).unwrap().len(), 30 * 60 * audio::SAMPLE_RATE as usize);
    }

    #[test]
    fn purge_drops_expired_and_orphaned_recordings() {
        let dir = temp_dir("purge");
        let r = Recovery::new(dir.clone());
        let a = [7i16; 4000];
        let one = r.save(1, &a).unwrap();
        r.save(2, &a).unwrap();
        let three = r.save(3, &a).unwrap();
        age(&three, TTL + Duration::from_secs(60));
        let pending = r.write_pending(&a).unwrap();
        let old_pending = r.write_pending(&a).unwrap();
        age(&old_pending, TTL * 2);
        std::fs::write(dir.join("notes.txt"), "not ours").unwrap();
        r.keep(Some(2), Arc::new(a.to_vec()), &ctx(), "2026-11-11T10:00:00+00:00");
        r.keep(Some(1), Arc::new(a.to_vec()), &ctx(), "2026-11-11T10:00:00+00:00");
        r.purge(SystemTime::now(), |id| id != 2);
        assert!(one.exists() && pending.exists());
        assert!(!dir.join("2.wav").exists(), "its row is gone");
        assert!(!three.exists() && !old_pending.exists(), "past 24 hours");
        assert!(dir.join("notes.txt").exists(), "files it didn't write are left alone");
        assert_eq!(ids(&r), vec![Some(1)]);
        assert_eq!(r.available(), HashSet::from([1]));
    }

    #[test]
    fn recordings_load_from_memory_or_disk_until_they_expire() {
        let r = Recovery::new(temp_dir("load"));
        let a: Vec<i16> = (0..8000).map(|i| (i % 300) as i16 - 150).collect();
        let path = r.save(9, &a).unwrap();
        assert_eq!(*r.load(9).unwrap(), a);
        assert_eq!(r.available(), HashSet::from([9]));
        age(&path, TTL);
        assert!(r.load(9).is_none() && r.available().is_empty(), "expired files are not offered");
        r.keep(Some(9), Arc::new(a.clone()), &ctx(), "2026-11-11T10:00:00+00:00");
        assert_eq!(*r.load(9).unwrap(), a, "memory wins");
        r.forget(9);
        assert!(r.load(9).is_none() && !path.exists());
        assert!(r.load(10).is_none());
    }

    #[test]
    fn switching_off_or_history_off_clears_the_disk_but_keeps_memory() {
        let r = Recovery::new(temp_dir("off"));
        let a = [3i16; 4000];
        r.save(1, &a).unwrap();
        r.write_pending(&a).unwrap();
        r.keep(Some(1), Arc::new(a.to_vec()), &ctx(), "2026-11-11T10:00:00+00:00");
        r.clear_disk();
        assert!(r.entries().is_empty());
        assert_eq!(r.available(), HashSet::from([1]), "still in memory");
        assert!(r.newest_unsaved().is_none());
        r.detach_rows();
        let k = r.newest_unsaved().expect("retryable without a row");
        assert_eq!((k.history_id, k.app_name.as_deref(), k.category.as_str()), (None, Some("Slack"), "work_messages"));
        r.forget_all();
        assert!(r.newest_unsaved().is_none());
    }

    #[test]
    fn a_failed_disk_write_leaves_memory_working() {
        let dir = temp_dir("fail");
        let blocked = dir.join("recordings");
        std::fs::write(&blocked, "a file where the folder should be").unwrap();
        let r = Recovery::new(blocked.clone());
        assert!(r.save(1, &[1; 100]).is_err());
        assert!(r.write_pending(&[1; 100]).is_err());
        assert!(!r.attach(&blocked.join("pending-x.wav"), Some(1)));
        r.keep(None, Arc::new(vec![1; 100]), &ctx(), "2026-11-11T10:00:00+00:00");
        assert!(r.newest_unsaved().is_some());
        assert!(r.available().is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn files_are_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = temp_dir("perm").join("recordings");
        let r = Recovery::new(dir.clone());
        let path = r.save(4, &[1; 100]).unwrap();
        assert_eq!(std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777, 0o700);
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
    }

    #[test]
    fn interrupted_recordings_become_rows_at_the_next_launch() {
        let dir = temp_dir("interrupted");
        let earlier = Recovery::new(dir.clone());
        let a = [5i16; 32000];
        let pending = earlier.write_pending(&a).unwrap();
        let lost = earlier.write_pending(&a).unwrap();
        std::fs::write(dir.join("11.wav.tmp"), "half").unwrap();
        age(&pending, Duration::from_secs(60));
        age(&lost, Duration::from_secs(60));
        age(&dir.join("11.wav.tmp"), Duration::from_secs(60));
        let r = Recovery::new(dir.clone());
        // File times can lag the clock by a few ms (Linux).
        std::thread::sleep(Duration::from_millis(50));
        let current = r.write_pending(&a).unwrap();
        let mut rows = Vec::new();
        let n = r.recover_interrupted(SystemTime::now(), |_, ms| {
            rows.push(ms);
            (rows.len() == 1).then_some(42)
        });
        assert_eq!((n, rows), (1, vec![2000, 2000]));
        assert!(dir.join("42.wav").exists());
        assert!(!pending.exists() && !lost.exists(), "renamed, or deleted when no row was saved");
        assert!(!dir.join("11.wav.tmp").exists(), "half-written files are dropped");
        assert!(current.exists(), "this run's own dictation is untouched");
    }

    #[test]
    fn only_names_it_creates_are_recognised() {
        assert_eq!(parse_name("12.wav"), Some(Name::Saved(12)));
        assert_eq!(parse_name(&format!("pending-{}.wav", uuid::Uuid::new_v4())), Some(Name::Pending));
        assert_eq!(parse_name("12.wav.tmp"), Some(Name::Partial));
        for bad in ["../1.wav", "-1.wav", "1a.wav", ".wav", "pending-x.wav", "12.wav.tmp.tmp", "12.mp3", "99999999999999999999.wav"] {
            assert_eq!(parse_name(bad), None, "{bad}");
        }
    }

    #[test]
    fn failed_mode_keeps_only_failed_rows_recordings() {
        assert!(keeps(Retention::All, Some("ok")) && keeps(Retention::All, Some("error")));
        assert!(!keeps(Retention::Failed, Some("ok")), "switching all → failed drops the rest");
        assert!(keeps(Retention::Failed, Some("error")));
        assert!(keeps(Retention::Off, Some("error")), "memory still keeps a failed one");
        assert!(!keeps(Retention::Off, Some("ok")) && !keeps(Retention::All, None));
        // As enforce applies it after "all" → "failed": the ok row's file goes, the failed one's stays.
        let r = Recovery::new(temp_dir("to-failed"));
        let (ok, failed) = (r.save(1, &[1; 100]).unwrap(), r.save(2, &[1; 100]).unwrap());
        r.purge(SystemTime::now(), |id| keeps(Retention::Failed, Some(if id == 2 { "error" } else { "ok" })));
        assert!(!ok.exists() && failed.exists());
    }

    #[test]
    fn a_retry_waits_for_recording_processing_and_other_retries() {
        let busy = AtomicBool::new(false);
        assert!(!reserve(&Mutex::new(Some(())), &busy), "while recording");
        let idle: Mutex<Option<()>> = Mutex::new(None);
        busy.store(true, Ordering::SeqCst);
        assert!(!reserve(&idle, &busy), "while processing");
        busy.store(false, Ordering::SeqCst);
        assert!(reserve(&idle, &busy) && busy.load(Ordering::SeqCst));
        assert!(!reserve(&idle, &busy), "a second retry at once");
    }

    #[test]
    fn disk_retention_needs_history_and_old_settings_mean_off() {
        let mut s: Settings = serde_json::from_str(r#"{ "privacy": { "historyRetentionDays": 7, "syncUsage": false } }"#).unwrap();
        assert_eq!((s.privacy.audio_retention.as_str(), disk_retention(&s)), ("off", Retention::Off));
        s.privacy.audio_retention = "failed".into();
        assert_eq!(disk_retention(&s), Retention::Failed);
        s.privacy.audio_retention = "all".into();
        assert_eq!(disk_retention(&s), Retention::All);
        s.privacy.history_retention_days = Some(0);
        assert_eq!(disk_retention(&s), Retention::Off, "History off: memory only");
        s.privacy.history_retention_days = None;
        s.privacy.audio_retention = "sometimes".into();
        assert_eq!(disk_retention(&s), Retention::Off);
    }

    #[test]
    fn a_recovered_dictation_counts_once_and_failures_not_at_all() {
        let dir = temp_dir("stats");
        let db = crate::db::Db::open(&dir.join("db")).unwrap();
        let today = chrono::Local::now().date_naive();
        let failed = HistoryItem {
            created_at: crate::db::now_rfc3339(), category: "other".into(), audio_ms: 6000, status: "error".into(),
            error: Some("All transcription providers failed".into()), mode: "dictate".into(), ..Default::default()
        };
        let id = db.add_history(&failed, true).unwrap().unwrap();
        assert_eq!(db.stats(today).unwrap().total_dictations, 0, "a failure is not a dictation");
        assert_eq!(db.failed_dictation_ids(5).unwrap(), vec![id]);
        let old = db.get_history(id).unwrap().unwrap();
        let new = HistoryItem { raw_text: "hello there".into(), text: "Hello there.".into(), words: 2, fixes: 1, provider: Some("local".into()), ..Default::default() };
        assert!(db.retranscribed_history(&old, &new).unwrap());
        let s = db.stats(today).unwrap();
        assert_eq!((s.total_dictations, s.total_words, s.fixes, s.wpm), (1, 2, 1, 20));
        let row = db.get_history(id).unwrap().unwrap();
        assert_eq!((row.status.as_str(), row.error.as_deref(), row.text.as_str()), ("ok", None, "Hello there."));
        assert!(db.failed_dictation_ids(5).unwrap().is_empty());
        assert!(!db.retranscribed_history(&old, &new).unwrap(), "a stale copy of the row changes nothing");
        let again = HistoryItem { text: "Hello there, friend.".into(), words: 3, ..new.clone() };
        assert!(db.retranscribed_history(&row, &again).unwrap());
        assert_eq!(db.stats(today).unwrap().total_dictations, 1, "transcribing again doesn't count twice");
        let current = db.get_history(id).unwrap().unwrap();
        db.delete_history(id).unwrap();
        assert!(!db.retranscribed_history(&current, &new).unwrap(), "a deleted row isn't recreated");
        assert!(db.get_history(id).unwrap().is_none());
        assert_eq!(db.stats(today).unwrap().total_words, 2);
        drop(db);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
