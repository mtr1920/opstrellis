//! One dictation, end to end: hotkey → record (streaming to the primary provider when it can) →
//! transcribe with fallback → clean up (rules, optional LLM) or run a command → insert → history.

use crate::audio::{self, Recording};
use crate::context::{self, AppContext};
use crate::db::{HistoryItem, UsageEvent};
use crate::settings::{ProviderKind, Settings};
use crate::stt::{PartialFn, Provider, Request};
use crate::{llm, text, App};
use anyhow::{anyhow, Result};
use serde::Serialize;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tauri::{Emitter, Manager};
use tauri::async_runtime::JoinHandle;

#[derive(Clone, Copy, PartialEq, Debug, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Dictate,
    Command,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DictationState {
    pub status: &'static str,
    pub mode: Mode,
    pub hands_free: bool,
    /// While processing: "transcribing" | "polishing" (empty otherwise).
    pub stage: &'static str,
    /// While recording: the microphone delivers audio (it can take seconds to open on Bluetooth).
    pub mic_ready: bool,
}

/// How a dictation ended, shown briefly in the pill: "ok" | "warn" | "error".
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Outcome {
    pub kind: &'static str,
    pub message: String,
}

pub struct Session {
    id: u64,
    mode: Mode,
    hands_free: bool,
    pressed_at: Instant,
    mic_ready: Arc<std::sync::atomic::AtomicBool>,
    recording: Recording,
    stream: Option<(usize, JoinHandle<Result<String>>)>,
    ctx: AppContext,
    providers: Vec<Provider>,
    /// Set when recording ends; stops the local segments loop.
    done: Arc<std::sync::atomic::AtomicBool>,
    /// Local model at the chain head: phrases transcribed while recording (see `spawn_local_segments`).
    segments: Option<std::thread::JoinHandle<Option<Segments>>>,
}

/// Phrases a local model already transcribed while the user was speaking: the text and how many
/// samples of the recording it covers.
type Segments = (usize, Vec<String>);


/// A press shorter than this is a "tap": it switches to hands-free mode instead of push-to-talk.
const TAP: Duration = Duration::from_millis(350);
/// Hands-free recordings stop by themselves (and are transcribed) after this long without speech,
/// or at this total length, so a forgotten open mic doesn't record for hours.
const HANDS_FREE_SILENCE: Duration = Duration::from_secs(120);
const HANDS_FREE_MAX: Duration = Duration::from_secs(30 * 60);
/// How long the pill keeps showing how a dictation ended.
const LINGER_OK: u64 = 900;
const LINGER_PROBLEM: u64 = 3000;

static SESSION_IDS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// Whether a hands-free recording should stop by itself.
fn hands_free_expired(elapsed: Duration, since_speech: Duration) -> bool {
    elapsed >= HANDS_FREE_MAX || since_speech >= HANDS_FREE_SILENCE
}

impl App {
    pub fn emit_state(&self, status: &'static str, mode: Mode, hands_free: bool) {
        self.emit_dictation(DictationState { status, mode, hands_free, stage: "", mic_ready: false });
    }

    fn emit_stage(&self, mode: Mode, stage: &'static str) {
        self.emit_dictation(DictationState { status: "processing", mode, hands_free: false, stage, mic_ready: true });
    }

    /// The current recording's state (mode, hands-free, microphone ready).
    fn emit_recording(&self) {
        let session = self.session.lock().unwrap();
        let state = session.as_ref().map(|s| DictationState {
            status: "recording",
            mode: s.mode,
            hands_free: s.hands_free,
            stage: "",
            mic_ready: s.mic_ready.load(std::sync::atomic::Ordering::SeqCst),
        });
        if let Some(state) = state {
            self.emit_dictation(state);
        }
    }

    fn emit_dictation(&self, state: DictationState) {
        use std::sync::atomic::Ordering;
        let _overlay = self.overlay_lock.lock().unwrap();
        let idle = state.status == "idle";
        let _ = self.handle.emit("dictation-state", state);
        let generation = self.overlay_gen.fetch_add(1, Ordering::SeqCst) + 1;
        if !idle {
            self.set_overlay_visible(generation, self.settings().overlay);
            return;
        }
        // Keep an outcome on screen for a moment; a newer state (e.g. the next press) wins.
        match self.overlay_linger.swap(0, Ordering::SeqCst) {
            0 => {
                self.set_overlay_visible(generation, false);
            }
            ms => self.hide_overlay_after(generation, ms),
        }
    }

    /// Check the generation on the event loop, where show/hide actually happen. Checking on a
    /// timer thread before queueing a hide still lets it run after a newer recording's show.
    fn set_overlay_visible(&self, generation: u64, visible: bool) {
        let handle = self.handle.clone();
        let _ = self.handle.run_on_main_thread(move || {
            let Some(app) = handle.try_state::<Arc<App>>() else { return };
            if app.overlay_gen.load(std::sync::atomic::Ordering::SeqCst) == generation {
                if let Some(w) = handle.get_webview_window("overlay") {
                    let _ = if visible { w.show() } else { w.hide() };
                }
            }
        });
    }

