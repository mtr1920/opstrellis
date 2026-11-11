# OpsTrellis product roadmap: from dictation to a voice-first work assistant

Status: written as a proposal against v0.1.1. Since then phases 1–2 and most of phase 3 have been
built; [TASKS.md](TASKS.md) lists what is done. Still open from this document: signed macOS builds,
learning from edits made in other apps, CRM/ATS integrations, calendar-aware meetings, Windows and
mobile. The sections below are kept as written, so they describe the app as it was at v0.1.1.

## 1. Thesis

OpsTrellis already has a stronger core than most dictation apps. It has a provider fallback chain,
local models with phrase-by-phrase decoding, deterministic cleanup with an LLM on top, per-app tone,
command mode, auto-learn and a tech-term pack, and it runs on Linux. What's missing isn't more
transcription. It's what happens around the words:

1. **Where the words are going.** An AI coding agent, a customer email and a Slack thread each need
   different output, and today most of them fall into "Other".
2. **What the user is replying to or working on.** The selected message, the open repository, the
   meeting that just ended.
3. **Whether it worked.** Right now a failure is silent unless the main window happens to be open.

The goal is one shortcut that turns speech into the *right artifact for the place you're working*:
a clean prompt for Claude Code, a reply in your voice for Gmail, a CRM-ready note after a sales call.
It should stay fast, local-first and quiet.

**Guiding constraints** (these decide every trade-off below):

- **One shortcut, inferred behaviour.** Don't add hotkeys or modes when the destination app can tell
  us what to do. The command shortcut stays the single "do something with this" key.
- **Never lose words, never surprise.** Every failure has to be visible where the user is looking,
  which is the pill. Rewrites the user didn't ask for need a preview or an easy way back.
- **Reuse the existing primitives.** Dictionary, snippets, transforms, style categories and the
  pipeline stages cover most of the new features. Extend them; don't add parallel systems.
- **Local-first by default.** New context sources (selection, repository index) stay on the device.
  Only the text that is already sent today goes to the user's LLM.
- **Evolve Quiet Glass, don't replace it.** The no-gradient, one-accent rules are enforced by
  `e2e/design_check.mjs` and came from user feedback. Make it distinctive through behaviour, motion
  and typography instead.

---

## 2. What exists today: inventory and assessment

| Area | Implemented (where) | Assessment |
|---|---|---|
| Hotkeys | Hold = push-to-talk, tap = hands-free, Esc cancel, command, paste-last; Wayland CLI flags (`hotkeys.rs`, `pipeline.rs:68-138`) | Solid, with carefully handled timing edge cases. There's no auto-stop in hands-free mode and no elapsed timer. |
| STT | 8 provider kinds, streaming for 3 of them, fallback chain with buffered audio, local phrase segmentation (`stt/`, `pipeline.rs:211-258, 600-644`) | A real differentiator, and well tested. |
| Cleanup | Rules (fillers, voice commands, dictionary, snippets, tech terms), then the LLM, then the rules again (`text.rs`, `pipeline.rs:382-428`) | Good layering and output guards (`llm.rs:773`). The LLM prompt is generic: AI-prompt destinations get the same treatment as documents. |
| Style | 4 categories × 4 tones, chosen by matching the app name or window title (`context.rs:929-972`) | `ai_prompts` is detected but mapped to the "other" style (`context.rs:906-912`). Substring rules misfire: "word" matches "password" and "WordPress", "signal" and "teams" match browser tab titles. Users can't override the category. |
| Auto transform | Per category, a second LLM call after cleanup (`pipeline.rs:432-446`) | Two LLM round trips in series. Skipped in all terminals, which also blocks Claude Code, Codex and Aider, the most important developer destinations. |
| Command mode | Copies the selection, matches a transform by name or uses free text, replaces the selection (`pipeline.rs:448-464`) | Powerful but blind: no preview, no output guard, the result always replaces the selection, and the selection is only copied after transcription finishes. |
| Dictionary / auto-learn | Hints ranked by use, learning from History edits, Undo (`learn.rs`, `db.rs:322-452`) | Excellent and careful. Mostly invisible, though: it's off by default and the toggle is buried in Style. |
| Snippets | Static trigger → expansion (`text.rs`) | No variables (date, clipboard, selection). |
| Notetaker | Mic plus an optional second input mixed together, 30 s chunks, one summary prompt (`notetaker.rs`) | Speakers are mixed together, there's only one summary format, and no follow-up actions. Meetings are told apart from notes by a title prefix (`Notetaker.tsx:19`). |
| History | Day groups, search, correct, copy, delete (`Home.tsx`) | Shows only the final text, although `raw_text` is stored. Loads a fixed 200 rows. Failed AI cleanup can't be retried. |
| Insights | WPM, streaks, heatmap, categories (`Insights.tsx`) | Fine as is. It isn't the reason people stay. |
| Overlay pill | Dot, waveform, partial text, CMD badge, "Transcribing…" (`Overlay.tsx`) | No success state, no error state, and the same label covers transcription and AI polish. It hides on idle, so errors only reach the main window toast (`App.tsx:51`), which is usually hidden in the tray. |
| Onboarding | Welcome/permissions, provider, try it (`Onboarding.tsx`) | Never introduces AI polish, command mode or snippets. No mic level check. |
| Tray | Open, Start/stop, Quit (`lib.rs:525-528`) | Minimal. |
| Privacy | Keychain, retention, metadata-only sync | Strong. One bug: paste-last reads from history, so it fails in privacy mode (`db.rs:263`, retention 0 never writes history). |
| Cloud | Gateway, device sign-in, Stripe, admin (`server/`) | Ready to host team features later. No org or team model yet. |

---

## 3. Roadmap overview

Effort: **S** ≤ 2 days · **M** ≤ 1 week · **L** 2–4 weeks (one engineer who knows the codebase).

