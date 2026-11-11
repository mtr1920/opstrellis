# Desktop IPC contract (Rust core ↔ React UI)

All commands are called with `invoke(name, args)` from `@tauri-apps/api/core`. Argument keys are
camelCase on the JS side; Tauri maps them to snake_case in Rust. Errors reject with a string.

## Types (TypeScript)

```ts
type Tone = "formal" | "casual" | "very_casual" | "excited";
type Category = "personal" | "work" | "email" | "ai" | "other";
type ProviderKind = "local" | "openai" | "deepgram" | "assemblyai" | "google" | "azure" | "elevenlabs" | "cloud";

interface SttEntry {
  id: string;              // stable id, also the keychain secret id (`stt:<id>`)
  provider: ProviderKind;
  model: string;           // e.g. "nova-3", "gpt-4o-mini-transcribe", local model id "moonshine-tiny-en"
  baseUrl?: string | null; // custom endpoint (OpenAI-compatible / self-hosted); Azure: region in `region`
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
  cleanup: { removeFillers: boolean; voiceCommands: boolean; autoLearn: boolean; techTerms: boolean };
  autoTransforms: Partial<Record<Category, number>>;   // category -> transform id run after every dictation (AI required)
  appCategories: Record<string, string>;   // user's app rules: lowercase app name / title text -> insight category; win over built-in rules
  styleNotes: Partial<Record<Category, string>>;   // "how you write here", added to AI cleanup (≤ 300 chars)
  promptStyle: "clean" | "structured";   // AI prompts: structured adds light headings to long prompts
  styles: Record<Category, Tone>;
  insertion: { method: "paste" | "type"; restoreClipboard: boolean; pasteDelayMs: number };
  privacy: { historyRetentionDays: number | null; syncUsage: boolean };   // null = forever, 0 = don't save history
  cloud: { baseUrl: string; email: string | null };
  overlay: boolean; sounds: boolean; launchAtLogin: boolean; showInDock: boolean; onboarded: boolean;
}

interface HistoryItem {
  id: number; createdAt: string /* RFC3339 */; rawText: string; text: string;
  appName: string | null; category: string /* insight category */; provider: string | null; model: string | null;
  audioMs: number; latencyMs: number; words: number; status: "ok" | "cancelled" | "error"; error: string | null; mode: "dictate" | "command";
}

interface Stats {
  totalWords: number; totalDictations: number; wpm: number; fixes: number;
  streak: number; longestStreak: number;
  days: { date: string /* YYYY-MM-DD */; words: number }[];           // last 365 days with activity
  categories: { category: string; count: number; words: number }[];  // insight categories: ai_prompts, work_messages, emails, documents, personal_messages, other
}

interface DictEntry { id: number; phrase: string; replacement: string | null; createdAt: string; learned: boolean }  // replacement null = vocabulary hint only
interface Snippet { id: number; trigger: string; expansion: string; createdAt: string }
interface Transform { id: number; name: string; prompt: string; builtin: boolean; output: "replace" | "copy" }   // command mode: replace the selection, or copy the result
interface Note { id: number; title: string; body: string; updatedAt: string; kind: "note" | "meeting" }
interface AppUse { appName: string; category: string; count: number }
interface LocalModel { id: string; name: string; languages: string; sizeMb: number; installed: boolean; downloading: boolean; description: string; langs: string[]; recommended: boolean }
interface CloudStatus { signedIn: boolean; email: string | null; plan: string | null; usage: any | null }
type DictationState = {
  status: "idle" | "recording" | "processing"; mode: "dictate" | "command"; handsFree: boolean;
  stage: "" | "transcribing" | "polishing";   // while processing
  micReady: boolean;                         // while recording: the mic delivers audio (start cue plays then)
};
type Outcome = { kind: "ok" | "warn" | "error"; message: string };   // how a dictation ended, shown in the pill
```

## Commands

