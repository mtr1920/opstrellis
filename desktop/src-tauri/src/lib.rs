mod agent;
mod ask;
mod audio;
mod backup;
pub mod cli;
mod cloud;
mod context;
mod db;
mod endpoint;
mod fnkey;
mod hotkeys;
mod insert;
mod learn;
mod llm;
mod mcp;
mod notetaker;
mod pipeline;
mod polish;
mod recovery;
mod secrets;
mod settings;
mod stt;
mod team;
mod terms;
mod text;
mod update;
mod vad;
mod wayland;
mod workspace;

use pipeline::Mode;
use serde_json::{json, Value};
use settings::Settings;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, RwLock};
use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder};
use tauri_plugin_global_shortcut::Shortcut;

pub struct App {
    pub handle: AppHandle,
    settings: RwLock<Settings>,
    settings_path: PathBuf,
    pub models_dir: PathBuf,
    pub db: db::Db,
    pub secrets: secrets::Secrets,
    pub inserter: insert::Inserter,
    pub session: Mutex<Option<pipeline::Session>>,
    pub busy: AtomicBool,
    pub shortcuts: Mutex<Vec<(Shortcut, hotkeys::Action)>>,
    pub meeting: Mutex<Option<notetaker::Meeting>>,
    /// Shortcut registration failure at startup (e.g. another app owns the key).
    pub hotkey_error: Mutex<Option<String>>,
    /// The last text dictated, for paste-last when history isn't kept.
    pub last_text: Mutex<Option<LastTranscript>>,
    /// Bumped on every dictation state change; a pending overlay hide only runs if it's unchanged.
    pub overlay_gen: std::sync::atomic::AtomicU64,
    /// How long the overlay should keep showing an outcome once the dictation is idle (ms).
    pub overlay_linger: std::sync::atomic::AtomicU64,
    /// Serialize state and delayed-hide window operations so an old timer cannot hide a new pill.
    pub overlay_lock: Mutex<()>,
    /// The tray's "AI polish" check item, kept in sync with the setting.
    tray_ai: Mutex<Option<CheckMenuItem<tauri::Wry>>>,
    /// Recordings kept so a failed dictation can be transcribed again.
    pub recovery: recovery::Recovery,
    /// While a command's rewrite waits in the pill: send true to insert it, false to discard it.
    pub review: Mutex<Option<tokio::sync::oneshot::Sender<bool>>>,
}

#[derive(Clone)]
pub struct LastTranscript {
    text: String,
    history_id: Option<i64>,
}

impl LastTranscript {
    fn resolve(&self, db: &db::Db) -> String {
        // Honor corrections to this dictation, but never substitute an older successful row
        // for a newer transcript whose insertion or history save failed.
        self.history_id.and_then(|id| db.get_history(id).ok().flatten()).map(|item| item.text).unwrap_or_else(|| self.text.clone())
    }
}

#[cfg(test)]
mod transcript_tests {
    use super::*;

