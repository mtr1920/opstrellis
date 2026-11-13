# Desktop IPC contract (Rust core ↔ React UI)

All commands are called with `invoke(name, args)` from `@tauri-apps/api/core`. Argument keys are
camelCase on the JS side; Tauri maps them to snake_case in Rust. Errors reject with a string.

## Types (TypeScript)

```ts
type Tone = "formal" | "casual" | "very_casual" | "excited";
type Category = "personal" | "work" | "email" | "ai" | "other";
type ProviderKind = "local" | "openai" | "deepgram" | "assemblyai" | "google" | "azure" | "elevenlabs" | "cloud";

interface SttEntry {
  id: string;              // stable id, also the keychain secret id (`stt:<id>`; deleted on save when the entry is removed or its provider or base URL changes)
  provider: ProviderKind;
  model: string;           // e.g. "nova-3", "gpt-4o-mini-transcribe", local model id "moonshine-tiny-en"
  baseUrl?: string | null; // custom endpoint (OpenAI-compatible / self-hosted); Azure: region in `region`. Every base URL here, in `llm` and in `cloud` must be https://; http:// only to this computer or a private network (localhost, 127/8, ::1, 0.0.0.0, ::, 10/8, 172.16/12, 192.168/16, 100.64/10, link-local, fc00::/7, single-label names, .local/.lan/.internal/.home/.home.arpa/.localdomain/.ts.net; not fritz.box, as .box is a public domain), else requests fail before anything is sent
  region?: string | null;  // Azure region
  streaming: boolean;      // use realtime streaming when the provider supports it
  enabled: boolean;
}

interface Settings {
  hotkeys: { dictate: string; command: string; pasteLast: string };   // accelerator strings, e.g. "Ctrl+Shift+Space", or one single key on macOS for dictate or command: "Fn", "RightCommand", "RightOption", "RightControl", "RightShift"
  microphone: string | null;          // device name, null = system default
  language: string;                   // "auto" or ISO code ("en", "de", ...)
  sttChain: SttEntry[];               // ordered; first enabled = primary, rest = fallbacks
  llm: { enabled: boolean; provider: "openai" | "anthropic" | "cloud"; baseUrl: string | null; model: string };
  cleanup: { removeFillers: boolean; voiceCommands: boolean; autoLearn: boolean; techTerms: boolean; termPacks: string[] };   // termPacks: ids of the other term packs in use (built-in or imported), default []; unknown ids are ignored
  autoTransforms: Partial<Record<Category, number>>;   // category -> transform id run after every dictation (AI required)
  appCategories: Record<string, string>;   // user's app rules: lowercase app name / title text -> insight category; win over built-in rules
  styleNotes: Partial<Record<Category, string>>;   // "how you write here", added to AI cleanup (≤ 300 chars)
  promptStyle: "clean" | "structured";   // AI prompts: structured adds light headings to long prompts
  outputLanguages: Partial<Record<Category, string>>;   // category -> language AI polish writes in (default {} = as spoken); see "Output language" below
  reviewRewrites: "long" | "always" | "never";   // command mode: show a rewrite for review before it replaces the selection (default "long"); see "Review card" below
  styles: Record<Category, Tone>;
  insertion: { method: "paste" | "type"; restoreClipboard: boolean; pasteDelayMs: number };
  privacy: { historyRetentionDays: number | null; syncUsage: boolean; audioRetention: "off" | "failed" | "all" };   // null = forever, 0 = don't save history; audioRetention below; syncUsage off also drops usage not yet sent
  cloud: { baseUrl: string; email: string | null; teamLibrary: boolean };   // changing baseUrl signs out: the token, unsent usage and the team library are dropped, email cleared, `cloud-status` emitted. teamLibrary (default true): sync the team library while signed in; turning it off deletes the local team entries, turning it on syncs at once (see "Team library")
  overlay: boolean; sounds: boolean; launchAtLogin: boolean; showInDock: boolean; onboarded: boolean;
  checkUpdates: boolean;   // opt-in check of github.com for the latest version number, at most once a day; default false
  readonly updateCheckedAt?: number | null;   // unix seconds of the last successful automatic check; backend-owned (save_settings keeps the backend's value)
  workspaceReplace: boolean;   // default false: write workspace names said as words as code without AI, in editors and terminals only (can rewrite ordinary phrases: "plain text" -> plain_text)
  readonly workspaces?: string[];   // canonical absolute folder paths for repo-aware dictation, at most 10, default []; backend-owned: changed only by add_workspace / remove_workspace (save_settings keeps the backend's value)
  agents: { enabled: boolean; history: boolean; notes: boolean; addTerms: boolean; voice: boolean };   // default { false, true, true, false, false }. What `opstrellis mcp` and the agent CLI commands may use (they re-read settings.json on every request): history = dictations and the dictionary, notes = notes and meeting notes, addTerms = add_dictionary_term, voice = ask_user / `opstrellis ask` (the app checks enabled && voice, and the pill being on, for every question). Nothing while enabled is false
}

interface HistoryItem {
  id: number; createdAt: string /* RFC3339 */; rawText: string; text: string;
  appName: string | null; category: string /* insight category */; provider: string | null; model: string | null;
  audioMs: number; latencyMs: number; words: number; status: "ok" | "cancelled" | "error"; error: string | null; mode: "dictate" | "command";
  hasAudio: boolean;   // computed, not stored: a recording of this dictation is kept (memory or disk), so it can be transcribed again
}

// privacy.audioRetention (default "off"): what happens to recordings.
// - "off": never written to disk. A dictation whose transcription failed keeps its recording in memory
//   (at most 3, 20 minutes in total, oldest dropped first; never the only one) until it is retried
//   successfully, its row is deleted, history is cleared, or the app quits.
// - "failed": a failed one is also written to <app data>/recordings/<history id>.wav.
// - "all": every dictation's recording is written there (pending-<uuid>.wav while it is processed); one
//   interrupted by a quit or crash becomes a History row with status "error" at the next launch.
// Disk retention needs History (historyRetentionDays != 0); with History off only memory is used. Files are
// 0600 in a 0700 folder, named only from history ids or uuids. A file is no longer offered once it is 24 hours
// old, and is deleted within an hour after that while the app runs (checked hourly, on each saved dictation and
// on a settings change), or at the next launch. It is also deleted with its row (delete, clear, retention),
// after a successful retry unless "all", when "all" becomes "failed" (rows that didn't fail), and all at once
// when the setting becomes "off" or History is turned off. Only dictations are kept: a failed command can't be
// retried (its selection is gone). The tray's "Retry failed dictation" retries the newest kept failure, with
// History off too.

interface Stats {
  totalWords: number; totalDictations: number; wpm: number; fixes: number;
  streak: number; longestStreak: number;
  days: { date: string /* YYYY-MM-DD */; words: number }[];           // last 365 days with activity
  categories: { category: string; count: number; words: number }[];  // insight categories: ai_prompts, work_messages, emails, documents, personal_messages, other
}

interface DictEntry { id: number; phrase: string; replacement: string | null; createdAt: string; learned: boolean; source?: "manual" | "learned" | "agent" | "team"; unused?: TeamUnused | null }  // replacement null = vocabulary hint only; source and unused are set by list_dictionary (learned = source "learned"; "agent" = added through `opstrellis mcp`; "team" = synced from the team library, read-only; editing an entry makes it "manual")
interface Snippet { id: number; trigger: string; expansion: string; createdAt: string; team?: boolean; unused?: TeamUnused | null }   // team, unused: set by list_snippets
interface Transform { id: number; name: string; prompt: string; builtin: boolean; output: "replace" | "copy"; team?: boolean; unused?: TeamUnused | null; edited?: boolean; defaultName?: string }   // command mode: replace the selection, or copy the result; team, unused, edited, defaultName: set by list_transforms (edited: a built-in whose name, prompt or output is no longer its default; defaultName: the name of the default it restores to, only on a built-in that has one)
type TeamUnused = "shadowed" | "limit" | "complex";   // why a team entry is not used: one of the user's own (or a built-in action) has its phrase, trigger or name, it is past a limit, or (dictionary) the team's words would make the matcher fail to build; null for entries in use and the user's own
interface TeamStatus { team: { id: string; name: string } | null; dictionary: number; snippets: number; actions: number; skipped: number; syncedAt: string | null /* RFC3339, last successful sync */; error: string | null /* last failed sync, cleared by the next success */ }   // counts are the team entries stored here; skipped = invalid ones plus those past a limit
interface Note { id: number; title: string; body: string; updatedAt: string; kind: "note" | "meeting" }
interface AppUse { appName: string; category: string; count: number }
interface LocalModel { id: string; name: string; languages: string; sizeMb: number; installed: boolean; downloading: boolean; description: string; langs: string[]; recommended: string[] }
interface CloudStatus { signedIn: boolean; email: string | null; plan: string | null; usage: any | null }
interface AddSkip { add: number; skip: number }   // entries added / already there (an existing entry always wins)
interface Workspace { path: string; name: string /* folder name */; files: number; identifiers: number /* declared names kept, ≤ 3000 */; indexedAt: string | null /* RFC3339, when the last run ended (also a failed one) */; indexing: boolean; error: string | null /* folder missing, or "Too many names…" when spoken names couldn't be built (hints and mentions still work) */ }
interface TermPack { id: string; name: string; description: string; terms: number; builtin: boolean; enabled: boolean }   // "tech" first (enabled = cleanup.techTerms), then built-in packs, then imported ones by id
interface TermPackPreview { id: string; name: string; description: string; terms: number; dropped: number; replaces: boolean /* an imported pack with this id exists */; replacements: { alias: string; term: string; codeOnly: boolean }[] /* the first 100 */; replacementCount: number }
interface BackupSummary { dictionary: AddSkip; snippets: AddSkip; actions: AddSkip; notes: number /* new ones */; hasSettings: boolean; settings: SettingsDetail | null }
interface SettingsDetail { language: string | null; promptStyle: string | null; cleanup: number; styles: Record<string, Tone>; styleNotes: number; appRules: number; termPacks: string[] | null /* built-in pack ids to have on */; outputLanguages: Record<string, string>; reviewRewrites: string | null; autoActions: Record<string, string> /* category -> action name, only actions that exist here */ }
interface UpdateInfo { current: string; latest: string /* release tag, e.g. "v0.3.3" */; newer: boolean; url: string /* release page, built locally */ }
type DictationState = {
  status: "idle" | "recording" | "processing" | "review"; mode: "dictate" | "command"; handsFree: boolean;
  stage: "" | "transcribing" | "polishing" | "inserting";   // while processing ("inserting": after Enter on a review card)
  micReady: boolean;                         // while recording: the mic delivers audio (start cue plays then)
  text?: string;                             // only with status "review": the rewrite waiting for Enter / Esc
  workspace: string | null;                  // while recording: folder name of the workspace this dictation uses (pill chip); null otherwise
  answering: boolean;                        // while recording: this dictation answers an agent's question; it is sent to the agent, not inserted
};
type Outcome = { kind: "ok" | "warn" | "error"; message: string };   // how a dictation ended, shown in the pill
```

