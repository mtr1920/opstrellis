# Task checklist

## Desktop core
- [x] Tauri 2 scaffold, tray, single instance, main window + overlay window
- [x] Audio capture (cpal; native PulseAudio/PipeWire on Linux, CoreAudio on macOS) → 16 kHz mono, level meter
- [x] Global hotkeys: hold = push-to-talk, tap = hands-free, Esc cancel, command mode, paste-last; CLI `--toggle/--cancel/--command`
- [x] Text insertion: clipboard paste + restore, terminal detection, Wayland tools, typing fallback
- [x] Local STT (sherpa-onnx: Moonshine, Parakeet, Whisper, SenseVoice) + in-app model downloader + live partials
- [x] STT providers: OpenAI-compatible (batch + realtime), Deepgram (batch + WS), AssemblyAI (batch + WS), Google, Azure, ElevenLabs, OpsTrellis Cloud
- [x] Fallback chain with buffered audio
- [x] Text pipeline: fillers, voice commands, dictionary, snippets, tones
- [x] LLM cleanup (OpenAI-compatible + Anthropic), style per app category, self-correction, output guards
- [x] Command mode / transforms on selected text
- [x] SQLite storage, history retention / privacy mode, keychain secrets
- [x] Cloud sign-in (device code) + usage sync
- [x] Notetaker (mic + optional system audio, chunked transcription, AI summary)

## Desktop UI
- [x] Home, Notetaker, Insights, Dictionary, Snippets, Style, Transforms, Scratchpad, Settings, Onboarding
- [x] Overlay pill (waveform, partials, cancel/stop, sound cues)
- [x] Quiet Glass redesign, no gradients (docs/DESIGN.md), enforced by e2e/design_check.mjs

## Server (OpsTrellis Cloud)
- [x] Auth (email/password, sessions, API tokens, device flow), roles
- [x] Gateway `/v1/audio/transcriptions`, `/v1/chat/completions`, `/v1/models` with metering, quota, fallback
- [x] Upstream adapters + self-hosted local model
- [x] Usage ingestion, stats, cost tracking
- [x] Plans, Stripe Checkout/Portal/webhooks, metered overage
- [x] Admin: users, catalog/pricing, provider keys, revenue vs cost
- [x] Dashboard UI
- [x] Quiet Glass redesign (server/web/design_check.mjs)

## Validation
- [x] Rust unit tests (text, providers' request/response, fallback, stats)
- [x] Local model accuracy (WER) + latency benchmark
- [x] Desktop E2E under Xvfb: hotkey → virtual mic → text lands in a focused app (8 scenarios)
- [x] UI E2E through WebDriver against the real app
- [x] Server integration tests (Postgres, billing math, Stripe webhook signatures, gateway → local model)
- [x] Desktop → Cloud gateway E2E
- [x] Packaging: .deb (14.8 MB) and AppImage (88.8 MB) built; installed .deb passes the dictation E2E; macOS .dmg via CI workflow

## Blocked here (needs credentials/network)
- Live calls to OpenAI / Deepgram / AssemblyAI / Google / Azure / ElevenLabs / Groq / Anthropic / Stripe (gated tests exist)
- macOS build/signing (CI runner + Apple Developer ID)

## Review round (3 independent reviewers → verified fixes)
- [x] Desktop core: 14 findings fixed (tap/hold timing, settings resilience, stream error closes, clipboard images, language-aware fillers, technical contexts, macOS key codes, notetaker drift/retry, retention, Wayland paste, hotkey deadlock, empty results, symbol dictionary entries)
- [x] Desktop UI: settings-save lockout, onboarding key/id, Scratchpad stale select, concurrent downloads, notetaker finishing state, cloud sign-in cleanup
- [x] Server: 10 findings fixed (WAV duration forgery, local-model OOM, quota races, LLM body passthrough, Stripe ordering, XFF spoofing, ADMIN_EMAILS, body limits, undecryptable keys, double approve)
- [x] Rebuilt .deb / AppImage with the new UI and fixes; installed .deb passes dictation + UI E2E; AppImage passes dictation E2E

## Roadmap phases 1–2 ([implementation plan](IMPLEMENTATION_PLAN.md))
- [x] Pill feedback: mic-ready state (start cue when the mic delivers audio), transcribing/polishing stages, hands-free timer, outcome messages (inserted, copied, warnings, errors) that linger, errors shown in the pill rather than only the hidden main window
- [x] One LLM call per dictation (auto action folded into cleanup), whole-snippet dictations skip the LLM, 10 s cleanup timeout with rule-based fallback
- [x] Paste-last with history off; hands-free auto-stop (2 min silence / 30 min); meetings stored as their own note kind
- [x] App rules (Style → Apps) over whole-word built-in rules; "AI prompts" style category; AI agents in terminals (Claude Code, Codex, Aider…) treated as prompts
- [x] Prompt mode (clean / structured) and "how you write here" notes per category
- [x] Actions with a copy output; built-in Reply and Summarize; command output guard
- [x] Snippet placeholders `{date}` `{weekday}` `{time}` `{clipboard}`
- [x] Meeting templates (General, Sales call, Interview, 1:1, Standup), follow-up email and CRM note drafts, copy action items
- [x] Navigation 9 → 6 (Notes and Library tabs), AI setup in Settings → AI with Test, History original/polish again/show more, tray (copy last, AI polish, meeting), onboarding teaches command mode

## Recovery, repo-aware dictation, agents, teams and a security pass
- [x] Never lose a dictation: a failed recording is kept in memory and retried from History or the tray (in parts of at most 30 s); optional on-disk retention for 24 hours ("failed" or "all"), with interrupted dictations recovered at launch
- [x] Meetings: "Me / Them" labels when a second input is recorded; auto-stop after 15 minutes without transcribed speech or at 4 hours
- [x] Review card before a long command-mode rewrite replaces the selection (Enter inserts, Esc discards)
- [x] Write in another language than the one spoken, per style category (needs a cloud or OpenAI-compatible AI provider)
- [x] Workspaces: file and identifier names from chosen folders as hints, spoken names written as identifiers, `@file` mentions in AI prompts
- [x] Term packs: three more built-in packs (off by default) and imported packs with a preview
- [x] Agent access (off by default): MCP server and CLI over dictations, notes and the dictionary; an agent can ask a question the user answers by voice
- [x] Team library: teams, invite codes and a shared library on OpsTrellis Cloud; read-only sync on the desktop
- [x] Backup, restore and dictionary list import; opt-in update check
- [x] Security review of the desktop app, installer, CI and server, with fixes
- [x] Developer-speech benchmark ([BENCHMARK.md](BENCHMARK.md))

Not run in this round: the Linux end-to-end suites (`desktop/e2e`). The review card, voice answers,
the tray items and folder drag-and-drop were not exercised in the running app.
