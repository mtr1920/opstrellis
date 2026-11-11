//! An agent's question for the user (`ask_user`). `opstrellis mcp` or `opstrellis ask` hands it to
//! the running app over a Unix socket in the data folder; the pill shows it, and the user's next
//! dictation (started at least `ANSWER_DELAY` later) goes back to the agent instead of into an app.
//! The app never starts recording for a question and grabs no keys for it.
//!
//! Wire format: one JSON line in, `{"ask": "<question>"}`, one JSON line out, `{"answer": "…"}` or
//! `{"error": "…"}`. Closing the connection withdraws the question.

use crate::context::AppContext;
use serde_json::{json, Value};
use std::io::{BufReader, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

pub const SOCKET: &str = "agent.sock";
pub const QUESTION_TIMEOUT: Duration = Duration::from_secs(60);
/// After an answer attempt that sent nothing (cancelled, no words), at least this long is left to try again.
pub const RETRY_GRACE: Duration = Duration::from_secs(15);
/// However long answering takes, a requester never waits longer than this.
pub const MAX_WAIT: Duration = Duration::from_secs(5 * 60);
/// A dictation started sooner after the question appeared is an ordinary one: the user had no time to read it.
pub const ANSWER_DELAY: Duration = Duration::from_secs(2);
pub const MAX_QUESTION_CHARS: usize = 300;
pub const MAX_REPLY_BYTES: usize = 256 * 1024;
const MAX_ANSWER_CHARS: usize = 100_000;
const MAX_REQUEST_BYTES: usize = 4096;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
const POLL: Duration = Duration::from_millis(200);
const MAX_CONNECTIONS: usize = 4;
/// The app name a dictation that answered an agent is saved with in History.
pub const ANSWER_APP: &str = "Answer to an agent";

pub const OFF: &str = "Voice questions are off in OpsTrellis. The user can turn them on in OpsTrellis → Settings → Agents.";
const BUSY: &str = "OpsTrellis is already waiting for the user to answer another question. Try again later.";
const NO_ANSWER: &str = "The user didn't answer.";
const DECLINED: &str = "The user declined to answer.";
const QUIT: &str = "OpsTrellis quit before the user answered.";
const TURNED_OFF: &str = "The user turned voice questions off in OpsTrellis.";

/// The question waiting for an answer, at most one.
pub static QUESTIONS: Board = Board::new();

struct Pending {
    id: u64,
    /// The text the pill shows (`shown`).
    shown: String,
    /// The pill's page says it renders this question (`question_shown`).
    rendered: bool,
    /// Since when the user could see it: the window shown and the question rendered. None while not.
    visible_since: Option<Instant>,
    arrived: Instant,
    /// A dictation accepted as the answer is being recorded or processed.
    answering: bool,
    /// When the last answer attempt ended without an answer.
    attempt_ended: Option<Instant>,
    reply: UnixStream,
}

impl Pending {
    fn refresh(&mut self, window: bool) {
        if !(window && self.rendered) {
            self.visible_since = None;
        } else if self.visible_since.is_none() {
            self.visible_since = Some(Instant::now());
        }
    }
}

type Notify = Box<dyn Fn(Option<&str>) + Send + Sync>;

pub struct Board {
    slot: Mutex<Option<Pending>>,
    next: AtomicU64,
    /// The pill's window is on screen.
    window: AtomicBool,
    /// The window has the card's size, so a question drawn in it isn't cut off.
    card: AtomicBool,
    /// Told the shown text when a question appears, None when it ends. Never called with `slot` locked.
    notify: OnceLock<Notify>,
}

impl Default for Board {
    fn default() -> Self {
        Board::new()
    }
}

impl Board {
    pub const fn new() -> Board {
        Board { slot: Mutex::new(None), next: AtomicU64::new(1), window: AtomicBool::new(false), card: AtomicBool::new(false), notify: OnceLock::new() }
    }

    pub fn on_change(&self, f: impl Fn(Option<&str>) + Send + Sync + 'static) {
        let _ = self.notify.set(Box::new(f));
    }

    fn lock(&self) -> MutexGuard<'_, Option<Pending>> {
        self.slot.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn changed(&self, shown: Option<&str>) {
        if let Some(f) = self.notify.get() {
            f(shown);
        }
    }

    /// None while another question waits.
    fn post(&self, question: &str, reply: UnixStream) -> Option<u64> {
        let text = shown(question);
        let mut slot = self.lock();
        if slot.is_some() {
            return None;
        }
        let id = self.next.fetch_add(1, Ordering::SeqCst);
        *slot = Some(Pending { id, shown: text.clone(), rendered: false, visible_since: None, arrived: Instant::now(), answering: false, attempt_ended: None, reply });
        drop(slot);
        self.changed(Some(&text));
        Some(id)
    }

    pub fn is_pending(&self) -> bool {
        self.lock().is_some()
    }

    /// None once question `id` has ended; otherwise whether it ran out of time (see `expired`).
    fn expired_now(&self, id: u64, timeout: Duration) -> Option<bool> {
        let slot = self.lock();
        let p = slot.as_ref().filter(|p| p.id == id)?;
        Some(expired(timeout, p.arrived, p.answering, p.attempt_ended, Instant::now()))
    }

    /// Ends the attempt to answer `id` when dropped (`end_answer`).
    pub fn attempt(&'static self, id: u64) -> Attempt {
        Attempt(self, id)
    }

    /// A dictation accepted as the answer to `id` started: the question waits for it.
    pub fn begin_answer(&self, id: u64) {
        if let Some(p) = self.lock().as_mut().filter(|p| p.id == id) {
            p.answering = true;
        }
    }

    /// That dictation ended without sending an answer (cancelled, no words, failed): the question
    /// waits on, at least `RETRY_GRACE` longer. Nothing to do once the question has ended.
    pub fn end_answer(&self, id: u64) {
        if let Some(p) = self.lock().as_mut().filter(|p| p.id == id) {
            p.answering = false;
            p.attempt_ended = Some(Instant::now());
        }
    }

    /// Whether the window can show the whole question now: on screen, at the card's size.
    fn window_ready(&self) -> bool {
        self.window.load(Ordering::SeqCst) && self.card.load(Ordering::SeqCst)
    }

    /// The pill's window was shown or hidden (`pipeline.rs`, on the main thread).
    pub fn window_shown(&self, shown: bool) {
        self.window.store(shown, Ordering::SeqCst);
        let ready = self.window_ready();
        if let Some(p) = self.lock().as_mut() {
            p.refresh(ready);
        }
    }

    /// The pill's window was resized to the card (true) or the pill (`pipeline.rs`, on the main thread).
    pub fn window_card(&self, card: bool) {
        self.card.store(card, Ordering::SeqCst);
        let ready = self.window_ready();
        if let Some(p) = self.lock().as_mut() {
            p.refresh(ready);
        }
    }

    /// The pill's page renders this question text, or no question (None).
    pub fn rendered(&self, text: Option<&str>) {
        let window = self.window_ready();
        if let Some(p) = self.lock().as_mut() {
            p.rendered = text == Some(p.shown.as_str());
            p.refresh(window);
        }
    }

    /// The question a recording started at `started` answers, if it is a dictation (see `answers`).
    pub fn answered_by(&self, dictation: bool, started: Instant) -> Option<u64> {
        self.lock().as_ref().filter(|p| answers(dictation, p.visible_since, started)).map(|p| p.id)
    }

    /// Ends question `id` (or whichever waits, for None), sending `reply` to the agent when given.
    /// False when it had already ended.
    fn end(&self, id: Option<u64>, reply: Option<Value>) -> bool {
        let taken = {
            let mut slot = self.lock();
            if slot.as_ref().is_some_and(|p| id.is_none_or(|id| p.id == id)) {
                slot.take()
            } else {
                None
            }
        };
        let Some(mut p) = taken else { return false };
        if let Some(reply) = reply {
            let _ = writeln!(p.reply, "{reply}");
        }
        let _ = p.reply.shutdown(std::net::Shutdown::Write);
        self.changed(None);
        true
    }

    /// Sends the user's answer to question `id`; false if the agent stopped waiting meanwhile.
    pub fn answer(&self, id: u64, text: &str) -> bool {
        let text: String = text.chars().take(MAX_ANSWER_CHARS).collect();
        self.end(Some(id), Some(json!({ "answer": text })))
    }

    pub fn decline(&self) -> bool {
        self.end(None, Some(json!({ "error": DECLINED })))
    }

    pub fn close(&self) {
        self.end(None, Some(json!({ "error": QUIT })));
    }

    /// Consent was withdrawn while a question waited.
    pub fn turned_off(&self) -> bool {
        self.end(None, Some(json!({ "error": TURNED_OFF })))
    }
}

/// An answer attempt in progress; however it ends without an answer, the question's time runs on.
pub struct Attempt(&'static Board, u64);

impl Drop for Attempt {
    fn drop(&mut self) {
        self.0.end_answer(self.1);
    }
}

/// Whether a question that arrived at `arrived` has run out of time at `now`: `timeout` after it
/// arrived, but never while an answer is being recorded or processed, and not sooner than
/// `RETRY_GRACE` after an answer attempt ended; always after `MAX_WAIT`.
pub fn expired(timeout: Duration, arrived: Instant, answering: bool, attempt_ended: Option<Instant>, now: Instant) -> bool {
    let waited = now.saturating_duration_since(arrived);
    if waited >= MAX_WAIT {
        return true;
    }
    if answering {
        return false;
    }
    let grace = attempt_ended.map_or(Duration::ZERO, |e| now.saturating_duration_since(e));
    waited >= timeout && (attempt_ended.is_none() || grace >= RETRY_GRACE)
}

/// What a question needs from the user's settings, checked when it arrives, when settings change
/// and when an answer would be sent.
pub fn voice_allowed(s: &crate::settings::Settings) -> bool {
    s.agents.enabled && s.agents.voice && s.overlay
}

/// Whether a recording that started at `started` answers a question on screen since `visible_since`
/// (None: not on screen now): only a dictation (never a command), and only one the user started
/// after having had the question in front of them for `ANSWER_DELAY`.
pub fn answers(dictation: bool, visible_since: Option<Instant>, started: Instant) -> bool {
    dictation && visible_since.and_then(|v| started.checked_duration_since(v)).is_some_and(|d| d >= ANSWER_DELAY)
}

/// The overlay page reports what it renders: the question text, or null when it shows anything else.
#[tauri::command]
pub fn question_shown(text: Option<String>) {
    QUESTIONS.rendered(text.as_deref());
}

/// The question as the card shows it, in full: no control or text-direction characters, one paragraph.
pub fn shown(question: &str) -> String {
    let hidden = |c: char| c.is_control() || c.is_whitespace() || matches!(c, '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}');
    let text = question.split(hidden).filter(|w| !w.is_empty()).collect::<Vec<_>>().join(" ");
    // Requests are already limited to this; nothing longer can reach the card.
    text.chars().take(MAX_QUESTION_CHARS).collect()
}

/// The context an answer is cleaned up in: a prompt for an AI, never a shell command.
pub fn answer_context() -> AppContext {
    AppContext { app_name: Some(ANSWER_APP.into()), title: None, category: "ai_prompts", is_terminal: false, is_agent: false, window: None }
}

/// The question in a request line, checked.
fn parse_request(line: &[u8]) -> Result<String, String> {
    let v: Value = serde_json::from_slice(line).map_err(|_| "The request isn't valid JSON.".to_string())?;
    let question = match v.as_object() {
        Some(m) if m.len() == 1 => m.get("ask").and_then(Value::as_str),
        _ => None,
    }
    .ok_or("The request must be {\"ask\": \"<question>\"}.")?
    .trim();
    let chars = question.chars().count();
    if chars == 0 || chars > MAX_QUESTION_CHARS {
        return Err(format!("The question must be 1 to {MAX_QUESTION_CHARS} characters."));
    }
    Ok(question.to_string())
}

/// Reads from the stream until `deadline`, however slowly the bytes arrive.
struct Deadline<'a>(&'a UnixStream, Instant);

impl Read for Deadline<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let left = self.1.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(std::io::ErrorKind::TimedOut.into());
        }
        self.0.set_read_timeout(Some(left))?;
        let mut stream = self.0;
        stream.read(buf)
    }
}

