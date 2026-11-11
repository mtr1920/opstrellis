//! Global shortcuts with press/release events (hold = push-to-talk, tap = hands-free). Single-key
//! shortcuts on macOS (Fn, Right ⌥, …) are handled by `fnkey` instead (hold = push-to-talk,
//! double-tap = hands-free).
//! Esc is grabbed only while recording, and Enter and Esc only while a rewrite waits for review,
//! so they keep working everywhere else.

use crate::fnkey;
use crate::pipeline::Mode;
use crate::App;
use anyhow::{anyhow, Result};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::Manager;
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutEvent, ShortcutState};

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Action {
    Dictate,
    Command,
    PasteLast,
    Cancel,
    /// `FALLBACK`: each press starts or finishes a hands-free dictation.
    Fallback,
    /// Enter / Esc while a command's rewrite waits for review in the pill.
    ReviewInsert,
    ReviewDiscard,
}

impl Action {
    /// Grabbed only for a moment (`set_cancel_key`), not part of the configured shortcuts.
    fn transient(self) -> bool {
        matches!(self, Action::Cancel | Action::ReviewInsert | Action::ReviewDiscard)
    }
}

/// Always registered for dictation, so there is a way to dictate even when the configured
/// shortcut fails (a single key without its permission, a combination another app took).
pub const FALLBACK: &str = "Ctrl+Shift+Space";

fn parse(s: &str) -> Result<Shortcut> {
    s.parse::<Shortcut>().map_err(|e| anyhow!("Invalid shortcut \"{s}\": {e}"))
}

/// (Re-)registers the configured shortcuts. On failure the previous ones are restored. A
/// single-key shortcut (macOS) isn't a global shortcut: it is handed to `fnkey`.
/// `startup`: the first registration, with nothing to roll back to.
pub fn register(app: &Arc<App>, startup: bool) -> Result<()> {
    let s = app.settings();
    let all = [(&s.hotkeys.dictate, Action::Dictate), (&s.hotkeys.command, Action::Command), (&s.hotkeys.paste_last, Action::PasteLast)];
    let mut single = single_key(&all)?;
    let mut unsupported = None;
    // Only macOS can watch a single key. One stored elsewhere (settings copied from a Mac or edited
    // by hand) is skipped, so the other shortcuts and the fallback keep working.
    if let Some((key, _)) = single.filter(|_| !cfg!(target_os = "macos")) {
        unsupported = Some(format!("{} can't be a shortcut on this system, so {FALLBACK} is used instead. Choose a key combination in Settings → Shortcuts.", key.name()));
        single = None;
    }
    let wanted = all.iter().filter(|(k, _)| fnkey::Key::parse(k).is_none()).map(|(k, a)| Ok((parse(k)?, *a))).collect::<Result<Vec<_>>>()?;
    let mut ids: Vec<u32> = wanted.iter().map(|(sc, _)| sc.id()).collect();
    ids.sort_unstable();
    ids.dedup();
    if ids.len() != wanted.len() {
        return Err(anyhow!("Each shortcut must be different"));
    }
    // The shortcut recorder is open: checked, and applied when it closes (`resume`).
    if SUSPENDED.load(Ordering::SeqCst) {
        return Ok(());
    }
    let gs = app.handle.global_shortcut();
    // A copy: the temporary keys (`sync_transient_keys`) stay listed throughout, so a grab made
    // meanwhile is never lost from the list (it could then never be released).
    let previous: Vec<(Shortcut, Action)> = app.shortcuts.lock().unwrap().iter().filter(|(_, a)| !a.transient()).copied().collect();
    // unregister_multiple, not unregister_all: the latter holds the plugin's lock across a
    // main-thread round-trip and can deadlock with a key event being dispatched.
    let _ = gs.unregister_multiple(previous.iter().map(|(s, _)| *s).collect::<Vec<_>>());
    let binding = single.map(|(key, a)| (key, if a == Action::Command { Mode::Command } else { Mode::Dictate }));
    // Nothing to roll back to at startup: a combination another app owns mustn't also disable Fn.
    let mut registered: Vec<(Shortcut, Action)> = Vec::new();
    for (sc, action) in &wanted {
        if let Err(e) = gs.register(*sc) {
            let _ = gs.unregister_multiple(registered.iter().map(|(s, _)| *s).collect::<Vec<_>>());
            for (p, _) in &previous {
                let _ = gs.register(*p);
            }
            let mut kept = previous;
            add_fallback(gs, &mut kept);
            with_configured(&mut app.shortcuts.lock().unwrap(), kept);
            if startup {
                fnkey::set(app, binding);
            }
            return Err(anyhow!("Could not register {sc}: {e} (is another app using it?)"));
        }
        registered.push((*sc, *action));
    }
    add_fallback(gs, &mut registered);
    with_configured(&mut app.shortcuts.lock().unwrap(), registered);
    *app.hotkey_error.lock().unwrap() = None;
    if !fnkey::set(app, binding) {
        // Not fatal: Fn starts working by itself once the permission is granted (and then clears
        // this error, so set it first and recheck in case that just happened).
        *app.hotkey_error.lock().unwrap() = Some(fnkey::permission_error());
        if fnkey::active() == Some(true) {
            *app.hotkey_error.lock().unwrap() = None;
        } else {
            app.report_error(&fnkey::permission_error());
        }
    }
    if let Some(msg) = unsupported {
        *app.hotkey_error.lock().unwrap() = Some(msg.clone());
        app.report_error(&msg);
    }
    Ok(())
}