    #[test]
    fn last_transcript_recovers_new_text_and_keeps_corrections() {
        let dir = std::env::temp_dir().join(format!("opstrellis-last-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = db::Db::open(&dir.join("db")).unwrap();
        let _ = db.add_history(&db::HistoryItem { text: "older success".into(), status: "ok".into(), ..Default::default() }, true).unwrap();
        let mut last = LastTranscript { text: "new transcript".into(), history_id: None };
        assert_eq!(last.resolve(&db), "new transcript", "history off or save failure never returns an older dictation");
        last.history_id = db.add_history(&db::HistoryItem { text: last.text.clone(), status: "error".into(), ..Default::default() }, true).unwrap();
        assert_eq!(last.resolve(&db), "new transcript", "failed insertion stays recoverable");
        db.update_history_text(last.history_id.unwrap(), "corrected transcript").unwrap();
        assert_eq!(last.resolve(&db), "corrected transcript");
        db.clear_history().unwrap();
        assert_eq!(last.resolve(&db), "new transcript", "in-memory paste-last survives retention");
        drop(db);
        std::fs::remove_dir_all(dir).unwrap();
    }
}

impl App {
    pub fn settings(&self) -> Settings {
        self.settings.read().unwrap().clone()
    }

    pub fn update_settings(&self, f: impl FnOnce(&mut Settings)) -> anyhow::Result<Settings> {
        let mut s = self.settings.write().unwrap();
        let mut next = s.clone();
        f(&mut next);
        next.save(&self.settings_path)?;
        let packs_changed = term_packs_changed(&s, &next);
        *s = next.clone();
        if packs_changed {
            refresh_terms(&self.handle);
        }
        Ok(next)
    }

    /// Tray: turn AI polish on or off; the open window picks up the change.
    fn set_ai_polish(&self, on: bool) {
        match self.update_settings(|s| s.llm.enabled = on) {
            Ok(s) => {
                if let Some(item) = self.tray_ai.lock().unwrap().as_ref() {
                    let _ = item.set_checked(on);
                }
                let _ = self.handle.emit("settings-changed", s);
                sync_polish(self);
            }
            Err(e) => self.report_error(&format!("Couldn't save settings: {e:#}")),
        }
    }

    /// Tray: put the last transcript on the clipboard.
    fn copy_last(&self) {
        let text = self.last_transcript().ok().flatten();
        match text {
            Some(t) => match self.inserter.set_clipboard(&t) {
                Ok(()) => self.outcome("ok", "Last transcript copied"),
                Err(e) => self.report_error(&format!("{e:#}")),
            },
            None => self.outcome("warn", "Nothing dictated yet."),
        }
    }

    fn last_transcript(&self) -> anyhow::Result<Option<String>> {
        let cached = self.last_text.lock().unwrap().clone();
        match cached {
            Some(last) => Ok(Some(last.resolve(&self.db))),
            None => self.db.last_text(),
        }
    }
}

type Res<T> = Result<T, String>;
type State<'a> = tauri::State<'a, Arc<App>>;

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

#[tauri::command]
fn get_settings(app: State) -> Settings {
    app.settings()
}

#[tauri::command]
fn save_settings(app: State, mut settings: Settings) -> Res<Settings> {
    let app = app.inner().clone();
    let old = app.settings();
    // The signed-in email is owned by the backend (set by cloud sign-in); the UI copy may be stale.
    settings.cloud.email = old.cloud.email.clone();
    settings.update_checked_at = old.update_checked_at;
    settings.workspaces = old.workspaces.clone();
    let moved = cloud::base_of(&old.cloud.base_url) != cloud::base_of(&settings.cloud.base_url);
    if moved {
        settings.cloud.email = None;
    }
    let stale = stale_secrets(&old, &settings);
    // All or nothing: every step that can fail runs while the app still uses the old settings, the
    // irreversible key deletions last of them, and a failure puts back the old file and shortcuts.
    // Only after the keys are gone does the app see the new settings, so none reaches a new host.
    let new_keys = old.hotkeys != settings.hotkeys;
    let undo = |e: anyhow::Error, file: bool| -> String {
        if file {
            if let Err(e) = old.save(&app.settings_path) {
                log::error!("restoring settings.json failed: {e:#}");
            }
        }
        *app.settings.write().unwrap() = old.clone();
        if new_keys {
            let _ = hotkeys::register(&app, false);
        }
        format!("{e:#}")
    };
    if new_keys {
        // Only touch global shortcuts when they changed, so an unrelated save can't fail on them.
        *app.settings.write().unwrap() = Settings { hotkeys: settings.hotkeys.clone(), ..old.clone() };
        hotkeys::register(&app, false).map_err(|e| undo(e, false))?;
    }
    // Before the keys go, so a failed write loses nothing; the agent CLI reads consent from it.
    settings.save(&app.settings_path).map_err(|e| undo(e, false))?;
    let dropped = (|| -> anyhow::Result<()> {
        if moved {
            cloud::forget_account(&app)?;
        }
        stale.iter().try_for_each(|id| app.secrets.delete(id))
    })();
    dropped.map_err(|e| undo(e, true))?;
    *app.settings.write().unwrap() = settings.clone();
    // Consent or the pill withdrawn: a waiting agent question goes too (`ask.rs`).
    if !ask::voice_allowed(&settings) {
        ask::QUESTIONS.turned_off();
    }
    // After the new settings are visible, so a sync in flight sees the setting off (`team::sync`).
    if old.cloud.team_library != settings.cloud.team_library {
        if settings.cloud.team_library {
            let app = app.clone();
            // Also when nothing changed, so Settings shows the result (e.g. "not in a team").
            tauri::async_runtime::spawn(async move {
                let _ = team::sync(&app).await;
                let _ = app.handle.emit("team-updated", ());
            });
        } else if let Err(e) = team::clear(&app) {
            log::warn!("clearing the team library failed: {e:#}");
        }
    }
    if old.launch_at_login != settings.launch_at_login {
        use tauri_plugin_autostart::ManagerExt;
        let al = app.handle.autolaunch();
        let _ = if settings.launch_at_login { al.enable() } else { al.disable() };
    }
    if old.show_in_dock != settings.show_in_dock {
        apply_dock(&app);
        // Leaving the Dock deactivates the app; keep the window the user is changing it in in front.
        show_main(&app.handle);
    }
    if old.theme != settings.theme {
        apply_theme(&app.handle, &settings.theme);
    }
    if (old.privacy.history_retention_days, &old.privacy.audio_retention) != (settings.privacy.history_retention_days, &settings.privacy.audio_retention) {
        apply_retention(&app);
    }
    if settings.stt_chain.first().map(|e| (e.provider, &e.model)) != old.stt_chain.first().map(|e| (e.provider, &e.model)) {
        warm_up(&app);
    }
    if term_packs_changed(&old, &settings) {
        refresh_terms(&app.handle);
    }
    if let Some(item) = app.tray_ai.lock().unwrap().as_ref() {
        let _ = item.set_checked(settings.llm.enabled);
    }
    if (old.llm.enabled, &old.llm.provider) != (settings.llm.enabled, &settings.llm.provider) {
        sync_polish(&app);
    }
    // Nothing queued is sent while sync is off (`flush_usage`), so a failure here is only logged.
    if old.privacy.sync_usage && !settings.privacy.sync_usage {
        if let Err(e) = app.db.clear_usage() {
            log::warn!("clearing unsent usage failed: {e:#}");
        }
    }
    if moved {
        let _ = app.handle.emit("cloud-status", cloud::CloudStatus::default());
    }
    Ok(settings)
}

/// Keys that a save makes stale. A key belongs to the provider and server it was entered for: a
/// transcription entry's (`stt:<id>`) must not outlive the entry or follow it to another provider
/// or base URL, and the AI key (`llm`) must not follow a change of AI provider or base URL.
fn stale_secrets(old: &Settings, new: &Settings) -> Vec<String> {
    let base = |b: &Option<String>| b.as_deref().map(cloud::base_of).filter(|b| !b.is_empty()).map(str::to_string);
    let mut stale: Vec<String> = old
        .stt_chain
        .iter()
        .filter(|o| !new.stt_chain.iter().any(|n| n.id == o.id && n.provider == o.provider && base(&n.base_url) == base(&o.base_url)))
        .map(|o| format!("stt:{}", o.id))
        .collect();
    if old.llm.provider != new.llm.provider || base(&old.llm.base_url) != base(&new.llm.base_url) {
        stale.push("llm".into());
    }
    stale
}

#[cfg(test)]
mod save_tests {
    use super::*;
    use settings::{ProviderKind, SttEntry};

    #[test]
    fn typed_text_never_reaches_the_log() {
        use log::Level::*;
        assert!(hidden("enigo", Debug) && hidden("enigo::platform::macos_impl", Trace) && hidden("enigo::linux", Debug));
        assert!(!hidden("enigo", Info) && !hidden("enigo::linux", Warn), "its warnings and errors stay");
        assert!(!hidden("opstrellis_lib::pipeline", Debug) && !hidden("enigoish", Debug));
    }

    #[test]
    fn keys_stay_with_their_provider_and_server() {
        let e = |id: &str, provider, base: Option<&str>| SttEntry { id: id.into(), provider, model: "m".into(), base_url: base.map(String::from), region: None, streaming: false, enabled: true };
        let with = |chain: Vec<SttEntry>, llm_base: Option<&str>| {
            let mut s = Settings::default();
            s.stt_chain = chain;
            s.llm.base_url = llm_base.map(String::from);
            s
        };
        let old = with(vec![e("a", ProviderKind::Deepgram, None), e("b", ProviderKind::Openai, Some("https://api.groq.com/openai/v1")), e("c", ProviderKind::Assemblyai, None)], None);
        let new = with(vec![e("b", ProviderKind::Openai, Some(" https://api.groq.com/openai/v1/ ")), e("a", ProviderKind::Openai, None), e("d", ProviderKind::Google, None)], None);
        assert_eq!(stale_secrets(&old, &new), vec!["stt:a", "stt:c"], "changed provider, removed entry; same base written differently");
        assert!(stale_secrets(&old, &old).is_empty(), "reordering or other edits keep keys");
        let moved = with(vec![e("a", ProviderKind::Deepgram, None), e("b", ProviderKind::Openai, Some("https://evil.example/v1")), e("c", ProviderKind::Assemblyai, None)], None);
        assert_eq!(stale_secrets(&old, &moved), vec!["stt:b"], "another server");
        let defaulted = with(vec![e("a", ProviderKind::Deepgram, None), e("b", ProviderKind::Openai, None), e("c", ProviderKind::Assemblyai, None)], None);
        assert_eq!(stale_secrets(&old, &defaulted), vec!["stt:b"], "back to the provider default is another server too");
        assert_eq!(stale_secrets(&with(vec![], None), &with(vec![], Some("https://api.openai.com/v1"))), vec!["llm"], "None vs the default URL counts");
        assert!(stale_secrets(&with(vec![], Some("http://localhost:11434/v1/")), &with(vec![], Some("http://localhost:11434/v1"))).is_empty());
        let mut anthropic = with(vec![], None);
        anthropic.llm.provider = "anthropic".into();
        assert_eq!(stale_secrets(&with(vec![], None), &anthropic), vec!["llm"]);
    }
}

#[tauri::command]
fn set_secret(app: State, id: String, value: String) -> Res<()> {
    app.secrets.set(&id, &value).map_err(err)
}

#[tauri::command]
fn delete_secret(app: State, id: String) -> Res<()> {
    app.secrets.delete(&id).map_err(err)
}

#[tauri::command]
fn has_secrets(app: State) -> Vec<String> {
    let s = app.settings();
    s.stt_chain
        .iter()
        .map(|e| format!("stt:{}", e.id))
        .chain(["llm".to_string(), "cloud".to_string()])
        .filter(|id| app.secrets.has(id))
        .collect()
}

#[tauri::command]
fn list_input_devices() -> Vec<String> {
    audio::input_devices()
}

#[tauri::command]
fn list_history(app: State, query: Option<String>, limit: i64, offset: i64) -> Res<Vec<db::HistoryItem>> {
    let mut items = app.db.list_history(query.as_deref(), limit, offset).map_err(err)?;
    let kept = app.recovery.available();
    for item in &mut items {
        item.has_audio = item.mode == "dictate" && kept.contains(&item.id);
    }
    Ok(items)
}

/// Saves a corrected dictation. With auto-learn on, small word fixes become dictionary entries;
/// returns what was learned and what will be learned if the user makes the same fix again.
#[tauri::command]
fn update_history(app: State, id: i64, text: String) -> Res<db::Learned> {
    let text = text.trim();
    if text.is_empty() {
        return Err("Text can't be empty".into());
    }
    let Some(old) = app.db.update_history_text(id, text).map_err(err)? else { return Err("Dictation not found".into()) };
    if !app.settings().cleanup.auto_learn {
        return Ok(db::Learned::default());
    }
    app.db.learn(learn::corrections(&old, text)).map_err(err)
}

/// Runs cleanup again on a dictation's original transcript (e.g. after AI polish was unavailable,
/// or with a new style). Not a correction: nothing is learned from it.
#[tauri::command]
async fn repolish_history(app: State<'_>, id: i64) -> Res<String> {
    let app = app.inner().clone();
    let item = app.db.get_history(id).map_err(err)?.ok_or("Dictation not found")?;
    if item.mode != "dictate" || item.raw_text.trim().is_empty() {
        return Err("Only dictations can be polished again".into());
    }
    let settings = app.settings();
    if !settings.llm.enabled {
        return Err("Turn on AI polish in Settings → AI polish first".into());
    }
    let ctx = context::from_history(item.app_name.clone(), &item.category, &settings.app_categories);
    let cleaned = app.clean_up(&item.raw_text, &ctx, &settings, &app.vocabulary()).await.map_err(|e| format!("{e:#}"))?;
    if cleaned.warning.is_some() {
        return Err("AI polish unavailable; the text is unchanged.".into());
    }
    if !app.db.repolish_history_text(&item, &cleaned.text).map_err(err)? {
        return Err("The dictation changed while polishing; your text is unchanged. Try again.".into());
    }
    Ok(cleaned.text)
}

/// Reverts what a correction taught (the `undo` rows `update_history` returned).
#[tauri::command]
fn undo_learned(app: State, rows: Vec<db::UndoRow>) -> Res<()> {
    app.db.undo_learn(&rows).map_err(err)
}

#[tauri::command]
fn list_apps(app: State) -> Res<Vec<db::AppUse>> {
    app.db.list_apps(30).map_err(err)
}

#[tauri::command]
fn delete_history(app: State, id: i64) -> Res<()> {
    app.db.delete_history(id).map_err(err)?;
    app.recovery.forget(id);
    Ok(())
}

#[tauri::command]
fn clear_history(app: State) -> Res<()> {
    app.db.clear_history().map_err(err)?;
    app.recovery.forget_all();
    Ok(())
}

/// Transcribes a dictation's kept recording again with the current providers; returns the new text.
#[tauri::command]
async fn retranscribe_history(app: State<'_>, id: i64) -> Res<String> {
    let app = app.inner().clone();
    app.retry(recovery::Target::Row(id)).await.map_err(|e| format!("{e:#}"))
}

#[tauri::command]
fn get_stats(app: State) -> Res<db::Stats> {
    app.db.stats(chrono::Local::now().date_naive()).map_err(err)
}

#[tauri::command]
fn list_dictionary(app: State) -> Res<Vec<db::DictRow>> {
    app.db.list_dictionary_rows().map_err(err)
}
#[tauri::command]
fn upsert_dictionary(app: State, entry: db::DictEntry) -> Res<db::DictEntry> {
    app.db.upsert_dictionary(entry).map_err(err)
}
#[tauri::command]
fn delete_dictionary(app: State, id: i64) -> Res<()> {
    app.db.delete_dictionary(id).map_err(err)
}
#[tauri::command]
fn list_snippets(app: State) -> Res<Vec<db::LibraryRow<db::Snippet>>> {
    app.db.list_snippet_rows().map_err(err)
}
#[tauri::command]
fn upsert_snippet(app: State, snippet: db::Snippet) -> Res<db::Snippet> {
    app.db.upsert_snippet(snippet).map_err(err)
}
#[tauri::command]
fn delete_snippet(app: State, id: i64) -> Res<()> {
    app.db.delete_snippet(id).map_err(err)
}
#[tauri::command]
fn list_transforms(app: State) -> Res<Vec<db::LibraryRow<db::Transform>>> {
    app.db.list_transform_rows().map_err(err)
}
#[tauri::command]
fn upsert_transform(app: State, transform: db::Transform) -> Res<db::Transform> {
    app.db.upsert_transform(transform).map_err(err)
}
#[tauri::command]
fn delete_transform(app: State, id: i64) -> Res<()> {
    app.db.delete_transform(id).map_err(err)
}
#[tauri::command]
fn list_notes(app: State, kind: Option<String>) -> Res<Vec<db::Note>> {
    app.db.list_notes(kind.as_deref()).map_err(err)
}
#[tauri::command]
fn upsert_note(app: State, note: db::Note) -> Res<db::Note> {
    app.db.upsert_note(note).map_err(err)
}
#[tauri::command]
fn delete_note(app: State, id: i64) -> Res<()> {
    app.db.delete_note(id).map_err(err)
}

#[tauri::command]
fn list_local_models(app: State) -> Vec<stt::local::LocalModel> {
    stt::local::list(&app.models_dir)
}

#[tauri::command]
async fn download_local_model(app: State<'_>, id: String, allow_unverified: Option<bool>) -> Res<()> {
    let app = app.inner().clone();
    let h = app.handle.clone();
    let id2 = id.clone();
    let r = stt::local::download(&app.models_dir, &id, allow_unverified.unwrap_or(false), move |downloaded, total| {
        let _ = h.emit("model-download", json!({ "id": id2, "downloaded": downloaded, "total": total, "done": false, "error": null }));
    })
    .await;
    let cancelled = r.as_ref().is_err_and(|e| e.is::<stt::local::Cancelled>());
    let error = r.as_ref().err().filter(|_| !cancelled).map(|e| format!("{e:#}"));
    // The UI offers "Download anyway" for this one.
    let changed = r.as_ref().err().is_some_and(|e| e.downcast_ref::<stt::local::Changed>().is_some());
    let _ = app.handle.emit("model-download", json!({ "id": id, "downloaded": 0, "total": 0, "done": true, "error": error, "cancelled": cancelled, "changed": changed }));
    if cancelled {
        return Ok(());
    }
    if r.is_ok() {
        warm_up(&app);
    }
    r.map_err(|e| format!("{e:#}"))
}

#[tauri::command]
fn cancel_local_download(id: String) {
    stt::local::cancel_download(&id);
}

#[tauri::command]
fn delete_local_model(app: State, id: String) -> Res<()> {
    stt::local::delete(&app.models_dir, &id).map_err(err)
}

#[tauri::command]
fn polish_model_status(app: State) -> polish::Status {
    polish::status(&app.models_dir)
}

/// Progress arrives as `model-download` events with the id "s1-mini".
#[tauri::command]
async fn download_polish_model(app: State<'_>) -> Res<()> {
    let app = app.inner().clone();
    let h = app.handle.clone();
    let r = polish::download(&app.models_dir, move |downloaded, total| {
        let _ = h.emit("model-download", json!({ "id": "s1-mini", "downloaded": downloaded, "total": total, "done": false, "error": null }));
    })
    .await;
    let error = r.as_ref().err().map(|e| format!("{e:#}"));
    let _ = app.handle.emit("model-download", json!({ "id": "s1-mini", "downloaded": 0, "total": 0, "done": true, "error": error }));
    sync_polish(&app);
    r.map_err(|e| format!("{e:#}"))
}

#[tauri::command]
fn delete_polish_model(app: State) -> Res<()> {
    polish::delete(&app.models_dir).map_err(err)
}

/// Keeps the local polish model loaded only while it is the AI provider in use.
fn sync_polish(app: &App) {
    let s = app.settings();
    if s.llm.enabled && s.llm.provider == polish::PROVIDER {
        polish::warm(&app.models_dir);
    } else {
        polish::unload();
    }
}

#[tauri::command]
async fn test_stt(app: State<'_>, entry_id: String) -> Res<Value> {
    let app = app.inner().clone();
    let settings = app.settings();
    let mut provider = settings.stt_chain.iter().find(|e| e.id == entry_id).cloned().ok_or("Unknown provider entry")?;
    provider.enabled = true;
    let mut settings_one = settings.clone();
    settings_one.stt_chain = vec![provider];
    let p = app.providers(&settings_one).pop().ok_or("Unknown provider entry")?;
    let audio = audio::read_wav(&sample_path(&app)).map_err(err)?;
    let language = Some(settings.language.clone()).filter(|l| l != "auto");
    let started = std::time::Instant::now();
    let text = p
        .transcribe(&stt::Request { audio: &audio, language: language.as_deref(), vocabulary: &[] })
        .await
        .map_err(|e| format!("{e:#}"))?;
    Ok(json!({ "text": text, "latencyMs": started.elapsed().as_millis() as u64 }))
}

/// Runs a short sample through AI cleanup with the saved provider, model and key.
#[tauri::command]
async fn test_llm(app: State<'_>) -> Res<Value> {
    let app = app.inner().clone();
    let settings = app.settings();
    let sample = "um so lets meet at two actually three pm on friday";
    let started = std::time::Instant::now();
    if settings.llm.provider == polish::PROVIDER {
        let (dir, deadline) = (app.models_dir.clone(), started + std::time::Duration::from_secs(30));
        let control = polish::control_line("casual", "work");
        let text = tokio::task::spawn_blocking(move || polish::polish_blocking(&dir, sample, &control, deadline))
            .await
            .map_err(err)?
            .map_err(|e| format!("{e:#}"))?;
        return Ok(json!({ "text": text, "latencyMs": started.elapsed().as_millis() as u64 }));
    }
    let ctx = context::classify(Some("Slack".into()), None);
    let system = llm::cleanup_prompt(&ctx, &llm::Cleanup { tone: "casual", ..Default::default() });
    let text = app
        .llm_client(&settings)
        .complete_within(&system, &llm::wrap_dictation(sample), llm::CLEANUP_TIMEOUT)
        .await
        .map_err(|e| format!("{e:#}"))?;
    Ok(json!({ "text": text, "latencyMs": started.elapsed().as_millis() as u64 }))
}

#[tauri::command]
fn start_dictation(app: State) {
    let app = app.inner().clone();
    if !app.is_recording() {
        app.toggle(Mode::Dictate);
    }
}

#[tauri::command]
fn stop_dictation(app: State) {
    app.inner().stop();
}

#[tauri::command]
fn cancel_dictation(app: State) {
    app.inner().cancel_or_discard();
}

#[tauri::command]
async fn paste_last(app: State<'_>) -> Res<()> {
    app.inner().paste_last().await.map_err(|e| format!("{e:#}"))
}

#[tauri::command]
async fn cloud_login(app: State<'_>) -> Res<cloud::LoginStart> {
    cloud::login(app.inner().clone()).await.map_err(|e| format!("{e:#}"))
}

#[tauri::command]
fn cloud_logout(app: State) -> Res<cloud::CloudStatus> {
    cloud::logout(&app).map_err(err)?;
    Ok(cloud::CloudStatus::default())
}

#[tauri::command]
async fn cloud_status(app: State<'_>) -> Res<cloud::CloudStatus> {
    Ok(cloud::status(&app).await)
}

#[tauri::command]
fn team_status(app: State) -> Res<team::TeamStatus> {
    team::status(&app.db).map_err(err)
}

/// Syncs the team library now (nothing without sign-in or with the setting off); a failure is in `error`.
#[tauri::command]
async fn team_sync_now(app: State<'_>) -> Res<team::TeamStatus> {
    let _ = team::sync(&app).await;
    team::status(&app.db).map_err(err)
}

#[tauri::command]
fn meeting_start(app: State, second_device: Option<String>, template: Option<String>) -> Res<()> {
    notetaker::start(app.inner(), second_device, template).map_err(|e| format!("{e:#}"))
}

#[tauri::command]
fn meeting_templates() -> Vec<Value> {
    notetaker::TEMPLATES.iter().map(|(id, name, _)| json!({ "id": id, "name": name })).collect()
}

#[tauri::command]
async fn meeting_follow_up(app: State<'_>, note_id: i64, kind: String) -> Res<String> {
    notetaker::follow_up(app.inner(), note_id, &kind).await.map_err(|e| format!("{e:#}"))
}

#[tauri::command]
fn meeting_stop(app: State) -> Res<()> {
    notetaker::stop(app.inner()).map_err(|e| format!("{e:#}"))
}

#[tauri::command]
fn meeting_status(app: State) -> notetaker::MeetingStatus {
    notetaker::status(&app)
}

/// Async so the Wayland checks (they run wtype and gsettings) stay off the main thread.
#[tauri::command]
async fn permissions(app: State<'_>) -> Res<Value> {
    Ok(json!({
        "hotkeyError": app.hotkey_error.lock().unwrap().clone(),
        "accessibility": insert::accessibility_granted(false),
        "platform": if cfg!(target_os = "macos") { "macos" } else { "linux" },
        "wayland": insert::is_wayland(),
        // null unless Wayland.
        "waylandShortcuts": insert::is_wayland().then(wayland::status),
        "insertion": insert::is_wayland().then(insert::wayland_tools_status),
        // null unless Fn is a shortcut.
        "inputMonitoring": fnkey::active(),
        "fnSystemAction": if fnkey::fn_bound() { fnkey::system_action() } else { None },
    }))
}

/// Asks the desktop to bind the shortcuts again (Wayland, GlobalShortcuts portal).
#[tauri::command]
fn rebind_wayland_shortcuts(app: State) -> Res<()> {
    wayland::rebind(app.inner()).map_err(|e| format!("{e:#}"))
}

/// Adds GNOME custom shortcuts that run `opstrellis --toggle` and friends (Wayland without the portal).
#[tauri::command]
async fn add_gnome_shortcuts(app: State<'_>) -> Res<()> {
    wayland::add_gnome_shortcuts(&app.settings()).map_err(|e| format!("{e:#}"))
}

/// Settings → Permissions reloads on this (e.g. the desktop bound the Wayland shortcuts).
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn emit_permissions_changed(app: &App) {
    let _ = app.handle.emit("permissions-changed", ());
}

#[tauri::command]
fn request_accessibility(app: State) {
    if !insert::accessibility_granted(false) {
        reset_permission(&app, "Accessibility");
    }
    insert::accessibility_granted(true);
}

/// The shortcut recorder in Settings opened (`on`) or closed: while it is open, pressing a
/// shortcut reaches the recorder instead of triggering it, and single keys (Fn, Right ⌥, …) are
/// captured natively.
#[tauri::command]
fn set_shortcut_recording(app: State, on: bool) {
    let app = app.inner().clone();
    if on {
        hotkeys::suspend(&app);
    } else {
        hotkeys::resume(&app);
    }
}

#[tauri::command]
fn request_input_monitoring(app: State) {
    if !fnkey::permission_granted() {
        reset_permission(&app, "ListenEvent");
    }
    fnkey::request_permission();
}

/// Builds are unsigned, so each update has a new signature and macOS silently ignores the earlier
/// grant while System Settings still shows it switched on. Removing our stale entry lets the
/// prompt that follows register this build.
fn reset_permission(app: &App, service: &str) {
    if cfg!(target_os = "macos") {
        let _ = std::process::Command::new("tccutil").args(["reset", service, &app.handle.config().identifier]).status();
    }
}

#[tauri::command]
fn open_logs(app: State) -> Res<()> {
    let file = log_file(&app.handle).map_err(|e| format!("{e:#}"))?;
    tauri_plugin_opener::reveal_item_in_dir(file).map_err(|e| format!("{e:#}"))
}

/// Writes a backup of the library to the Downloads folder and reveals it there.
#[tauri::command]
fn export_backup(app: State, include_notes: bool) -> Res<backup::ExportResult> {
    backup::export(&app, include_notes).map_err(|e| format!("{e:#}"))
}

/// Validates a backup file and reports what importing it would do; changes nothing.
#[tauri::command]
fn preview_backup(app: State, json: String) -> Res<backup::Summary> {
    backup::preview(&app.db, &json).map_err(|e| format!("{e:#}"))
}

#[tauri::command]
fn import_backup(app: State, json: String, settings: bool, notes: bool) -> Res<backup::Summary> {
    backup::import(&app, &json, settings, notes).map_err(|e| format!("{e:#}"))
}

/// One dictionary entry per line: `phrase` or `phrase<separator>replacement`.
#[tauri::command]
fn import_dictionary_text(app: State, text: String) -> Res<backup::AddSkip> {
    backup::import_dictionary_text(&app.db, &text).map_err(|e| format!("{e:#}"))
}

fn term_packs_changed(old: &Settings, new: &Settings) -> bool {
    (old.cleanup.tech_terms, &old.cleanup.term_packs) != (new.cleanup.tech_terms, &new.cleanup.term_packs)
}

/// Rebuilds the active term packs on another thread, off the dictation path.
fn refresh_terms(handle: &AppHandle) {
    let handle = handle.clone();
    std::thread::spawn(move || apply_terms(&handle));
}

fn apply_terms(handle: &AppHandle) {
    let app = handle.state::<Arc<App>>();
    terms::refresh(|| {
        let c = app.settings().cleanup;
        (c.tech_terms, c.term_packs)
    });
}

fn term_packs_dir(app: &App) -> Res<PathBuf> {
    app.handle.path().app_data_dir().map(|d| d.join("term-packs")).map_err(err)
}

#[tauri::command]
fn list_term_packs(app: State) -> Vec<terms::PackInfo> {
    let c = app.settings().cleanup;
    terms::list(c.tech_terms, &c.term_packs)
}

/// Validates and saves an imported pack, replacing an imported one with the same id.
#[tauri::command]
async fn import_term_pack(app: State<'_>, json: String) -> Res<Value> {
    let dir = term_packs_dir(&app)?;
    // Checking a large pack builds its matcher; keep that off the main thread.
    let (mut pack, dropped) = tokio::task::spawn_blocking(move || terms::import(&dir, &json)).await.map_err(err)??;
    pack.enabled = app.settings().cleanup.term_packs.contains(&pack.id);
    if pack.enabled {
        refresh_terms(&app.handle);
    }
    Ok(json!({ "pack": pack, "dropped": dropped }))
}

/// What importing a pack would do (its replacements), before anything is saved.
#[tauri::command]
async fn preview_term_pack(json: String) -> Res<terms::Preview> {
    tokio::task::spawn_blocking(move || terms::preview(&json)).await.map_err(err)?
}

#[tauri::command]
async fn delete_term_pack(app: State<'_>, id: String) -> Res<()> {
    let (dir, pack) = (term_packs_dir(&app)?, id.clone());
    // A background rebuild holds the packs' lock while it builds; wait for it off the main thread.
    tokio::task::spawn_blocking(move || terms::delete(&dir, &pack)).await.map_err(err)??;
    // Rebuilds the active packs if it was enabled.
    app.update_settings(|s| s.cleanup.term_packs.retain(|p| p != &id)).map_err(err)?;
    Ok(())
}

#[tauri::command]
fn list_workspaces() -> Vec<workspace::Info> {
    workspace::list()
}

/// Adds a folder for repo-aware dictation and indexes it in the background.
#[tauri::command]
fn add_workspace(app: State, path: String) -> Res<Vec<workspace::Info>> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let path = workspace::validate(&app.settings().workspaces, &path, home.as_deref())?;
    let s = app
        .update_settings(|s| {
            if !s.workspaces.contains(&path) && s.workspaces.len() < workspace::MAX_WORKSPACES {
                s.workspaces.push(path);
            }
        })
        .map_err(err)?;
    workspace::sync(&s.workspaces);
    Ok(workspace::list())
}

#[tauri::command]
fn remove_workspace(app: State, path: String) -> Res<Vec<workspace::Info>> {
    let s = app.update_settings(|s| s.workspaces.retain(|p| p != &path)).map_err(err)?;
    workspace::sync(&s.workspaces);
    Ok(workspace::list())
}

#[tauri::command]
fn reindex_workspace(path: String) -> Res<Vec<workspace::Info>> {
    if !workspace::reindex(&path) {
        return Err("That folder isn't a workspace.".into());
    }
    Ok(workspace::list())
}

/// Manual update check; works whether or not the automatic check is on.
#[tauri::command]
async fn check_update(app: State<'_>) -> Res<update::UpdateInfo> {
    update::check(&app.handle.package_info().version.to_string()).await.map_err(|e| format!("{e:#}"))
}

/// Opens the release page for `tag`; the URL is built here, only for a plain version tag.
#[tauri::command]
fn open_release_page(tag: String) -> Res<()> {
    let url = update::release_url(&tag).map_err(err)?;
    tauri_plugin_opener::open_url(url, None::<&str>).map_err(err)
}

fn log_file(handle: &AppHandle) -> tauri::Result<PathBuf> {
    Ok(handle.path().app_log_dir()?.join("opstrellis.log"))
}

/// Writes each log record to stderr and to the log file, so a slow or failed dictation can be
/// diagnosed after the fact (apps opened from Finder or the tray have no visible stderr).
struct Tee(std::fs::File);

impl std::io::Write for Tee {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let _ = std::io::stderr().write_all(buf);
        self.0.write_all(buf)?;
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.0.flush()
    }
}