    fn hide_overlay_after(&self, generation: u64, ms: u64) {
        let handle = self.handle.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(ms));
            let Some(app) = handle.try_state::<Arc<App>>() else { return };
            app.set_overlay_visible(generation, false);
        });
    }

    /// GNOME on Wayland focuses the pill when it's shown (unfocusable is an X11/macOS hint), so a
    /// synthetic Ctrl+V or Ctrl+C would go to the pill. Hide it and let focus go back to the
    /// user's app before sending keys. Call from a blocking thread, not the main thread.
    fn hide_overlay_for_keys(&self) {
        use std::sync::atomic::Ordering;
        if !crate::insert::is_wayland() || !self.settings().overlay {
            return;
        }
        {
            let _overlay = self.overlay_lock.lock().unwrap();
            // Drops a show or hide still queued from before.
            self.overlay_gen.fetch_add(1, Ordering::SeqCst);
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let handle = self.handle.clone();
        let _ = self.handle.run_on_main_thread(move || {
            if let Some(w) = handle.get_webview_window("overlay") {
                let _ = w.hide();
            }
            let _ = tx.send(());
        });
        let _ = rx.recv_timeout(Duration::from_millis(500));
        std::thread::sleep(Duration::from_millis(150));
    }

    /// Tells the pill how the dictation ended. While idle (e.g. a press that couldn't start), the
    /// pill is shown just for the message.
    pub fn outcome(&self, kind: &'static str, message: impl Into<String>) {
        let active = self.session.lock().unwrap().is_some() || self.busy.load(std::sync::atomic::Ordering::SeqCst);
        self.emit_outcome(kind, message, active);
    }

    fn emit_outcome(&self, kind: &'static str, message: impl Into<String>, active: bool) {
        use std::sync::atomic::Ordering;
        let _overlay = self.overlay_lock.lock().unwrap();
        let ms = if kind == "ok" { LINGER_OK } else { LINGER_PROBLEM };
        let _ = self.handle.emit("dictation-outcome", Outcome { kind, message: message.into() });
        if active {
            // Applied when the dictation goes idle.
            self.overlay_linger.store(ms, Ordering::SeqCst);
        } else if self.settings().overlay {
            let generation = self.overlay_gen.fetch_add(1, Ordering::SeqCst) + 1;
            self.set_overlay_visible(generation, true);
            self.hide_overlay_after(generation, ms);
        }
    }

    pub fn is_recording(&self) -> bool {
        self.session.lock().unwrap().is_some()
    }

    /// Hotkey pressed. Returns quickly; recording runs on its own thread.
    pub fn on_press(self: &Arc<Self>, mode: Mode) {
        // Measure the hold from the key press, not from when the mic finished opening (which can
        // take seconds on Bluetooth): otherwise short holds would be misread as taps.
        let pressed_at = Instant::now();
        let mut guard = self.session.lock().unwrap();
        if let Some(s) = guard.as_ref() {
            if s.hands_free {
                let s = self.take_for_finish(&mut guard).unwrap();
                drop(guard);
                self.finish(s);
            }
            return;
        }
        if self.busy.load(std::sync::atomic::Ordering::SeqCst) {
            return;
        }
        match self.start(mode, pressed_at) {
            Ok(s) => {
                *guard = Some(s);
                drop(guard);
                crate::hotkeys::set_cancel_key(self, true);
                self.emit_recording();
            }
            Err(e) => {
                drop(guard);
                self.report_error(&format!("{e:#}"));
            }
        }
    }

    /// Hotkey released: a tap enters hands-free mode, a hold ends push-to-talk.
    pub fn on_release(self: &Arc<Self>) {
        let mut guard = self.session.lock().unwrap();
        let Some(s) = guard.as_mut() else { return };
        if s.hands_free {
            return;
        }
        if s.pressed_at.elapsed() < TAP {
            s.hands_free = true;
            let id = s.id;
            drop(guard);
            self.emit_recording();
            self.watch_hands_free(id);
            return;
        }
        let s = self.take_for_finish(&mut guard).unwrap();
        drop(guard);
        self.finish(s);
    }

    /// Start/stop without press/release semantics (CLI `--toggle`, tray, UI buttons).
    pub fn toggle(self: &Arc<Self>, mode: Mode) {
        let session = self.take_for_finish(&mut self.session.lock().unwrap());
        match session {
            Some(s) => self.finish(s),
            None => {
                self.on_press(mode);
                let id = self.session.lock().unwrap().as_mut().map(|s| {
                    s.hands_free = true;
                    s.id
                });
                if let Some(id) = id {
                    self.emit_recording();
                    self.watch_hands_free(id);
                }
            }
        }
    }

    /// Keeps the current recording going without the key held (Fn double-tap).
    pub fn set_hands_free(self: &Arc<Self>) {
        let mut guard = self.session.lock().unwrap();
        let Some(s) = guard.as_mut() else { return };
        if s.hands_free {
            return;
        }
        s.hands_free = true;
        let id = s.id;
        drop(guard);
        self.emit_recording();
        self.watch_hands_free(id);
    }

    pub fn stop(self: &Arc<Self>) {
        let session = self.take_for_finish(&mut self.session.lock().unwrap());
        if let Some(s) = session {
            self.finish(s);
        }
    }

    /// Reserve processing before releasing the session lock, so another press cannot start in
    /// the gap between taking a recording (including auto-stop) and calling `finish`.
    fn take_for_finish(&self, slot: &mut Option<Session>) -> Option<Session> {
        let session = slot.take();
        if session.is_some() {
            self.busy.store(true, std::sync::atomic::Ordering::SeqCst);
        }
        session
    }

    /// Stops hands-free recording `id` after a long silence or at the length cap. Only the audio
    /// recorded since the last check is measured, once a second.
    fn watch_hands_free(self: &Arc<Self>, id: u64) {
        let app = self.clone();
        std::thread::spawn(move || {
            let (mut checked, mut last_speech) = (0usize, Instant::now());
            loop {
                std::thread::sleep(Duration::from_secs(1));
                let mut guard = app.session.lock().unwrap();
                let Some(s) = guard.as_ref().filter(|s| s.id == id) else { return };
                let fresh = {
                    let buffer = s.recording.buffer();
                    let b = buffer.lock().unwrap_or_else(|e| e.into_inner());
                    let fresh = b.get(checked..).map(|new| !audio::is_silent(new));
                    checked = b.len();
                    fresh
                };
                if fresh == Some(true) {
                    last_speech = Instant::now();
                }
                if hands_free_expired(s.pressed_at.elapsed(), last_speech.elapsed()) {
                    log::info!("hands-free recording stopped automatically");
                    let s = app.take_for_finish(&mut guard).unwrap();
                    drop(guard);
                    app.finish(s);
                    return;
                }
            }
        });
    }

    pub fn cancel(self: &Arc<Self>) {
        let mut guard = self.session.lock().unwrap();
        if let Some(s) = guard.take() {
            self.cancel_session(s);
        }
    }

    fn cancel_session(self: &Arc<Self>, s: Session) {
        s.done.store(true, std::sync::atomic::Ordering::SeqCst);
        if let Some((_, task)) = s.stream {
            task.abort();
        }
        // Closing waits for the audio thread, which can take seconds while a Bluetooth mic is
        // still opening; callers are key handlers that must not block.
        let recording = s.recording;
        std::thread::spawn(move || drop(recording));
        crate::hotkeys::set_cancel_key(self, false);
        self.emit_state("idle", s.mode, false);
    }

    fn start(self: &Arc<Self>, mode: Mode, pressed_at: Instant) -> Result<Session> {
        let settings = self.settings();
        let providers = self.providers(&settings);
        if providers.is_empty() {
            return Err(anyhow!("No transcription provider is enabled. Add one in Settings → Transcription."));
        }
        if mode == Mode::Command && !settings.llm.enabled {
            return Err(anyhow!("Command mode needs AI. Turn on AI polish in Settings → AI polish."));
        }
        if mode == Mode::Command && settings.llm.provider == crate::polish::PROVIDER {
            return Err(anyhow!("Command mode needs a cloud or OpenAI-compatible AI provider; {} only polishes dictation.", crate::polish::NAME));
        }
        let ctx = context::current(&settings.app_categories);
        let language = lang(&settings);
        let vocabulary = self.vocabulary();

        // Stream to the first streaming-capable provider in the chain head, if configured.
        let (chunk_tx, stream) = match providers.first().filter(|p| p.supports_streaming()) {
            Some(p) => {
                let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Vec<i16>>();
                let (p, app) = (p.clone(), self.clone());
                let on_partial: PartialFn = Arc::new(move |t: String| {
                    let _ = app.handle.emit("partial-transcript", t);
                });
                let task = tauri::async_runtime::spawn(async move {
                    // Chunks recorded while connecting wait in the channel.
                    let connecting = Instant::now();
                    let stream = p.stream(language, vocabulary, on_partial).await?;
                    log::debug!("{} stream connected in {} ms", p.label(), connecting.elapsed().as_millis());
                    while let Some(chunk) = rx.recv().await {
                        if stream.audio.send(chunk).is_err() {
                            break;
                        }
                    }
                    stream.finish(Duration::from_secs(6)).await
                });
                (Some(tx), Some((0, task)))
            }
            None => (None, None),
        };

        let app = self.clone();
        // Don't wait for the device here: this runs on the hotkey thread, and blocking it delays the
        // release event, which would make a quick tap look like a hold (and vice versa).
        let (recording, ready) = Recording::open(settings.microphone.clone(), chunk_tx, move |level| {
            let _ = app.handle.emit("audio-level", level);
        })?;
        let (app, stop) = (self.clone(), recording.stop_flag());
        let mic_ready = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = mic_ready.clone();
        let id = SESSION_IDS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        std::thread::spawn(move || {
            let result = Recording::wait_ready_with(&stop, &ready);
            let mut guard = app.session.lock().unwrap();
            // A cancelled mic may finish opening after the next dictation has started.
            if guard.as_ref().is_none_or(|s| s.id != id) {
                return;
            }
            match result {
                Ok(()) => {
                    flag.store(true, std::sync::atomic::Ordering::SeqCst);
                    drop(guard);
                    app.emit_recording();
                }
                Err(e) => {
                    let session = guard.take().unwrap();
                    let message = format!("Microphone: {e:#}");
                    app.toast_error(&message);
                    app.emit_outcome("error", message, true);
                    app.cancel_session(session);
                }
            }
        });
        let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let segments = providers
            .first()
            .filter(|p| p.entry.provider == ProviderKind::Local)
            .map(|p| self.spawn_local_segments(p.clone(), lang(&settings), recording.buffer(), done.clone(), settings.overlay));
        Ok(Session { id, mode, hands_free: false, pressed_at, mic_ready, recording, stream, ctx, providers, done, segments })
    }

    /// Local models have no streaming API, and one decode of a whole recording is slow (it grows
    /// much faster than the audio: 98 s took 57 s with Moonshine Tiny) and makes the model repeat
    /// itself after about a minute. So each phrase is transcribed when a pause ends it, and on
    /// release only the last one is left. In between, the last few seconds are re-decoded about
    /// once a second as a preview for the overlay. Returns None if the model failed.
    fn spawn_local_segments(
        self: &Arc<Self>,
        p: Provider,
        language: Option<String>,
        buffer: Arc<std::sync::Mutex<Vec<i16>>>,
        done: Arc<std::sync::atomic::AtomicBool>,
        preview: bool,
    ) -> std::thread::JoinHandle<Option<Segments>> {
        let app = self.clone();
        std::thread::spawn(move || {
            use std::sync::atomic::Ordering;
            const RATE: usize = audio::SAMPLE_RATE as usize;
            let decode = |a: &[i16]| crate::stt::local::transcribe_blocking(&p.models_dir, &p.entry.model, language.as_deref(), a);
            let (mut committed, mut texts) = (0usize, Vec::<String>::new());
            let mut last_preview = Instant::now();
            // Whisper: one pass on release unless the recording outgrows its 30 s window.
            let fixed = crate::stt::local::fixed_window(&p.entry.model);
            let shortest = if fixed { RATE * 20 } else { 0 };
            while !done.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(300));
                // Empty once the recording is stopped and its buffer taken.
                let pending = buffer.lock().unwrap_or_else(|e| e.into_inner()).get(committed..).map(<[i16]>::to_vec).unwrap_or_default();
                // A fixed-window model only hears its first 30 s, so a segment must end within them.
                let (window, max) = if fixed { (&pending[..pending.len().min(RATE * 30)], RATE * 29) } else { (&pending[..], RATE * 30) };
                let cut = if pending.len() >= shortest { audio::split_point(window, RATE * 6 / 10, max) } else { None };
                let shown = if let Some(cut) = cut {
                    if !audio::is_silent(&pending[..cut]) {
                        texts.push(decode(&pending[..cut]).ok()?);
                    }
                    committed += cut;
                    texts.join(" ")
                } else if preview && last_preview.elapsed() >= Duration::from_millis(900) {
                    last_preview = Instant::now();
                    let tail = &pending[pending.len().saturating_sub(RATE * 5)..];
                    if audio::is_silent(tail) {
                        continue;
                    }
                    let t = decode(tail).ok()?;
                    texts.iter().map(String::as_str).chain([t.as_str()]).collect::<Vec<_>>().join(" ")
                } else {
                    continue;
                };
                if preview && !done.load(Ordering::SeqCst) && !shown.trim().is_empty() {
                    let _ = app.handle.emit("partial-transcript", shown.trim().to_string());
                }
            }
            Some((committed, texts))
        })
    }

    fn finish(self: &Arc<Self>, s: Session) {
        s.done.store(true, std::sync::atomic::Ordering::SeqCst);
        crate::hotkeys::set_cancel_key(self, false);
        self.busy.store(true, std::sync::atomic::Ordering::SeqCst);
        self.emit_stage(s.mode, "transcribing");
        let app = self.clone();
        tauri::async_runtime::spawn(async move {
            let mode = s.mode;
            // Its own task, so a panic in it still ends here and doesn't leave the app busy for good.
            let worker = app.clone();
            match tauri::async_runtime::spawn(async move { worker.process(s).await }).await {
                Ok(Ok(())) => {}
                Ok(Err(e)) => app.report_error(&format!("{e:#}")),
                Err(e) => app.report_error(&format!("Dictation failed unexpectedly: {e}")),
            }
            let _session = app.session.lock().unwrap_or_else(|e| e.into_inner());
            app.busy.store(false, std::sync::atomic::Ordering::SeqCst);
            app.emit_state("idle", mode, false);
        });
    }

    async fn process(self: &Arc<Self>, s: Session) -> Result<()> {
        let stopped = Instant::now();
        let Session { mode, recording, stream, ctx, providers, segments, .. } = s;
        let audio = tokio::task::spawn_blocking(move || recording.stop()).await?;
        let stop_ms = stopped.elapsed().as_millis();
        let audio_ms = audio::duration_ms(&audio);
        // Debugging aid: OPSTRELLIS_DUMP_AUDIO=/some/dir saves the last recording as last.wav.
        if let Ok(dir) = std::env::var("OPSTRELLIS_DUMP_AUDIO") {
            let _ = std::fs::write(std::path::Path::new(&dir).join("last.wav"), audio::to_wav(&audio));
        }
        if audio::is_silent(&audio) {
            if let Some((_, t)) = stream {
                t.abort();
            }
            log::info!("no speech detected ({audio_ms} ms)");
            self.outcome("warn", "No speech heard. Check your microphone.");
            return Ok(());
        }
        let settings = self.settings();
        let vocabulary = self.vocabulary();
        let transcribing = Instant::now();
        let local = match segments {
            Some(seg) => finish_local_segments(seg, &providers[0], &audio, lang(&settings)).await,
            None => None,
        };
        let (raw, used) = match local {
            Some(text) => (text, &providers[0]),
            // No local head, the model failed, or it heard nothing: the normal chain.
            None => transcribe_with_fallback(&providers, stream, &audio, lang(&settings).as_deref(), &vocabulary).await?,
        };
        // Tags for noise ("[BLANK_AUDIO]") and sign-offs invented from it aren't words.
        let raw = crate::stt::clean_transcript(&raw);
        let transcribe_ms = transcribing.elapsed().as_millis();
        let mut item = HistoryItem {
            created_at: crate::db::now_rfc3339(),
            raw_text: raw.clone(),
            app_name: ctx.app_name.clone(),
            category: ctx.category.to_string(),
            provider: Some(provider_name(used.entry.provider).into()),
            model: Some(used.entry.model.clone()),
            audio_ms,
            status: "ok".into(),
            mode: if mode == Mode::Command { "command".into() } else { "dictate".into() },
            ..Default::default()
        };
        if raw.trim().is_empty() {
            self.outcome("warn", "Didn't catch any words.");
            return Ok(());
        }

        let cleaning = Instant::now();
        if settings.llm.enabled {
            self.emit_stage(mode, "polishing");
        }
        let outcome = match mode {
            Mode::Dictate => self.clean_up(&raw, &ctx, &settings, &vocabulary).await,
            Mode::Command => self.run_command(&raw, &ctx, &settings).await,
        };
        let cleanup_ms = cleaning.elapsed().as_millis();
        let Cleaned { text: final_text, fixes, press_enter, used: used_entries, warning, copy } = match outcome {
            Ok(v) => v,
            Err(e) => {
                item.status = "error".into();
                item.error = Some(format!("{e:#}"));
                let _ = self.save_history(&item, &settings);
                return Err(e);
            }
        };
        item.text = final_text.clone();
        item.words = text::word_count(&final_text) as i64;
        item.fixes = fixes as i64;
        *self.last_text.lock().unwrap() = Some(crate::LastTranscript { text: final_text.clone(), history_id: None });

        let ins = self.clone();
        let (t, c, method, restore, delay) =
            (final_text.clone(), ctx.clone(), settings.insertion.method.clone(), settings.insertion.restore_clipboard, settings.insertion.paste_delay_ms);
        let inserting = Instant::now();
        // Some(app) when focus moved to another window while processing: the text goes to the
        // clipboard instead of into the wrong app.
        let inserted = tokio::task::spawn_blocking(move || -> Result<Option<String>> {
            // Actions that produce text for elsewhere (e.g. a reply) leave the target untouched.
            if copy {
                return ins.inserter.set_clipboard(&t).map(|_| None);
            }
            // ⌘Tab completes only when ⌘ is let go, so check focus after the keys are up.
            if crate::insert::wait_for_keys_released() {
                std::thread::sleep(Duration::from_millis(150));
            }
            if let Some(app) = context::focus_moved(&c) {
                ins.inserter.set_clipboard(&t)?;
                return Ok(Some(app));
            }
            ins.hide_overlay_for_keys();
            ins.inserter.insert(&t, &c, &method, restore, delay)?;
            if press_enter {
                ins.inserter.press_enter()?;
            }
            Ok(None)
        })
        .await?;
        item.latency_ms = stopped.elapsed().as_millis() as i64;
        log::info!(
            "after release: {} ms total = stop mic {stop_ms} + transcribe {transcribe_ms} ({}) + cleanup {cleanup_ms} + insert {} ms; {audio_ms} ms of audio",
            item.latency_ms,
            used.label(),
            inserting.elapsed().as_millis()
        );
        let moved = inserted.as_ref().ok().cloned().flatten();
        if let Some(app) = &moved {
            log::info!("focus moved to {app} before pasting; text left on the clipboard");
        }
        if let Err(e) = inserted {
            // Never lose a dictation: leave it on the clipboard.
            let recovery = match self.inserter.set_clipboard(&final_text) {
                Ok(()) => "The text was copied to your clipboard instead.",
                Err(_) => "Clipboard unavailable; use Paste last transcript to recover the text.",
            };
            let operation = if copy { "copy" } else { "insert" };
            item.status = "error".into();
            item.error = Some(format!("Could not {operation} text ({e:#}). {recovery}"));
            self.save_transcript(&item, &settings);
            return Err(anyhow!("Could not {operation} text: {e:#}. {recovery}"));
        }
        self.save_transcript(&item, &settings);
        if mode == Mode::Dictate {
            let terms = if settings.cleanup.tech_terms { crate::terms::find(&final_text) } else { Vec::new() };
            if let Err(e) = self.db.record_usage(&used_entries, &terms) {
                log::warn!("recording dictionary usage failed: {e:#}");
            }
        }
        self.queue_usage(&item, used.entry.provider, &settings);
        let _ = self.handle.emit("dictation-result", serde_json::json!({ "text": final_text, "latencyMs": item.latency_ms }));
        let secs = format!("{:.1} s", item.latency_ms as f64 / 1000.0);
        match (warning, copy) {
            _ if moved.is_some() => {
                let app = moved.unwrap();
                let app = if app.trim().is_empty() { "another window".to_string() } else { app };
                self.outcome("warn", format!("You switched to {app}, so the text is on your clipboard. Paste it where you want it."))
            }
            (Some(w), _) => {
                let _ = self.handle.emit("dictation-error", w.clone());
                self.outcome("warn", w)
            }
            (None, true) => self.outcome("ok", format!("Copied · {secs}")),
            (None, false) => self.outcome("ok", format!("Inserted · {secs}")),
        }
        log::info!("dictation done: {} words, {} ms after release via {}", item.words, item.latency_ms, used.label());
        Ok(())
    }

    /// Rules, then (with AI on) one LLM call that cleans up the text and applies the category's auto
    /// action, then the rules again on its output (dictionary, snippets, tone).
    pub async fn clean_up(&self, raw: &str, ctx: &AppContext, settings: &Settings, vocabulary: &[String]) -> Result<Cleaned> {
        let dictionary = self.db.list_dictionary()?;
        let snippets = self.db.list_snippets()?;
        let tone = settings.tone(ctx.style_category()).to_string();
        // Read at most once, only when a matching snippet expands. The contents stay local and
        // are never included in the AI request.
        let clipboard = std::sync::OnceLock::new();
        let read_clipboard = || clipboard.get_or_init(|| self.inserter.get_clipboard()).clone();
        let full = text::Options {
            remove_fillers: settings.cleanup.remove_fillers,
            voice_commands: settings.cleanup.voice_commands,
            dictionary: &dictionary,
            snippets: &snippets,
            tone: &tone,
            english: settings.language == "en" || settings.language.starts_with("en-"),
            technical: ctx.is_technical(),
            code: ctx.is_technical() || ctx.category == "ai_prompts",
            backticks: ctx.category == "ai_prompts" && !ctx.is_terminal,
            terms: settings.cleanup.tech_terms,
            preserve_formatting: false,
            clipboard: Some(&read_clipboard),
        };
        // Local polish skips code editors, terminals and other languages: the rules alone, as with AI off.
        let local = settings.llm.provider == crate::polish::PROVIDER;
        if !settings.llm.enabled || (local && !crate::polish::applies(ctx, &settings.language, raw)) {
            let p = text::process(raw, &full);
            return Ok(Cleaned::from(p));
        }
        // Built-in terms and snippets run after the LLM, so a snippet trigger that is also an
        // alias ("type script") still expands.
        let pre = text::process(raw, &text::Options { snippets: &[], tone: "none", terms: false, ..full });
        // Nothing left to polish ("uhm", a lone "press enter"): any model output would be invented.
        if pre.text.trim().is_empty() {
            return Ok(Cleaned::from(text::process(raw, &full)));
        }
        // A dictation that is only a snippet trigger needs no AI.
        if let Some(s) = text::whole_snippet(&pre.text, &snippets) {
            return Ok(Cleaned { text: full.expand_snippet(&s.expansion), fixes: pre.fixes, press_enter: pre.press_enter, used: pre.used, ..Default::default() });
        }
        // The local model only cleans up; it can't follow an action's instructions.
        let action = if local { None } else { self.auto_transform(ctx, settings) };
        let rewrite = action.as_ref().map(|t| t.prompt.as_str());
        // A rewrite must not reword the snippet triggers it's given, or they wouldn't expand.
        let mut keep = vocabulary.to_vec();
        if rewrite.is_some() {
            keep.extend(text::snippet_triggers_in(&pre.text, &snippets));
        }
        let mut warning = None;
        let system = llm::cleanup_prompt(
            ctx,
            &llm::Cleanup {
                tone: &tone,
                vocabulary: &keep,
                rewrite,
                notes: settings.style_notes.get(ctx.style_category()).map(String::as_str),
                structured: settings.prompt_style == "structured",
            },
        );
        let request = if local {
            let (dir, input, control) = (self.models_dir.clone(), pre.text.clone(), crate::polish::control_line(&tone, ctx.style_category()));
            let deadline = Instant::now() + llm::CLEANUP_TIMEOUT;
            tokio::task::spawn_blocking(move || crate::polish::polish_blocking(&dir, &input, &control, deadline)).await.map_err(anyhow::Error::from).and_then(|r| r)
        } else {
            self.llm_client(settings).complete_within(&system, &llm::wrap_dictation(&pre.text), llm::CLEANUP_TIMEOUT).await
        };
        // A cleanup (not a rewrite action) must keep every dictionary term that was said, offline
        // or cloud: the local model can't be told the user's words, so this is how it respects them.
        let terms: Vec<String> = dictionary.iter().map(|e| e.replacement.clone().unwrap_or_else(|| e.phrase.clone())).collect();
        let lost = match &request {
            Ok(out) if rewrite.is_none() => llm::lost_term(&pre.text, out, &terms).map(str::to_string),
            _ => None,
        };
        let cleaned = match request {
            Ok(_) if lost.is_some() => {
                let term = lost.unwrap_or_default();
                log::warn!("discarded AI cleanup that changed a dictionary term");
                warning = Some(format!("AI polish changed “{term}” from your dictionary. Inserted plain text."));
                pre.text.clone()
            }
            Ok(out) if llm::accept_cleanup(&pre.text, &out, rewrite.is_some()) => out,
            Ok(out) => {
                log::warn!("discarded implausible LLM cleanup output ({} chars)", out.len());
                warning = Some("AI polish skipped (odd result). Inserted plain text.".to_string());
                pre.text.clone()
            }
            Err(e) => {
                log::warn!("LLM cleanup failed, using rule-based cleanup: {e:#}");
                let reason: String = e.root_cause().to_string().chars().take(80).collect();
                warning = Some(match &action {
                    Some(t) => format!("AI unavailable ({reason}): “{}” skipped. Inserted plain text.", t.name),
                    None => format!("AI polish unavailable ({reason}). Inserted plain text."),
                });
                pre.text.clone()
            }
        };
        // A rewritten text keeps the action's formatting rather than the category's tone rules.
        let tone_after = if rewrite.is_some() && warning.is_none() { "none" } else { tone.as_str() };
        let post = text::process(&cleaned, &text::Options {
            // The local model doesn't write backticks: wrap code again (it skips what is already wrapped).
            remove_fillers: false, voice_commands: false, backticks: local && full.backticks, tone: tone_after,
            preserve_formatting: rewrite.is_some() && warning.is_none(), ..full
        });
        let mut used = pre.used;
        for id in post.used {
            if !used.contains(&id) {
                used.push(id);
            }
        }
        Ok(Cleaned { text: post.text, fixes: pre.fixes + post.fixes, press_enter: pre.press_enter, used, warning, copy: false })
    }

    /// The action the user chose for this kind of app (Style → Auto transform). Not in shells (a
    /// rewritten command is dangerous), but in AI agents running in a terminal.
    fn auto_transform(&self, ctx: &AppContext, settings: &Settings) -> Option<crate::db::Transform> {
        let id = settings.auto_transforms.get(ctx.style_category())?;
        if !ctx.allows_rewrite() {
            return None;
        }
        // Only actions that replace the dictation can run on every dictation (Style lists only those),
        // so one later switched to copying its result stops applying instead of hiding as "Off".
        self.db.list_transforms().unwrap_or_default().into_iter().find(|t| t.id == Some(*id) && t.output == "replace")
    }

    async fn run_command(self: &Arc<Self>, instruction: &str, ctx: &AppContext, settings: &Settings) -> Result<Cleaned> {
        let instruction = &text::apply_dictionary(instruction, &self.db.list_dictionary()?);
        let ins = self.clone();
        let c = ctx.clone();
        // Copying in another window would send ⌘C/Ctrl+C there (interrupting a terminal) and its
        // selection to the AI.
        let selection = tokio::task::spawn_blocking(move || {
            if crate::insert::wait_for_keys_released() {
                std::thread::sleep(Duration::from_millis(150));
            }
            if let Some(app) = context::focus_moved(&c) {
                return Err(anyhow!("You switched to {app}, so the command was cancelled. Select the text again and retry."));
            }
            ins.hide_overlay_for_keys();
            ins.inserter.copy_selection(&c)
        })
        .await??;
        // "polish" / "make formal" etc. run the saved transform prompt.
        let spoken = instruction.trim().trim_end_matches(['.', '!', '?']).to_lowercase();
        let transform = command_action(&spoken, self.db.list_transforms()?);
        let copy = transform.as_ref().is_some_and(|t| t.output == "copy");
        // A named action keeps what was said around its name ("reply saying Thursday works").
        let instruction = match transform {
            Some(t) if spoken == t.name.to_lowercase() => t.prompt,
            Some(t) => format!("{}\nThe user also said: {}", t.prompt, instruction.trim()),
            None => instruction.to_string(),
        };
        let out = self
            .llm_client(settings)
            .complete(&llm::command_prompt(selection.as_deref()), &llm::command_input(selection.as_deref(), &instruction))
            .await?;
        llm::check_command_output(selection.as_deref(), &out)?;
        Ok(Cleaned { text: out, copy, ..Default::default() })
    }

    pub fn llm_client(&self, settings: &Settings) -> llm::Client {
        let cloud = settings.llm.provider == "cloud";
        llm::Client {
            provider: settings.llm.provider.clone(),
            base_url: match settings.llm.provider.as_str() {
                "cloud" => Some(format!("{}/v1", settings.cloud.base_url.trim_end_matches('/'))),
                "openai" => settings.llm.base_url.clone(),
                // The field is only shown for OpenAI-compatible providers; a stale value from one
                // must not send the Anthropic key and transcripts to that host.
                _ => None,
            },
            model: settings.llm.model.clone(),
            key: self.secrets.get(if cloud { "cloud" } else { "llm" }),
        }
    }

    /// Enabled chain entries with their keys resolved.
    pub fn providers(&self, settings: &Settings) -> Vec<Provider> {
        settings
            .stt_chain
            .iter()
            .filter(|e| e.enabled)
            .map(|e| {
                let mut entry = e.clone();
                let key = if e.provider == ProviderKind::Cloud {
                    entry.base_url = Some(settings.cloud.base_url.clone());
                    self.secrets.get("cloud")
                } else {
                    self.secrets.get(&format!("stt:{}", e.id))
                };
                Provider { entry, key, models_dir: self.models_dir.clone() }
            })
            .collect()
    }

    /// Transcription hints: the dictionary (most used first), then up to 30 built-in terms the
    /// user says most.
    pub fn vocabulary(&self) -> Vec<String> {
        let terms = if self.settings().cleanup.tech_terms { 30 } else { 0 };
        self.db.vocabulary(100, terms).unwrap_or_default()
    }

    fn save_transcript(&self, item: &HistoryItem, settings: &Settings) {
        let history_id = self.save_history(item, settings);
        *self.last_text.lock().unwrap() = Some(crate::LastTranscript { text: item.text.clone(), history_id });
    }

    fn save_history(&self, item: &HistoryItem, settings: &Settings) -> Option<i64> {
        let keep = settings.privacy.history_retention_days != Some(0);
        let id = self.db.add_history(item, keep).unwrap_or_else(|e| {
            log::error!("saving history failed: {e:#}");
            None
        });
        // Enforce retention continuously (the app can run in the tray for weeks).
        if let Some(days) = settings.privacy.history_retention_days.filter(|d| *d > 0) {
            let _ = self.db.purge_history_older_than(days);
        }
        let _ = self.handle.emit("history-updated", ());
        id
    }

    fn queue_usage(&self, item: &HistoryItem, kind: ProviderKind, settings: &Settings) {
        // Cloud gateway calls are metered server-side; only BYOK/local usage is reported.
        if !settings.privacy.sync_usage || kind == ProviderKind::Cloud || !self.secrets.has("cloud") {
            return;
        }
        let ev = UsageEvent {
            id: uuid::Uuid::new_v4().to_string(),
            provider: item.provider.clone().unwrap_or_default(),
            model: item.model.clone().unwrap_or_default(),
            mode: if kind == ProviderKind::Local { "local".into() } else { "byok".into() },
            audio_seconds: item.audio_ms as f64 / 1000.0,
            words: item.words,
            app_category: item.category.clone(),
            created_at: chrono::Utc::now().to_rfc3339(),
        };
        if let Err(e) = self.db.queue_usage(&ev) {
            log::warn!("queue usage failed: {e:#}");
            return;
        }
        let app = self.handle.state::<Arc<App>>().inner().clone();
        tauri::async_runtime::spawn(async move {
            if let Err(e) = crate::cloud::flush_usage(&app).await {
                log::debug!("usage sync deferred: {e:#}");
            }
        });
    }

    /// An error in the main window only (e.g. cloud sign-in).
    pub fn toast_error(&self, msg: &str) {
        log::error!("{msg}");
        let _ = self.handle.emit("dictation-error", msg.to_string());
    }

    /// An error in the main window and in the pill, where the user is looking while dictating.
    pub fn report_error(&self, msg: &str) {
        self.toast_error(msg);
        self.outcome("error", msg);
    }

    pub async fn paste_last(self: &Arc<Self>) -> Result<()> {
        // History has the latest correction; with history off, the last text is only in memory.
        let text = self.last_transcript()?.ok_or_else(|| anyhow!("Nothing dictated yet"))?;
        let settings = self.settings();
        let app = self.clone();
        tokio::task::spawn_blocking(move || {
            let ctx = context::current(&settings.app_categories);
            app.inserter.insert(&text, &ctx, &settings.insertion.method, settings.insertion.restore_clipboard, settings.insertion.paste_delay_ms)
        })
        .await?
    }
}