| # | Item | Phase | Impact | Effort |
|---|---|---|---|---|
| 1.1 | Pill feedback: stages, success, errors | 1 | Very high | S |
| 1.2 | One LLM call for cleanup + auto transform; tighter timeout | 1 | High | S |
| 1.3 | App rules: user overrides + "AI prompts" as a 5th style | 1 | High | M |
| 1.4 | AI agents in terminals are prompts, not shell commands | 1 | High (devs) | S |
| 1.5 | Paste-last in privacy mode; hands-free auto-stop and timer | 1 | Medium | S |
| 1.6 | History: original vs polished, retry AI, filters, paging | 1 | Medium | S–M |
| 1.7 | Navigation 9 → 6; AI settings move to Settings | 1 | Medium | M |
| 1.8 | Onboarding that shows the product's value | 1 | High | M |
| 1.9 | Meetings stored as a note kind | 1 | Low (correctness) | S |
| 1.10 | Richer tray menu | 1 | Low–medium | S |
| 2.1 | Prompt mode for AI coding assistants | 2 | Very high (devs) | M |
| 2.2 | Review card before replacing or inserting AI output | 2 | High | M |
| 2.3 | Snippet variables | 2 | High (sales, recruiting) | S |
| 2.4 | Selection as context ("select, then speak") | 2 | High | S–M |
| 2.5 | Meeting templates + follow-up actions | 2 | High (sales, recruiting, managers) | M |
| 2.6 | "Me / Them" speaker split in Notetaker | 2 | Medium–high | M |
| 2.7 | "How I write" notes per category | 2 | Medium | S |
| 3.1 | Project-aware vocabulary and `@file` mentions | 3 | Very high (devs) | L |
| 3.2 | Actions: transforms with an output target | 3 | High | M |
| 3.3 | Developer actions: commit message, ticket, ask about code | 3 | Medium–high | M |
| 3.4 | Team library on OpsTrellis Cloud | 3 | High (teams, paid) | L |
| 3.5 | Bundled local LLM for fully offline polish | 3 | Medium | M–L |
| 4.x | Accessibility-API context, learning from edits in other apps, MCP bridge, CRM/ATS, Windows, calendar | 4 | Exploratory | L+ |

Order of work: Phase 1 makes what exists trustworthy. 2.1, 2.2 and 2.4 make OpsTrellis an assistant
rather than a transcriber. 3.1 is the developer moat. 3.4 is the business moat.

---

## 4. Phase 1: polish and fix what exists (2–4 weeks)

### 1.1 The pill tells the truth: stages, success, errors
- **Problem.** The pill has one processing label ("Transcribing…") for both transcription and AI
  polish, and disappears on idle. Failures go to `dictation-error`, which only the main window
  listens to (`App.tsx:51`), and the main window normally lives hidden in the tray. "No speech
  detected" (`pipeline.rs:286-292`) and empty transcripts (`pipeline.rs:318`) finish silently, and a
  failed insert leaves the text on the clipboard without saying so where the user is looking. Users
  conclude "it didn't work" and dictate again.
- **Experience.** Listening → Transcribing → Polishing → **✓ Inserted · 0.6 s** (about 700 ms, then
  the pill collapses). Problems stay in the pill for 3 s with the reason and what happened to the
  text:
  - "No speech heard. Check your mic."
  - "AI polish unavailable, inserted plain text."
  - "Couldn't paste. Copied, press Ctrl+V."
- **Build.**
  - Add a `stage` field to `DictationState` (`"transcribing" | "polishing" | "inserting"`). Emit it
    from `process()` before `clean_up` / `run_command` and before insertion.
  - Add a `dictation-outcome {kind: ok|empty|silent|fallback|error, message, latencyMs}` event,
    emitted at every exit of `process()`. It replaces the scattered `dictation-error` emits; keep
    `dictation-error` for the main window.
  - In `emit_state`, delay hiding the overlay while an outcome is showing: the overlay sets a timer
    and Rust hides it after an `overlay-idle` ack, or simply after a fixed 1.2 s or 3 s.
  - `Overlay.tsx` renders the extra states. No new windows.
- **Mic-ready cue (clipped first words).** The start beep and the red dot fire on key press
  (`Overlay.tsx` reacts to the `recording` state, emitted in `on_press`). The mic, though, can take
  seconds to open on Bluetooth or PulseAudio. The ready signal is awaited in `pipeline.rs:197-202`,
  but nothing is emitted when it arrives. Users start speaking at the beep, and the first words are
  lost. That's the most common latency complaint about Superwhisper too.
  - Emit `mic-ready` from that thread.
  - Until it arrives, the pill shows a hollow, non-pulsing dot ("Opening mic…").
  - Play the start cue on `mic-ready`, not on press.
  - An always-open pre-roll buffer would also fix this, but it contradicts the "mic open only while
    recording" promise, so it isn't proposed.
- **Undo hint.** A dictation is a single paste, so the target app's own Ctrl/⌘+Z undoes it in one
  step. Say so in the success state ("Inserted · Ctrl+Z to undo") instead of building an undo
  system. No product does undo well today.
- **Why.** It's the cheapest change that builds trust, and it's a prerequisite for every AI feature
  below. Competitors' most common complaint is silent failure and not knowing whether it's still
  working.

### 1.2 One LLM round trip, with a latency budget
- **Problem.** With an auto transform on, every dictation makes two LLM calls in series:
  `clean_up`, then `auto_transform` (`pipeline.rs:406, 438`). The LLM timeout is 30 s
  (`llm.rs:703, 722`), which is far too long for dictation: a stalled provider leaves the user staring
  at the pill.
- **Experience.** Auto-transformed dictations arrive about as fast as plain AI cleanup. When the
  provider is slow, plain cleaned text arrives after a few seconds, marked "AI polish skipped".
- **Build.**
  - Fold the transform's instruction into `cleanup_prompt` as an extra rule ("Then apply: <prompt>").
    `auto_transform` stays as the fallback path for the snippet check.
  - Relax `accept_cleanup` for this case. It uses the same 3× + 200 ceiling as `auto_transform`
    today; a transform such as "Bullet points" legitimately changes the length.
  - Give `Client::complete` a timeout parameter: about 8 s for dictation cleanup and 30 s for
    commands and meeting summaries.
  - Log the LLM time separately; it's already part of `cleanup_ms`.
- **Why.** Latency is the main reason people turn AI cleanup off. Fixing it is about 30 lines.

### 1.3 App rules: let users correct the category, and give AI prompts their own style
- **Problem.** Categorization is a hard-coded substring list (`context.rs:929-966`): "word" matches
  "password" and "WordPress", and "signal", "teams" and "pages" match arbitrary browser tab titles.
  `ai_prompts` is detected but shares the "Other" tone and auto transform with documents
  (`context.rs:906-912`). The Style page even says "Other: … AI chats". Users can't fix any of this.
