//! Notetaker: records a meeting (microphone, optionally with a second input such as a system-audio
//! monitor or BlackHole), transcribes it in ~30 s chunks through the normal provider chain while
//! recording, and on stop saves a note with an AI summary (when AI is enabled). With a second input
//! each source is transcribed on its own, so the transcript reads `Me: …` / `Them: …`.

use crate::audio::{self, Recording, SAMPLE_RATE};
use crate::db::Note;
use crate::pipeline::transcribe_with_fallback;
use crate::App;
use anyhow::{anyhow, Result};
use serde::Serialize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::Emitter;

pub struct Meeting {
    stop: Arc<AtomicBool>,
    task: tauri::async_runtime::JoinHandle<()>,
    pub started_at: String,
    pub transcript: Arc<Mutex<String>>,
}

/// Note formats for the AI summary: (id, name, what to write). `summary_prompt` says whether the
/// transcript has speaker labels.
pub const TEMPLATES: &[(&str, &str, &str)] = &[
    ("general", "General", "Write concise Markdown with these sections: ## Summary (3-5 sentences), ## Key points (bullets), ## Decisions (bullets, or \"None\"), ## Action items (bullets as \"- [ ] owner: task\" when an owner is mentioned)."),
    ("sales", "Sales call", "This is a sales conversation. Write concise Markdown with these sections: ## Summary (2-3 sentences), ## Customer needs and pain points, ## Current solution, ## Budget, timeline and decision process (write \"Not discussed\" for anything not covered), ## Objections and concerns, ## Next steps (bullets as \"- [ ] owner: task\")."),
    ("interview", "Interview", "This is a job interview. Write concise Markdown with these sections: ## Summary (2-3 sentences), ## Experience and skills discussed (bullets), ## Evidence (notable examples the candidate gave), ## Open questions to follow up, ## Next steps (bullets as \"- [ ] owner: task\"). Report only what was said: no ratings, no hiring recommendation, and nothing about age, gender, origin, health, family or other personal characteristics."),
    ("one_on_one", "1:1", "This is a one-on-one meeting. Write concise Markdown with these sections: ## Updates, ## Blockers and concerns, ## Feedback, ## Decisions, ## Action items (bullets as \"- [ ] owner: task\")."),
    ("standup", "Standup", "This is a team standup. Write concise Markdown with these sections: ## Done, ## Next, ## Blockers, ## Action items (bullets as \"- [ ] owner: task\"). Group by person when names are mentioned."),
];

fn summary_prompt(template: &str, labelled: bool) -> String {
    let format = TEMPLATES.iter().find(|(id, ..)| *id == template).unwrap_or(&TEMPLATES[0]).2;
    let speakers = if labelled {
        "inside <transcript> tags). Each transcript line starts with its speaker: \"Me:\" is the user, \"Them:\" is the other participants (possibly several people). Attribute commitments, decisions and action items to the side that made them, using \"Me\" or \"Them\" as the owner when no name is mentioned."
    } else {
        "inside <transcript> tags; speakers are not labelled)."
    };
    format!("You write meeting notes from a raw transcript ({speakers} {format} Use only information from the transcript. Write in the transcript's language. Output only the notes.")
}

/// Follow-ups drafted from a saved meeting note: (kind, instruction).
const FOLLOW_UPS: &[(&str, &str)] = &[
    ("email", "Draft a short follow-up email to the other participants of this meeting, from the user's point of view: thank them, recap the key points and decisions in a few bullets, and list next steps with owners. Write in the meeting's language. Output only the email body, starting with a greeting, without a subject line or placeholders like [Name]."),
    ("crm", "Write a compact CRM activity note for this meeting as plain text lines in the form \"Field: value\", with these fields: Summary, Needs, Objections, Decision process, Next step, Next step date. Write \"Not discussed\" when the meeting didn't cover a field. Output only the lines."),
];