/// The text a dictation or command produced, and how to deliver it.
#[derive(Default)]
pub struct Cleaned {
    pub text: String,
    /// Dictionary corrections + removed fillers (shown as "fixes" in Insights).
    pub fixes: usize,
    pub press_enter: bool,
    /// Dictionary entries used (ranks vocabulary hints).
    pub used: Vec<i64>,
    /// Shown in the pill instead of "Inserted", e.g. when AI polish was skipped.
    pub warning: Option<String>,
    /// Put the text on the clipboard instead of inserting it.
    pub copy: bool,
}

impl From<text::Processed> for Cleaned {
    fn from(p: text::Processed) -> Self {
        Cleaned { text: p.text, fixes: p.fixes, press_enter: p.press_enter, used: p.used, ..Default::default() }
    }
}

/// New built-ins such as "Reply" must not shadow a user's more specific "Reply formally" action.
fn command_action(spoken: &str, actions: Vec<crate::db::Transform>) -> Option<crate::db::Transform> {
    actions.into_iter().filter_map(|action| {
        let name = action.name.to_lowercase();
        let exact = spoken == name;
        (exact || spoken.ends_with(&format!(" {name}")) || spoken.starts_with(&format!("{name} ")))
            .then_some(((exact, name.len(), !action.builtin), action))
    }).max_by_key(|(rank, _)| *rank).map(|(_, action)| action)
}