/// The previous file is kept as opstrellis.log.1 once it passes this size (checked at startup).
const LOG_ROTATE_BYTES: u64 = 5 * 1024 * 1024;

fn init_logging(handle: &AppHandle) {
    let mut builder = env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info,opstrellis_lib=debug"));
    let file = log_file(handle).map_err(anyhow::Error::from).and_then(|path| -> anyhow::Result<std::fs::File> {
        secrets::private_dir(path.parent().unwrap_or(&path))?;
        if std::fs::metadata(&path).is_ok_and(|m| m.len() > LOG_ROTATE_BYTES) {
            std::fs::rename(&path, path.with_extension("log.1"))?;
        }
        Ok(std::fs::OpenOptions::new().create(true).append(true).open(&path)?)
    });
    match file {
        Ok(f) => {
            builder.write_style(env_logger::WriteStyle::Never).target(env_logger::Target::Pipe(Box::new(Tee(f))));
        }
        Err(e) => eprintln!("logging to stderr only: {e:#}"),
    }
    let logger = builder.build();
    let max = logger.filter();
    if log::set_boxed_logger(Box::new(Redacting(logger))).is_ok() {
        log::set_max_level(max);
    }
    log::info!("OpsTrellis {} starting", handle.package_info().version);
}

/// enigo logs the text it types at debug level; whatever RUST_LOG says, the log never holds what
/// the user dictated.
fn hidden(target: &str, level: log::Level) -> bool {
    level > log::Level::Info && (target == "enigo" || target.starts_with("enigo::"))
}