- **Experience.**
  - Style gets a fifth tab, **AI prompts** (ChatGPT, Claude, Cursor, Claude Code…), with its own
    tone and auto action.
  - A compact **Apps** list under Style shows every app you've dictated into (from history), its
    detected category and a dropdown to change it. "Apps" here means the category assignment shown
    per app; it's not a separate page.
  - Tighten the default rules: match whole words, and match the browser title only for the web-app
    rules.
- **Build.**
  - Add `Settings.app_categories: HashMap<String, String>`, keyed by lowercased app name or, for
    browsers, by a title needle the user picks from the row.
  - `context::classify` checks the overrides first.
  - Add an `"ai"` style category and `style_category() → "ai"` for `ai_prompts`.
  - The list comes from `SELECT DISTINCT app_name, category FROM history`, so no new tracking.
  - Migration: copy the "other" tone to "ai" on first load.
- **Why.** Every context-aware feature (tone, prompt mode, auto actions) is only as good as this
  mapping. Today a wrong guess can't be fixed.

### 1.4 AI agents in a terminal are prompts, not shell commands
- **Problem.** Claude Code, Codex CLI, Gemini CLI and Aider run inside terminals. `is_terminal`
  blocks auto transforms (`pipeline.rs:434`) and applies command-line conventions, so prompt
  dictation into these agents gets the most conservative treatment, even though it's the developer
  use case that matters most.
- **Experience.** Dictating into a Claude Code session gets prompt cleanup (2.1). Dictating into a
  plain shell keeps today's verbatim command behaviour, with no tone and no transform.
- **Build.**
  - In `classify`, when `is_terminal` and the window title matches an agent needle (`claude`,
    `codex`, `aider`, `gemini`, `opencode`, `amp`…), set `category = "ai_prompts"` and a new
    `is_agent_prompt` flag.
  - Insertion still uses terminal paste.
  - The auto-transform skip becomes `is_terminal && !is_agent_prompt`.
  - The 1.3 overrides cover terminals whose title doesn't show the agent.
- **Why.** It's a one-day change, and it unlocks the flagship developer feature for the tools
  developers are adopting fastest.

### 1.5 Small reliability fixes
- **Paste-last in privacy mode.** With retention set to "off", history is never written
  (`db.rs:223`), so `last_text()` returns nothing and paste-last says "Nothing dictated yet". Keep the
  last inserted text in memory (`App.last_text: Mutex<Option<String>>`) and fall back to the
  database. **S.**
- **Hands-free safety.** Show an elapsed timer in the pill while hands-free is on. Auto-stop after a
  configurable silence (default 60 s) or a hard cap (default 30 min, with a pill warning 1 minute
  before), using `audio::is_silent` on the buffer tail, which is already computed for local
  segments. This protects people who forget the mic is on. The cap should stay generous: session
  caps are a common complaint about competitors (Apple stops after 30 s of silence, Wispr Flow
  after 20 min), and local phrase segmentation already makes long dictations cheap. **S.**
- **Command mode latency.** Copy the selection in parallel with transcription (spawn it at `finish()`
  time) instead of after it (`pipeline.rs:451`). **S.**

### 1.6 History becomes a working tool
- **Problem.** History shows only the final text, though `raw_text` is stored (`db.rs:166`). It loads
  a fixed 200 rows (`api.ts:98`). A dictation whose AI step failed can't be re-polished, and there's
  no way to tell cleanup changes from recognition errors.
- **Experience.**
  - Each row expands to **Original / Polished**. Words that changed are lightly marked, using the
    word diff `learn.rs` already computes.
  - Row actions: **Insert again**, **Copy original**, **Polish again** (re-runs cleanup and the auto
    action from `raw_text`; the audio isn't stored, so recognition can't be redone).
  - Filter chips: app category · Command · Errors.
  - Infinite scroll in pages of 100.
- **Build.** All of it is UI plus one command, `repolish_history {id}`, which runs `clean_up` on
  `raw_text`. Reuse `text::process` and `llm::cleanup_prompt` with the stored `app_name` /
  `category`. **S–M.**
- **Why.** It turns History into the place where users fix and reuse text, and it feeds auto-learn,
  which only learns from History edits.

### 1.7 Navigation 9 → 6, with consistent homes for settings
- **Problem.** Nine top-level pages. Dictionary, Snippets and Transforms are three versions of the
  same thing (your vocabulary and phrases). Scratchpad and Notetaker both store notes. AI provider
  setup lives in **Style** (`Style.tsx:136`) while speech provider setup lives in **Settings**,
  so users look in the wrong place.
- **Proposed IA:**

  | Nav | Contains | Was |
  |---|---|---|
  | **Home** | Shortcut hint, today's stats, History | Home |
  | **Notes** | Tabs: Notes · Meetings (record button in the header) | Scratchpad + Notetaker |
  | **Library** | Tabs: Dictionary · Snippets · Actions | Dictionary + Snippets + Transforms |
  | **Style** | Per-category tone, auto action, Apps list (1.3), cleanup toggles | Style, minus the AI provider form |
  | **Insights** | Unchanged | Insights |
  | **Settings** | Adds an **AI** section (provider, model, key, Test) next to Transcription | Settings |

- **Build.**
  - Page components are already self-contained, so this is a tab wrapper plus moving `LlmSection`
    into Settings.
  - Add a `test_llm` command mirroring `test_stt`, which sends the sample cleanup prompt.
  - Update `PAGES` in `App.tsx`, `design_check.mjs` and the UI e2e page list.
  - Rename **Transforms → Actions** in the UI only. The table, the IPC and "say the name in command
    mode" stay the same. "Actions" also covers 3.2.
- **Why.** Less to scan, and every concept has one home. Nothing is removed.

### 1.8 Onboarding that shows the value
- **Problem.** Onboarding sets up speech and then stops (`Onboarding.tsx`). AI polish (off by
  default), command mode, snippets and auto-learn, the features that make OpsTrellis more than
  dictation, are never mentioned. There's no mic check, so a wrong default device shows up as
  "nothing happens".
- **Experience** (4 steps, each one skippable):
  1. **Welcome + permissions.** As now, plus a live mic meter using the `audio-level` events and a
     device picker.
  2. **Speech.** As now.
  3. **"What do you write most?"** Multi-select: Prompts & code · Email & messages · Meeting notes ·
     Documents. This sets defaults:
     - Prompts & code: AI-prompt tone, tech terms, prompt mode.
     - Email & messages: formal email, casual work.
     - Meeting notes: the Notetaker template default.
     It offers **Turn on AI polish** (Cloud sign-in or paste a key, with a Test button).
  4. **Try it.** Dictate into the box, then *select your sentence, hold the command key and say
     "make it more formal"*. The command-mode lesson happens in place.