fn lang(settings: &Settings) -> Option<String> {
    Some(settings.language.clone()).filter(|l| !l.is_empty() && l != "auto")
}

pub fn provider_name(kind: ProviderKind) -> &'static str {
    match kind {
        ProviderKind::Local => "local",
        ProviderKind::Openai => "openai",
        ProviderKind::Deepgram => "deepgram",
        ProviderKind::Assemblyai => "assemblyai",
        ProviderKind::Google => "google",
        ProviderKind::Azure => "azure",
        ProviderKind::Elevenlabs => "elevenlabs",
        ProviderKind::Cloud => "cloud",
    }
}

/// Waits for the phrases transcribed during recording, then transcribes the rest of `audio`.
/// None when the model failed or produced no text, so the caller can fall back.
async fn finish_local_segments(seg: std::thread::JoinHandle<Option<Segments>>, p: &Provider, audio: &[i16], language: Option<String>) -> Option<String> {
    let (p, audio) = (p.clone(), audio.to_vec());
    let text = tokio::task::spawn_blocking(move || {
        let (committed, mut texts) = seg.join().ok()??;
        let decode = |a: &[i16]| crate::stt::local::transcribe_blocking(&p.models_dir, &p.entry.model, language.as_deref(), a);
        let mut rest = audio.get(committed..).unwrap_or_default();
        // A fixed-window model only hears 30 s: the part left on release can be longer if decoding
        // fell behind, so split it the same way as during recording.
        const RATE: usize = audio::SAMPLE_RATE as usize;
        while crate::stt::local::fixed_window(&p.entry.model) && rest.len() > RATE * 30 {
            let cut = audio::split_point(&rest[..RATE * 30], RATE * 6 / 10, RATE * 29).unwrap_or(RATE * 30);
            if !audio::is_silent(&rest[..cut]) {
                texts.push(decode(&rest[..cut]).ok()?);
            }
            rest = &rest[cut..];
        }
        if !audio::is_silent(rest) {
            texts.push(decode(rest).ok()?);
        }
        Some(texts.join(" ").trim().to_string())
    })
    .await
    .ok()??;
    Some(text).filter(|t| !t.is_empty())
}