/// Drafts a follow-up (`kind`: "email" or "crm") from a meeting note's summary and transcript.
pub async fn follow_up(app: &App, note_id: i64, kind: &str) -> Result<String> {
    let instruction = FOLLOW_UPS.iter().find(|(k, _)| *k == kind).map(|(_, i)| *i).ok_or_else(|| anyhow!("Unknown follow-up \"{kind}\""))?;
    let settings = app.settings();
    if !settings.llm.enabled {
        return Err(anyhow!("Follow-ups need AI. Turn on AI polish in Settings → AI polish."));
    }
    let note = app.db.get_note(note_id)?.filter(|n| n.kind == "meeting").ok_or_else(|| anyhow!("Meeting not found"))?;
    let system = "You help the user follow up on a meeting. The user message contains the meeting notes inside <meeting> tags; use only information from them.";
    let out = app.llm_client(&settings).complete(system, &format!("<meeting>\n{}\n</meeting>\n{instruction}", note.body)).await?;
    crate::llm::check_command_output(None, &out)?;
    Ok(out)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeetingStatus {
    pub active: bool,
    /// A stopped meeting is still being transcribed/summarized.
    pub finishing: bool,
    pub started_at: Option<String>,
    pub transcript: String,
}

const CHUNK_MAX: usize = SAMPLE_RATE as usize * 30;
const CHUNK_MIN: usize = SAMPLE_RATE as usize * 10;

pub fn start(app: &Arc<App>, second_device: Option<String>, template: Option<String>) -> Result<()> {
    let mut slot = app.meeting.lock().unwrap();
    if slot.is_some() {
        return Err(anyhow!("A meeting is already being recorded"));
    }
    let settings = app.settings();
    if app.providers(&settings).is_empty() {
        return Err(anyhow!("No transcription provider is enabled"));
    }
    let mic = Recording::start(settings.microphone.clone(), None, |_| {})?;
    let other = match second_device.filter(|d| !d.is_empty()) {
        Some(d) => Some(Recording::start(Some(d), None, |_| {})?),
        None => None,
    };
    let stop = Arc::new(AtomicBool::new(false));
    let transcript = Arc::new(Mutex::new(String::new()));
    let started_at = crate::db::now_rfc3339();
    let template = template.filter(|t| TEMPLATES.iter().any(|(id, ..)| id == t)).unwrap_or_else(|| "general".into());
    let task = tauri::async_runtime::spawn(run(app.clone(), mic, other, stop.clone(), transcript.clone(), started_at.clone(), template));
    *slot = Some(Meeting { stop, task, started_at, transcript });
    let _ = app.handle.emit("meeting-status", ());
    Ok(())
}

pub fn stop(app: &Arc<App>) -> Result<()> {
    let m = app.meeting.lock().unwrap().take().ok_or_else(|| anyhow!("No meeting is being recorded"))?;
    end(app, m);
    Ok(())
}

fn end(app: &App, m: Meeting) {
    m.stop.store(true, Ordering::SeqCst);
    FINISHING.fetch_add(1, Ordering::SeqCst);
    // The task finishes the last chunk, summarizes and emits `meeting-done`.
    drop(m.task);
    let _ = app.handle.emit("meeting-status", ());
}

/// Stops the meeting whose flag is `stop`, unless the user already stopped it (and maybe started another).
fn end_if_current(app: &App, stop: &Arc<AtomicBool>) -> bool {
    let mut slot = app.meeting.lock().unwrap();
    if !slot.as_ref().is_some_and(|m| Arc::ptr_eq(&m.stop, stop)) {
        return false;
    }
    let m = slot.take().unwrap();
    drop(slot);
    end(app, m);
    true
}

/// A meeting stops by itself (and is saved) after this long without speech on any input, or at
/// this length: one tray click must not keep the mic open and audio going to a provider for hours.
const MEETING_SILENCE: Duration = Duration::from_secs(15 * 60);
const MEETING_MAX: Duration = Duration::from_secs(4 * 60 * 60);

/// Why a meeting should stop by itself now, if it should. `failing`: transcription is failing, so
/// the silence was measured by sound, not words.
fn meeting_expired(elapsed: Duration, since_speech: Duration, failing: bool) -> Option<&'static str> {
    match (elapsed >= MEETING_MAX, since_speech >= MEETING_SILENCE, failing) {
        (true, _, false) => Some("Meeting recording stopped at the 4-hour limit."),
        (true, _, true) => Some("Meeting recording stopped at the 4-hour limit. Transcription was failing, so part of it may be missing."),
        (_, true, false) => Some("Meeting recording stopped after 15 minutes without speech."),
        (_, true, true) => Some("Meeting recording stopped after 15 minutes of silence while transcription was failing."),
        _ => None,
    }
}

/// Whether this moment counts as speech for the silence rule: a chunk that produced words, or,
/// while transcription is failing (a provider outage), any sound, so an outage doesn't end the
/// meeting. Typing, a fan or music alone never keep a working meeting going.
fn heard_speech(words: bool, failing: bool, loud: bool) -> bool {
    words || (failing && loud)
}

