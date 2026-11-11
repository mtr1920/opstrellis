# OpsTrellis — architecture

OpsTrellis is an open, cross-platform (macOS + Linux) voice dictation app. You hold a hotkey, speak, and
cleaned-up text is inserted into whatever app has focus. Speech-to-text (STT) providers and LLMs are
pluggable: bring your own key (BYOK), use local models, or use the managed OpsTrellis Cloud.

## Research summary (what we are matching)

- **Interaction.** Hold the hotkey to talk (push-to-talk). A quick tap toggles hands-free mode. On
  macOS the shortcut can be the Fn key: hold to talk, double-tap for hands-free. Esc cancels, but
  only while recording. A shortcut pastes the last transcript.
- **Command mode.** Select text, hold the command hotkey and speak an instruction ("make this more
  formal"). The rewritten text replaces the selection.
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
| `hotkeys.rs` | `tauri-plugin-global-shortcut` with press/release events. Hold = push-to-talk, tap = hands-free toggle. Esc is registered only while recording. Ctrl+Shift+Space is always registered as a fallback dictate shortcut (press to start, press again to finish) unless a configured shortcut uses it. Single keys (Fn, Right ⌥, …) are handed to `fnkey.rs`. While the shortcut recorder in Settings is open the shortcuts are unregistered, so pressing one records it. CLI flags `--toggle`, `--command`, `--paste-last` and `--cancel` reach the running instance through single-instance, for desktop shortcut bindings. |
| `wayland.rs` | Wayland shortcuts, since apps can't grab keys there: the XDG GlobalShortcuts portal (GNOME 48+, KDE) over zbus, registering the app id `opstrellis` first because GNOME refuses unidentified apps; presses and releases go to the pipeline like `hotkeys.rs`. Without the portal on GNOME, custom shortcuts running the CLI flags are added with gsettings on request. |
| `fnkey.rs` | A single key (Fn/🌐, Right ⌘, Right ⌥, Right ⌃ or Right ⇧) as the dictate or command shortcut on macOS. Carbon hot keys can't register a bare modifier, so a listen-only Quartz event tap (Input Monitoring permission; retried every second until granted) watches flags-changed events in every app; it also captures such a key for the shortcut recorder. Hold = push-to-talk; double-tap = hands-free, next press finishes; a single tap or the key used with another key is discarded. macOS runs its own 🌐 action (emoji picker, input source) ahead of any tap, so while Fn is a shortcut it is set to "Do Nothing" through `TISUpdateFnUsageType` (what System Settings calls) and restored on switching away, on quit, or after a crash at the next launch. |
| `stt/` | Provider adapters (below). |
| `llm.rs` | OpenAI-compatible chat (OpenAI, Groq, OpenRouter, Ollama, LM Studio, vLLM, OpsTrellis Cloud) and the Anthropic Messages API. |
| `pipeline.rs` | Ties one dictation together: record → transcribe (with fallback) → text processing → LLM cleanup / command → insert → history → usage report. |
| `polish.rs` | Offline AI polish with S1-mini by Superwhisper (llama.cpp, linked statically): download, load and keep loaded while selected, Qwen3 chat prompt with thinking off and the model's style control line, token and time caps. Dictated text can't inject control tokens. |
| `text.rs` | Deterministic processing: fillers, spoken commands, dictionary (one longest-first pass; entries' spellings restored after capitalization, tone and AI cleanup), contextual spoken symbols and backticks, snippets, spacing and capitalization. |
| `context.rs` | Focused app name and window title (`active-win-pos-rs`) → app category and terminal detection. |
| `insert.rs` | Clipboard paste with save/restore (arboard + enigo; on Wayland wtype, then ydotool, and an error naming the fix if neither works), with typing as a fallback. Also copies the selection for command mode. |
| `db.rs` | SQLite (rusqlite, bundled): history, dictionary, snippets, transforms, notes, settings (JSON). Purges history according to retention. |
| `secrets.rs` | API keys in the OS keychain (`keyring`: macOS Keychain, Secret Service), falling back to a `0600` file. |
| `cloud.rs` | Device-code sign-in to OpsTrellis Cloud and batched usage reporting. |

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

**Web dashboard.**
- User pages: usage stats, cost by provider, plan and billing (Stripe Checkout and Portal),
  API tokens, devices, managed model preference.
- Admin pages: users (role, plan, disable), model catalog and pricing, upstream provider keys
  (AES-GCM encrypted with `MASTER_KEY`), revenue versus provider cost.

**Billing.**

| Plan | Price | Managed minutes | Overage |
|---|---|---|---|
| Free | $0 | 15 / month | — |
| Pro | $12 / month | 600 / month | $0.02 / min through a Stripe Billing Meter |

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