| command | args | returns |
|---|---|---|
| `get_settings` | – | `Settings` |
| `save_settings` | `{ settings }` | `Settings` (re-registers hotkeys; rejects with message on invalid hotkey) |
| `set_secret` / `delete_secret` | `{ id, value }` / `{ id }` | – (ids: `stt:<entryId>`, `llm`) |
| `has_secrets` | – | `string[]` ids that have a stored secret |
| `list_input_devices` | – | `string[]` |
| `list_history` | `{ query?: string, limit: number, offset: number }` | `HistoryItem[]` |
| `update_history` | `{ id, text }` | `{ added: DictEntry[], pending: { phrase, replacement }[], undo: UndoRow[] }`: entries learned from the correction when auto-learn is on, fixes learned once they are made a second time within 30 days, and the dictionary rows changed (before/after) |
| `undo_learned` | `{ rows: UndoRow[] }` | `void`: restores the rows a correction changed; rows edited since are left alone |
| `repolish_history` | `{ id }` | `string`: runs cleanup again on the original transcript and saves it (nothing is learned) |
| `list_apps` | – | `AppUse[]`: apps dictated into, most used first (Style → Apps) |
| `delete_history` | `{ id }` | – |
| `clear_history` | – | – |
| `get_stats` | – | `Stats` |
| `list_dictionary` / `upsert_dictionary` / `delete_dictionary` | – / `{ entry: {id?, phrase, replacement} }` / `{ id }` | `DictEntry[]` / `DictEntry` / – |
| `list_snippets` / `upsert_snippet` / `delete_snippet` | – / `{ snippet: {id?, trigger, expansion} }` / `{ id }` | same pattern |
| `list_transforms` / `upsert_transform` / `delete_transform` | – / `{ transform: {id?, name, prompt} }` / `{ id }` | same pattern |
| `list_notes` / `upsert_note` / `delete_note` | `{ kind?: "note" \| "meeting" }` / `{ note: {id?, title, body} }` / `{ id }` | same pattern |
| `list_local_models` | – | `LocalModel[]` |
| `download_local_model` / `delete_local_model` | `{ id }` | – (progress via events) |
| `cancel_local_download` | `{ id }` | – (the download ends with `done: true, error: null, cancelled: true`) |
| `test_stt` | `{ entryId }` | `{ text: string, latencyMs: number }` (transcribes a bundled sample) |
| `test_llm` | – | `{ text: string, latencyMs: number }` (cleans up a sample sentence with the saved AI settings) |
| `meeting_start` / `meeting_stop` / `meeting_status` | `{ secondDevice?, template? }` / – / – | – / – / `MeetingStatus` |
| `meeting_templates` | – | `{ id, name }[]` (General, Sales call, Interview, 1:1, Standup) |
| `meeting_follow_up` | `{ noteId, kind: "email" \| "crm" }` | `string`: a follow-up email or CRM note drafted from the meeting |
| `start_dictation` / `stop_dictation` / `cancel_dictation` | – | – (UI buttons; same as hotkeys) |
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

## Events (`listen(name, cb)` from `@tauri-apps/api/event`)

| event | payload |
|---|---|
| `dictation-state` | `DictationState` |
| `audio-level` | `number` (0..1, ~20 Hz while recording) |
| `partial-transcript` | `string` |
| `dictation-result` | `{ text: string, latencyMs: number }` |
| `dictation-error` | `string` (main window toast) |
| `dictation-outcome` | `Outcome` (pill: inserted / copied / warning / error) |
| `meeting-status` | – (a meeting recording started or stopped, also from the tray) |
| `settings-changed` | `Settings` (changed outside the window, e.g. the tray's AI polish toggle) |
| `permissions-changed` | – (something `permissions` reports changed, e.g. the desktop bound the Wayland shortcuts) |
| `shortcut-captured` | `string` (a single key such as `"Fn"` or `"RightOption"`, pressed on its own while the recorder is open; macOS) |
| `history-updated` | – |
| `model-download` | `{ id: string, downloaded: number, total: number, done: boolean, error: string \| null, cancelled?: boolean }` |
| `cloud-status` | `CloudStatus` |

## Windows

- `main`: the app UI (route by sidebar state; no router dependency needed).
- `overlay`: loads `index.html#overlay`. A small transparent, always-on-top pill near the bottom
  centre (window 360×64). It shows the live lattice waveform from `audio-level`, the last partial
  transcript (truncated), the processing stage, how the dictation ended (`dictation-outcome`, kept on
  screen ~0.9 s / 3 s by Rust), and cancel (✕) and stop (■) buttons. It must never take focus; Rust
  handles that.