struct Redacting(env_logger::Logger);

impl log::Log for Redacting {
    fn enabled(&self, m: &log::Metadata) -> bool {
        !hidden(m.target(), m.level()) && self.0.enabled(m)
    }
    fn log(&self, r: &log::Record) {
        if !hidden(r.target(), r.level()) {
            self.0.log(r);
        }
    }
    fn flush(&self) {
        self.0.flush();
    }
}

fn sample_path(app: &App) -> PathBuf {
    app.handle
        .path()
        .resource_dir()
        .map(|d| d.join("sample.wav"))
        .ok()
        .filter(|p| p.exists())
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("resources/sample.wav"))
}

fn apply_retention(app: &App) {
    match app.settings().privacy.history_retention_days {
        Some(0) => {
            let _ = app.db.clear_history();
        }
        Some(days) => {
            let _ = app.db.purge_history_older_than(days);
        }
        None => {}
    }
    app.enforce_recordings();
}

/// Loads the primary local model in the background so the first dictation is fast.
fn warm_up(app: &Arc<App>) {
    let s = app.settings();
    if let Some(e) = s.stt_chain.iter().find(|e| e.enabled).filter(|e| e.provider == settings::ProviderKind::Local) {
        let (dir, id) = (app.models_dir.clone(), e.model.clone());
        let language = Some(s.language.clone()).filter(|l| !l.is_empty() && l != "auto");
        std::thread::spawn(move || stt::local::warm_up(&dir, &id, language.as_deref()));
    }
}