/// Streaming result if it worked; otherwise batch over the chain (starting with the streaming
/// provider itself, since a failed socket says little about its batch API).
pub async fn transcribe_with_fallback<'a>(
    providers: &'a [Provider],
    stream: Option<(usize, JoinHandle<Result<String>>)>,
    audio: &[i16],
    language: Option<&str>,
    vocabulary: &[String],
) -> Result<(String, &'a Provider)> {
    if let Some((idx, task)) = stream {
        let waiting = Instant::now();
        let result = task.await;
        let (label, ms) = (providers[idx].label(), waiting.elapsed().as_millis());
        match result {
            Ok(Ok(text)) if !text.trim().is_empty() => {
                log::debug!("{label} stream final transcript after {ms} ms");
                return Ok((text, &providers[idx]));
            }
            Ok(Ok(_)) => log::warn!("{label} stream returned no text after {ms} ms; retrying in batch"),
            Ok(Err(e)) => log::warn!("{label} stream failed after {ms} ms: {e:#}; falling back to batch"),
            Err(e) => log::warn!("stream task failed after {ms} ms: {e}"),
        }
    }
    let req = Request { audio, language, vocabulary };
    let mut errors = Vec::new();
    let mut empty = None;
    for p in providers {
        let started = Instant::now();
        let result = tokio::time::timeout(Duration::from_secs(45), p.transcribe(&req)).await;
        log::debug!("{} batch attempt took {} ms", p.label(), started.elapsed().as_millis());
        match result {
            Ok(Ok(text)) if !text.trim().is_empty() => return Ok((text, p)),
            // Nothing recognized (e.g. Azure NoMatch, wrong-language local model): try the next.
            Ok(Ok(_)) => {
                empty.get_or_insert(p);
                continue;
            }
            Ok(Err(e)) => errors.push(format!("{}: {e:#}", p.label())),
            Err(_) => errors.push(format!("{}: timed out", p.label())),
        }
        log::warn!("transcription failed: {}", errors.last().unwrap());
    }
    if let Some(p) = empty {
        return Ok((String::new(), p));
    }
    Err(anyhow!("All transcription providers failed — {}", errors.join("; ")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::SttEntry;

    fn entry(provider: ProviderKind, model: &str) -> SttEntry {
        SttEntry { id: model.into(), provider, model: model.into(), base_url: None, region: None, streaming: false, enabled: true }
    }

    #[test]
    fn hands_free_stops_after_silence_or_cap() {
        let s = Duration::from_secs;
        assert!(!hands_free_expired(s(600), s(30)), "talking, under the cap");
        assert!(hands_free_expired(s(600), s(120)), "two minutes of silence");
        assert!(hands_free_expired(s(30 * 60), s(0)), "length cap even while talking");
    }

    #[test]
    fn specific_user_actions_win_over_builtin_prefixes() {
        let actions = vec![
            crate::db::Transform { id: Some(1), name: "Reply".into(), prompt: "builtin reply".into(), builtin: true, output: "copy".into() },
            crate::db::Transform { id: Some(2), name: "Reply formally".into(), prompt: "my reply".into(), builtin: false, output: "replace".into() },
        ];
        for spoken in ["reply formally", "reply formally saying thursday works", "please reply formally"] {
            let chosen = command_action(spoken, actions.clone()).unwrap();
            assert_eq!((chosen.id, chosen.output.as_str()), (Some(2), "replace"), "{spoken}");
        }
        assert_eq!(command_action("reply saying thursday works", actions.clone()).unwrap().output, "copy");
        assert!(command_action("some unrelated request", actions).is_none());
    }

    /// Fallback with a real failing provider (no key) followed by the real local model.
    #[tokio::test]
    async fn falls_back_to_local_model() {
        let models_dir = std::path::PathBuf::from(std::env::var("OPSTRELLIS_TEST_MODELS").unwrap_or_else(|_| "/tmp/opstrellis-models".into()));
        if !crate::stt::local::is_installed(&models_dir, crate::stt::local::info("moonshine-tiny-en").unwrap()) {
            eprintln!("skipping: moonshine-tiny-en not installed in {}", models_dir.display());
            return;
        }
        let providers = vec![
            Provider { entry: entry(ProviderKind::Deepgram, "nova-3"), key: None, models_dir: models_dir.clone() },
            Provider { entry: entry(ProviderKind::Local, "moonshine-tiny-en"), key: None, models_dir },
        ];
        let audio = crate::stt::sample_audio();
        let (text, used) = transcribe_with_fallback(&providers, None, &audio, None, &[]).await.unwrap();
        assert_eq!(used.entry.provider, ProviderKind::Local);
        assert!(text.to_lowercase().contains("yellow lamps"), "{text}");
    }
}