fn read_request(stream: &UnixStream) -> Result<String, String> {
    let limited = Deadline(stream, Instant::now() + REQUEST_TIMEOUT).take(MAX_REQUEST_BYTES as u64 + 1);
    match crate::mcp::read_message(&mut BufReader::new(limited), MAX_REQUEST_BYTES) {
        Ok(Some(crate::mcp::Message::Line(line))) => parse_request(&line),
        Ok(Some(crate::mcp::Message::TooLong)) => Err("The request is too large.".into()),
        Ok(None) => Err("No question was sent.".into()),
        Err(_) => Err("Timed out waiting for the question.".into()),
    }
}

fn send(stream: &UnixStream, reply: Value) {
    let _ = writeln!(&mut &*stream, "{reply}");
}

/// One requester: reads its question, shows it if the user allows it and nothing else waits, and
/// waits until it is answered or declined, `timeout` passes, or the requester goes away.
pub fn serve_connection(stream: UnixStream, board: &Board, allowed: &dyn Fn() -> Result<(), String>, timeout: Duration) {
    let _ = stream.set_write_timeout(Some(Duration::from_secs(1)));
    let question = match read_request(&stream) {
        Ok(q) => q,
        Err(e) => return send(&stream, json!({ "error": e })),
    };
    if let Err(why) = allowed() {
        return send(&stream, json!({ "error": why }));
    }
    let Some(id) = stream.try_clone().ok().and_then(|reply| board.post(&question, reply)) else {
        return send(&stream, json!({ "error": BUSY }));
    };
    log::info!("an agent's question is waiting for an answer");
    let _ = stream.set_read_timeout(Some(POLL));
    let mut buf = [0u8; 64];
    loop {
        let Some(expired) = board.expired_now(id, timeout) else { return };
        if expired {
            board.end(Some(id), Some(json!({ "error": NO_ANSWER })));
            log::info!("an agent's question timed out");
            return;
        }
        match (&mut &stream).read(&mut buf) {
            // The requester went away (e.g. the agent was stopped or cancelled the call).
            Ok(0) => {
                board.end(Some(id), None);
                return;
            }
            Ok(_) => {
                board.end(Some(id), Some(json!({ "error": "One question per connection." })));
                return;
            }
            Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut | std::io::ErrorKind::Interrupted) => {}
            Err(_) => {
                board.end(Some(id), None);
                return;
            }
        }
    }
}