/// What transcribing one chunk gave: any text, and whether a provider failed.
#[derive(Default)]
struct Heard {
    words: bool,
    failed: bool,
}

/// Meetings stopped but not yet saved (their task is still finishing).
static FINISHING: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

pub fn status(app: &App) -> MeetingStatus {
    let finishing = FINISHING.load(Ordering::SeqCst) > 0;
    match &*app.meeting.lock().unwrap() {
        Some(m) => MeetingStatus { active: true, finishing, started_at: Some(m.started_at.clone()), transcript: m.transcript.lock().unwrap().clone() },
        None => MeetingStatus { active: false, finishing, started_at: None, transcript: String::new() },
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Speaker {
    Me,
    Them,
}

impl Speaker {
    fn label(self) -> &'static str {
        match self {
            Speaker::Me => "Me",
            Speaker::Them => "Them",
        }
    }
}

/// Takes what both sources have delivered, aligned by sample index. The longer one's remainder
/// waits for the next round, unless `flush` (the devices stopped) pads the shorter with silence.
fn align(a: &mut Vec<i16>, b: &mut Vec<i16>, flush: bool) -> (Vec<i16>, Vec<i16>) {
    // If one input stops delivering (unplugged, stream error), treat it as silence instead of
    // holding back the other input forever.
    const MAX_DRIFT: usize = SAMPLE_RATE as usize * 2;
    if flush || a.len() > b.len() + MAX_DRIFT {
        b.resize(b.len().max(a.len()), 0);
    }
    if flush || b.len() > a.len() + MAX_DRIFT {
        a.resize(a.len().max(b.len()), 0);
    }
    let n = a.len().min(b.len());
    (a.drain(..n).collect(), b.drain(..n).collect())
}

fn mix(a: &[i16], b: &[i16]) -> Vec<i16> {
    a.iter().zip(b).map(|(x, y)| (*x as i32 + *y as i32).clamp(i16::MIN as i32, i16::MAX as i32) as i16).collect()
}

/// Cut at a pause after 10 s, or hard at 30 s, so words aren't split across chunks. With two
/// sources the pause is found in their sum, and both are cut at the same sample.
fn chunk_ready(me: &[i16], them: Option<&[i16]>) -> bool {
    let n = me.len();
    if n >= CHUNK_MAX {
        return true;
    }
    if n <= CHUNK_MIN {
        return false;
    }
    let tail = n - SAMPLE_RATE as usize * 6 / 10;
    match them {
        Some(them) => audio::is_silent(&mix(&me[tail..], &them[tail..])),
        None => audio::is_silent(&me[tail..]),
    }
}

/// A chunk of the microphone's audio and, with a second input, that input's audio for the same span.
type Chunk = (Vec<i16>, Option<Vec<i16>>);

/// Audio not yet transcribed: `mic` / `other` as delivered, `me` / `them` aligned on one timeline
/// (equally long) until the next chunk boundary.
struct Timeline {
    mic: Vec<i16>,
    other: Vec<i16>,
    me: Vec<i16>,
    them: Option<Vec<i16>>,
}

impl Timeline {
    fn new(second_input: bool) -> Self {
        Timeline { mic: Vec::new(), other: Vec::new(), me: Vec::new(), them: second_input.then(Vec::new) }
    }

    /// Adds what the devices delivered; returns a chunk once one is complete.
    fn push(&mut self, mic: Vec<i16>, other: Vec<i16>) -> Option<Chunk> {
        self.feed(mic, other, false);
        chunk_ready(&self.me, self.them.as_deref()).then(|| self.take())
    }

    /// The rest once the devices stopped, including the last moments of both sources.
    fn finish(mut self, mic: Vec<i16>, other: Vec<i16>) -> Chunk {
        self.feed(mic, other, true);
        self.take()
    }

    fn feed(&mut self, mic: Vec<i16>, other: Vec<i16>, flush: bool) {
        self.mic.extend(mic);
        match &mut self.them {
            Some(them) => {
                self.other.extend(other);
                let (me, other) = align(&mut self.mic, &mut self.other, flush);
                self.me.extend(me);
                them.extend(other);
            }
            None => self.me.append(&mut self.mic),
        }
    }

    fn take(&mut self) -> Chunk {
        (std::mem::take(&mut self.me), self.them.as_mut().map(std::mem::take))
    }
}

async fn run(app: Arc<App>, mic: Recording, other: Option<Recording>, stop: Arc<AtomicBool>, transcript: Arc<Mutex<String>>, started_at: String, template: String) {
    let (mic_buf, other_buf) = (mic.buffer(), other.as_ref().map(|o| o.buffer()));
    let labelled = other.is_some();
    let mut timeline = Timeline::new(labelled);
    let mut last = None;
    let mut total_ms = 0i64;
    let (started, mut last_speech, mut stopped_why, mut failing) = (Instant::now(), Instant::now(), None, false);
    while !stop.load(Ordering::SeqCst) {
        tokio::time::sleep(Duration::from_millis(500)).await;
        let mic_new = std::mem::take(&mut *mic_buf.lock().unwrap());
        let other_new = other_buf.as_ref().map(|ob| std::mem::take(&mut *ob.lock().unwrap())).unwrap_or_default();
        let loud = !audio::is_silent(&mic_new) || !audio::is_silent(&other_new);
        let mut words = false;
        if let Some((me, them)) = timeline.push(mic_new, other_new) {
            total_ms += audio::duration_ms(&me);
            let heard = transcribe_turns(&app, &me, them.as_deref(), &transcript, &mut last).await;
            (words, failing) = (heard.words, heard.failed && !heard.words);
        }
        if heard_speech(words, failing, loud) {
            last_speech = Instant::now();
        }
        if let Some(why) = meeting_expired(started.elapsed(), last_speech.elapsed(), failing) {
            if end_if_current(&app, &stop) {
                log::info!("{why}");
                stopped_why = Some(why);
            }
        }
    }
    // Stop the devices first so the final moments are included.
    let mic_rest = tokio::task::spawn_blocking(move || mic.stop()).await.unwrap_or_default();
    let other_rest = match other {
        Some(o) => tokio::task::spawn_blocking(move || o.stop()).await.unwrap_or_default(),
        None => Vec::new(),
    };
    let (me, them) = timeline.finish(mic_rest, other_rest);
    total_ms += audio::duration_ms(&me);
    transcribe_turns(&app, &me, them.as_deref(), &transcript, &mut last).await;

    let text = transcript.lock().unwrap().clone();
    let note = finish_note(&app, &text, &started_at, total_ms, &template, labelled, stopped_why).await;
    FINISHING.fetch_sub(1, Ordering::SeqCst);
    if let Some(why) = stopped_why {
        app.outcome("warn", if note.is_ok() { format!("{why} The note is saved.") } else { format!("{why} No note was saved.") });
    }
    let _ = app.handle.emit("meeting-done", serde_json::json!({ "noteId": note.as_ref().ok().and_then(|n| n.id), "error": note.err().map(|e| format!("{e:#}")) }));
}

/// Transcribes a chunk into the live transcript: as before for the microphone alone, otherwise
/// each source on its own as `Me` / `Them` turns.
async fn transcribe_turns(app: &App, me: &[i16], them: Option<&[i16]>, transcript: &Mutex<String>, last: &mut Option<Speaker>) -> Heard {
    let failed = AtomicBool::new(false);
    let turns: Vec<(Option<Speaker>, String)> = match them {
        None => transcribe_chunk(app, me, &failed).await.map(|text| (None, text)).into_iter().collect(),
        Some(them) => {
            let local = app.settings().stt_chain.iter().find(|e| e.enabled).is_some_and(|e| e.provider == crate::settings::ProviderKind::Local);
            // A cloud provider handles both sides at once, so chunks don't back up. A local model
            // decodes one at a time, and waiting for it would use up the second side's timeout.
            let (me_text, them_text) = if local {
                (transcribe_chunk(app, me, &failed).await, transcribe_chunk(app, them, &failed).await)
            } else {
                tokio::join!(transcribe_chunk(app, me, &failed), transcribe_chunk(app, them, &failed))
            };
            let me_first = me_text.is_some() && them_text.is_some() && speech_onset(me) <= speech_onset(them);
            chunk_turns(me_text, them_text, me_first).into_iter().map(|(s, text)| (Some(s), text)).collect()
        }
    };
    let failed = failed.load(Ordering::SeqCst);
    if turns.is_empty() {
        return Heard { words: false, failed };
    }
    let mut t = transcript.lock().unwrap();
    for (speaker, text) in turns {
        append_turn(&mut t, last, speaker, &text);
    }
    let _ = app.handle.emit("meeting-transcript", t.clone());
    Heard { words: true, failed }
}

/// The chunk's text, or None when it is silent, nothing was recognized or every provider failed
/// (then `failed` is set).
async fn transcribe_chunk(app: &App, chunk: &[i16], failed: &AtomicBool) -> Option<String> {
    if audio::is_silent(chunk) {
        return None;
    }
    let settings = app.settings();
    let providers = app.providers(&settings);
    let language = Some(settings.language.clone()).filter(|l| l != "auto");
    // One retry: a transient provider error shouldn't drop 30 s of the meeting.
    let mut result = transcribe_with_fallback(&providers, None, chunk, language.as_deref(), &app.vocabulary()).await;
    if result.is_err() {
        tokio::time::sleep(Duration::from_secs(2)).await;
        result = transcribe_with_fallback(&providers, None, chunk, language.as_deref(), &app.vocabulary()).await;
    }
    match result {
        Ok((text, _)) if !crate::stt::clean_transcript(&text).is_empty() => {
            Some(crate::text::apply_dictionary(&crate::stt::clean_transcript(&text), &app.db.list_dictionary().unwrap_or_default()))
        }
        Ok(_) => None,
        Err(e) => {
            failed.store(true, Ordering::SeqCst);
            app.report_error(&format!("Notetaker: {e:#}"));
            None
        }
    }
}

/// Where speech starts in a chunk (the first half-second window with speech), to order the turns.
fn speech_onset(samples: &[i16]) -> usize {
    const WINDOW: usize = SAMPLE_RATE as usize / 2;
    (0..samples.len()).step_by(WINDOW / 2).find(|&i| !audio::is_silent(&samples[i..samples.len().min(i + WINDOW)])).unwrap_or(samples.len())
}

/// One chunk's turns: the microphone's text is dropped when it only echoes the speakers, and the
/// side that started speaking first comes first.
fn chunk_turns(me: Option<String>, them: Option<String>, me_first: bool) -> Vec<(Speaker, String)> {
    let me = me.filter(|m| !matches!(&them, Some(t) if is_echo(m, t)));
    let mut turns: Vec<_> = me.map(|m| (Speaker::Me, m)).into_iter().chain(them.map(|t| (Speaker::Them, t))).collect();
    if !me_first {
        turns.reverse();
    }
    turns
}

/// True when over 70% of the microphone's words also appear in the other source's text (the mic
/// heard the speakers). They must also make up half of the other side's words, so a short reply
/// that reuses their words ("sounds good") is kept.
fn is_echo(mic: &str, other: &str) -> bool {
    // Short replies often mirror the other side ("Thank you."); a dropped `Me:` line is lost,
    // while a short echo kept is only a duplicate.
    const MIN_WORDS: usize = 5;
    let words = |s: &str| -> Vec<String> {
        s.split_whitespace().map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase()).filter(|w| !w.is_empty()).collect()
    };
    let (mic, other) = (words(mic), words(other));
    if mic.len() < MIN_WORDS || other.is_empty() {
        return false;
    }
    let mut left: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for w in &other {
        *left.entry(w.as_str()).or_default() += 1;
    }
    let shared = mic
        .iter()
        .filter(|w| match left.get_mut(w.as_str()) {
            Some(n) if *n > 0 => {
                *n -= 1;
                true
            }
            _ => false,
        })
        .count();
    shared * 10 > mic.len() * 7 && shared * 2 >= other.len()
}