/// Replaces the configured shortcuts in the list, keeping the temporary keys as they are.
fn with_configured(list: &mut Vec<(Shortcut, Action)>, configured: Vec<(Shortcut, Action)>) {
    list.retain(|(_, a)| a.transient());
    list.extend(configured);
}

/// Registers `FALLBACK` unless `list` has it already, or a configured shortcut uses the same
/// combination (that one wins). Failing to get it is not an error: it is only a fallback.
fn add_fallback<R: tauri::Runtime>(gs: &tauri_plugin_global_shortcut::GlobalShortcut<R>, list: &mut Vec<(Shortcut, Action)>) {
    let fallback = parse(FALLBACK).expect("valid");
    if list.iter().any(|(s, _)| s.id() == fallback.id()) {
        return;
    }
    match gs.register(fallback) {
        Ok(()) => list.push((fallback, Action::Fallback)),
        Err(e) => log::warn!("fallback shortcut {FALLBACK} unavailable: {e}"),
    }
}

/// The single-key shortcut, if any, and its action.
fn single_key(all: &[(&String, Action)]) -> Result<Option<(fnkey::Key, Action)>> {
    let mut singles = all.iter().filter_map(|(k, a)| fnkey::Key::parse(k).map(|key| (key, *a)));
    let first = singles.next();
    if singles.next().is_some() {
        return Err(anyhow!("Only one shortcut can be a single key like Fn or Right Option"));
    }
    if matches!(first, Some((_, Action::PasteLast))) {
        return Err(anyhow!("Paste last needs a key combination, not a single key"));
    }
    Ok(first)
}

/// The shortcut recorder in Settings is open. Pressing a shortcut there must record it, not
/// trigger it, so they are unregistered (and single keys are captured, see `fnkey`) until
/// `resume`. They stay listed, so `resume` can put them back if the new ones can't be registered.
static SUSPENDED: AtomicBool = AtomicBool::new(false);

pub fn suspend(app: &Arc<App>) {
    if SUSPENDED.swap(true, Ordering::SeqCst) {
        return;
    }
    fnkey::set_recording(true);
    let list: Vec<Shortcut> = app.shortcuts.lock().unwrap().iter().filter(|(_, a)| !a.transient()).map(|(s, _)| *s).collect();
    let _ = app.handle.global_shortcut().unregister_multiple(list);
}

/// The recorder closed: registers the shortcuts as they are now saved. A no-op unless suspended,
/// so the window losing focus or the page reloading can call it to be safe.
pub fn resume(app: &Arc<App>) {
    if !SUSPENDED.swap(false, Ordering::SeqCst) {
        return;
    }
    fnkey::set_recording(false);
    if let Err(e) = register(app, false) {
        let msg = format!("{e:#}");
        *app.hotkey_error.lock().unwrap() = Some(msg.clone());
        app.report_error(&msg);
    }
}