- **Build.** New step components reuse `ChooseProvider`, `LlmSection` (after 1.7) and the
  `dictation-result` listener. The persona choice only writes existing settings fields, so there's
  no new state. **M.**
- **Why.** Most users never discover command mode. Showing it once during setup is the cheapest
  activation lever available.

### 1.9 Meetings are a note kind, not a title prefix
- `Notetaker.tsx:19` filters notes by `title.startsWith("Meeting")`. So a Scratchpad note titled
  "Meeting prep" shows up as a meeting, and every meeting also clutters Scratchpad.
- Fix: add a `notes.kind TEXT NOT NULL DEFAULT 'note'` column, backfill it from the existing prefix,
  have `finish_note` write `'meeting'`, and add a `kind` filter to `list_notes`. **S.**

### 1.10 A tray menu worth opening
- Add:
  - **Copy last transcript.**
  - **AI polish** (checkbox).
  - **Language** submenu (Auto plus recently used languages).
  - **Pause shortcuts** (for games, screen sharing or presenting).
  - **Start meeting recording.**
- The `lib.rs:525` menu builder handles all of this; items toggle the existing settings. **S.**

---

## 5. Phase 2: high-impact productivity (4–8 weeks)

### 2.1 Prompt mode for AI coding assistants (developer flagship)
- **Problem.** Developers dictate long, rambling instructions to Claude Code, Cursor and ChatGPT.
  Today they get either generic grammar cleanup or the "Prompt engineer" transform, which rewrites
  heavily and can drift from what they meant. Neither handles code-speak: "the process function in
  pipeline dot r s", "snake case user id", "back tick npm test back tick".
- **Experience.** In the **AI prompts** category (1.3/1.4) the cleanup prompt changes behaviour, with
  two levels chosen per category in Style:
  - **Clean** (default). Keep the user's words and imperative voice, and fix grammar.
    Mark identifiers, file names, commands and paths with backticks. Turn spoken symbols into
    characters ("dot r s" → `.rs`, "dash dash watch" → `--watch`). Keep sentences short. Never answer
    or execute.
  - **Structured.** For dictations over about 60 words: the same, plus light sections only where the
    content has them (**Goal**, **Context**, **Constraints**, **Done when**), and numbered steps
    when the user lists steps. No invented requirements.
  - The "Prompt engineer" action remains for an explicit, heavier rewrite in command mode.
- **Build.**
  - Add a branch in `llm::cleanup_prompt` for `ctx.category == "ai_prompts"` with the rules above and
    3 short examples (few-shot examples matter for the backtick rules).
  - Add the level to settings as `prompt_style: "clean" | "structured"`.
  - Add a deterministic pre-pass in `text.rs` for spoken symbols when `technical`: "dot", "dash
    dash", "slash", "underscore", "back tick", "open paren". This is opt-out, since the vocabulary
    rules already handle code contexts.
  - E2E: a new scenario against `llm_standin.py` asserting the prompt text and that a terminal agent
    title (1.4) selects it.
- **Why.** Developers are the most frequent, highest-volume dictation users. A prompt that comes out
  ready to send, with correct identifiers, is the clearest "this isn't just speech-to-text" moment.

### 2.2 Review card: see AI output before it replaces your text
- **Problem.** Command mode replaces the selection with whatever the LLM returns, with no output
  guard (`pipeline.rs:459-463`). For a 3-paragraph email, a bad rewrite costs more time than it
  saves, so cautious users stop using command mode.
- **Experience.** When command mode or an auto action changes text substantially (more than about
  40% different, or output over 280 characters), the pill expands into a card about 400 px wide
  showing the result. Keys:
  - **Enter** inserts.
  - **C** copies.
  - **R** retries with the same instruction.
  - **Esc** discards.
  - Holding the command key again refines the card's text ("shorter", "less formal").

  Setting: *Review AI rewrites: Always · When long (default) · Never*.
- **Build.**
  - The overlay window grows: `set_size` happens in Rust when the card state is emitted.
  - The pill must still not steal focus, so register Enter/Esc/C/R as global shortcuts only while
    the card is visible. That's the same pattern as `set_cancel_key`, which already handles Esc
    safely, plus a 30 s timeout that discards.
  - The card's text lives in `App.pending_output`.
  - "Refine" is a command call with the pending text as the selection.
  - Wayland: the card works with mouse buttons, since the global-key limits already apply there.