/// Binds the socket (0600; the folder is 0700). Replaces a stale socket, never another kind of file.
pub fn bind(path: &Path) -> std::io::Result<UnixListener> {
    use std::os::unix::fs::{FileTypeExt, PermissionsExt};
    if let Ok(meta) = std::fs::symlink_metadata(path) {
        if !meta.file_type().is_socket() {
            return Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, "a file that isn't a socket is in the way"));
        }
        std::fs::remove_file(path)?;
    }
    let listener = UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

/// Serves requesters on their own threads, at most `MAX_CONNECTIONS` at a time.
pub fn accept_loop(listener: UnixListener, board: &'static Board, allowed: Arc<dyn Fn() -> Result<(), String> + Send + Sync>, timeout: Duration) {
    let open = Arc::new(AtomicUsize::new(0));
    for conn in listener.incoming() {
        let stream = match conn {
            Ok(s) => s,
            Err(_) => {
                std::thread::sleep(Duration::from_millis(100));
                continue;
            }
        };
        if open.fetch_add(1, Ordering::SeqCst) >= MAX_CONNECTIONS {
            open.fetch_sub(1, Ordering::SeqCst);
            let _ = stream.set_write_timeout(Some(Duration::from_secs(1)));
            send(&stream, json!({ "error": BUSY }));
            continue;
        }
        let (count, allowed) = (open.clone(), allowed.clone());
        let spawned = std::thread::Builder::new().name("agent-question".into()).spawn(move || {
            serve_connection(stream, board, &*allowed, timeout);
            count.fetch_sub(1, Ordering::SeqCst);
        });
        if spawned.is_err() {
            open.fetch_sub(1, Ordering::SeqCst);
        }
    }
}

