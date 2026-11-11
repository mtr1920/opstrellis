# OpsTrellis — architecture

OpsTrellis is an open, cross-platform (macOS + Linux) voice dictation app. You hold a hotkey, speak, and
cleaned-up text is inserted into whatever app has focus. Speech-to-text (STT) providers and LLMs are
pluggable: bring your own key (BYOK), use local models, or use the managed OpsTrellis Cloud.

## Research summary (what we are matching)

- **Interaction.** Hold the hotkey to talk (push-to-talk). A quick tap toggles hands-free mode. On
  macOS the shortcut can be the Fn key: hold to talk, double-tap for hands-free. Esc cancels, but
  only while recording. A shortcut pastes the last transcript.
- **Command mode.** Select text, hold the command hotkey and speak an instruction ("make this more
  formal"). The rewritten text replaces the selection; in a plain terminal shell it is copied instead,
  for the user to check and paste. A long rewrite is first shown in the pill: Enter inserts it, Esc discards it.
- **Insertion.** Text is not typed live into the target app. Audio streams while you speak; on
  release, the final transcript is cleaned up and inserted once, by pasting through the clipboard.
  The clipboard is saved first and restored afterwards. Terminals on Linux paste with Ctrl+Shift+V.
- **Latency target.** Under 1 s from release to text. Streaming providers stay connected while you
  speak, so only finalization plus LLM cleanup happen after release.
- **Cleanup.**
  - Deterministic: filler removal, spoken punctuation and commands, dictionary replacements,
    snippet expansion.
  - Optional LLM step: grammar fixes, self-corrections ("actually, make that 3pm"), lists, and style
    per app category.
- **Style categories.** Personal messages, work messages, email, other. Tones: formal, casual,
  very casual, excited. The category is chosen from the focused app.
- **Other surfaces.**
  - Dictionary: vocabulary hints and replacements.
  - Snippets: a spoken trigger expands to saved text.
  - Transforms: saved rewrite prompts.
  - Scratchpad.
  - Insights: words, WPM, streak, app categories, fixes.
  - Notetaker: long recordings, transcript and summary.
  - History with a retention policy, and a privacy mode.
- **What we add.**
  - Linux support.
  - BYOK across many STT and LLM providers.
  - Local offline models.
  - A configurable provider fallback chain.
  - A self-hostable gateway.

## Repository layout

```
desktop/            Tauri 2 app
  src-tauri/        Rust core: audio, hotkeys, STT providers, text pipeline, insertion, storage
  src/              React UI: main window and overlay pill
server/             OpsTrellis Cloud: Hono (Node) API + Postgres + Stripe, with a React dashboard in server/web
docs/               This file, TASKS.md, TESTING.md
```

## Desktop (Rust core)

| Module | Responsibility |
|---|---|
| `audio.rs` | Capture from the chosen mic with cpal, downmix to mono and resample to 16 kHz. Keeps the full buffer, sends 20–100 ms chunks to the active stream, and emits level events. |
| `hotkeys.rs` | `tauri-plugin-global-shortcut` with press/release events. Hold = push-to-talk, tap = hands-free toggle. Esc is registered only while recording, and Enter and Esc only while a command's rewrite waits for review in the pill. Ctrl+Shift+Space is always registered as a fallback dictate shortcut (press to start, press again to finish) unless a configured shortcut uses it. Single keys (Fn, Right ⌥, …) are handed to `fnkey.rs`. While the shortcut recorder in Settings is open the shortcuts are unregistered, so pressing one records it. CLI flags `--toggle`, `--command`, `--paste-last` and `--cancel` reach the running instance through single-instance, for desktop shortcut bindings. |
| `wayland.rs` | Wayland shortcuts, since apps can't grab keys there: the XDG GlobalShortcuts portal (GNOME 48+, KDE) over zbus, registering the app id `opstrellis` first because GNOME refuses unidentified apps; presses and releases go to the pipeline like `hotkeys.rs`. Without the portal on GNOME, custom shortcuts running the CLI flags are added with gsettings on request. |
| `fnkey.rs` | A single key (Fn/🌐, Right ⌘, Right ⌥, Right ⌃ or Right ⇧) as the dictate or command shortcut on macOS. Carbon hot keys can't register a bare modifier, so a listen-only Quartz event tap (Input Monitoring permission; retried every second until granted) watches flags-changed events in every app; it also captures such a key for the shortcut recorder. Hold = push-to-talk; double-tap = hands-free, next press finishes; a single tap or the key used with another key is discarded. macOS runs its own 🌐 action (emoji picker, input source) ahead of any tap, so while Fn is a shortcut it is set to "Do Nothing" through `TISUpdateFnUsageType` (what System Settings calls) and restored on switching away, on quit, or after a crash at the next launch. |
| `stt/` | Provider adapters (below). |
| `llm.rs` | OpenAI-compatible chat (OpenAI, Groq, OpenRouter, Ollama, LM Studio, vLLM, OpsTrellis Cloud) and the Anthropic Messages API. |
| `pipeline.rs` | Ties one dictation together: record → transcribe (with fallback) → text processing → LLM cleanup / command → insert → history → usage report. |
| `polish.rs` | Offline AI polish with S1-mini by Superwhisper (llama.cpp, linked statically): download, load and keep loaded while selected, Qwen3 chat prompt with thinking off and the model's style control line, token and time caps. Dictated text can't inject control tokens. |
| `text.rs` | Deterministic processing: fillers, spoken commands, dictionary (one longest-first pass; entries' spellings restored after capitalization, tone and AI cleanup), contextual spoken symbols and backticks, snippets, spacing and capitalization. |
| `context.rs` | Focused app name and window title (`active-win-pos-rs`) → app category and terminal detection. |
| `workspace.rs` | Repo-aware dictation. Indexes the folders in `workspaces` in memory, on a background thread at startup, when one is added, on Refresh, and when a dictation uses an index over 10 minutes old (one run per folder at a time; removing a folder cancels its run). It never starts a process: a walk that never follows symlinks, skips hidden entries, `node_modules`, `target`, `dist`, `build`, `out`, `coverage`, `vendor`, `Pods`, `__pycache__` and what a small subset of `.gitignore` excludes (read as text, ≤ 64 KB, not through symlinks), and stops at 20 000 files, 20 000 folders, depth 12 or 5 s. Then the type, function, method and constant names declared in source files (one regex per language family; no locals, test files or Rust `#[cfg(test)]` modules; files ≤ 512 KB, UTF-8, not minified; 64 MB / 5 s budget), ranked by use, top 3000. Contents never leave the indexing function. File names and path segments must be letters, digits and `._+-` (1–60 characters) to be hinted, listed or mentioned. Multi-word names get spoken forms as tech aliases of a `terms::Matcher` (the import rules plus: no bare-number word, no all-common phrase, no common word in a two-word form, nothing two names share or the term packs own). In editors, terminals and AI prompts the workspace named in the window title (or the only one) adds up to 40 transcription hints after the dictionary and pack terms, lists the names said (≤ 30) for AI polish under their own prompt rule, and turns `@name` into `@path` in AI prompts; with `workspaceReplace` on, a rule pass in `text.rs` writes spoken names as code in editors and terminals (dictionary and snippets win). |
| `insert.rs` | Clipboard paste with save/restore (arboard + enigo; on Wayland wtype, then ydotool, and an error naming the fix if neither works), with typing as a fallback. Also copies the selection for command mode. |
| `db.rs` | SQLite (rusqlite, bundled): history, dictionary, snippets, transforms, notes, settings (JSON). Purges history according to retention. |
| `recovery.rs` | Keeps the recording of a dictation whose transcription failed (in memory; optionally on disk, deleted once 24 h old, checked hourly) so it can be retried from History or the tray, and recovers one interrupted by a crash. |
| `secrets.rs` | API keys in the OS keychain (`keyring`: macOS Keychain, Secret Service), falling back to a `0600` file. |
| `cloud.rs` | Device-code sign-in to OpsTrellis Cloud, batched usage reporting, and the background loop that also runs the team library sync. |
| `team.rs` | The team library, synced down read-only from `GET /api/library` (the client rule in server/README.md): right after sign-in, every 5 minutes while signed in (doubling up to an hour after failures, at least 15 minutes after a 429), and on Sync now; never on the dictation path. One run at a time; each response (body ≤ 16 MB, ≤ 20 pages a run) is applied in one SQLite transaction by a pure function over the parsed response, and only if the stored cursor (`team_sync`: epoch, team, version) is still the one the request was sent with, so a sign-out or the setting turned off while a request is out drops its answer. Entries are untrusted: each is checked with the backup rules (lengths, control characters, `output`), snippets that expand `{clipboard}` are refused, and invalid ones are skipped and counted (`team_skipped`). They are stored in `dictionary` (`source = 'team'`), `snippets` and `transforms` with the server's id in `team_id`, read-only. `db.rs` decides what is used: the user's own entries (and built-in actions) win over a team entry with the same phrase or trigger as the matchers compare them (`terms::norm`), a team phrase equal to one of the user's outputs, or an action name; lists hand dictation the user's own first; team dictionary entries fill the dictionary budget the user's own leave, in arrival order, and are all dropped if they would make the matcher fail to build (checked once per dictionary change); at most 50 team snippets and 200 team actions; a team action runs only in command mode, when the instruction is exactly its name and no action of the user's matches, never as an auto action. A response with over 2000 entries, or that would store over 2100 team rows and skipped ids, is refused; error bodies are read up to 64 KB. Deleted on sign-out, a 401, a new server URL, leaving the team, and turning `cloud.teamLibrary` off; never in a backup. |
| `cli.rs` | Subcommands answered in `main.rs` before Tauri starts (no window, tray or single instance): `mcp`, `last`, `history`, `note`, `notes`, `ask`, `help` and `--help`/`-h`. An unknown subcommand, or a flag other than the app's own before a subcommand (`--json last`), prints help and exits 2; no arguments and the app's flags (`--background`, `--toggle`, `--command`, `--paste-last`, `--cancel`, `--version`, `-psn_…`) start or reach the app as before. |
| `mcp.rs` | `opstrellis mcp`: a tools-only MCP server over stdio (one JSON-RPC 2.0 message per line, at most 1 MiB; no batches). Dual-era: `initialize` negotiates 2025-11-25 or 2025-06-18; per-request `_meta` requests and `server/discover` use 2026-07-28. `server/discover` and the -32022 unsupported-version error list the same per-request versions (only 2026-07-28), and a legacy version in `_meta` gets -32022 instead of being served as legacy: legacy versions are only reached through `initialize`. Requests are read one at a time, but an `ask_user` call waits on its own thread, so `ping` and other calls are answered meanwhile and `notifications/cancelled` withdraws the question (no response is sent for it). stdout carries protocol messages only, written a whole line at a time; nothing the user dictated reaches stderr. |
| `ask.rs` | An agent's question for the user. The app listens on `<app data>/agent.sock` (0600; a socket path too long for the platform makes the feature unavailable, logged once); a requester thread per connection (at most 4) reads one question within 5 s, checks Settings → Agents (`enabled && voice`) and the pill, and waits 60 s from arrival, held while the user is speaking or OpsTrellis is processing an accepted answer, at least 15 s more after an attempt that sent nothing, 5 min at most (`ask::expired`). Turning agent access, voice questions or the pill off withdraws it, and consent is checked again before an answer is sent. One question at a time (`QUESTIONS`). While idle the overlay shows it in full, in the review card's window; a dictation whose key press came at least 2 s after the question became visible (the window shown and the overlay page reporting it draws the question, `question_shown`) answers it; one posted during a dictation counts from when it appears (`pipeline.rs` hands the cleaned text back instead of inserting it). The app never starts recording or grabs keys for a question. |
| `agent.rs` | The agent tools, shared by `mcp.rs` and `cli.rs`. Finds the data folder as Tauri does (`dirs::data_dir()` + the bundle identifier), re-reads `settings.json` → `agents` on every request (off unless the user turned it on), and reuses `db.rs` on a connection opened read-only, without migrations (`Db::open_existing`), so a read never changes the database file or its WAL (with the app closed, SQLite may leave an empty `-wal` and a `-shm` index). Only `add_dictionary_term` opens it read-write, for one transaction that adds a vocabulary hint (no replacement) with `source = 'agent'`. A missing database is reported, never created. |

### STT providers

There is one trait with two capabilities: `transcribe(wav)` (batch) and, optionally, `stream()`.
Users configure an ordered fallback chain of provider entries:
`{provider, model, api_key ref, base_url, language}`.

| Provider | Batch | Streaming |
|---|---|---|
| OpenAI-compatible (OpenAI, Groq, Fireworks, self-hosted speaches/whisper.cpp/vLLM, OpsTrellis Cloud) | `/audio/transcriptions` | OpenAI Realtime (`intent=transcription`) |
| Deepgram | `/v1/listen` | WebSocket `/v1/listen` |
| AssemblyAI | upload + poll | v3 Universal-Streaming WebSocket |
| Google Cloud Speech | v1 `speech:recognize` (API key) | — |
| Azure Speech | short-audio REST | — |
| ElevenLabs Scribe | `/v1/speech-to-text` | — |
| Local (official `sherpa-onnx` crate) | 27 curated models (Parakeet, Whisper, Moonshine, Qwen3-ASR, Cohere Transcribe, Canary, Omnilingual, Dolphin, SenseVoice, GigaAM, Zipformer, Tara…), commercial-use licences only, downloaded from sherpa-onnx's GitHub releases (Tara, our int8 ONNX export of Trelis/tara, from huggingface.co/mtr1920/sherpa-onnx-whisper-tara-int8 at a pinned revision); catalog in `stt/local.rs` | partials by re-decoding the growing buffer |

Fallback rules:
- If the stream fails at any point, the full recorded buffer is sent to the next provider in batch
  mode.
- If a batch request fails or times out, the next provider is tried.
- The dictionary is passed as provider-native vocabulary hints wherever supported: Deepgram
  `keyterm`, AssemblyAI `keyterms_prompt`, OpenAI `prompt`, Google `speechContexts`.

## OpsTrellis Cloud (server)

**Stack:** Hono on Node 22, Postgres through Drizzle, the Stripe SDK, `sherpa-onnx-node` for the
self-hosted model, and a React + Vite SPA served by the same process.

**Gateway.** The gateway is OpenAI-compatible, so the desktop needs no extra client code: OpsTrellis Cloud
is just an OpenAI-compatible provider with base URL `https://host/v1` and the user's API token as
the key.

- `POST /v1/audio/transcriptions`: the `model` is a catalog id (`deepgram/nova-3`,
  `groq/whisper-large-v3-turbo`, `local/moonshine-base`, …). The server routes it to the upstream
  using the platform key, meters the call, enforces the quota, and falls back along the catalog's
  fallback chain.
- `POST /v1/chat/completions`: an LLM proxy, metered in tokens.
- `GET /v1/models`.

**Desktop API** (Bearer API token):

| Endpoint | Purpose |
|---|---|
| `POST /api/device/start` | Returns `{device_code, user_code, verification_url, interval}`. |
| `POST /api/device/poll {device_code}` | Returns `200 {token}`, or `428` while pending. The user approves at `/device?code=…`. |
| `GET /api/me` | User, plan, usage this period, and limits. |
| `GET /api/catalog` | Providers and models with pricing. |
| `POST /api/usage` | Idempotent batch of BYOK/local usage events. Metadata only, never text. |
| `GET /api/team` | The user's team: `{team: null}` or `{team: {id, name, role, members, libraryVersion}}`. |
| `GET /api/library?since=N` | Team library changes after version `N`: `{team: {id, name} \| null, version, entries: [{id, kind, data, deleted}], more, reset}`, oldest first. A full sync (`since=0` or a `reset` answer) returns all live entries in one response; incremental syncs include tombstones and are paged at 500 (`more`, with `version` as the next cursor). Read-only on the desktop. Client rule: `server/README.md` → Library sync. |

**Web dashboard.**
- User pages: usage stats, cost by provider, plan and billing (Stripe Checkout and Portal),
  API tokens, devices, managed model preference, and the team (members, invite codes, shared library).
- Admin pages: users (role, plan, disable), model catalog and pricing, upstream provider keys
  (AES-GCM encrypted with `MASTER_KEY`), revenue versus provider cost.

**Teams.** A user is in at most one team (`orgs`, `org_members`) and joins with a 7-day, multi-use
invite code stored only as a SHA-256 hash (`org_invites`). Team admins edit a shared library of
dictionary entries, snippets and voice actions (`org_library`) on the dashboard; every write locks the
team row and bumps `orgs.library_version`, and deletes are tombstones, so desktops sync incrementally
with `GET /api/library?since=N`; tombstones are purged after 30 days, and a desktop whose cursor is
older is told to `reset`. A team whose last member leaves or is deleted goes with its library; one whose
last active admin is deleted or disabled gets its longest-standing active member as admin.

| Dashboard endpoint (session only) | Who |
|---|---|
| `POST /api/team {name}`, `POST /api/team/join {code}` | not in a team |
| `POST /api/team/leave`, `GET /api/team/members`, `GET /api/team/library` | member |
| `PATCH`/`DELETE /api/team/members/:userId`, `POST`/`GET /api/team/invites`, `DELETE /api/team/invites/:id`, `POST /api/team/library`, `PUT`/`DELETE /api/team/library/:id` | team admin |

Full request and response shapes: `server/README.md` → Teams.

**Billing.**

| Plan | Price | Managed minutes | Overage |
|---|---|---|---|
| Free | $0 | 15 / month | — |
| Pro | $12 / month | 600 / month | $0.02 / min through a Stripe Billing Meter, capped at `MAX_OVERAGE_MINUTES` (default 3000) a period |

- BYOK and local use are free and unlimited; they are tracked for stats only.
- Every gateway call records the provider cost (from the catalog) and the billed amount.
- Without Stripe keys configured, an admin assigns plans manually.

## Privacy

- API keys are stored in the OS keychain and never sent to OpsTrellis Cloud.
- History retention can be forever, 30 days, 7 days, or off ("privacy mode"). Audio is not stored
  by default.
- Usage sync sends metadata only and can be turned off.
- The microphone stream is open only while recording.
- On macOS, Accessibility and Microphone permissions are requested with explanations.
- AI agents (`opstrellis mcp` and the CLI) get nothing unless Settings → Agents is on, and then only
  the scopes the user allows. It is the user's consent for what agents are handed, not a security
  boundary against software already running as the user.