/// Hidden from the Dock, OpsTrellis is reached from its menu bar / tray icon. macOS: an "accessory"
/// app has no Dock icon or ⌘Tab entry. Linux: the window asks the dock and taskbar to skip it
/// (X11 docks honour this; some Wayland desktops ignore it).
fn apply_dock(app: &App) {
    let show = app.settings().show_in_dock;
    #[cfg(target_os = "macos")]
    let result = app.handle.set_activation_policy(if show { tauri::ActivationPolicy::Regular } else { tauri::ActivationPolicy::Accessory });
    #[cfg(not(target_os = "macos"))]
    let result = match app.handle.get_webview_window("main") {
        Some(w) => w.set_skip_taskbar(!show),
        None => Ok(()),
    };
    if let Err(e) = result {
        log::warn!("changing the Dock visibility failed: {e}");
    }
}

fn apply_theme(handle: &AppHandle, theme: &str) {
    let native = match theme {
        "light" => Some(tauri::Theme::Light),
        "dark" => Some(tauri::Theme::Dark),
        _ => None,
    };
    for w in handle.webview_windows().values() {
        if let Err(e) = w.set_theme(native) {
            log::warn!("setting the window theme failed: {e}");
        }
    }
    let _ = handle.emit("theme-changed", theme);
}