/// Grabs Esc while recording, and Enter and Esc while a rewrite waits for review. Runs on its own
/// thread: (un)registering waits for the main thread, which may be inside a shortcut callback (and
/// doing it from inside a callback deadlocks the X11 hotkey thread). `sync_transient_keys`
/// re-reads the state when it runs, so out-of-order calls still converge.
pub fn set_cancel_key(app: &Arc<App>, _on: bool) {
    let app = app.clone();
    std::thread::spawn(move || {
        sync_transient_keys(&app);
    });
}

/// The keys grabbed for a state: a review takes Esc over from a recording.
fn transient_keys(recording: bool, reviewing: bool) -> &'static [(&'static str, Action)] {
    if reviewing {
        &[("Enter", Action::ReviewInsert), ("Escape", Action::ReviewDiscard)]
    } else if recording {
        &[("Escape", Action::Cancel)]
    } else {
        &[]
    }
}

/// Every key `transient_keys` can ask for.
const TRANSIENT: [&str; 2] = ["Enter", "Escape"];

/// What `sync_transient_keys` changes.
#[derive(Debug, Default, PartialEq)]
struct Plan {
    /// Ungrabbed, listed or not: a grab that somehow went missing from the list is released too.
    release: Vec<Shortcut>,
    /// Grabbed already: only what the key does changes (Esc from a recording to a review).
    retag: Vec<(Shortcut, Action)>,
    grab: Vec<(Shortcut, Action)>,
    /// A wanted key is also a configured shortcut (a hand-edited setting): that one is left alone.
    blocked: bool,
}

/// `held`: the key that ended a review or a recording and is still down (`hold`): kept grabbed.
fn plan(list: &[(Shortcut, Action)], wanted: &[(Shortcut, Action)], held: Option<u32>) -> Plan {
    let configured = |id: u32| list.iter().any(|(s, a)| !a.transient() && s.id() == id);
    let mut p = Plan::default();
    for sc in TRANSIENT.iter().map(|k| k.parse::<Shortcut>().expect("valid")) {
        if !wanted.iter().any(|(w, _)| w.id() == sc.id()) && held != Some(sc.id()) && !configured(sc.id()) {
            p.release.push(sc);
        }
    }
    for (sc, action) in wanted {
        if configured(sc.id()) {
            p.blocked = true;
        } else if let Some((_, a)) = list.iter().find(|(s, a)| a.transient() && s.id() == sc.id()) {
            if a != action {
                p.retag.push((*sc, *action));
            }
        } else {
            p.grab.push((*sc, *action));
        }
    }
    p
}

/// Grabs exactly the keys of the current state (`transient_keys`), plus a held one (`hold`); true
/// if all the wanted ones are grabbed. Blocks until done: never call it on the main thread. The
/// shortcut list lock is never held while registering (the shortcut handler on the main thread
/// needs it).
pub fn sync_transient_keys(app: &Arc<App>) -> bool {
    static SERIAL: Mutex<()> = Mutex::new(());
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let gs = app.handle.global_shortcut();
    let wanted: Vec<(Shortcut, Action)> =
        transient_keys(app.is_recording(), app.is_reviewing()).iter().map(|(k, a)| (k.parse().expect("valid"), *a)).collect();
    let list = app.shortcuts.lock().unwrap().clone();
    let p = plan(&list, &wanted, held_key());
    for sc in &p.release {
        app.shortcuts.lock().unwrap().retain(|(s, a)| !(a.transient() && s.id() == sc.id()));
        let _ = gs.unregister(*sc);
    }
    for (sc, action) in &p.retag {
        for entry in app.shortcuts.lock().unwrap().iter_mut().filter(|(s, a)| a.transient() && s.id() == sc.id()) {
            entry.1 = *action;
        }
    }
    let mut all = !p.blocked;
    for (sc, action) in &p.grab {
        // Listed before it is grabbed, so the grab can never exist without its entry.
        app.shortcuts.lock().unwrap().push((*sc, *action));
        // It may be our own grab, lost from the list: release it and grab it again.
        if gs.register(*sc).is_err() {
            let _ = gs.unregister(*sc);
            if gs.register(*sc).is_err() {
                app.shortcuts.lock().unwrap().retain(|(s, a)| !(a.transient() && s.id() == sc.id()));
                all = false;
            }
        }
    }
    all
}

