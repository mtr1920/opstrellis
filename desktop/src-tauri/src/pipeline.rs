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
    /// While processing: "transcribing" | "polishing" | "inserting" (empty otherwise).
    pub stage: &'static str,
    /// While recording: the microphone delivers audio (it can take seconds to open on Bluetooth).
    pub mic_ready: bool,
    /// Status "review": the rewrite waiting for Enter (insert) or Esc (discard).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub text: String,
    /// While recording: the folder name of the workspace this dictation uses (`workspace.rs`).
    pub workspace: Option<String>,
    /// While recording: this dictation is the answer to an agent's question (`ask.rs`).
    pub answering: bool,
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
    segments: Option<std::thread::JoinHandle<Option<Live>>>,
    /// The folder name of the workspace in use, shown in the pill.
    workspace: Option<String>,
    /// The agent's question this dictation answers (`ask.rs`).
    answers: Option<u64>,
}

use crate::stt::live::Live;


/// A press shorter than this is a "tap": it switches to hands-free mode instead of push-to-talk.
const TAP: Duration = Duration::from_millis(350);
/// Hands-free recordings stop by themselves (and are transcribed) after this long without speech,
/// or at this total length, so a forgotten open mic doesn't record for hours.
const HANDS_FREE_SILENCE: Duration = Duration::from_secs(120);
const HANDS_FREE_MAX: Duration = Duration::from_secs(30 * 60);
/// How long the pill keeps showing how a dictation ended.
const LINGER_OK: u64 = 900;
const LINGER_PROBLEM: u64 = 3000;
/// Transcription hints sent with a dictation, at most.
const HINTS: usize = 100;

static SESSION_IDS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
/// The overlay window has the card's size, for the review card or an agent's question (changed
/// with the state, in `emit_dictation` and `show_question`).
static CARD: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// The last state the pill was given is "idle" (set with it, under `overlay_lock`).
static PILL_IDLE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);
/// A rewrite not inserted or discarded by then is discarded (Enter stays grabbed meanwhile).
const REVIEW_TIMEOUT: Duration = Duration::from_secs(30);
/// "Only long text": a selection or rewrite longer than this many characters is reviewed.
const REVIEW_LONG: usize = 280;

/// Whether a hands-free recording should stop by itself.
fn hands_free_expired(elapsed: Duration, since_speech: Duration) -> bool {
    elapsed >= HANDS_FREE_MAX || since_speech >= HANDS_FREE_SILENCE
}

impl App {
    pub fn emit_state(&self, status: &'static str, mode: Mode, hands_free: bool) {
        self.emit_dictation(DictationState { status, mode, hands_free, stage: "", mic_ready: false, text: String::new(), workspace: None, answering: false });
    }