### Output language (`outputLanguages`)

Codes and names are a fixed table in `llm::OUTPUT_LANGUAGES` (27: en, es, fr, de, it, pt, nl, pl, cs, ro, el, sv,
ru, uk, tr, ar, he, hi, bn, ur, ta, id, vi, th, ja, ko, zh), mirrored in `Style.tsx`; unknown codes are ignored.
The dictation's style category picks the entry. It applies only with AI polish on, a provider other than the offline
model (`"local"`), and not in a plain terminal shell (an AI agent in a terminal is fine); otherwise the text is
inserted as spoken. The cleanup prompt then asks for the text in that language, translating faithfully, never
translating names, code, file names, commands, URLs or dictionary terms. The length guard is the relaxed one used for
auto actions, a dictionary term the model drops still discards the result, and the rules after the model use the
output language. If the AI fails, the rule-based text is inserted untranslated and the warning says "not
translated". History keeps the spoken transcript in `rawText`.

### Review card (`reviewRewrites`)

A command whose result replaces a non-empty selection (an action whose output is `"replace"`, not in a shell) is
reviewed when `reviewRewrites` is `"always"`, or `"long"` and the selection or the result is over 280 characters.
Skipped (inserted as before) on Wayland, with `overlay: false`, or when Enter or Esc can't be registered. While it
waits, `dictation-state` is `{ status: "review", mode: "command", text }`, the overlay window is 436×280 and
click-through, the app stays busy, and Enter and Esc are global shortcuts. Enter inserts it as usual (emitting stage
`"inserting"`; the focus-moved rule still applies). Esc, `cancel_dictation`, `opstrellis --cancel`, a press of the
dictate or command shortcut (or `--toggle`, `--command`, the tray's start/stop, `start_dictation`), or 30 s without
an answer discard it: nothing is inserted, it becomes the paste-last text and a History row with status
`"cancelled"` holding it, and the pill says so. Its waiting time isn't counted in `latencyMs`.
The key that ends it (and Esc that cancels a recording) stays grabbed until it is let go, at most 2 s, so its
auto-repeat never reaches the app below; after Enter the paste waits for that.

## Commands

| command | args | returns |
|---|---|---|
| `get_settings` | – | `Settings` |
| `reset_settings` | – | `Settings`: preferences back to their defaults, saved through `save_settings` (same side effects and errors). Kept: transcription providers, the AI service and model (not its on/off), privacy, cloud server, email, sign-in and team library sync, onboarding, workspaces, `updateCheckedAt`, imported term packs. Keys, history and the library are untouched |
| `save_settings` | `{ settings }` | `Settings` (re-registers hotkeys; rejects with message on invalid hotkey; rebuilds the term packs in use, in the background, when `cleanup.techTerms` or `cleanup.termPacks` changed) |
| `set_secret` / `delete_secret` | `{ id, value }` / `{ id }` | – (ids: `stt:<entryId>`, `llm`; `llm` is deleted on save when the AI provider or base URL changes) |
| `has_secrets` | – | `string[]` ids that have a stored secret |
| `list_input_devices` | – | `string[]` |
| `list_history` | `{ query?: string, limit: number, offset: number }` | `HistoryItem[]` |
| `update_history` | `{ id, text }` | `{ added: DictEntry[], pending: { phrase, replacement }[], undo: UndoRow[], full: boolean }`: `full` is true when a fix was not learned because the dictionary is at its budget of 2000 entries or 150000 bytes (updating a learned row still works); entries learned from the correction when auto-learn is on, fixes learned once they are made a second time within 30 days, and the dictionary rows changed (before/after) |
| `undo_learned` | `{ rows: UndoRow[] }` | `void`: restores the rows a correction changed; rows edited since are left alone |
| `repolish_history` | `{ id }` | `string`: runs cleanup again on the original transcript and saves it (nothing is learned) |
| `retranscribe_history` | `{ id }` | `string`: transcribes the dictation's kept recording again with the current provider chain (batch, no stream; a recording over 30 s is sent in parts of at most 30 s cut at pauses, silent parts skipped, and any failed part fails the retry and keeps the recording), cleans it up for the row's app, saves it (status `ok`, raw text, text, provider and model of the first part, words; a failed row now counts once in Insights and usage sync, an `ok` row keeps its count), copies it to the clipboard, makes it the paste-last text, emits `history-updated` and shows the outcome in the pill. Rejects while a dictation is recording or processing or another retry runs, when the row is gone or changed meanwhile, or the recording is no longer kept |
| `list_apps` | – | `AppUse[]`: apps dictated into, most used first (Style → Apps) |
| `delete_history` | `{ id }` | – (also deletes its kept recording) |
| `clear_history` | – | – (also deletes every kept recording, in memory and on disk; Insights totals are kept) |
| `get_stats` | – | `Stats` |
| `list_dictionary` / `upsert_dictionary` / `delete_dictionary` | – / `{ entry: {id?, phrase, replacement} }` / `{ id }` | `DictEntry[]` / `DictEntry` / – (`upsert_dictionary` rejects control characters and anything past the dictionary budget of 2000 entries and 150000 bytes of phrases plus replacements; `upsert_snippet` and `upsert_transform` reject control characters, allowing only newline and tab in expansions and prompts) |
| `list_snippets` / `upsert_snippet` / `delete_snippet` | – / `{ snippet: {id?, trigger, expansion} }` / `{ id }` | same pattern |
| `list_transforms` / `upsert_transform` / `delete_transform` | – / `{ transform: {id?, name, prompt} }` / `{ id }` | same pattern. The list commands return every entry, team ones included; every upsert and delete of a team entry rejects with "Team entries are edited in the team dashboard" |
| `restore_transform` | `{ id }` | `Transform`: puts a built-in action's name, prompt and output back to its default. Rejects a non-built-in, a team entry or a built-in whose default isn't known. The caller drops auto actions pointing at it when its output changed |
| `team_status` | – | `TeamStatus` (local only, no request) |
| `team_sync_now` | – | `TeamStatus` after a sync attempt (none when signed out or `cloud.teamLibrary` is off; a failure is in `error`) |
| `list_notes` / `upsert_note` / `delete_note` | `{ kind?: "note" \| "meeting" }` / `{ note: {id?, title, body} }` / `{ id }` | same pattern |
| `list_local_models` | – | `LocalModel[]` |
| `download_local_model` / `delete_local_model` | `{ id, allowUnverified?: boolean }` / `{ id }` | – (progress via events). A pinned archive that no longer matches its checksum is refused (event `changed: true`) unless `allowUnverified` |
| `cancel_local_download` | `{ id }` | – (the download ends with `done: true, error: null, cancelled: true`) |
| `test_stt` | `{ entryId }` | `{ text: string, latencyMs: number }` (transcribes a bundled sample) |
| `test_llm` | – | `{ text: string, latencyMs: number }` (cleans up a sample sentence with the saved AI settings) |
| `meeting_start` / `meeting_stop` / `meeting_status` | `{ secondDevice?, template? }` / – / – | – / – / `MeetingStatus` (a meeting stops by itself, and is saved, after 15 minutes in which no input produced transcribed text, or at 4 hours; the pill says why) |
| `meeting_templates` | – | `{ id, name }[]` (General, Sales call, Interview, 1:1, Standup) |
| `meeting_follow_up` | `{ noteId, kind: "email" \| "crm" }` | `string`: a follow-up email or CRM note drafted from the meeting |
| `start_dictation` / `stop_dictation` / `cancel_dictation` | – | – (UI buttons; same as hotkeys; `cancel_dictation` and `start_dictation` also discard a rewrite waiting for review) |
| `paste_last` | – | – |
| `cloud_login` | – | `{ userCode: string, verificationUrl: string }` (opens browser, polls in background, emits `cloud-status`) |
| `cloud_logout` / `cloud_status` | – | `CloudStatus` |
| `permissions` | – | `{ accessibility: boolean, platform: "macos" \| "linux", wayland: boolean, hotkeyError: string \| null, inputMonitoring: boolean \| null, fnSystemAction: string \| null, waylandShortcuts: { portal: { state: "unavailable" \| "binding" \| "bound" \| "failed", shortcuts?: { id, trigger }[], error?: string }, gnome: boolean, gnomeShortcuts: boolean } \| null, insertion: { ready: "wtype" \| "ydotool" \| null, problem: string \| null, fix: string \| null } \| null }` (`inputMonitoring` and `fnSystemAction` are null unless the Fn key is a shortcut; the last two are null unless Wayland) |
| `request_accessibility` | – | – (macOS prompt) |
| `request_input_monitoring` | – | – (macOS prompt the first time, then opens System Settings → Input Monitoring) |
| `rebind_wayland_shortcuts` | – | – (Wayland: asks the desktop's GlobalShortcuts portal to bind the shortcuts again) |
| `add_gnome_shortcuts` | – | – (GNOME without the portal: adds custom shortcuts running `opstrellis --toggle`, `--command`, `--paste-last` with the configured keys) |
| `set_shortcut_recording` | `{ on }` | – (shortcut recorder open: shortcuts are paused so pressing one records it; macOS single keys arrive as `shortcut-captured`) |
| `open_logs` | – | – (reveals `opstrellis.log` in Finder / the file manager) |
| `export_backup` | `{ includeNotes }` | `{ path, dictionary, snippets, actions, notes, skipped }`: writes `opstrellis-backup-YYYYMMDD-HHMMSS.json` (version 1) to Downloads and reveals it. Holds the dictionary, snippets, user-made actions (never built-ins), optionally notes and meetings, and the style, cleanup (including the enabled built-in term packs, never imported ones), language, prompt-style, write-in languages (`outputLanguages`), `reviewRewrites`, app-rule and auto-action settings (auto actions by action name). Never secrets, the cloud email or URL, providers, hotkeys, microphone, privacy, UI, workspace, agent or update-check settings, nor team entries (they sync). `skipped` counts entries left out because import would refuse them: a field over a limit or with a control character, or beyond the dictionary budget (the oldest 2000 entries / 150000 bytes are kept); rejects if the file would pass 5 MB |
| `preview_backup` | `{ json }` | `BackupSummary`: validates the file and says what an import would do; changes nothing. `settings` says what restoring settings would replace (null if the file has none worth restoring). Rejects (with a message) over 5 MB, a `version` other than 1, over 2000 dictionary entries or 5000 of anything else, over-long or empty fields, control characters (none in phrases, replacements, triggers, names or note titles; only newline and tab in expansions, prompts and note bodies), an unknown action `output` or note `kind`, and a dictionary that would pass the dictionary budget (2000 entries, 150000 bytes of phrases plus replacements; the message says how many fit) |
| `import_backup` | `{ json, settings, notes }` | `BackupSummary` of what was done. One transaction (all or nothing). Existing entries win: dictionary by phrase, snippets by trigger, actions by name (also against built-in names), all trimmed and case-insensitive; a note is added unless one with the same title and body exists. `notes: true` adds notes. `settings: true` replaces the matching current settings with the backed-up ones (known categories and tones only) through the settings store and emits `settings-changed`. Term packs: the built-in ones follow the file, imported packs stay as they are. Auto actions resolve only to actions that existed before the import (names that don't are dropped), and app rules whose category is `ai_prompts` are never imported |
| `import_dictionary_text` | `{ text }` | `AddSkip`: one entry per line, blank lines and `#` lines ignored; `phrase` (hint) or `phrase<sep>replacement`, `<sep>` = tab, ` => `, ` -> `, ` → ` or `,` (first found in that order; empty replacement = hint). Same merge rule and limits; a bad line rejects the whole import |
| `list_term_packs` | – | `TermPack[]` |
| `preview_term_pack` | `{ json }` | `TermPackPreview`: runs every `import_term_pack` check and lists the replacements the pack would make; saves nothing. The UI shows it and imports only on confirm |
| `import_term_pack` | `{ json }` | `{ pack: TermPack, dropped: number }`: validates a pack file (format and rules in CONTRIBUTING.md) and saves it as `<app data>/term-packs/<id>.json`, replacing an imported pack with the same id (it stays on if it was). Rejects over 1 MB, invalid JSON, an id not matching `^[a-z0-9][a-z0-9-]{0,39}$` or taken by a built-in pack, a name of 0 or over 60 characters, a description over 200, over 2000 terms, or nothing usable left. Terms and aliases that break a safety rule or a length limit (60 characters, 10 aliases + 10 tech aliases per term) are dropped and counted in `dropped`; for an imported pack a term must also be letters, digits, spaces and `. + # & / - _ '` in at most 4 words, and an alias must sound like its term. Its terms are sent as hints only once the user has said them. Not turned on: enable it through `save_settings` |
| `delete_term_pack` | `{ id }` | – (imported packs only; also removes the id from `cleanup.termPacks`) |
| `list_workspaces` | – | `Workspace[]` in the order added |
| `add_workspace` | `{ path }` | `Workspace[]`: adds the folder (stored canonical) and indexes it in the background (`indexing: true` until `workspaces-changed`). Rejects a path that is empty, not absolute, missing, a file, the filesystem root, a top-level folder (`/Users`, `/home`), the home folder or any folder holding it (also through `/System/Volumes/Data`), already a workspace (also by another path to it), inside or containing a workspace, or past 10 |
| `remove_workspace` | `{ path }` | `Workspace[]` (its index is dropped) |
| `reindex_workspace` | `{ path }` | `Workspace[]`: indexes it again in the background, unless a run for it is already going; rejects an unknown path |
| `check_update` | – | `UpdateInfo`: asks `api.github.com` for the latest release (only its `tag_name`, which must be `v?N.N.N`), whatever `checkUpdates` says |
| `open_release_page` | `{ tag }` | – (opens `https://github.com/mtr1920/opstrellis/releases/tag/<tag>`; rejects a tag that isn't `v?N.N.N`) |
| `self_update_supported` | – | `boolean`: true on macOS when running from an app bundle, and on Linux for the AppImage build running as an AppImage (`APPIMAGE` set and that file exists, and the binary inside `APPDIR`); false for a `.deb` install (updated by installing the new `.deb` from the release page) or an unbundled dev build. The UI then shows "Open release page" and the install command |
| `download_update` | – | – (reads `https://github.com/mtr1920/opstrellis/releases/latest/download/latest.json` through the updater plugin, downloads this platform's build and verifies its signature, then keeps it in memory; progress and outcome as `update-download` events. Never a downgrade. A call while a download runs joins it; after one finished it reports `ready` again without downloading. Rejects when `self_update_supported` is false; rejects, also as a `failed` event, when GitHub can't be reached, the latest release has no `latest.json` (releases before in-app updates), the signature doesn't match, or the download breaks off) |
| `install_update` | – | – (installs the downloaded update and restarts the app; never returns on success. Rejects with "Finish the recording first, then restart." while a dictation is recording or processing or a meeting is recording or finishing. Before restarting it writes `<app data>/update-restart`; the launch that finds it deletes it, ignores its launch arguments and opens the main window. macOS replaces the app bundle, asking for an administrator password if its folder isn't writable; Linux replaces the AppImage and, if it is the one `install.sh` manages, rewrites its `VERSION` marker. On failure the installed app is unchanged and the download is kept for another try; rejects when nothing was downloaded) |

## Events (`listen(name, cb)` from `@tauri-apps/api/event`)

| event | payload |
|---|---|
| `dictation-state` | `DictationState` |
| `audio-level` | `number` (0..1, ~20 Hz while recording) |
| `partial-transcript` | `string` |
| `dictation-result` | `{ text: string, latencyMs: number }` |
| `dictation-error` | `string` (main window toast) |
| `dictation-outcome` | `Outcome` (pill: inserted / copied / warning / error) |
| `agent-question` | `string \| null`: an agent's question waiting for a spoken answer, in full (up to 300 characters; control and text-direction characters removed, whitespace collapsed), or null when it was answered, declined, withdrawn or timed out. While idle with a question waiting, the overlay window has the review card's size (436 × 280, bottom-centre, click-through) and the page draws the question card, wrapped (10 lines of 18 px hold 300 characters of any script), with an outcome shown in the card's head; the backend keeps it shown as long as the question waits and restores the pill size when the card goes |
| `meeting-status` | – (a meeting recording started or stopped, also from the tray) |
| `workspaces-changed` | – (an indexing run ended; call `list_workspaces`) |
| `settings-changed` | `Settings` (changed outside the window, e.g. the tray's AI polish toggle) |
| `permissions-changed` | – (something `permissions` reports changed, e.g. the desktop bound the Wayland shortcuts) |
| `update-download` | `{ state: "downloading" \| "ready" \| "installing" \| "failed", percent: number \| null, error: string \| null }` (`percent` is null until the size is known, stays below 100 until the signature is checked; `installing` while `install_update` runs, back to `ready` if it fails) |
| `update-available` | `UpdateInfo` (only while `checkUpdates` is on. The backend looks every 30 s and checks when the last successful check, `updateCheckedAt`, is over 24 h old or missing, so a restart does not trigger one; a failed try is repeated after an hour; the setting is read every time; failures are only logged) |
| `shortcut-captured` | `string` (a single key such as `"Fn"` or `"RightOption"`, pressed on its own while the recorder is open; macOS) |
| `history-updated` | – |
| `model-download` | `{ id: string, downloaded: number, total: number, done: boolean, error: string \| null, cancelled?: boolean, changed?: boolean }` (`changed`: the archive isn't the one this version pinned; the UI offers "Download anyway") |
| `cloud-status` | `CloudStatus` |
| `team-updated` | – (a sync changed the team library, or it was deleted, or the setting was turned on; reload the library pages and `team_status`) |

### Team library

When signed in with `cloud.teamLibrary` on, the app pulls the team's shared dictionary entries, snippets and voice actions
from `GET /api/library` (server/README.md, "Library sync") after sign-in, every 5 minutes, and on `team_sync_now`; never
while dictating. They are read-only here and never in a backup. Invalid entries (the import rules for lengths and control
characters, an unknown `output`, a snippet using `{clipboard}`) are skipped. The user's own entries win: a team entry with
the same phrase or trigger as the matchers compare them (any case, any run of spaces or hyphens between words; a trigger's
end punctuation ignored), a team phrase equal to the output of one of the user's own entries, or a team action named like
one of the user's own or a built-in, is listed with `unused: "shadowed"` and not used. Lists put the user's own rows
first (each group newest first), and dictation uses them in that order, so a team snippet's expansion never triggers one
of the user's snippets. Team dictionary entries use only the room the user's own leave in the dictionary budget, oldest
first, and at most 50 team snippets (each trigger costs about 0.2 ms per dictation) and 200 team actions are used; the
rest are `unused: "limit"`. If the team's words would make the dictionary matcher fail to build, none of them is used
(`unused: "complex"`, counted in `skipped`, and `team_status` says so in `error`). A team action runs only in command
mode, only when the whole instruction is exactly its name and none of the user's own or built-in actions matches; it is
never an auto action (`list_transforms` marks it `team: true`; Style lists only the user's own and built-in ones). A
response with more than 2000 entries, or one that would leave more than 2100 team rows and skipped ids stored, is
refused as a whole. Signing out, a new server URL, leaving the team and turning the setting off delete the team entries.

## Windows

- `main`: the app UI (route by sidebar state; no router dependency needed).
- `overlay`: loads `index.html#overlay`. A small transparent, always-on-top pill near the bottom
  centre (window 360×64). It shows the live lattice waveform from `audio-level`, the last partial
  transcript (truncated), the workspace in use (a chip, while recording), the processing stage, how the dictation ended (`dictation-outcome`, kept on
  screen ~0.9 s / 3 s by Rust), and cancel (✕) and stop (■) buttons. It must never take focus; Rust
  handles that. While a command's rewrite waits for review (status `"review"`) Rust makes the window 436×280,
  anchored at the same bottom centre and click-through on every platform, and shows the review card; it goes
  back to 360×64 with the next state.

## Agent questions (`ask.rs`)

`opstrellis mcp` (`ask_user`) and `opstrellis ask` reach the running app over a Unix socket,
`<app data>/agent.sock` (0600, in the 0700 data folder; a stale socket is replaced at startup, the
socket is removed on quit). One JSON line in, `{"ask": "<question>"}` (1–300 characters, at most 4 KiB,
within 5 s), one JSON line out: `{"answer": "<text>"}` or `{"error": "<message>"}`. Errors: agent access
or voice questions off, the pill off, another question already waiting (one at a time, at most 4
connections), malformed or oversized requests, "The user didn't answer." (timing below), "The user declined
to answer." (tray **Decline agent question**, or `--cancel` while nothing records and no review card is
shown), and "OpsTrellis quit before the user answered.". Closing the connection withdraws the question
(the MCP server does this on `notifications/cancelled`). Nothing received is logged. A dictation (not a
command) whose key press came at least 2 s after the question became visible is the answer. Visible means
the overlay window is shown at the card's size (tracked where the backend shows, hides and resizes it) and
the page draws the whole question: the overlay calls `question_shown { text: string | null }` with the
question text it draws, or null while it shows a recording, processing or the review card. A question posted during a
dictation counts from when it appears afterwards; one never shown, or hidden at the key press, can't be
answered; showing it again restarts the 2 s. The timeout counts from when the question arrived: 60 s, but it never runs out while a dictation accepted
as the answer is being recorded or processed, and an attempt that ends without an answer (cancelled, no
words, transcription failed) leaves at least 15 s more; after 5 minutes it ends regardless (the client
waits 5 min 10 s at most). Turning agent access, voice questions or the pill off withdraws a waiting
question ("The user turned voice questions off in OpsTrellis."), and consent is checked again when an
answer would be sent: without it nothing goes to the agent and the text goes to the clipboard. The
answer is transcribed and cleaned up as an AI prompt (`ai_prompts`, never as a shell command), sent instead of
inserted, saved to History with the app name "Answer to an agent", and the pill says "Sent to the agent".
If the question ended while the user spoke, the text goes to the clipboard instead. A cancelled
recording, a failed transcription, or one with no words (also after cleanup, e.g. only "um") leaves the
question waiting.
