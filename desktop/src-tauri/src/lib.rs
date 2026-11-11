mod audio;
mod cloud;
mod context;
mod db;
mod fnkey;
mod hotkeys;
mod insert;
mod learn;
mod llm;
mod notetaker;
mod pipeline;
mod polish;
mod secrets;
mod settings;
mod stt;
mod terms;
mod text;
mod wayland;

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
        *s = next.clone();
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
    *app.settings.write().unwrap() = settings.clone();
    // Only touch global shortcuts when they changed, so an unrelated save can't fail on them.
    if old.hotkeys != settings.hotkeys {
        if let Err(e) = hotkeys::register(&app, false) {
            *app.settings.write().unwrap() = old;
            return Err(err(format!("{e:#}")));
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
    if old.privacy.history_retention_days != settings.privacy.history_retention_days {
        apply_retention(&app);
    }
    if settings.stt_chain.first().map(|e| (e.provider, &e.model)) != old.stt_chain.first().map(|e| (e.provider, &e.model)) {
        warm_up(&app);
    }
    if let Some(item) = app.tray_ai.lock().unwrap().as_ref() {
        let _ = item.set_checked(settings.llm.enabled);
    }
    if (old.llm.enabled, &old.llm.provider) != (settings.llm.enabled, &settings.llm.provider) {
        sync_polish(&app);
    }
    settings.save(&app.settings_path).map_err(err)?;
    Ok(settings)
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
    app.db.list_history(query.as_deref(), limit, offset).map_err(err)
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
    app.db.delete_history(id).map_err(err)
}

#[tauri::command]
fn clear_history(app: State) -> Res<()> {
    app.db.clear_history().map_err(err)
}

#[tauri::command]
fn get_stats(app: State) -> Res<db::Stats> {
    app.db.stats(chrono::Local::now().date_naive()).map_err(err)
}

#[tauri::command]
fn list_dictionary(app: State) -> Res<Vec<db::DictEntry>> {
    app.db.list_dictionary().map_err(err)
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
fn list_snippets(app: State) -> Res<Vec<db::Snippet>> {
    app.db.list_snippets().map_err(err)
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
fn list_transforms(app: State) -> Res<Vec<db::Transform>> {
    app.db.list_transforms().map_err(err)
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
async fn download_local_model(app: State<'_>, id: String) -> Res<()> {
    let app = app.inner().clone();
    let h = app.handle.clone();
    let id2 = id.clone();
    let r = stt::local::download(&app.models_dir, &id, move |downloaded, total| {
        let _ = h.emit("model-download", json!({ "id": id2, "downloaded": downloaded, "total": total, "done": false, "error": null }));
    })
    .await;
    let cancelled = r.as_ref().is_err_and(|e| e.is::<stt::local::Cancelled>());
    let error = r.as_ref().err().filter(|_| !cancelled).map(|e| format!("{e:#}"));
    let _ = app.handle.emit("model-download", json!({ "id": id, "downloaded": 0, "total": 0, "done": true, "error": error, "cancelled": cancelled }));
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
    app.inner().cancel();
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
        std::fs::create_dir_all(path.parent().unwrap_or(&path))?;
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
    builder.init();
    log::info!("OpsTrellis {} starting", handle.package_info().version);
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
        app.cancel();
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

fn create_overlay(app: &tauri::App) -> tauri::Result<()> {
    let (w, h) = (360.0, 64.0);
    let win = WebviewWindowBuilder::new(app, "overlay", WebviewUrl::App("index.html#overlay".into()))
        .title("OpsTrellis overlay")
        .inner_size(w, h)
        .decorations(false)
        .transparent(true)
        .always_on_top(true)
        .skip_taskbar(true)
        .resizable(false)
        .shadow(false)
        .focused(false)
        // Never become the key window: on macOS `show()` would otherwise take keyboard focus,
        // so a paste into OpsTrellis's own window (e.g. the onboarding test box) went to the pill.
        .focusable(false)
        .visible(false)
        .build()?;
    if let Ok(Some(m)) = win.primary_monitor() {
        let scale = m.scale_factor();
        let size = m.size().to_logical::<f64>(scale);
        let pos = m.position().to_logical::<f64>(scale);
        let _ = win.set_position(tauri::LogicalPosition::new(pos.x + (size.width - w) / 2.0, pos.y + size.height - h - 72.0));
    }
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
            let data_dir = tapp.path().app_data_dir()?;
            std::fs::create_dir_all(&data_dir)?;
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
            });
            tapp.manage(app.clone());
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
            apply_theme(&app.handle, &app.settings().theme);
            apply_retention(&app);
            warm_up(&app);
            sync_polish(&app);
            cloud::spawn_sync_loop(app.clone());

            let open = MenuItem::with_id(tapp, "open", "Open OpsTrellis", true, None::<&str>)?;
            let toggle = MenuItem::with_id(tapp, "toggle", "Start / stop dictation", true, None::<&str>)?;
            let copy_last = MenuItem::with_id(tapp, "copy_last", "Copy last transcript", true, None::<&str>)?;
            let ai = CheckMenuItem::with_id(tapp, "ai", "AI polish", true, app.settings().llm.enabled, None::<&str>)?;
            let meeting = MenuItem::with_id(tapp, "meeting", "Start / stop meeting recording", true, None::<&str>)?;
            let quit = MenuItem::with_id(tapp, "quit", "Quit OpsTrellis", true, None::<&str>)?;
            let sep = PredefinedMenuItem::separator(tapp)?;
            let sep2 = PredefinedMenuItem::separator(tapp)?;
            let menu = Menu::with_items(tapp, &[&open, &sep, &toggle, &copy_last, &meeting, &ai, &sep2, &quit])?;
            *app.tray_ai.lock().unwrap() = Some(ai);
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
            list_dictionary, upsert_dictionary, delete_dictionary, list_snippets, upsert_snippet, delete_snippet,
            list_transforms, upsert_transform, delete_transform, list_notes, upsert_note, delete_note,
            list_local_models, download_local_model, cancel_local_download, delete_local_model, polish_model_status, download_polish_model, delete_polish_model, test_stt, test_llm,
            start_dictation, stop_dictation, cancel_dictation, paste_last,
            cloud_login, cloud_logout, cloud_status, permissions, request_accessibility, request_input_monitoring, open_logs,
            rebind_wayland_shortcuts, add_gnome_shortcuts,
            set_shortcut_recording,
            meeting_start, meeting_stop, meeting_status, meeting_templates, meeting_follow_up
        ])
        .build(tauri::generate_context!())
        .expect("error while building OpsTrellis")
        .run(|_, event| {
            if let tauri::RunEvent::Exit = event {
                // Give the 🌐 key its own action back (a crash is undone at the next launch).
                fnkey::release_system_action();
            }
        });
}