    pub(crate) fn emit_stage(&self, mode: Mode, stage: &'static str) {
        self.emit_dictation(DictationState { status: "processing", mode, hands_free: false, stage, mic_ready: true, text: String::new(), workspace: None, answering: false });
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
            text: String::new(),
            workspace: s.workspace.clone(),
            answering: s.answers.is_some(),
        });
        if let Some(state) = state {
            self.emit_dictation(state);
        }
    }

    fn emit_dictation(&self, state: DictationState) {
        use std::sync::atomic::Ordering;
        let _overlay = self.overlay_lock.lock().unwrap();
        let idle = state.status == "idle";
        PILL_IDLE.store(idle, Ordering::SeqCst);
        // Idle with a question waiting, the page draws the question card (`Overlay.tsx`).
        let card = state.status == "review" || (idle && crate::ask::QUESTIONS.is_pending());
        if CARD.swap(card, Ordering::SeqCst) != card {
            self.size_overlay(card);
        }
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
                // An agent's question keeps the pill up when nothing else needs it.
                let visible = visible || (crate::ask::QUESTIONS.is_pending() && app.settings().overlay);
                if let Some(w) = handle.get_webview_window("overlay") {
                    let result = if visible { w.show() } else { w.hide() };
                    // A question counts as seen only while the window is up (`ask::answers`).
                    crate::ask::QUESTIONS.window_shown(visible && result.is_ok());
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

    /// Queued on the event loop in order with the show and hide of the same state change. The card
    /// is click-through on every platform: it covers more of the app below than the pill.
    fn size_overlay(&self, card: bool) {
        let handle = self.handle.clone();
        let _ = self.handle.run_on_main_thread(move || {
            if let Some(w) = handle.get_webview_window("overlay") {
                crate::place_overlay(&w, card);
                let _ = w.set_ignore_cursor_events(card || cfg!(target_os = "macos"));
                crate::ask::QUESTIONS.window_card(card);
            }
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
                crate::ask::QUESTIONS.window_shown(false);
            }
            let _ = tx.send(());
        });
        let _ = rx.recv_timeout(Duration::from_millis(500));
        std::thread::sleep(Duration::from_millis(150));
    }

    /// An agent's question appeared (`Some`, as shown) or ended. While idle the question card
    /// shows it at once; during a dictation or review the pill keeps that state, and
    /// `emit_dictation` brings the card when it goes idle.
    pub(crate) fn show_question(&self, text: Option<&str>) {
        use std::sync::atomic::Ordering;
        let _overlay = self.overlay_lock.lock().unwrap();
        // Decided under the lock, so a recording that just started keeps its pill.
        let idle = PILL_IDLE.load(Ordering::SeqCst);
        let card = text.is_some();
        if idle && CARD.swap(card, Ordering::SeqCst) != card {
            self.size_overlay(card);
        }
        let _ = self.handle.emit("agent-question", text);
        if idle {
            // Nothing else needs the pill: it stays up only for a waiting question with the pill
            // setting on (`set_overlay_visible`), so an ended one is always hidden.
            let generation = self.overlay_gen.fetch_add(1, Ordering::SeqCst) + 1;
            self.set_overlay_visible(generation, false);
        }
    }

    /// Tray, `--cancel`: the user doesn't want to answer the agent's question.
    pub fn decline_question(&self) -> bool {
        let declined = crate::ask::QUESTIONS.decline();
        if declined {
            self.outcome("ok", "Declined the agent's question");
        }
        declined
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
        // A press while a rewrite waits for review discards it; the next press records.
        if self.decide_review(false) || self.busy.load(std::sync::atomic::Ordering::SeqCst) {
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

    /// The pill's ✕, `--cancel`: also discards a rewrite waiting for review, or, with nothing
    /// recording or processing, declines an agent's question.
    pub fn cancel_or_discard(self: &Arc<Self>) {
        if self.decide_review(false) {
            return;
        }
        if self.is_recording() {
            self.cancel();
        } else if !self.busy.load(std::sync::atomic::Ordering::SeqCst) {
            self.decline_question();
        }
    }

    pub fn is_reviewing(&self) -> bool {
        self.review.lock().unwrap_or_else(|e| e.into_inner()).is_some()
    }

    /// Ends the review, if one is waiting: `insert` or discard the rewrite. Releases Enter and Esc
    /// at once (on another thread: this runs in shortcut callbacks).
    pub fn decide_review(self: &Arc<Self>, insert: bool) -> bool {
        let Some(tx) = self.review.lock().unwrap_or_else(|e| e.into_inner()).take() else { return false };
        let _ = tx.send(insert);
        crate::hotkeys::set_cancel_key(self, false);
        true
    }

    /// Shows a command's rewrite in the pill and waits for Enter (true: insert) or Esc, another
    /// shortcut press or `REVIEW_TIMEOUT` (false). Without the pill or the keys (Wayland can't grab
    /// keys; another app may hold one) it returns true at once: the rewrite is inserted as before.
    async fn confirm_rewrite(self: &Arc<Self>, text: &str) -> bool {
        /// Ends the review however this returns or unwinds, so Enter and Esc are never left grabbed.
        struct Done(Arc<App>);
        impl Drop for Done {
            fn drop(&mut self) {
                self.0.review.lock().unwrap_or_else(|e| e.into_inner()).take();
                crate::hotkeys::set_cancel_key(&self.0, false);
            }
        }
        if !self.settings().overlay || crate::insert::is_wayland() {
            return true;
        }
        let (tx, rx) = tokio::sync::oneshot::channel();
        *self.review.lock().unwrap_or_else(|e| e.into_inner()) = Some(tx);
        let _done = Done(self.clone());
        let app = self.clone();
        let keys = tokio::task::spawn_blocking(move || crate::hotkeys::sync_transient_keys(&app)).await.unwrap_or(false);
        if !keys && self.is_reviewing() {
            log::warn!("couldn't grab Enter and Esc for the review; inserting the rewrite");
            return true;
        }
        // Already decided (a shortcut pressed while the keys were being grabbed): no card.
        if self.is_reviewing() {
            self.emit_dictation(DictationState { status: "review", mode: Mode::Command, hands_free: false, stage: "", mic_ready: true, text: text.to_string(), workspace: None, answering: false });
        }
        let insert = tokio::time::timeout(REVIEW_TIMEOUT, rx).await.ok().and_then(Result::ok).unwrap_or(false);
        log::info!("rewrite review: {}", if insert { "insert" } else { "discard" });
        if insert {
            self.emit_stage(Mode::Command, "inserting");
            // Enter must be up before the paste: until then X11 sends every key, the paste's too, to us.
            let _ = tokio::task::spawn_blocking(crate::hotkeys::wait_held_released).await;
        }
        insert
    }

    fn cancel_session(self: &Arc<Self>, s: Session) {
        s.done.store(true, std::sync::atomic::Ordering::SeqCst);
        if let Some(question) = s.answers {
            crate::ask::QUESTIONS.end_answer(question);
        }
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
        if mode == Mode::Command && settings.llm.provider == crate::polish::PROVIDER && crate::polish::instruct_model(&settings, &self.models_dir).is_none() {
            return Err(anyhow!(
                "Command mode needs an AI that follows instructions; {}. Download a local instruction model or choose a cloud provider in Settings → AI polish.",
                crate::polish::no_instruct_reason(&settings)
            ));
        }
        // A dictation started after an agent's question appeared answers it (`ask.rs`).
        let answers = crate::ask::QUESTIONS.answered_by(mode == Mode::Dictate, pressed_at);
        let ctx = if answers.is_some() { crate::ask::answer_context() } else { context::current(&settings.app_categories) };
        let language = lang(&settings);
        let workspace = crate::workspace::active(&ctx);
        let vocabulary = self.vocabulary_for(workspace.as_ref());

        // Stream to the first streaming-capable provider in the chain head, if configured.
        let (chunk_tx, stream) = match providers.first().filter(|p| p.supports_streaming()) {
            Some(p) => {
                let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Vec<i16>>();
                let (p, app) = (p.clone(), self.clone());
                let on_partial: PartialFn = Arc::new(move |t: String| {
                    let _ = app.handle.emit("partial-transcript", t);
                });
                let vocabulary = vocabulary.clone();
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
            .map(|p| self.spawn_local_segments(p.clone(), lang(&settings), vocabulary, recording.buffer(), done.clone(), settings.overlay));
        let workspace = workspace.map(|w| w.name);
        if let Some(question) = answers {
            crate::ask::QUESTIONS.begin_answer(question);
        }
        Ok(Session { id, mode, hands_free: false, pressed_at, mic_ready, recording, stream, ctx, providers, done, segments, workspace, answers })
    }

    /// Local models have no streaming API, and one decode of a whole recording is slow (it grows
    /// much faster than the audio: 98 s took 57 s with Moonshine Tiny) and makes the model repeat
    /// itself after about a minute. So the recording is committed piece by piece at pauses while
    /// it is recorded, and on release only the last piece is left; a preview for the overlay is
    /// re-decoded about once a second (see `stt::live`). Returns None if the model failed.
    fn spawn_local_segments(
        self: &Arc<Self>,
        p: Provider,
        language: Option<String>,
        vocabulary: Vec<String>,
        buffer: Arc<std::sync::Mutex<Vec<i16>>>,
        done: Arc<std::sync::atomic::AtomicBool>,
        preview: bool,
    ) -> std::thread::JoinHandle<Option<Live>> {
        let app = self.clone();
        std::thread::spawn(move || {
            use std::sync::atomic::Ordering;
            let model = &p.entry.model;
            let hotwords = crate::stt::local::hotwords_for(&p.models_dir, model, &vocabulary);
            let mut decode = |a: &[i16]| crate::stt::local::decode_blocking(&p.models_dir, model, language.as_deref(), &hotwords, a);
            let mut live = Live::new(crate::stt::local::plan(model, language.as_deref()));
            while !done.load(Ordering::SeqCst) {
                std::thread::park_timeout(crate::stt::live::TICK);
                // What's left after the release is decoded by finish_local_segments.
                if done.load(Ordering::SeqCst) {
                    break;
                }
                let new = buffer.lock().unwrap_or_else(|e| e.into_inner()).get(live.len()..).map(<[i16]>::to_vec).unwrap_or_default();
                live.push(&new);
                let shown = live.tick(&mut decode, preview).ok()?;
                if let Some(shown) = shown.filter(|_| !done.load(Ordering::SeqCst)) {
                    let _ = app.handle.emit("partial-transcript", shown);
                }
            }
            Some(live)
        })
    }

    fn finish(self: &Arc<Self>, s: Session) {
        s.done.store(true, std::sync::atomic::Ordering::SeqCst);
        // Its loop waits between ticks; the release shouldn't wait with it.
        if let Some(seg) = &s.segments {
            seg.thread().unpark();
        }
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
        let Session { mode, recording, stream, ctx, providers, segments, answers, .. } = s;
        // However this ends without an answer sent, the question's time runs on (`ask::expired`).
        let _attempt = answers.map(|question| crate::ask::QUESTIONS.attempt(question));
        let audio = tokio::task::spawn_blocking(move || recording.stop()).await?;
        let stop_ms = stopped.elapsed().as_millis();
        let audio_ms = audio::duration_ms(&audio);
        // Debugging aid in debug builds: OPSTRELLIS_DUMP_AUDIO=/some/dir saves the last recording as last.wav.
        if let Some(dir) = std::env::var("OPSTRELLIS_DUMP_AUDIO").ok().filter(|_| cfg!(debug_assertions)) {
            let _ = std::fs::write(std::path::Path::new(&dir).join("last.wav"), audio::to_wav(&audio));
        }
        // Loud enough is speech as before; only what reads as silence is checked for quiet speech.
        let heard = !audio::is_silent(&audio) || {
            let quiet = audio.clone();
            tokio::task::spawn_blocking(move || crate::vad::has_speech(&quiet)).await.unwrap_or(false)
        };
        if !heard {
            if let Some((_, t)) = stream {
                t.abort();
            }
            log::info!("no speech detected ({audio_ms} ms)");
            self.outcome("warn", "No speech heard. Check your microphone.");
            return Ok(());
        }
        let settings = self.settings();
        let audio = Arc::new(audio);
        let pending = if mode == Mode::Dictate { self.start_pending(&audio, &settings) } else { None };
        let vocabulary = self.vocabulary_for(crate::workspace::active(&ctx).as_ref());
        let transcribing = Instant::now();
        let local = match segments {
            Some(seg) => finish_local_segments(seg, &providers[0], &audio, lang(&settings), &vocabulary).await,
            None => None,
        };
        let transcribed = match local {
            Some(text) => Ok((text, &providers[0])),
            // No local head, the model failed, or it heard nothing: the normal chain.
            None => transcribe_with_fallback(&providers, stream, &audio, lang(&settings).as_deref(), &vocabulary).await,
        };
        let (raw, used) = match transcribed {
            Ok(v) => v,
            // The selection a command needs is gone by now, so only dictations are kept for a retry.
            Err(e) if mode == Mode::Dictate => return self.keep_failed(e, audio, pending, &ctx, audio_ms, &settings).await,
            Err(e) => return Err(e),
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
            self.attach_pending(pending, None).await;
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
        let Cleaned { text: final_text, fixes, press_enter, used: used_entries, warning, copy, copy_note, review } = match outcome {
            Ok(v) => v,
            Err(e) => {
                item.status = "error".into();
                item.error = Some(format!("{e:#}"));
                let id = self.save_history(&item, &settings);
                self.attach_pending(pending, id).await;
                return Err(e);
            }
        };
        item.text = final_text.clone();
        item.words = text::word_count(&final_text) as i64;
        item.fixes = fixes as i64;
        *self.last_text.lock().unwrap() = Some(crate::LastTranscript { text: final_text.clone(), history_id: None });
        // The time the rewrite waited for review isn't latency.
        let reviewing = Instant::now();
        if review && !self.confirm_rewrite(&final_text).await {
            item.status = "cancelled".into();
            item.latency_ms = reviewing.duration_since(stopped).as_millis() as i64;
            let id = self.save_transcript(&item, &settings);
            self.attach_pending(pending, id).await;
            self.outcome("warn", "Discarded. Paste last inserts it anyway.");
            return Ok(());
        }
        let waited = reviewing.elapsed();

        // ---------- An answer to an agent's question goes back to the agent, never into an app. ----------
        if let Some(question) = answers {
            // Nothing left after cleanup ("um"): as with no words, the question stays.
            if final_text.trim().is_empty() {
                self.attach_pending(pending, None).await;
                self.outcome("warn", "Didn't catch any words.");
                return Ok(());
            }
            item.latency_ms = stopped.elapsed().as_millis() as i64;
            // Consent can be withdrawn while the user speaks: then nothing goes to the agent.
            let allowed = crate::ask::voice_allowed(&self.settings());
            if !allowed {
                crate::ask::QUESTIONS.turned_off();
            }
            let sent = allowed && crate::ask::QUESTIONS.answer(question, &final_text);
            // The agent stopped waiting while the user spoke: keep the words where they can be found.
            let copied = !sent && self.inserter.set_clipboard(&final_text).is_ok();
            let id = self.save_transcript(&item, &settings);
            let terms = if settings.cleanup.terms_on() { crate::terms::find(&final_text) } else { Vec::new() };
            if let Err(e) = self.db.record_usage(&used_entries, &terms) {
                log::warn!("recording dictionary usage failed: {e:#}");
            }
            self.queue_usage(&item, used.entry.provider, &settings);
            match (sent, copied, warning) {
                _ if !allowed && copied => self.outcome("warn", "Voice questions are off, so nothing was sent to the agent. Your answer is on the clipboard."),
                _ if !allowed => self.outcome("warn", "Voice questions are off, so nothing was sent to the agent. Your answer is in History."),
                (true, _, None) => self.outcome("ok", "Sent to the agent"),
                (true, _, Some(_)) => self.outcome("warn", "Sent to the agent, without AI polish"),
                (false, true, _) => self.outcome("warn", "The agent stopped waiting, so your answer is on the clipboard."),
                (false, false, _) => self.outcome("warn", "The agent stopped waiting. Your answer is in History."),
            }
            log::info!("answer to an agent: {} words, sent: {sent}", item.words);
            self.attach_pending(pending, id).await;
            return Ok(());
        }

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
                // As long as when the clipboard was restored first: Enter mustn't overtake a slow app's paste.
                if method != "type" && restore {
                    std::thread::sleep(Duration::from_millis(delay.max(100)));
                }
                ins.inserter.press_enter()?;
            }
            Ok(None)
        })
        .await?;
        item.latency_ms = stopped.elapsed().saturating_sub(waited).as_millis() as i64;
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
            let id = self.save_transcript(&item, &settings);
            self.attach_pending(pending, id).await;
            return Err(anyhow!("Could not {operation} text: {e:#}. {recovery}"));
        }
        let id = self.save_transcript(&item, &settings);
        if mode == Mode::Dictate {
            let terms = if settings.cleanup.terms_on() { crate::terms::find(&final_text) } else { Vec::new() };
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
            (None, true) => match copy_note {
                Some(note) => self.outcome("warn", note),
                None => self.outcome("ok", format!("Copied · {secs}")),
            },
            (None, false) => self.outcome("ok", format!("Inserted · {secs}")),
        }
        log::info!("dictation done: {} words, {} ms after release via {}", item.words, item.latency_ms, used.label());
        self.attach_pending(pending, id).await;
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
        let workspace = crate::workspace::active(ctx);
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
            terms: settings.cleanup.terms_on(),
            preserve_formatting: false,
            clipboard: Some(&read_clipboard),
            workspace: workspace.as_ref().map(|w| &*w.index),
            workspace_names: crate::workspace::replaces_names(settings.workspace_replace, ctx),
            mentions: ctx.category == "ai_prompts",
        };
        let local = settings.llm.provider == crate::polish::PROVIDER;
        // A local cleanup model can't follow an action's instructions: a paired instruction model does.
        let instruct = if local { crate::polish::instruct_model(settings, &self.models_dir) } else { None };
        let action = self.auto_transform(ctx, settings);
        let output = llm::output_language(settings, ctx, &self.models_dir);
        // The local model that runs: the instruction model for an action or a translation, else the chosen one.
        let local_model = local.then(|| match instruct {
            Some(m) if action.is_some() || output.is_some() => m,
            _ => crate::polish::selected(settings),
        });
        let skipped_action = action.as_ref().filter(|_| local_model.is_some_and(|m| m.kind == crate::polish::Kind::Cleanup)).map(|t| t.name.clone());
        let action = if skipped_action.is_some() { None } else { action };
        let cleanup_model = local_model.is_some_and(|m| m.kind == crate::polish::Kind::Cleanup);
        // A local cleanup model skips code editors, terminals and other languages: the rules alone, as with AI off.
        if !settings.llm.enabled || local_model.is_some_and(|m| !crate::polish::applies(m, ctx, &settings.language, raw)) {
            let p = text::process(raw, &full);
            return Ok(Cleaned::from(p));
        }
        // Built-in terms and snippets run after the LLM, so a snippet trigger that is also an
        // alias ("type script") still expands.
        let pre = text::process(raw, &text::Options { snippets: &[], tone: "none", terms: false, workspace: None, ..full });
        // Nothing left to polish ("uhm", a lone "press enter"): any model output would be invented.
        if pre.text.trim().is_empty() {
            return Ok(Cleaned::from(text::process(raw, &full)));
        }
        // A dictation that is only a snippet trigger needs no AI.
        if let Some(s) = text::whole_snippet(&pre.text, &snippets) {
            return Ok(Cleaned { text: full.expand_snippet(&s.expansion), fixes: pre.fixes, press_enter: pre.press_enter, used: pre.used, ..Default::default() });
        }
        let rewrite = action.as_ref().map(|t| t.prompt.as_str());
        // A rewrite must not reword the snippet triggers it's given, or they wouldn't expand.
        let mut keep = vocabulary.to_vec();
        // Workspace names aren't terms to force: the model gets the ones said, under their own rule.
        let code_names = workspace.as_ref().map(|w| w.index.names_in(&pre.text)).unwrap_or_default();
        if let Some(w) = &workspace {
            keep.retain(|t| !w.index.hints().contains(t) || dictionary.iter().any(|e| e.replacement.as_ref().unwrap_or(&e.phrase) == t));
        }
        if rewrite.is_some() {
            keep.extend(text::snippet_triggers_in(&pre.text, &snippets));
        }
        let mut warning = None;
        let plain = if output.is_some() { "Inserted plain text, not translated." } else { "Inserted plain text." };
        let system = llm::cleanup_prompt(
            ctx,
            &llm::Cleanup {
                tone: &tone,
                vocabulary: &keep,
                rewrite,
                notes: settings.style_notes.get(ctx.style_category()).map(String::as_str),
                structured: settings.prompt_style == "structured",
                output_language: output.map(|(_, name)| name),
                code_names: &code_names,
            },
        );
        let request = match local_model {
            Some(m) if m.kind == crate::polish::Kind::Cleanup => {
                let (dir, input, tone, category, app) = (self.models_dir.clone(), pre.text.clone(), tone.clone(), ctx.style_category().to_string(), ctx.app_name.clone());
                let deadline = Instant::now() + llm::CLEANUP_TIMEOUT;
                tokio::task::spawn_blocking(move || {
                    let hints = crate::polish::Hints { tone: &tone, style_category: &category, app: app.as_deref() };
                    crate::polish::cleanup_blocking(&dir, m, &input, &hints, deadline)
                })
                .await
                .map_err(anyhow::Error::from)
                .and_then(|r| r)
                .map(|t| llm::strip_controls(&t))
            }
            Some(m) => self.local_client(m).complete_within(&system, &llm::wrap_dictation(&pre.text, rewrite), llm::CLEANUP_TIMEOUT).await,
            None => self.llm_client(settings).complete_within(&system, &llm::wrap_dictation(&pre.text, rewrite), llm::CLEANUP_TIMEOUT).await,
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
                warning = Some(format!("AI polish changed “{term}” from your dictionary. {plain}"));
                pre.text.clone()
            }
            Ok(out) if llm::accept_cleanup(&pre.text, &out, rewrite.is_some() || output.is_some()) => without_backticks(out, rewrite.is_some(), ctx),
            Ok(out) => {
                log::warn!("discarded implausible LLM cleanup output ({} chars)", out.len());
                warning = Some(format!("AI polish skipped (odd result). {plain}"));
                pre.text.clone()
            }
            Err(e) => {
                log::warn!("LLM cleanup failed, using rule-based cleanup: {e:#}");
                let reason: String = e.root_cause().to_string().chars().take(80).collect();
                warning = Some(match &action {
                    Some(t) => format!("AI unavailable ({reason}): “{}” skipped. {plain}", t.name),
                    None => format!("AI polish unavailable ({reason}). {plain}"),
                });
                pre.text.clone()
            }
        };
        // Once per run of the app for each action and kind of app: it's a setting, not a failure.
        static TOLD: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
        if let Some(name) = skipped_action.filter(|_| warning.is_none()) {
            let key = format!("{name}\u{0}{}", ctx.style_category());
            let mut told = TOLD.lock().unwrap_or_else(|e| e.into_inner());
            if !told.contains(&key) {
                told.push(key);
                warning = Some(format!(
                    "“{name}” needs an AI that follows instructions, and {}: the text was only cleaned up. Add a local instruction model or a cloud provider in Settings → AI polish.",
                    crate::polish::no_instruct_reason(settings)
                ));
            }
        }
        let cleaned = if warning.is_none() && !ctx.allows_rewrite() { llm::limit_breaks(&pre.text, &cleaned) } else { cleaned };
        // A rewritten text keeps the action's formatting rather than the category's tone rules.
        let tone_after = if rewrite.is_some() && warning.is_none() { "none" } else { tone.as_str() };
        let english = match output {
            Some((code, _)) if warning.is_none() => code == "en",
            _ => full.english,
        };
        let post = text::process(&cleaned, &text::Options {
            // A local cleanup model doesn't write backticks: wrap code again (it skips what is already wrapped).
            remove_fillers: false, voice_commands: false, backticks: cleanup_model && full.backticks, tone: tone_after,
            preserve_formatting: rewrite.is_some() && warning.is_none(), english, ..full
        });
        let mut used = pre.used;
        for id in post.used {
            if !used.contains(&id) {
                used.push(id);
            }
        }
        Ok(Cleaned { text: post.text, fixes: pre.fixes + post.fixes, press_enter: pre.press_enter, used, warning, copy: false, copy_note: None, review: false })
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
        // Never a team action (`list_transforms` leaves them out): its admin could change the prompt later.
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
        let matched = spoken_action(&spoken, &self.db)?;
        // Free writing that named a copy action ("reply saying Thursday works") is still copied, not pasted.
        let copy = matched.as_ref().is_some_and(|t| t.output == "copy");
        let transform = action_for_selection(&spoken, selection.is_some(), matched)?;
        // A named action keeps what was said around its name ("reply saying Thursday works").
        let named = transform.is_some();
        let instruction = match transform {
            Some(t) => llm::action_instruction(&t.prompt, (spoken != t.name.to_lowercase()).then_some(instruction.trim())),
            None => instruction.to_string(),
        };
        let out = self
            .llm_client(settings)
            .complete(&llm::command_prompt(selection.as_deref(), copy), &llm::command_input(selection.as_deref(), &instruction))
            .await?;
        llm::check_command_output(selection.as_deref(), &out)?;
        let out = without_backticks(out, named, ctx);
        // In a shell the model's answer would run as typed: the user reads it and pastes it.
        let shell = !ctx.allows_rewrite();
        let copy_note = shell.then_some("Copied, not pasted: check it, then paste it into the terminal.");
        let review = !(copy || shell) && needs_review(&settings.review_rewrites, selection.as_deref(), &out);
        Ok(Cleaned { text: out, copy: copy || shell, copy_note, review, ..Default::default() })
    }

    /// A client for a local model.
    fn local_client(&self, m: &crate::polish::Model) -> llm::Client {
        llm::Client { provider: crate::polish::PROVIDER.into(), model: m.id.into(), local_dir: Some(self.models_dir.clone()), ..Default::default() }
    }

    pub fn llm_client(&self, settings: &Settings) -> llm::Client {
        // Local: the model that follows instructions (commands, actions, summaries), if there is one.
        if settings.llm.provider == crate::polish::PROVIDER {
            let m = crate::polish::instruct_model(settings, &self.models_dir).unwrap_or_else(|| crate::polish::selected(settings));
            return self.local_client(m);
        }
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
            key: if cloud { self.secrets.get("cloud") } else { llm::key_id(settings).and_then(|id| self.secrets.get(&id)) },
            local_dir: None,
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

    /// Transcription hints: the dictionary (most used first), then up to 30 terms of the packs in
    /// use: the ones the user says most, then the enabled extra packs' terms in pack order.
    pub fn vocabulary(&self) -> Vec<String> {
        let terms = if self.settings().cleanup.terms_on() { 30 } else { 0 };
        let packs = crate::terms::active();
        self.db.vocabulary(HINTS, terms, |t| packs.is_term(t), packs.pack_terms()).unwrap_or_default()
    }

    /// The hints followed by the workspace's names (the dictionary keeps priority).
    pub fn vocabulary_for(&self, workspace: Option<&crate::workspace::Active>) -> Vec<String> {
        let hints = self.vocabulary();
        match workspace {
            Some(w) => crate::workspace::with_hints(hints, w.index.hints(), HINTS),
            None => hints,
        }
    }

    fn save_transcript(&self, item: &HistoryItem, settings: &Settings) -> Option<i64> {
        let history_id = self.save_history(item, settings);
        *self.last_text.lock().unwrap() = Some(crate::LastTranscript { text: item.text.clone(), history_id });
        history_id
    }

    pub(crate) fn save_history(&self, item: &HistoryItem, settings: &Settings) -> Option<i64> {
        let keep = settings.privacy.history_retention_days != Some(0);
        let id = self.db.add_history(item, keep).unwrap_or_else(|e| {
            log::error!("saving history failed: {e:#}");
            None
        });
        // Enforce retention continuously (the app can run in the tray for weeks).
        if let Some(days) = settings.privacy.history_retention_days.filter(|d| *d > 0) {
            let _ = self.db.purge_history_older_than(days);
        }
        self.enforce_recordings();
        let _ = self.handle.emit("history-updated", ());
        id
    }

    pub(crate) fn queue_usage(&self, item: &HistoryItem, kind: ProviderKind, _started_with: &Settings) {
        // Cloud gateway calls are metered server-side; only BYOK/local usage is reported.
        if kind == ProviderKind::Cloud || !self.secrets.has("cloud") {
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
        {
            // The setting now, not the one this dictation started with: turning sync off clears the
            // queue, and holding the lock keeps that clear from running between check and insert.
            let settings = self.settings.read().unwrap();
            if !settings.privacy.sync_usage {
                return;
            }
            if let Err(e) = self.db.queue_usage(&ev) {
                log::warn!("queue usage failed: {e:#}");
                return;
            }
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
    /// Shown instead of "Copied" when the text was copied for a reason the user should know.
    pub copy_note: Option<&'static str>,
    /// Command mode: show the rewrite and wait for Enter or Esc before it replaces the selection.
    pub review: bool,
}

impl From<text::Processed> for Cleaned {
    fn from(p: text::Processed) -> Self {
        Cleaned { text: p.text, fixes: p.fixes, press_enter: p.press_enter, used: p.used, ..Default::default() }
    }
}

/// An action's result for a terminal loses its backticks, as dictation there gets none.
fn without_backticks(text: String, rewrite: bool, ctx: &AppContext) -> String {
    if rewrite && ctx.is_terminal { text.replace('`', "") } else { text }
}

/// Whether a command's `result`, which replaces `selection`, waits for review first (`reviewRewrites`).
/// Nothing selected means nothing is replaced: no review.
fn needs_review(setting: &str, selection: Option<&str>, result: &str) -> bool {
    let Some(selection) = selection.filter(|s| !s.trim().is_empty()) else { return false };
    match setting {
        "never" => false,
        "always" => true,
        _ => selection.chars().count() > REVIEW_LONG || result.chars().count() > REVIEW_LONG,
    }
}

/// The user's own and built-in actions, by name anywhere at the start or end of the instruction,
/// win over the team's; a team action runs only when the whole instruction is its name, so it never
/// captures something the user phrased ("translate it").
fn spoken_action(spoken: &str, db: &crate::db::Db) -> Result<Option<crate::db::Transform>> {
    match command_action(spoken, db.list_transforms()?) {
        None => db.team_action(spoken),
        own => Ok(own),
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

/// With nothing selected a built-in action has nothing to work on: its bare name is an error, and a longer
/// instruction containing it ("summarize the pros and cons of Rust") is free writing. Other actions, and built-ins
/// whose prompt the user edited, may generate text.
fn action_for_selection(spoken: &str, selected: bool, action: Option<crate::db::Transform>) -> Result<Option<crate::db::Transform>> {
    let stock = |t: &crate::db::Transform| t.builtin && !selected && crate::db::is_builtin_prompt(&t.prompt);
    match action {
        Some(t) if stock(&t) && spoken == t.name.to_lowercase() => {
            Err(anyhow!("Nothing is selected. Select the text first, then say “{}”.", t.name))
        }
        Some(t) if stock(&t) => Ok(None),
        action => Ok(action),
    }
}

pub(crate) fn lang(settings: &Settings) -> Option<String> {
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
async fn finish_local_segments(seg: std::thread::JoinHandle<Option<Live>>, p: &Provider, audio: &[i16], language: Option<String>, vocabulary: &[String]) -> Option<String> {
    let (p, audio, vocabulary) = (p.clone(), audio.to_vec(), vocabulary.to_vec());
    let text = tokio::task::spawn_blocking(move || {
        let mut live = seg.join().ok()??;
        let model = &p.entry.model;
        let hotwords = crate::stt::local::hotwords_for(&p.models_dir, model, &vocabulary);
        live.push(audio.get(live.len()..).unwrap_or_default());
        live.finish(&mut |a: &[i16]| crate::stt::local::decode_blocking(&p.models_dir, model, language.as_deref(), &hotwords, a)).ok()
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

    #[test]
    fn team_actions_match_only_their_exact_name_and_never_beat_the_users() {
        let dir = std::env::temp_dir().join(format!("opstrellis-actions-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = crate::db::Db::open(&dir.join("t.db")).unwrap();
        db.upsert_transform(crate::db::Transform { id: None, name: "Tidy".into(), prompt: "mine".into(), builtin: false, output: "replace".into() }).unwrap();
        let action = |id: &str, name: &str| serde_json::json!({ "id": id, "kind": "action", "data": { "name": name, "prompt": format!("team {name}"), "output": "replace" } });
        let page = crate::team::Page {
            team: Some(crate::team::TeamRef { id: "t".into(), name: "T".into() }),
            version: 3,
            entries: vec![action("a", "please"), action("b", "it"), action("c", "Tidy up the text"), action("d", "Summarize")],
            more: false,
            reset: false,
        };
        crate::team::apply(&db, &crate::team::cursor(&db).unwrap(), &page).unwrap();
        let prompt = |spoken: &str| spoken_action(spoken, &db).unwrap().map(|t| t.prompt);
        assert_eq!(prompt("tidy please").as_deref(), Some("mine"), "a longer team name never wins");
        assert_eq!(prompt("tidy up the text").as_deref(), Some("mine"), "the user's own wins even against an exact team name");
        assert_eq!(prompt("please").as_deref(), Some("team please"), "exactly its name");
        assert_eq!(prompt("translate it"), None, "never a prefix or suffix of an instruction");
        assert_eq!(prompt("summarize").as_deref().map(|p| p.starts_with("Summarize the selected text")), Some(true), "a built-in wins");
        drop(db);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn built_in_actions_need_a_selection() {
        let dir = std::env::temp_dir().join(format!("opstrellis-unselected-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = crate::db::Db::open(&dir.join("t.db")).unwrap();
        let seeded = db.list_transforms().unwrap();
        let builtin = |name: &str| seeded.iter().find(|t| t.builtin && t.name == name).cloned().unwrap();
        let mine = crate::db::Transform { id: Some(99), name: "Signature".into(), prompt: "Write my email signature.".into(), builtin: false, output: "replace".into() };
        let run = |spoken: &str, selected: bool, t: crate::db::Transform| action_for_selection(spoken, selected, Some(t)).map(|a| a.map(|a| a.prompt));
        let err = run("polish", false, builtin("Polish")).unwrap_err().to_string();
        assert_eq!(err, "Nothing is selected. Select the text first, then say “Polish”.");
        assert_eq!(run("summarize the pros and cons of rust", false, builtin("Summarize")).unwrap(), None, "prefix: free writing");
        assert_eq!(run("now please summarize", false, builtin("Summarize")).unwrap(), None, "suffix: free writing");
        assert_eq!(run("signature", false, mine.clone()).unwrap(), Some(mine.prompt.clone()), "the user's own may generate text");
        let renamed = crate::db::Transform { name: "Tidy".into(), ..builtin("Polish") };
        assert_eq!(run("tidy", false, renamed).unwrap_err().to_string(), "Nothing is selected. Select the text first, then say “Tidy”.", "renamed, prompt unedited");
        let edited = crate::db::Transform { prompt: "Write a haiku about the sea.".into(), ..builtin("Polish") };
        assert_eq!(run("polish", false, edited.clone()).unwrap(), Some(edited.prompt.clone()), "an edited built-in runs as before");
        assert_eq!(run("polish the sea", false, edited.clone()).unwrap(), Some(edited.prompt), "also with more words");
        for (spoken, t) in [("polish", builtin("Polish")), ("summarize the pros", builtin("Summarize")), ("signature", mine)] {
            assert_eq!(run(spoken, true, t.clone()).unwrap(), Some(t.prompt), "{spoken}: with a selection, unchanged");
        }
        assert!(action_for_selection("anything", false, None).unwrap().is_none());

        let entry = serde_json::json!({ "id": "a", "kind": "action", "data": { "name": "Standup", "prompt": "team standup", "output": "replace" } });
        let page = crate::team::Page { team: Some(crate::team::TeamRef { id: "t".into(), name: "T".into() }), version: 1, entries: vec![entry], more: false, reset: false };
        crate::team::apply(&db, &crate::team::cursor(&db).unwrap(), &page).unwrap();
        let team = action_for_selection("standup", false, spoken_action("standup", &db).unwrap()).unwrap();
        assert_eq!(team.map(|t| t.prompt).as_deref(), Some("team standup"), "a team action keeps running with nothing selected");
        assert!(action_for_selection("polish", false, spoken_action("polish", &db).unwrap()).is_err(), "the seeded Polish is built in");
        drop(db);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn rewrites_for_a_terminal_have_no_backticks() {
        let agent = crate::context::classify(Some("kitty".into()), Some("\u{2733} Claude Code".into()));
        let chat = crate::context::classify(Some("firefox".into()), Some("ChatGPT".into()));
        assert!(agent.is_terminal && agent.allows_rewrite() && !chat.is_terminal);
        let out = || "Fix `src/app.rs` so `cargo test` passes.\n```\nrun()\n```".to_string();
        assert_eq!(without_backticks(out(), true, &agent), "Fix src/app.rs so cargo test passes.\n\nrun()\n");
        assert_eq!(without_backticks(out(), true, &chat), out(), "other apps keep them");
        assert_eq!(without_backticks(out(), false, &agent), out(), "plain cleanup is untouched");
    }

    #[test]
    fn long_rewrites_wait_for_review() {
        let (short, long) = ("hey sam thanks", "ä".repeat(REVIEW_LONG + 1));
        let at_limit = "x".repeat(REVIEW_LONG);
        assert!(!needs_review("long", Some(short), "Hi Sam, thank you."));
        assert!(!needs_review("long", Some(&at_limit), &at_limit), "counted in characters, over the limit only");
        assert!(needs_review("long", Some(&long), "Short."), "long selection");
        assert!(needs_review("long", Some(short), &long), "long result");
        assert!(needs_review("someday-value", Some(short), &long), "unknown values act like the default");
        assert!(needs_review("always", Some(short), "Hi."));
        assert!(!needs_review("never", Some(&long), &long));
        for setting in ["long", "always"] {
            assert!(!needs_review(setting, None, &long), "{setting}: nothing selected, nothing replaced");
            assert!(!needs_review(setting, Some(" \n"), &long), "{setting}: blank selection");
        }
        let old: Settings = serde_json::from_str("{}").unwrap();
        assert_eq!((old.review_rewrites.as_str(), old.output_languages.len()), ("long", 0), "settings from before default to long, as spoken");
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