/// A temporary key that ended a review or a recording stays grabbed until it is let go: released
/// while still down, its auto-repeat would reach the app below (Enter sending a chat draft). On
/// X11 the key's press also grabs the whole keyboard until its release, so a paste must wait for
/// it (`wait_held_released`). At most `HOLD_MAX`, in case a release is never reported.
static HELD: Mutex<Option<(u32, Instant)>> = Mutex::new(None);
const HOLD_MAX: Duration = Duration::from_secs(2);

fn held_key() -> Option<u32> {
    HELD.lock().unwrap_or_else(|e| e.into_inner()).filter(|(_, until)| Instant::now() < *until).map(|(id, _)| id)
}

fn hold(app: &Arc<App>, id: u32) {
    if held_key() == Some(id) {
        return;
    }
    *HELD.lock().unwrap_or_else(|e| e.into_inner()) = Some((id, Instant::now() + HOLD_MAX));
    let app = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(HOLD_MAX);
        set_cancel_key(&app, false);
    });
}

fn release_held(app: &Arc<App>, id: u32) {
    let mut held = HELD.lock().unwrap_or_else(|e| e.into_inner());
    if held.is_some_and(|(h, _)| h == id) {
        *held = None;
        drop(held);
        set_cancel_key(app, false);
    }
}

/// Blocks until the key that ended a review is let go (at most `HOLD_MAX`). Not on the main thread.
pub fn wait_held_released() {
    while held_key().is_some() {
        std::thread::sleep(Duration::from_millis(10));
    }
}