fn show_main(handle: &AppHandle) {
    if let Some(w) = handle.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

/// CLI control for desktops where global shortcuts can't be grabbed (Wayland): bind
/// `opstrellis --toggle`, `--command`, `--paste-last` and `--cancel` to keys in the desktop's settings.
fn handle_cli(app: &Arc<App>, args: &[String]) -> bool {
    if args.iter().any(|a| a == "--toggle") {
        app.toggle(Mode::Dictate);
    } else if args.iter().any(|a| a == "--command") {
        app.toggle(Mode::Command);
    } else if args.iter().any(|a| a == "--cancel") {
        app.cancel_or_discard();
    } else if args.iter().any(|a| a == "--paste-last") {
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            if let Err(e) = app.paste_last().await {
                app.report_error(&format!("{e:#}"));
            }
        });
    } else {
        return false;
    }
    true
}

/// The pill's window, or the larger one of the review card; both bottom-centre on the primary monitor.
pub(crate) fn place_overlay(win: &tauri::WebviewWindow, card: bool) {
    let (w, h) = if card { (436.0, 280.0) } else { (360.0, 64.0) };
    let _ = win.set_size(tauri::LogicalSize::new(w, h));
    if let Ok(Some(m)) = win.primary_monitor() {
        let scale = m.scale_factor();
        let size = m.size().to_logical::<f64>(scale);
        let pos = m.position().to_logical::<f64>(scale);
        let _ = win.set_position(tauri::LogicalPosition::new(pos.x + (size.width - w) / 2.0, pos.y + size.height - h - 72.0));
    }
}