/// Appends a chunk's text. Labelled text is one paragraph per speaker turn, continued while the
/// same side keeps talking.
fn append_turn(t: &mut String, last: &mut Option<Speaker>, speaker: Option<Speaker>, text: &str) {
    let Some(speaker) = speaker else {
        if !t.is_empty() {
            t.push(' ');
        }
        t.push_str(text.trim());
        return;
    };
    // A provider's line breaks would start lines that carry no label.
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.is_empty() {
        return;
    }
    if *last == Some(speaker) {
        t.push(' ');
    } else {
        if !t.is_empty() {
            t.push_str("\n\n");
        }
        t.push_str(speaker.label());
        t.push_str(": ");
        *last = Some(speaker);
    }
    t.push_str(&text);
}

async fn finish_note(app: &App, transcript: &str, started_at: &str, total_ms: i64, template: &str, labelled: bool, stopped_why: Option<&str>) -> Result<Note> {
    if transcript.trim().is_empty() {
        return Err(anyhow!("Nothing was transcribed"));
    }
    let settings = app.settings();
    let when = chrono::DateTime::parse_from_rfc3339(started_at).map(|d| d.format("%b %-d, %H:%M").to_string()).unwrap_or_default();
    let minutes = (total_ms as f64 / 60_000.0).round().max(1.0);
    let mut body = String::new();
    if settings.llm.enabled && settings.llm.provider == crate::polish::PROVIDER && crate::polish::instruct_model(&settings, &app.models_dir).is_none() {
        body.push_str(&format!(
            "_No summary: {}. Add a local instruction model or choose a cloud provider in Settings → AI polish to summarize meetings._\n\n",
            crate::polish::no_instruct_reason(&settings)
        ));
    } else if settings.llm.enabled {
        match app.llm_client(&settings).complete(&summary_prompt(template, labelled), &format!("<transcript>\n{transcript}\n</transcript>")).await {
            Ok(summary) => body.push_str(&format!("{summary}\n\n")),
            Err(e) => {
                log::warn!("meeting summary failed: {e:#}");
                body.push_str("_No summary: the AI request failed. The transcript is below._\n\n");
            }
        }
    }
    if let Some(why) = stopped_why {
        body.push_str(&format!("_{why}_\n\n"));
    }
    body.push_str(&format!("## Transcript ({minutes} min)\n\n{transcript}"));
    let name = TEMPLATES.iter().find(|(id, ..)| *id == template).map(|t| t.1).filter(|_| template != "general");
    let title = match name {
        Some(name) => format!("Meeting · {when} · {name}"),
        None => format!("Meeting · {when}"),
    };
    app.db.upsert_note(Note { id: None, title, body, updated_at: None, kind: "meeting".into() })
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: usize = SAMPLE_RATE as usize;

    fn tone(secs: f64) -> Vec<i16> {
        (0..(secs * RATE as f64) as usize).map(|i| ((i as f64 * 0.05).sin() * 3000.0) as i16).collect()
    }

    fn quiet(secs: f64) -> Vec<i16> {
        vec![0; (secs * RATE as f64) as usize]
    }

    #[test]
    fn templates() {
        // The general template is the original summary format; unknown ids fall back to it.
        assert!(summary_prompt("general", false).contains("## Key points"));
        assert_eq!(summary_prompt("nope", false), summary_prompt("general", false));
        assert!(summary_prompt("sales", false).contains("Objections"));
        assert!(summary_prompt("interview", false).contains("no hiring recommendation"));
        assert!(TEMPLATES.iter().all(|t| t.2.contains("- [ ] owner: task")), "action items render as checkboxes");
        assert!(summary_prompt("general", false).contains("speakers are not labelled"));
        let labelled = summary_prompt("sales", true);
        assert!(labelled.contains("\"Me:\" is the user") && labelled.contains("\"Them:\"") && labelled.contains("Objections"));
        assert!(!labelled.contains("not labelled"));
    }

    #[test]
    fn meetings_stop_after_long_silence_or_at_the_cap() {
        let s = Duration::from_secs;
        assert_eq!(meeting_expired(s(3 * 3600), s(14 * 60), false), None, "talking, under the cap");
        assert_eq!(meeting_expired(s(20 * 60), s(15 * 60), false), Some("Meeting recording stopped after 15 minutes without speech."));
        assert_eq!(meeting_expired(s(4 * 3600), s(0), false), Some("Meeting recording stopped at the 4-hour limit."), "cap even while talking");
        assert_eq!(meeting_expired(s(5 * 3600), s(16 * 60), false), Some("Meeting recording stopped at the 4-hour limit."));
        assert!(meeting_expired(s(20 * 60), s(15 * 60), true).unwrap().contains("while transcription was failing"), "says what happened");
        assert!(meeting_expired(s(4 * 3600), s(0), true).unwrap().contains("Transcription was failing"));
    }

    #[test]
    fn a_provider_outage_does_not_end_a_meeting() {
        assert!(heard_speech(true, false, false), "words");
        assert!(!heard_speech(false, false, true), "sound alone (typing, a fan) while transcription works");
        assert!(heard_speech(false, true, true), "sound while every provider fails keeps it going");
        assert!(!heard_speech(false, true, false), "silence while failing still counts as silence");
    }

    #[test]
    fn aligns_and_keeps_remainder() {
        let (mut a, mut b) = (vec![1000i16, 2000, 30000], vec![1000i16, 2000]);
        assert_eq!(align(&mut a, &mut b, false), (vec![1000, 2000], vec![1000, 2000]));
        assert_eq!((a, b), (vec![30000], vec![]));
        assert_eq!(mix(&[1000, 30000], &[1000, 30000]), vec![2000, i16::MAX]);
        // A dead second input is padded with silence once it lags by more than 2 s.
        let (mut a, mut b) = (vec![7i16; 40_000], Vec::new());
        let (x, y) = align(&mut a, &mut b, false);
        assert_eq!((x.len(), y), (40_000, vec![0; 40_000]));
        let (mut a, mut b) = (Vec::new(), vec![7i16; 40_000]);
        assert_eq!(align(&mut a, &mut b, false).0, vec![0; 40_000]);
        // On stop both remainders are kept, the shorter padded.
        let (mut a, mut b) = (vec![1i16; 3], vec![2i16; 5]);
        assert_eq!(align(&mut a, &mut b, true), (vec![1, 1, 1, 0, 0], vec![2; 5]));
        assert!(a.is_empty() && b.is_empty());
    }

    #[test]
    fn chunks_at_a_shared_pause() {
        let speech = [tone(11.0), quiet(1.0)].concat();
        assert!(chunk_ready(&speech, None));
        assert!(!chunk_ready(&speech[..RATE * 9], None), "under 10 s");
        assert!(!chunk_ready(&tone(12.0), None), "still talking");
        assert!(chunk_ready(&tone(30.0), None), "hard cut");
        // The pause must be in both sources: the other side still talking keeps the chunk open.
        assert!(chunk_ready(&speech, Some(&quiet(12.0))));
        assert!(!chunk_ready(&speech, Some(&tone(12.0))));
        assert!(chunk_ready(&quiet(12.0), Some(&speech)));
        assert!(chunk_ready(&quiet(30.0), Some(&tone(30.0))));
    }

    /// Feeds the timeline 0.5 s ticks the way `run` does and collects the chunks.
    fn feed(t: &mut Timeline, mic: &[i16], other: Option<&[i16]>) -> Vec<Chunk> {
        let tick = RATE / 2;
        let mut chunks = Vec::new();
        for (i, m) in mic.chunks(tick).enumerate() {
            let o = other.map(|o| o.get(i * tick..((i + 1) * tick).min(o.len())).unwrap_or_default().to_vec()).unwrap_or_default();
            chunks.extend(t.push(m.to_vec(), o));
            // Never more than a chunk, a tick and the allowed drift waiting: no whole-meeting audio.
            assert!(t.me.len() <= CHUNK_MAX + tick && t.mic.len() <= RATE * 2 + tick && t.other.len() <= RATE * 2 + tick);
        }
        chunks
    }

    #[test]
    fn timeline_microphone_only_is_unchanged() {
        let mut t = Timeline::new(false);
        let mic = [tone(11.0), quiet(1.0), tone(3.0)].concat();
        let chunks = feed(&mut t, &mic, None);
        assert_eq!(chunks.len(), 1);
        assert!(chunks[0].1.is_none());
        let (rest, them) = t.finish(tone(0.2), Vec::new());
        assert!(them.is_none());
        assert_eq!(chunks[0].0.len() + rest.len(), mic.len() + RATE / 5);
    }

    #[test]
    fn timeline_dead_second_input_does_not_stall() {
        let mut t = Timeline::new(true);
        // Silence is padded in once the lag passes 2 s, so audio is aligned (and pauses are seen)
        // in 2.5 s steps: these pauses span the 12.5 s and 30 s steps.
        let mic = [tone(11.0), quiet(2.0), tone(15.0), quiet(3.0)].concat();
        let chunks = feed(&mut t, &mic, Some(&[]));
        assert_eq!(chunks.len(), 2, "both pauses cut a chunk although the second input sent nothing");
        for (me, them) in &chunks {
            let them = them.as_ref().unwrap();
            assert_eq!(me.len(), them.len());
            assert!(!audio::is_silent(me) && audio::is_silent(them), "only Me speaks");
        }
    }

    #[test]
    fn timeline_keeps_both_sides_on_one_timeline() {
        let mut t = Timeline::new(true);
        let mic = [quiet(5.0), tone(6.0), quiet(1.0)].concat();
        let other = [tone(5.0), quiet(7.0)].concat();
        let chunks = feed(&mut t, &mic, Some(&other));
        assert_eq!(chunks.len(), 1);
        let (me, them) = (&chunks[0].0, chunks[0].1.as_ref().unwrap());
        assert_eq!((&me[..], &them[..]), (&mic[..me.len()], &other[..me.len()]));
        assert!(speech_onset(them) < speech_onset(me));
        // Stopping keeps the last moments of both sources, the shorter padded with silence.
        let (me, them) = t.finish(tone(0.3), tone(1.0));
        let them = them.unwrap();
        assert_eq!(me.len(), them.len());
        assert!(!audio::is_silent(&me) && !audio::is_silent(&them));
        assert_eq!(chunks[0].0.len() + me.len(), mic.len().max(other.len()) + RATE);
    }

    #[test]
    fn timeline_silent_microphone() {
        let mut t = Timeline::new(true);
        let other = [tone(11.0), quiet(1.0)].concat();
        let chunks = feed(&mut t, &quiet(12.0), Some(&other));
        assert_eq!(chunks.len(), 1);
        assert!(audio::is_silent(&chunks[0].0) && !audio::is_silent(chunks[0].1.as_ref().unwrap()), "only Them speaks");
    }

    #[test]
    fn echo() {
        let them = "We can ship the beta next Tuesday, if legal signs off.";
        assert!(is_echo("we can ship the beta next tuesday if legal signs", them), "the mic heard the speakers");
        assert!(is_echo("We can ship the beta next Tuesday, if legal signs off.", them));
        assert!(!is_echo("Tuesday works, but legal needs the contract first.", them), "partial overlap");
        assert!(!is_echo("", them) && !is_echo("Sounds good.", "") && !is_echo("", "") && !is_echo("...", them));
        // Short replies that mirror the other side are kept, even when every word matches.
        for (me, them) in [("Good morning.", "Hi, good morning."), ("Yes.", "Yes, exactly."), ("Thank you.", "Thank you so much."), ("Okay.", "Okay."), ("Sounds good.", "Sounds good.")] {
            assert!(!is_echo(me, them), "{me:?} vs {them:?}");
        }
        // A longer reply that reuses a small part of a longer passage is kept too.
        assert!(!is_echo("That sounds good to me.", "Okay, that sounds good to me, let's move on to pricing then and wrap up."));
        // Repeating one shared word doesn't count as overlap more often than the other side said it.
        assert!(!is_echo("yes yes yes yes yes", "yes I think so"));
    }

    #[test]
    fn turns_in_a_chunk() {
        let s = |t: &str| Some(t.to_string());
        let them = "We can ship the beta next Tuesday.";
        assert_eq!(chunk_turns(s("we can ship the beta next Tuesday"), s(them), true), vec![(Speaker::Them, them.into())], "echo dropped");
        assert_eq!(chunk_turns(s("Great."), s("Hi."), true), vec![(Speaker::Me, "Great.".into()), (Speaker::Them, "Hi.".into())]);
        assert_eq!(chunk_turns(s("Great."), s("Hi."), false), vec![(Speaker::Them, "Hi.".into()), (Speaker::Me, "Great.".into())]);
        // One side failing or silent keeps the other side's text.
        assert_eq!(chunk_turns(None, s("Hi."), false), vec![(Speaker::Them, "Hi.".into())]);
        assert_eq!(chunk_turns(s("Hi."), None, false), vec![(Speaker::Me, "Hi.".into())]);
        assert!(chunk_turns(None, None, true).is_empty());
    }

    #[test]
    fn transcript_format() {
        let mut t = String::new();
        let mut last = None;
        append_turn(&mut t, &mut last, None, " Hello there. ");
        append_turn(&mut t, &mut last, None, "Para one.\n\nPara two.");
        assert_eq!(t, "Hello there. Para one.\n\nPara two.", "unlabelled text is joined as before");

        let (mut t, mut last) = (String::new(), None);
        append_turn(&mut t, &mut last, Some(Speaker::Me), "Hi, can you hear me?");
        append_turn(&mut t, &mut last, Some(Speaker::Me), "Hello?");
        append_turn(&mut t, &mut last, Some(Speaker::Them), "Yes.\n\nThem: we can.");
        append_turn(&mut t, &mut last, Some(Speaker::Them), "  ");
        append_turn(&mut t, &mut last, Some(Speaker::Me), "Great.");
        assert_eq!(t, "Me: Hi, can you hear me? Hello?\n\nThem: Yes. Them: we can.\n\nMe: Great.");
        assert!(t.lines().filter(|l| !l.is_empty()).all(|l| l.starts_with("Me: ") || l.starts_with("Them: ")));
    }
}