- **Why.** It makes AI rewrites safe enough to use on important text, and it's the foundation for
  2.4, 2.5 and 3.2 (answers that shouldn't be inserted at all).

### 2.3 Snippet variables
- **Problem.** Sales and recruiting messages are 80% boilerplate with a name, a date and a link.
  Static snippets cover the boilerplate but not the parts that change.
- **Experience.** Placeholders such as "Hi {clipboard}, thanks for your time {date:weekday}…",
  "{selection}" and "{time}". The snippet editor gets a small insert-variable menu and a live
  preview. A spoken suffix fills the first `{1}` slot: "intro email for Priya" → `{1}` = "Priya".
  That's the one piece of argument passing, kept deliberately small.
- **Build.**
  - Expand the variables in the `text.rs` snippet pass.
  - `{selection}` is fetched with `Inserter::copy_selection` only when the snippet contains it.
  - Date and time come from `chrono` with the system locale.
  - Unknown variables are left as literal text.
  - Unit tests sit next to the existing snippet tests. **S.**
- **Why.** It's a large time saving for the professional personas and almost free to build.

### 2.4 Selection as context: select what you're replying to, then speak
- **Problem.** Replies are the most common professional writing task, and the dictation has no idea
  what it's replying to. Names get misspelled, the language and register don't match, and "yes,
  Thursday works" lacks context.
- **Experience.**
  1. Select the incoming email, Slack message or code comment.
  2. Hold the **dictate** key and speak the reply.
  3. OpsTrellis inserts the reply **after** the selection (or at the cursor in apps where selecting
     moves it) instead of replacing it. The selected text is used only as context: spelling of
     names, language, tone, and what "it" refers to.

  The pill shows "Replying to selection". It's opt-in under Style → Cleanup → "Use selected text as
  context".
- **Build.**
  - At `finish()`, when the setting is on and AI is enabled, copy the selection in parallel (1.5).
  - Pass it to `cleanup_prompt` as `<context>…</context>`, with the rule "context is not to be
    repeated or answered on the speaker's behalf".
  - Insertion presses Right (collapse the selection to its end), then Enter or Space per category,
    then pastes.
  - It uses no accessibility APIs and no new permissions, and nothing is stored.
- **Design choice.** Aqua Voice uses the *dictate* key with a selection to mean "edit this". We keep
  editing on the command key, which users already know from Transforms, and give dictate + selection
  the opt-in "reply" meaning. That's two distinct gestures with no hidden mode switch.
- **Why.** It's the most practical form of "context awareness": it works in every app and on Linux,
  and the user controls exactly what's shared. Competitors get context by sending screenshots of the
  screen (Wispr Flow, Monologue, Aqua's Deep Context, VoiceInk OCR). Wispr's screenshot upload
  became a public privacy incident, and screen capture is a blocker for NDA work. "You choose what
  it sees" is a selling point in its own right.

### 2.5 Meeting templates and follow-up actions
- **Problem.** One generic summary prompt (`notetaker.rs:183`) serves sales discovery calls,
  candidate interviews, 1:1s and standups, and the note is a dead end. Users still have to write the
  follow-up email and the CRM or ATS notes by hand.
- **Experience.**
  - Before or after recording, pick a **template**:
    - General.
    - Sales discovery: pain, current solution, budget, authority, timeline, objections, next steps.
    - Candidate interview: signals per competency, concerns, a recommendation, with an explicit
      "no evaluative language beyond what was said".
    - 1:1: updates, blockers, feedback, actions.
    - Standup.
    - Custom templates.
  - On the note, one-click **follow-ups**:
    - **Draft follow-up email** (opens a review card, 2.2, using the email tone).
    - **Copy action items** as a checklist.
    - **Copy CRM note** (a compact field-per-line block that pastes cleanly into HubSpot, Salesforce
      or Greenhouse note fields).
- **Build.**
  - Templates are transforms with `kind = 'meeting'`: add a `kind` column to `transforms`, seed the
    built-ins, and show them under Library → Actions. `finish_note` takes a template id.
  - Follow-ups are command calls over the note body with fixed built-in prompts.
  - Re-running a different template on a saved meeting is just another LLM call over the stored
    transcript.
  - No CRM APIs yet (Phase 4).
  - **Consent.** Otter, Fireflies and Granola all face recording-consent lawsuits. Add a one-line
    "Recording notice" snippet ("I'm taking AI notes on this call, is that OK?") offered when a
    recording starts, and keep audio unstored as today. **M.**
- **Why.** It turns Notetaker from a recorder into the step that removes the post-meeting admin, which
  is where sales, recruiting and managers lose most time.

### 2.6 "Me / Them" speaker split
- **Problem.** The mic and system audio are mixed into one stream before transcription
  (`notetaker.rs:82-96`), so the transcript and summary can't tell the user apart from the customer
  or candidate. For sales and interviews that's the most important distinction.
- **Experience.** Transcripts read `Me: …` / `Them: …` when a second input is recorded. Summaries can
  attribute commitments ("Customer agreed to send the security questionnaire").
- **Build.**
  - Keep the two buffers separate instead of calling `mix`. Chunk on the shared timeline (cut both
    at the same sample index, using the existing pause detection on the sum), transcribe both chunks,
    and drop near-silent ones (`is_silent`).
  - Interleave by chunk with speaker labels.
  - That doubles STT calls only for the non-silent side and only when a second device is chosen.
  - Echo: when the user isn't on headphones, the mic hears the speakers. Suppress mic-chunk text that
    is mostly duplicated in the system chunk (word overlap above 70%).
  - Real diarization (several remote speakers) is deferred; Deepgram and AssemblyAI support it in
    batch, and that can be an option later. **M.**
- **Why.** It's the cheapest meaningful diarization possible, because the two sources are already
  physically separate.

### 2.7 "How I write" notes per category
- **Problem.** Tones are four presets. People have specific habits: a sign-off ("Cheers, M."),
  British spelling, never using exclamation marks, "we" rather than "I" in customer emails. Auto-learn
  covers words but not style.
- **Experience.** Each Style category gets an optional short text field: *"How you write here"* (up to
  300 characters), with 3 example chips to start from. It's applied to AI cleanup for that category.
- **Build.**
  - Add `Settings.style_notes: HashMap<String, String>`.
  - Append it to `cleanup_prompt` as a rule.
  - No learning, no ML, user-owned text. **S.**
- **Why.** It gets most of the benefit of "learns your voice" with none of the opacity. It can later
  be pre-filled from History (Phase 4) if users ask.

---

## 6. Phase 3: advanced AI and differentiated workflows (2–3 months)

### 3.1 Project-aware vocabulary and `@file` mentions (developer moat)
- **Problem.** Recognition of a repository's own words (`UserSessionStore`, `pipeline.rs`,
  `feature/nest_dev`, internal service names) is the main accuracy complaint from developers, and AI
  coding tools want `@path/to/file` mentions that nobody can dictate.
- **Experience.**
  - In Settings → **Workspaces**, add a folder, or let OpsTrellis offer the project it sees in the
    VS Code, Cursor, Zed or JetBrains window title.
  - While dictating into that editor or an agent terminal opened on it, the repo's file names and
    top identifiers become hints, and spoken forms resolve: "user session store" → `UserSessionStore`,
    "at pipeline dot r s" → `@desktop/src-tauri/src/pipeline.rs` in the AI-prompts category.
  - A workspace chip in the pill shows that it's active.
- **Build.**
  - A new `workspace.rs`. Index with `git ls-files` (respects `.gitignore`), capped at about 20k
    paths.
  - Extract identifiers with one regex per language family (`fn|def|class|struct|interface|type|const
    …`), capped at about 3k, ranked by frequency.
  - Generate spoken forms by splitting camelCase and snake_case: "user session store". Then reuse the
    `terms.rs` alias-regex machinery (`fix(text, technical, keep)`) with a per-workspace pack built
    in memory.
  - Hints: merge the top N workspace terms into `vocabulary()`, keeping the provider limits.
  - Match the workspace to the editor title by folder name.
  - Re-index on demand and on a file-watch debounce. Everything stays local.
- **Why, and how it differs from competitors.**
  - Wispr Flow (variable recognition, file tagging) and Willow already do a version of this, but only
    inside Cursor and Windsurf, and by reading the editor or screen.
  - Claude Code's own `/voice` adds only the project and branch names as hints, and needs a Claude.ai
    account.
  - The gap is a **repo-derived, local index that works everywhere the developer types**: VS Code,
    JetBrains, any terminal, Claude Code, Codex, a browser chat. It reads no screen, and the user
    sees exactly which folders are indexed.

  Developers are the persona that dictates most, so this is used many times a day.

### 3.2 Actions: transforms with an output target
- **Problem.** Transforms can only replace the selection. Many useful voice actions shouldn't
  replace anything: "summarize this thread" (wants a preview), "translate to German" (insert after),
  "turn this into a Jira ticket" (copy), "save to notes".
- **Experience.** Each action has an **output** setting:
  - Replace selection (today's behaviour).
  - Insert after.
  - Show in card (2.2).
  - Copy.
  - New note.

  Built-ins gain: *Reply*, *Summarize*, *Translate*, *Ticket*, *Explain*. Users keep saying the
  action name in command mode, as today. Optionally, an action can have its own shortcut, for the
  two or three actions people use constantly.
- **Build.**
  - Add `transforms.output TEXT DEFAULT 'replace'` and an optional `hotkey`.
  - `run_command` returns an output target, and the insert step switches on it.
  - Per-action shortcuts register through the existing `hotkeys::register`, with an action id
    payload. **M.**
- **Why.** It turns the command key into a general "do something with this" key without new
  concepts: it's still "name an action".

### 3.3 Developer actions
Built on 3.1 and 3.2. Each is a built-in action with a fixed prompt and context source:
- **Commit message:** context is `git diff --staged` of the active workspace; output goes to the
  card, then is inserted. The user says "commit message" in command mode while in the terminal.
- **Ticket / bug report:** turns a rambling description into Title / Steps / Expected / Actual /
  Notes, with the output copied.
- **Ask about selection:** "what does this do?" or "why might this fail?" over selected code,
  answered in the card and never inserted.
- **Why.** These are the daily micro-tasks where developers already reach for an LLM. Doing them by
  voice from where they are avoids a context switch. **M** in total, given 3.1 and 3.2.

### 3.4 Team library on OpsTrellis Cloud
- **Problem.** Sales and recruiting teams want shared terminology (product names, competitor names),
  approved snippets and standard meeting templates. Today every install is an island. The README
  lists this as not implemented.
- **Experience.** Signed-in users in an organization see **Team** entries in Library (dictionary,
  snippets, actions, meeting templates), read-only for members and editable by admins, with personal
  entries winning on conflict.
- **Build.**
  - Server: `orgs` and `org_members` tables, and an `org_library` table with JSON entries and a
    version. `GET /api/library?since=version`.
  - Desktop: sync into the local tables with `source = 'team'` (the dictionary already has a
    `source` column) on the existing `cloud::spawn_sync_loop`.
  - Admin UI goes in `server/web`.
  - Billing: a Team plan per seat in Stripe, reusing the existing checkout. **L.**
- **Why.** It's the natural paid upgrade, and it makes OpsTrellis stick for whole teams instead of
  individuals.

### 3.5 Fully offline polish with a bundled local LLM
- **Problem.** AI cleanup needs a cloud LLM or a separately installed Ollama. Privacy-sensitive users,
  such as legal, recruiting with candidate data and regulated sales, can't use AI polish at all.
- **Experience.** Settings → AI → **On this computer**: a one-click download of a small instruction
  model (about 1–2 GB, Q4) next to the STT models. It's used for cleanup and short commands; meeting
  summaries suggest a cloud model when the transcript is long.
- **Build.** Either ship `llama.cpp` server as a sidecar, or reuse the OpenAI-compatible client
  against a bundled `llama-server` on localhost. Download through the same model downloader. Bench
  latency on the CI-like 4-vCPU box before committing. It might only be viable on Apple silicon, in
  which case ship it there first. **M–L.**
- **Why.** "Private by default, including AI" is a clear position no major competitor holds on Linux
  and macOS.

---

## 7. Phase 4: longer-term bets (explore, prototype, measure)

| Idea | Why it's interesting | Why not now |
|---|---|---|
| **Accessibility-API context** (macOS AX, Linux AT-SPI): read the focused field's text around the cursor | Context without selecting first; spacing and capitalization fitted to the surrounding text | Per-app quirks, macOS permission friction, unreliable AT-SPI coverage. 2.4 covers most of the value. |
| **Learn from edits in other apps** | The real auto-learn loop (the README already lists it as not implemented) | Needs the same AX/AT-SPI access plus a diff of the field after insert. Do it after AX context exists. |
| **MCP bridge**: expose History, Notes, meeting action items and the dictionary to agents (Claude Code, Cursor) as an MCP server | "Claude, use the action items from my 2 pm call", and agents could add dictionary terms. Competitors are starting here: Superwhisper shipped a CLI and an MCP server over dictation history (July 2026), and Spokenly's MCP lets an agent ask the user for voice input. | Needs a security model (local socket, per-client consent). Valuable once 2.5 data exists. Start with a read-only stdio server over Notes and meetings: it's the first item to pull forward if agent usage grows. |
| **CRM / ATS push** (HubSpot, Salesforce, Greenhouse, Lever) | Closes the loop from meeting → CRM without copy and paste | OAuth apps, per-vendor field mapping and support burden. Validate with "Copy CRM note" (2.5) usage first. |
| **Windows** | The largest market for sales and recruiting | The Rust crates used (cpal, enigo, arboard, active-win, keyring) support Windows, but insertion, hotkeys and the overlay need dedicated work and CI. A strategic decision rather than a feature. |
| **Calendar-aware Notetaker**: suggest recording when a calendar meeting starts, and name the note after the event | Removes "forgot to record" | Calendar OAuth. The tray "Start meeting recording" (1.10) covers the basic need first. |
| **Whisper mode / low-voice gain** | Open offices | Needs gain normalization tuned per provider. Measure demand first. |
| **Mobile companion** | Capture on the go | A separate product surface. |

---

## 8. UI/UX modernization: "Quiet Glass, Live"

The current visual system is clean, but static and anonymous: it could be any settings app. The
aim is a recognisable identity that stays within the enforced rules (no gradients, one accent, 4 px
grid, glass only over content). The identity comes from **behaviour, a signature visual and
typography**, not decoration.

### 8.1 Signature: the trellis lattice
The product is named after a lattice, and the 4 px grid already is one. Make it the one recognisable
visual element:
- **Live audio as a dot lattice.** Replace the 24 bars in the pill with a 4-row × 16-column grid of
  2 px dots on a 4 px pitch. Dots light from the centre row outward as the level rises, and fade at
  the edges over time (opacity steps only, so no gradients). It reads as a signal rather than a
  generic equalizer, and it's monochrome white at 80% as the spec requires.
- **Processing.** The lattice dims and one column sweeps left to right (transcribing). A slower
  diagonal sweep means polishing. The stage is visible without colour.
- **Success.** The lattice collapses into a ✓ drawn on the same grid, in about 160 ms.
- **The logo mark** (currently "accent square with white bars") becomes a 3×3 dot lattice with the
  centre column lit, so the logo and the live state are visibly the same idea.
- It's used only where it carries meaning: the pill, the onboarding mic check and the sidebar status
  row. No decorative use, per DESIGN.md rule 6.

### 8.2 The pill as a capsule that changes shape
| State | Width | Contents |
|---|---|---|
| Listening (hold) | 240 | ● lattice · live text |
| Listening (hands-free) | 280 | ● lock · lattice · live text · `0:42` timer |
| Command | 240 | ● `CMD` · lattice · live text |
| Replying to selection (2.4) | 280 | ● `↩ Reply` · lattice · live text |
| Transcribing / Polishing | 200 | lattice sweep · "Transcribing" / "Polishing" |
| Inserted | 160 | ✓ "Inserted · 0.6 s", then collapses after 700 ms |
| Problem | 320 | "!" · one-line reason · what happened to the text (3 s) |
| Review card (2.2) | 400 × ≤240 | result text · `↵ Insert` `C Copy` `R Retry` `Esc` |

- **Motion.** Width changes over 160 ms (`cubic-bezier(.2,.8,.2,1)`) and content cross-fades over
  120 ms. Under `prefers-reduced-motion`, states switch instantly. Durations stay between 120 and
  180 ms, the same as today's `fade` and `pop`.
- **Target cue.** A muted 12 px label of the destination ("→ Slack", "→ Claude Code") for the first
  second of a recording. It quietly confirms that context detection worked, and it's how users
  discover 1.3 when the guess is wrong.
- **Accessibility.** Add `aria-live="polite"` on the stage and outcome text, keep the contrast at AA
  on the dark pill, and make every state readable without the red dot.

### 8.3 Main window
- **Typography.** Add a monospace face (system `ui-monospace`, falling back to JetBrains Mono, which
  is open source and can be bundled) for kbd caps, latencies, identifiers in History and the Original
  view in 1.6. Together with tabular numbers this gives the app a precise, technical voice that suits
  the product's name. Inter stays for everything else.
- **Command palette (Ctrl/⌘ K)** in the main window: jump to any page or setting, search History,
  run "Start meeting", "Toggle AI polish" or "Add dictionary word". Settings has about 40 controls
  and a palette makes them reachable without hunting. It's built from the page list plus the existing
  `SECTIONS`, as a single component. **S–M.**
- **Home becomes a cockpit, not a log.** The hint card shows *what will happen right now*: the
  current mic, whether AI polish is on, and the active workspace (3.1), all as clickable chips. The
  stat strip shows **time saved today**, computed as words ÷ typing WPM (default 40, editable) minus
  speaking time. People understand that better than WPM.
- **Empty states and copy.** Keep the "sentence + one button" rule, but make each one teach:
  - Library → Snippets: "Say 'my calendar link' to paste your booking URL. Add one."
  - Actions: "Select text, hold Ctrl Alt Space and say 'shorter'."
- **Consistent feedback.** The main window uses toasts and global events use the pill, never both for
  the same event (after 1.1, `dictation-error` toasts in the main window only appear when the window
  is focused).

### 8.4 Labels and icons
| Today | Proposed | Reason |
|---|---|---|
| Transforms | Actions | Covers output targets (3.2) and reads as something you *do* |
| Scratchpad | Notes (tab) | Plain language; it merges with Meetings |
| Notetaker | Meetings (tab) + "Record meeting" button | Describes the output, not the tool |
| "Polish with AI" / "AI cleanup" (both used) | **AI polish**, everywhere | One name for one feature |
| Style tabs Personal/Work/Email/Other | Personal · Work · Email · **AI prompts** · Other | 1.3 |
| Icons: hand-drawn 24px set (`icons.tsx`) | Keep, add `lattice`, `reply`, `workspace`, `library` | Consistent, no icon library needed |

---

## 9. Competitive position

*(Summary of the competitor research; see the appendix for products and sources.)*

| Capability | Typical leaders | OpsTrellis today | After this roadmap |
|---|---|---|---|
| Fast system-wide dictation with AI cleanup | Wispr Flow, Superwhisper, Aqua, Willow | ✓ (BYOK, local, Linux) | ✓, with one LLM call and visible stages |
| Voice editing of selected text | Wispr Flow (command mode), Superwhisper modes | ✓ (blind replace) | ✓, with review card, output targets and refine |
| Per-app context and tone | Wispr Flow, Superwhisper | ✓ (fixed rules) | ✓, user-correctable, with an AI-prompts category |
| Code- and prompt-aware dictation | Wispr Flow and Willow (`@file` tags and identifiers, Cursor and Windsurf only); Aqua (tech-term model, works in terminals); Claude Code `/voice` (project and branch hints) | Tech-term pack | Prompt mode, repo index, `@file` mentions in **any** editor, terminal or agent |
| Context awareness | Screenshots or OCR (Wispr, Monologue, Aqua Deep Context, VoiceInk) | App name + title | User-chosen context: selection (2.4), indexed folders (3.1). No screen capture. |
| Local / offline | MacWhisper, Superwhisper, VoiceInk | ✓ STT | ✓ STT + optional local LLM |
| Meeting notes | Granola, Otter, Fireflies (all facing consent lawsuits) | ✓ basic | Templates, Me/Them, follow-ups, consent notice |
| Team vocabulary and snippets | Wispr Flow, Willow (team plans) | ✗ | ✓ (3.4) |
| Linux | None of the leaders | ✓ | ✓ (a real wedge for developers) |
| Price | Wispr Flow / Willow / Monologue about $15/month; Superwhisper about $8.50/month; Aqua $8–30/month | Free BYOK/local; Pro $12/month | Unchanged; the Team plan is the upsell |

**Where competitors are weak, based on their user complaints:**
- Cloud-only, with privacy incidents (Wispr's screenshot upload, Granola training on data by
  default).
- Clipped first words from slow recording start (Superwhisper).
- Rewrites that drift from what the user said.
- No undo or recovery.
- Session caps.
- Developer features locked to one IDE.
- Heavy idle memory use (Wispr's Electron build, about 800 MB).

Several of these are already strengths of OpsTrellis: local models, a light Tauri runtime, no
screen capture, and long dictation via phrase segmentation. Items 1.1, 1.5, 1.6, 2.1 and 3.1 close
the rest.

**The resulting position:** *the voice layer for people who work with AI and with customers*. It
works in every editor, terminal and AI agent, knows your repository without reading your screen,
runs on Linux and can be fully private.

---

## 10. Deliberately not doing (YAGNI)

- **Live typing into the target app while speaking.** Insert-once-on-release is a deliberate
  reliability choice (`ARCHITECTURE.md`). The pill's live text covers the need to see progress.
- **Speculative LLM cleanup on partial transcripts.** It's complex and doubles the cost, and 1.2
  gives most of the latency win.
- **Free-form intent detection in normal dictation** ("…and send it to Sam" triggering actions). The
  false positives are costly. Intent lives behind the command key and named actions.
- **A plugin/extension system.** Actions with prompts and output targets cover the real requests.
  Revisit only with evidence.
- **Agentic "do things for me" flows** (sending email, editing files). OpsTrellis writes and the user
  sends. The MCP bridge (Phase 4) is the right shape if this is ever needed.
- **Gradients, glows, illustrated empty states.** Users already rejected "Aurora Glass".

---

## 11. Measuring success

The app already logs per-stage timings (`pipeline.rs:354`) and stores `raw_text` against `text`.
Add local-only counters (synced only as aggregate metadata if `syncUsage` is on, as today):

| Metric | Target | Validates |
|---|---|---|
| p50 / p95 release → inserted, AI polish on | < 1.2 s / < 2.5 s | 1.2 |
| Share of dictations that end in a visible error state | Tracked; trending down | 1.1 |
| Share of command-mode results discarded in the review card | < 15% | 2.1, 2.2 prompt quality |
| Weekly users of command mode / actions | 2× baseline | 1.8, 2.2, 3.2 |
| History corrections per 100 dictations in AI-prompts and editor categories | −30% after 3.1 | 2.1, 3.1 |
| Meetings with a follow-up action used | > 40% | 2.5 |

## 12. Suggested sequencing

1. **Sprint 1:** 1.1, 1.2, 1.5, 1.9. Trust and latency, all small.
2. **Sprint 2:** 1.3, 1.4, then 2.1. The context mapping, then prompt mode on top of it.
3. **Sprint 3:** 1.6, 1.7, 1.10, plus the lattice pill (8.1/8.2). The visible refresh, shipped
   together.
4. **Sprint 4:** 2.2, 2.4, 2.3. Safe AI rewrites and replies.
5. **Sprint 5:** 1.8 onboarding (now that there's more to show), 2.5, 2.6, 2.7.
6. **Then:** 3.1 → 3.2 → 3.3 for developers, and 3.4 when the first team customers ask. 3.5 after a
   latency spike.

Each sprint keeps the existing test discipline:
- Rust unit tests for the prompt builders, classification and snippet variables.
- An `e2e/scenarios.sh` case per pipeline behaviour, using `llm_standin.py`.
- `ui_test.py` coverage for new UI flows.
- `design_check.mjs` updated for the new navigation and pill states.

---

## Appendix: competitor notes (October 2026)

Gathered from web search summaries. Several review sites are run by competing vendors, so treat the
prices and benchmark claims as approximate.

- **Wispr Flow.**
  - Command Mode (select, hold, speak; paid and experimental).
  - "Variable recognition" and `@file` tagging in Cursor and Windsurf; whisper mode; learns from edits
    made right after dictating; team snippets and dictionaries.
  - Hands-free capped at 20 min.
  - Complaints: cloud-only, a screenshot-upload privacy incident, about 800 MB idle, Windows freezes.
  - Sources: docs.wisprflow.ai (command mode, variable recognition, hands-free), laxis.com/blog/wispr-flow
- **Superwhisper.**
  - Per-app "Modes" with custom prompts; local models or BYOK.
  - CLI and an MCP server over dictation history (2026).
  - Complaints: 1–5 s recording-start delay clips first words; complex setup.
  - Sources: superwhisper.com/blog/cli-mcp, superwhisper.userjot.com
- **Aqua Voice.**
  - Own speech model tuned for technical terms; about 450 ms claimed.
  - Edit Mode on the same key when text is selected; works in terminals, SSH and tmux.
  - Deep Context (screen reading) is opt-in.
  - Source: aquavoice.com (blog/introducing-edit-mode, blog/voice-dictation-claude-code)
- **Willow Voice.**
  - Style memory; reads the screen for identifiers and auto-tags files in Cursor.
  - Team dictionaries; SOC 2 and HIPAA.
  - Source: willowvoice.com/developers
- **Monologue (Every).** DeepContext via screenshots, an auto-learning dictionary, and paste-last
  without touching the clipboard.
- **VoiceInk.** Open source; per-app or per-URL "Power Mode"; OCR context.
- **MacWhisper.** File and meeting transcription.
- **Talon / Cursorless.** Grammar-based voice coding; powerful but steep to learn.
- **Claude Code `/voice`.** Hold Space; project and branch names as hints; needs a Claude.ai account.
  Source: code.claude.com/docs/en/voice-dictation
- **Cursor 3.1.** Agents-window voice (hold Ctrl+M, waveform, timer, cancel/confirm). The editor chat
  has no live transcript.
- **Built-in.**
  - macOS on-device dictation stops after 30 s of silence.
  - Windows 11 Fluid Dictation: on-device cleanup, Copilot+ PCs only, English only.
- **Meeting tools.**
  - Otter and Fireflies use meeting bots; Granola is bot-free and keeps no audio.
  - All three face recording-consent or biometric-privacy litigation.