fn create_overlay(app: &tauri::App) -> tauri::Result<()> {
    let (w, h) = (360.0, 64.0);
    let win = WebviewWindowBuilder::new(app, "overlay", WebviewUrl::App("index.html#overlay".into()))
        .title("OpsTrellis overlay")
        .inner_size(w, h)
        .decorations(false)
        .transparent(true)
        .always_on_top(true)
        .skip_taskbar(true)
        // GTK keeps a non-resizable window at its size request, so the card's resize (`place_overlay`)
        // could be ignored. Undecorated, it can't be resized by hand anyway.
        .resizable(cfg!(target_os = "linux"))
        .shadow(false)
        .focused(false)
        // Never become the key window: on macOS `show()` would otherwise take keyboard focus,
        // so a paste into OpsTrellis's own window (e.g. the onboarding test box) went to the pill.
        .focusable(false)
        .visible(false)
        .build()?;
    place_overlay(&win, false);
    #[cfg(target_os = "linux")]
    if let Ok(gtk_win) = win.gtk_window() {
        use gtk::prelude::GtkWindowExt;
        // Never take keyboard focus from the app being dictated into.
        gtk_win.set_accept_focus(false);
        gtk_win.set_focus_on_map(false);
        gtk_win.set_type_hint(gtk::gdk::WindowTypeHint::Notification);
    }
    #[cfg(target_os = "macos")]
    {
        // Clicking a regular window would activate OpsTrellis and steal focus; keep the pill click-through.
        let _ = win.set_ignore_cursor_events(true);
        let _ = win.set_visible_on_all_workspaces(true);
    }
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|handle, args, _cwd| {
            let app = handle.state::<Arc<App>>().inner().clone();
            if !handle_cli(&app, &args) {
                show_main(handle);
            }
        }))
        .plugin(tauri_plugin_global_shortcut::Builder::new().with_handler(hotkeys::handle).build())
        .plugin(tauri_plugin_autostart::init(tauri_plugin_autostart::MacosLauncher::LaunchAgent, Some(vec!["--background"])))
        .plugin(tauri_plugin_opener::init())
        .setup(|tapp| {
            init_logging(tapp.handle());
            std::thread::spawn(terms::warm);
            if let Some(vad) = tapp.path().resource_dir().ok().map(|d| d.join("silero_vad.onnx")).filter(|p| p.exists()) {
                vad::set_model(vad);
            }
            let data_dir = tapp.path().app_data_dir()?;
            secrets::private_dir(&data_dir)?;
            let settings_path = data_dir.join("settings.json");
            let models_dir = std::env::var("OPSTRELLIS_MODELS_DIR").map(PathBuf::from).unwrap_or_else(|_| data_dir.join("models"));
            let app = Arc::new(App {
                handle: tapp.handle().clone(),
                settings: RwLock::new(Settings::load(&settings_path)),
                settings_path,
                models_dir,
                db: db::Db::open(&data_dir.join("opstrellis.db"))?,
                secrets: secrets::Secrets::new(&data_dir),
                inserter: insert::Inserter::new(),
                session: Mutex::new(None),
                busy: AtomicBool::new(false),
                shortcuts: Mutex::new(Vec::new()),
                meeting: Mutex::new(None),
                hotkey_error: Mutex::new(None),
                last_text: Mutex::new(None),
                overlay_gen: Default::default(),
                overlay_linger: Default::default(),
                overlay_lock: Mutex::new(()),
                tray_ai: Mutex::new(None),
                recovery: recovery::Recovery::new(data_dir.join("recordings")),
                review: Mutex::new(None),
            });
            tapp.manage(app.clone());
            let (packs_dir, handle) = (data_dir.join("term-packs"), tapp.handle().clone());
            std::thread::spawn(move || {
                terms::load_user_packs(&packs_dir);
                apply_terms(&handle);
            });
            let handle = tapp.handle().clone();
            workspace::on_change(move || {
                let _ = handle.emit("workspaces-changed", ());
            });
            workspace::sync(&app.settings().workspaces);
            fnkey::init(&app, &data_dir);
            log::info!("data dir: {}", data_dir.display());

            match hotkeys::register(&app, true) {
                Ok(()) => log::info!("shortcuts registered"),
                Err(e) => {
                    // Emitted before the UI listens, so it's also kept for `permissions()`.
                    *app.hotkey_error.lock().unwrap() = Some(format!("{e:#}"));
                    app.report_error(&format!("{e:#}"));
                }
            }
            wayland::start(&app);
            create_overlay(tapp)?;
            ask::start(tapp.handle(), &data_dir);
            apply_theme(&app.handle, &app.settings().theme);
            apply_retention(&app);
            app.recover_interrupted();
            app.spawn_recordings_loop();
            warm_up(&app);
            let (models, db_path) = (app.models_dir.clone(), data_dir.join("opstrellis.db"));
            // Off the main thread: compacting the database takes a moment.
            std::thread::spawn(move || {
                stt::local::clean_staging(&models);
                if let Err(e) = db::Db::vacuum_once(&db_path) {
                    log::warn!("compacting the database failed: {e:#}");
                }
            });
            sync_polish(&app);
            cloud::spawn_sync_loop(app.clone());
            update::spawn_loop(app.clone());

            let open = MenuItem::with_id(tapp, "open", "Open OpsTrellis", true, None::<&str>)?;
            let toggle = MenuItem::with_id(tapp, "toggle", "Start / stop dictation", true, None::<&str>)?;
            let copy_last = MenuItem::with_id(tapp, "copy_last", "Copy last transcript", true, None::<&str>)?;
            let retry_failed = MenuItem::with_id(tapp, "retry_failed", "Retry failed dictation", true, None::<&str>)?;
            let decline_question = MenuItem::with_id(tapp, "decline_question", "Decline agent question", false, None::<&str>)?;
            let ai = CheckMenuItem::with_id(tapp, "ai", "AI polish", true, app.settings().llm.enabled, None::<&str>)?;
            let meeting = MenuItem::with_id(tapp, "meeting", "Start / stop meeting recording", true, None::<&str>)?;
            let quit = MenuItem::with_id(tapp, "quit", "Quit OpsTrellis", true, None::<&str>)?;
            let sep = PredefinedMenuItem::separator(tapp)?;
            let sep2 = PredefinedMenuItem::separator(tapp)?;
            let menu = Menu::with_items(tapp, &[&open, &sep, &toggle, &copy_last, &retry_failed, &decline_question, &meeting, &ai, &sep2, &quit])?;
            *app.tray_ai.lock().unwrap() = Some(ai);
            ask::set_tray_item(decline_question);
            apply_dock(&app);
            TrayIconBuilder::with_id("main")
                .icon(tapp.default_window_icon().cloned().expect("icon"))
                .tooltip("OpsTrellis")
                .menu(&menu)
                .on_menu_event(|handle, ev| {
                    let app = handle.state::<Arc<App>>().inner().clone();
                    match ev.id().as_ref() {
                        "open" => show_main(handle),
                        "toggle" => app.toggle(Mode::Dictate),
                        "copy_last" => app.copy_last(),
                        "retry_failed" => app.retry_from_tray(),
                        "decline_question" => {
                            if !app.decline_question() {
                                app.outcome("warn", "No agent question is waiting.");
                            }
                        }
                        "ai" => app.set_ai_polish(!app.settings().llm.enabled),
                        "meeting" => {
                            let running = app.meeting.lock().unwrap().is_some();
                            let r = if running { notetaker::stop(&app) } else { notetaker::start(&app, None, None) };
                            if let Err(e) = r {
                                app.report_error(&format!("Notetaker: {e:#}"));
                            }
                        }
                        "quit" => handle.exit(0),
                        _ => {}
                    }
                })
                .build(tapp)?;

            let args: Vec<String> = std::env::args().collect();
            if !args.iter().any(|a| a == "--background") && !handle_cli(&app, &args) {
                show_main(tapp.handle());
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            if window.label() != "main" {
                return;
            }
            match event {
                // Closing the main window keeps OpsTrellis running in the tray.
                tauri::WindowEvent::CloseRequested { api, .. } => {
                    api.prevent_close();
                    let _ = window.hide();
                }
                // A shortcut recorder can't be in use without the window: don't leave the
                // shortcuts paused if the page never says it closed.
                tauri::WindowEvent::Focused(false) => {
                    if let Some(app) = window.try_state::<Arc<App>>() {
                        hotkeys::resume(app.inner());
                    }
                }
                _ => {}
            }
        })
        .invoke_handler(tauri::generate_handler![
            get_settings, save_settings, set_secret, delete_secret, has_secrets, list_input_devices,
            list_history, update_history, repolish_history, undo_learned, delete_history, clear_history, get_stats, list_apps,
            retranscribe_history,
            list_dictionary, upsert_dictionary, delete_dictionary, list_snippets, upsert_snippet, delete_snippet,
            list_transforms, upsert_transform, delete_transform, list_notes, upsert_note, delete_note,
            list_local_models, download_local_model, cancel_local_download, delete_local_model, polish_model_status, download_polish_model, delete_polish_model, test_stt, test_llm,
            start_dictation, stop_dictation, cancel_dictation, paste_last,
            cloud_login, cloud_logout, cloud_status, team_status, team_sync_now, permissions, request_accessibility, request_input_monitoring, open_logs,
            rebind_wayland_shortcuts, add_gnome_shortcuts,
            set_shortcut_recording,
            meeting_start, meeting_stop, meeting_status, meeting_templates, meeting_follow_up,
            export_backup, preview_backup, import_backup, import_dictionary_text, check_update, open_release_page,
            list_term_packs, preview_term_pack, import_term_pack, delete_term_pack,
            list_workspaces, add_workspace, remove_workspace, reindex_workspace,
            ask::question_shown
        ])
        .build(tauri::generate_context!())
        .expect("error while building OpsTrellis")
        .run(|handle, event| {
            if let tauri::RunEvent::Exit = event {
                if let Some(app) = handle.try_state::<Arc<App>>() {
                    app.inserter.flush_restore();
                    ask::stop();
                }
                // Give the 🌐 key its own action back (a crash is undone at the next launch).
                fnkey::release_system_action();
            }
        });
}