pub fn handle(handle: &tauri::AppHandle, shortcut: &Shortcut, event: ShortcutEvent) {
    let Some(app) = handle.try_state::<Arc<App>>() else { return };
    let app = app.inner().clone();
    let action = app.shortcuts.lock().unwrap().iter().find(|(s, _)| s.id() == shortcut.id()).map(|(_, a)| *a);
    log::debug!("shortcut {shortcut} {:?} -> {action:?}", event.state());
    let Some(action) = action else { return };
    // Before acting: the action releases the temporary keys, and this one must stay grabbed.
    if action.transient() && event.state() == ShortcutState::Pressed {
        hold(&app, shortcut.id());
    }
    // Shortcut callbacks run on the event loop; never block it.
    match (action, event.state()) {
        (Action::Dictate, ShortcutState::Pressed) => app.on_press(Mode::Dictate),
        (Action::Command, ShortcutState::Pressed) => app.on_press(Mode::Command),
        (Action::Dictate | Action::Command, ShortcutState::Released) => app.on_release(),
        (Action::Cancel, ShortcutState::Pressed) => app.cancel(),
        (Action::Fallback, ShortcutState::Pressed) => app.toggle(Mode::Dictate),
        (Action::ReviewInsert, ShortcutState::Pressed) => {
            app.decide_review(true);
        }
        (Action::ReviewDiscard, ShortcutState::Pressed) => {
            app.decide_review(false);
        }
        (Action::Cancel | Action::ReviewInsert | Action::ReviewDiscard, ShortcutState::Released) => release_held(&app, shortcut.id()),
        (Action::PasteLast, ShortcutState::Pressed) => {
            tauri::async_runtime::spawn(async move {
                if let Err(e) = app.paste_last().await {
                    app.report_error(&format!("{e:#}"));
                }
            });
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fnkey::Key;

    fn bind(dictate: &str, command: &str, paste_last: &str) -> Result<Option<(Key, Action)>> {
        let (d, c, p) = (dictate.to_string(), command.to_string(), paste_last.to_string());
        single_key(&[(&d, Action::Dictate), (&c, Action::Command), (&p, Action::PasteLast)])
    }

    #[test]
    fn single_key_binding() {
        assert_eq!(bind("Ctrl+Shift+Space", "Ctrl+Alt+Space", "Ctrl+Alt+V").unwrap(), None);
        assert!(bind("Fn", "Fn", "Ctrl+Alt+V").is_err(), "Fn bound twice");
        assert!(bind("Fn", "RightOption", "Ctrl+Alt+V").is_err(), "two single-key shortcuts");
        assert!(bind("Ctrl+Shift+Space", "Ctrl+Alt+Space", "Fn").is_err(), "paste last can't be a single key");
        // Off macOS, `register` skips them.
        assert_eq!(bind("Fn", "Ctrl+Alt+Space", "Ctrl+Alt+V").unwrap(), Some((Key::Fn, Action::Dictate)));
        assert_eq!(bind("Ctrl+Shift+Space", "RightOption", "Ctrl+Alt+V").unwrap(), Some((Key::RightOption, Action::Command)));
        // Single keys are never parsed as regular global shortcuts.
        for k in ["Fn", "RightCommand", "RightOption", "RightControl", "RightShift"] {
            assert!(parse(k).is_err(), "{k}");
        }
    }

    #[test]
    fn transient_keys_follow_the_state() {
        assert!(transient_keys(false, false).is_empty());
        assert_eq!(transient_keys(true, false), &[("Escape", Action::Cancel)]);
        assert_eq!(transient_keys(false, true), &[("Enter", Action::ReviewInsert), ("Escape", Action::ReviewDiscard)]);
        assert_eq!(transient_keys(true, true), transient_keys(false, true), "a review owns Esc");
        for (k, a) in transient_keys(false, true) {
            assert!(parse(k).is_ok() && a.transient(), "{k}");
        }
        assert!(!Action::Dictate.transient() && !Action::Fallback.transient() && !Action::PasteLast.transient());
    }

    fn sc(k: &str) -> Shortcut {
        parse(k).unwrap()
    }

    #[test]
    fn configuring_shortcuts_keeps_temporary_keys() {
        // Enter was grabbed for a review while `register` was registering the new configuration.
        let mut list = vec![(sc("Ctrl+Shift+Space"), Action::Dictate), (sc("Escape"), Action::ReviewDiscard), (sc("Ctrl+Alt+V"), Action::PasteLast)];
        list.push((sc("Enter"), Action::ReviewInsert));
        with_configured(&mut list, vec![(sc("Ctrl+Alt+D"), Action::Dictate), (sc("Ctrl+Shift+Space"), Action::Fallback)]);
        assert_eq!(list, vec![
            (sc("Escape"), Action::ReviewDiscard), (sc("Enter"), Action::ReviewInsert),
            (sc("Ctrl+Alt+D"), Action::Dictate), (sc("Ctrl+Shift+Space"), Action::Fallback),
        ]);
    }

    #[test]
    fn sync_plan() {
        let configured = vec![(sc("Ctrl+Shift+Space"), Action::Dictate)];
        let want = |recording, reviewing| transient_keys(recording, reviewing).iter().map(|(k, a)| (sc(k), *a)).collect::<Vec<_>>();
        // Nothing wanted: both keys are released even when unlisted, which heals a lost grab.
        let p = plan(&configured, &[], None);
        assert_eq!((p.release, p.grab.len(), p.retag.len(), p.blocked), (vec![sc("Enter"), sc("Escape")], 0, 0, false));
        // A review starts during a recording: Esc changes what it does, Enter is grabbed.
        let recording = [configured.clone(), want(true, false)].concat();
        let p = plan(&recording, &want(false, true), None);
        assert_eq!((p.release, p.retag, p.grab), (vec![], vec![(sc("Escape"), Action::ReviewDiscard)], vec![(sc("Enter"), Action::ReviewInsert)]));
        // Enter ended the review and is still down: it stays grabbed, Esc goes.
        let reviewing = [configured.clone(), want(false, true)].concat();
        let p = plan(&reviewing, &[], Some(sc("Enter").id()));
        assert_eq!(p.release, vec![sc("Escape")]);
        // Already as wanted: nothing to do.
        assert_eq!(plan(&reviewing, &want(false, true), None), Plan { release: vec![], ..Default::default() });
        // A hand-edited setting made Esc a configured shortcut: never taken or released.
        let odd = vec![(sc("Escape"), Action::PasteLast)];
        let p = plan(&odd, &want(true, false), None);
        assert_eq!((p.release, p.grab.len(), p.blocked), (vec![sc("Enter")], 0, true));
    }
}