// ---------- In the app ----------

static SOCKET_PATH: OnceLock<PathBuf> = OnceLock::new();
static TRAY_ITEM: Mutex<Option<tauri::menu::MenuItem<tauri::Wry>>> = Mutex::new(None);

/// Listens for agents' questions. Without a socket (a path too long for the platform, a file in the
/// way) the feature is unavailable and `ask_user` says OpsTrellis can't be reached.
pub fn start(handle: &tauri::AppHandle, data_dir: &Path) {
    use tauri::Manager;
    let notify = handle.clone();
    QUESTIONS.on_change(move |text| {
        // Off the lock: from another thread this waits for the main thread, which may be declining.
        let item = TRAY_ITEM.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if let Some(item) = item {
            let _ = item.set_enabled(text.is_some());
        }
        if let Some(app) = notify.try_state::<Arc<crate::App>>() {
            app.show_question(text);
        }
    });
    let path = data_dir.join(SOCKET);
    let listener = match bind(&path) {
        Ok(l) => l,
        Err(e) => {
            log::warn!("agent questions unavailable: can't listen at {}: {e}", path.display());
            return;
        }
    };
    let _ = SOCKET_PATH.set(path);
    let settings = handle.clone();
    let allowed = Arc::new(move || -> Result<(), String> {
        let app = settings.try_state::<Arc<crate::App>>().ok_or("OpsTrellis is starting. Try again in a moment.")?;
        let s = app.settings();
        if !(s.agents.enabled && s.agents.voice) {
            return Err(OFF.into());
        }
        if !voice_allowed(&s) {
            return Err("The recording pill is off in OpsTrellis, so it can't show a question. The user can turn it on in Settings → General.".into());
        }
        Ok(())
    });
    std::thread::spawn(move || accept_loop(listener, &QUESTIONS, allowed, QUESTION_TIMEOUT));
}

pub fn set_tray_item(item: tauri::menu::MenuItem<tauri::Wry>) {
    let _ = item.set_enabled(QUESTIONS.is_pending());
    *TRAY_ITEM.lock().unwrap_or_else(|e| e.into_inner()) = Some(item);
}

/// On quit: the waiting agent gets an error, and the socket goes.
pub fn stop() {
    QUESTIONS.close();
    if let Some(path) = SOCKET_PATH.get() {
        let _ = std::fs::remove_file(path);
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::io::BufRead;

    pub(crate) fn leak() -> &'static Board {
        Box::leak(Box::new(Board::new()))
    }

    fn reply(client: &UnixStream) -> Value {
        let mut line = String::new();
        BufReader::new(client).read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap_or(Value::Null)
    }

    /// A requester connected to `serve_connection` on its own thread.
    fn connect(board: &'static Board, allowed: bool, timeout: Duration) -> (UnixStream, std::thread::JoinHandle<()>) {
        let (client, server) = UnixStream::pair().unwrap();
        let t = std::thread::spawn(move || serve_connection(server, board, &|| if allowed { Ok(()) } else { Err(OFF.into()) }, timeout));
        (client, t)
    }

    fn ask(client: &UnixStream, question: &str) {
        writeln!(&mut &*client, "{}", json!({ "ask": question })).unwrap();
    }

    fn wait_for(board: &Board) {
        for _ in 0..200 {
            if board.is_pending() {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("no question posted");
    }

    /// Waits for a question, puts it on screen as the pill would, and returns the id a dictation
    /// started `ANSWER_DELAY` later answers.
    pub(crate) fn show_and_wait(board: &Board) -> u64 {
        wait_for(board);
        board.window_shown(true);
        board.window_card(true);
        let text = board.lock().as_ref().map(|p| p.shown.clone());
        board.rendered(text.as_deref());
        board.answered_by(true, Instant::now() + ANSWER_DELAY).expect("on screen")
    }

    #[test]
    fn an_answer_reaches_the_requester() {
        let board = leak();
        let changes = Arc::new(Mutex::new(Vec::new()));
        let seen = changes.clone();
        board.on_change(move |t| seen.lock().unwrap().push(t.map(str::to_string)));
        let (client, t) = connect(board, true, QUESTION_TIMEOUT);
        ask(&client, "Which  branch\nshould I\u{7} deploy?");
        let id = show_and_wait(board);
        assert!(board.answer(id, "Main, after the tests pass."));
        assert_eq!(reply(&client), json!({ "answer": "Main, after the tests pass." }));
        t.join().unwrap();
        assert!(!board.is_pending() && !board.answer(id, "again"), "answered once");
        assert_eq!(*changes.lock().unwrap(), [Some("Which branch should I deploy?".to_string()), None]);
    }

    #[test]
    fn bad_requests_are_refused() {
        let board = leak();
        for (line, expect) in [
            ("not json".to_string(), "valid JSON"),
            ("{\"question\":\"x\"}".to_string(), "must be"),
            ("{\"ask\":\"x\",\"more\":1}".to_string(), "must be"),
            ("{\"ask\":5}".to_string(), "must be"),
            ("{\"ask\":\"   \"}".to_string(), "1 to 300"),
            (json!({ "ask": "x".repeat(MAX_QUESTION_CHARS + 1) }).to_string(), "1 to 300"),
            ("x".repeat(MAX_REQUEST_BYTES + 10), "too large"),
        ] {
            let (client, t) = connect(board, true, QUESTION_TIMEOUT);
            let _ = writeln!(&mut &client, "{line}");
            let r = reply(&client);
            assert!(r["error"].as_str().unwrap().contains(expect), "{expect}: {r}");
            t.join().unwrap();
        }
        let (client, t) = connect(board, true, QUESTION_TIMEOUT);
        client.shutdown(std::net::Shutdown::Write).unwrap();
        assert!(reply(&client)["error"].as_str().unwrap().contains("No question"));
        t.join().unwrap();
        assert!(!board.is_pending());
    }

    #[test]
    fn consent_busy_timeout_and_decline() {
        let board = leak();
        let (client, t) = connect(board, false, QUESTION_TIMEOUT);
        ask(&client, "Ready?");
        assert_eq!(reply(&client)["error"], OFF);
        t.join().unwrap();

        let (first, t1) = connect(board, true, QUESTION_TIMEOUT);
        ask(&first, "Ready?");
        wait_for(board);
        let (second, t2) = connect(board, true, QUESTION_TIMEOUT);
        ask(&second, "Me too?");
        assert_eq!(reply(&second)["error"], BUSY);
        t2.join().unwrap();
        assert!(board.decline());
        assert_eq!(reply(&first)["error"], DECLINED);
        t1.join().unwrap();
        assert!(!board.decline(), "nothing left to decline");

        let (client, t) = connect(board, true, Duration::from_millis(300));
        ask(&client, "Still there?");
        assert_eq!(reply(&client)["error"], NO_ANSWER);
        t.join().unwrap();
        assert!(!board.is_pending());
    }

    #[test]
    fn a_requester_that_leaves_withdraws_its_question() {
        let board = leak();
        let (client, t) = connect(board, true, QUESTION_TIMEOUT);
        ask(&client, "Ready?");
        wait_for(board);
        drop(client);
        t.join().unwrap();
        assert!(!board.is_pending());

        let (client, t) = connect(board, true, QUESTION_TIMEOUT);
        ask(&client, "Ready?");
        wait_for(board);
        board.close();
        assert_eq!(reply(&client)["error"], QUIT);
        t.join().unwrap();
    }

    #[test]
    fn only_a_dictation_started_after_reading_answers() {
        let s = Duration::from_secs;
        let posted = Instant::now();
        // Posted during a recording, shown 20 s later when the pill went idle.
        let shown = posted + s(20);
        assert!(!answers(true, Some(shown), posted + s(10)), "pressed after the post, before it was on screen");
        assert!(!answers(true, Some(shown), shown + s(1)), "shown, then a dictation 1 s later: an ordinary one");
        assert!(answers(true, Some(shown), shown + s(3)), "shown, then a dictation 3 s later: the answer");
        assert!(answers(true, Some(shown), shown + ANSWER_DELAY));
        assert!(!answers(true, Some(shown), shown + ANSWER_DELAY - Duration::from_millis(1)));
        assert!(!answers(true, None, posted + s(50)), "never shown (or hidden now): never answered");
        assert!(!answers(false, Some(shown), shown + s(5)), "a command never answers");
        let board = leak();
        assert_eq!(board.answered_by(true, Instant::now()), None, "nothing to answer");
    }

    #[test]
    fn an_answer_being_spoken_holds_the_deadline() {
        let s = Duration::from_secs;
        let t0 = Instant::now();
        let at = |secs: u64| t0 + s(secs);
        let expired = |answering, ended: Option<u64>, now: u64| expired(QUESTION_TIMEOUT, t0, answering, ended.map(at), at(now));
        assert!(!expired(false, None, 59) && expired(false, None, 60), "nobody answers: 60 s from arrival");
        assert!(!expired(true, None, 65), "started answering at 50 s and still speaking at 65 s");
        assert!(!expired(true, None, 299) && expired(true, None, 300), "a stuck answer ends at MAX_WAIT");
        assert!(!expired(false, Some(58), 70), "an attempt at 58 s that sent nothing leaves at least 15 s");
        assert!(expired(false, Some(58), 73));
        assert!(!expired(false, Some(10), 59) && expired(false, Some(10), 60), "an early attempt doesn't extend the 60 s");
        assert!(expired(false, Some(299), 300), "MAX_WAIT holds after an attempt too");
    }

    #[test]
    fn answering_pauses_the_timeout_and_a_failed_attempt_leaves_time() {
        let board = leak();
        let (client, t) = connect(board, true, Duration::from_millis(300));
        ask(&client, "Ready?");
        let id = show_and_wait(board);
        board.begin_answer(id);
        std::thread::sleep(Duration::from_millis(700));
        assert!(board.is_pending(), "not timed out while the user is answering");
        {
            // The attempt ends without an answer (cancelled, no words): the grace period starts.
            let _attempt = board.attempt(id);
        }
        let ended = board.lock().as_ref().and_then(|p| p.attempt_ended);
        assert!(ended.is_some() && board.is_pending());
        board.close();
        assert_eq!(reply(&client)["error"], QUIT);
        t.join().unwrap();
    }

    #[test]
    fn withdrawn_consent_ends_the_question() {
        let board = leak();
        let (client, t) = connect(board, true, QUESTION_TIMEOUT);
        ask(&client, "Ready?");
        wait_for(board);
        assert!(board.turned_off());
        assert_eq!(reply(&client)["error"], TURNED_OFF);
        t.join().unwrap();
        let mut s = crate::settings::Settings::default();
        assert!(!voice_allowed(&s));
        s.agents.enabled = true;
        s.agents.voice = true;
        assert!(voice_allowed(&s));
        s.overlay = false;
        assert!(!voice_allowed(&s), "no pill, no question");
    }

    #[test]
    fn a_question_counts_as_seen_only_while_the_pill_shows_it() {
        let board = leak();
        let (client, t) = connect(board, true, QUESTION_TIMEOUT);
        ask(&client, "Which branch?");
        wait_for(board);
        let later = || Instant::now() + s3();
        assert_eq!(board.answered_by(true, later()), None, "posted, not drawn yet");
        board.window_shown(true);
        assert_eq!(board.answered_by(true, later()), None, "the window shows a recording, not the question");
        board.rendered(Some("Which branch?"));
        assert_eq!(board.answered_by(true, later()), None, "drawn, but the window is still the pill's size: cut off");
        board.window_card(true);
        assert!(board.answered_by(true, later()).is_some(), "the whole card on screen");
        board.rendered(None);
        board.rendered(Some("Something else?"));
        assert_eq!(board.answered_by(true, later()), None, "another question's text");
        board.rendered(Some("Which branch?"));
        let id = board.answered_by(true, later()).expect("on screen");
        assert_eq!(board.answered_by(true, Instant::now() + Duration::from_secs(1)), None, "1 s after it appeared");
        board.window_card(false);
        assert_eq!(board.answered_by(true, later()), None, "back to the pill's size");
        board.window_card(true);
        board.window_shown(false);
        assert_eq!(board.answered_by(true, later()), None, "the pill was hidden");
        board.window_shown(true);
        assert_eq!(board.answered_by(true, Instant::now() + Duration::from_secs(1)), None, "the 2 s start again when it reappears");
        board.rendered(None);
        assert_eq!(board.answered_by(true, later()), None, "the page shows something else (a recording, the review card)");
        assert!(board.answer(id, "Main."));
        assert_eq!(reply(&client)["answer"], "Main.");
        t.join().unwrap();
    }

    fn s3() -> Duration {
        Duration::from_secs(3)
    }

    #[test]
    fn questions_are_shown_whole_without_hidden_characters() {
        assert_eq!(shown("  Deploy\r\nnow?\u{1b}[31m "), "Deploy now? [31m");
        assert_eq!(shown("Ship\u{202e}it\u{2066}?"), "Ship it ?", "text-direction overrides can't disguise the question");
        let longest = "Should I deploy? ".repeat(17) + "Deploy now?";
        assert_eq!(longest.trim().chars().count(), MAX_QUESTION_CHARS);
        assert_eq!(shown(&longest), longest.trim(), "the whole question, never cut");
        assert_eq!(answer_context().category, "ai_prompts");
        assert!(!answer_context().is_terminal);
    }

    #[test]
    fn the_socket_is_private_and_replaces_only_a_stale_socket() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("osk-{}", &uuid::Uuid::new_v4().to_string()[..8]));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(SOCKET);
        drop(bind(&path).unwrap());
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        assert!(path.exists(), "a stale socket left by a crash");
        drop(bind(&path).unwrap());
        std::fs::remove_file(&path).unwrap();
        std::fs::write(&path, "keep").unwrap();
        assert!(bind(&path).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "keep");
        assert!(bind(&dir.join("x".repeat(200))).is_err(), "too long for a socket path");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
