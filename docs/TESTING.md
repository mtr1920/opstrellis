# Testing

## Server (OpsTrellis Cloud)

Run from `server/` with a local Postgres (`opstrellis_test` database, rebuilt from the migrations each run)
and the self-hosted model downloaded (`npm run fetch-model`):

```sh
npm test        # builds the dashboard (pretest), then vitest
```

| File | Covers |
|---|---|
| `test/billing.test.ts` | Included/overage split in integer ms, per-started-minute billing that sums to `ceil(total)`, plan/quota rules, billing periods, streak math |
| `test/auth.test.ts` | Signup (first user admin, `ADMIN_EMAIL` startup admin, `ALLOW_SIGNUP`, global and per-IP daily caps), login, sessions, logout, password change, CSRF origin and JSON content-type checks, login rate limits (IP, IPv6 /64, account + IP, account ceiling), disabled users, API tokens (create, use, revoke, isolation), device-code flow (pending 428, approve, one-time token, expiry 400, deny) |
| `test/usage.test.ts` | `/api/usage` idempotency and validation (text rejected, ≤500), stats (words, WPM, current/longest streak, time zones, app category, per model), catalog, preferences |
| `test/gateway.test.ts` | `/v1/audio/transcriptions` with the real sherpa-onnx Moonshine model on `test_wavs/0.wav` ("yellow lamps"), response formats, `auto`, fallback (no key → skip; unreachable upstream → error event → local), 502 when all fail, free 402, Pro overage $0.02/min, concurrency under the advisory lock, validation, 25 MB limit, `/v1/models` |
| `test/adapters.test.ts` | Each upstream adapter against a local fake provider (endpoint, auth header, params, vocabulary hints, response mapping); `/v1/chat/completions` OpenAI proxy and Anthropic translation with token metering |
| `test/stripe.test.ts` | Webhook signatures via `stripe.webhooks.generateTestHeaderString` on the real endpoint; checkout → Pro → period sync → past_due → recovery → cancel; Billing Meter reporting (identifier = usage event id, sent once) against a fake Stripe API |
| `test/admin.test.ts` | Every admin route is 403 for non-admins; user search/pagination, plan/role/disable, usage view, global stats, catalog CRUD, encrypted provider keys never returned |
| `test/web.test.ts` | The built dashboard is served over real HTTP (index, client routes, assets, JSON 404s) |
| `test/live.test.ts` | Real OpenAI/Groq/Deepgram/AssemblyAI/ElevenLabs/Google/Azure/Anthropic/Stripe calls; each runs only when its key env var is set (`NODE_USE_ENV_PROXY=1` behind a proxy) |

## Desktop

### Unit and integration tests (`desktop/src-tauri`)

```sh
cargo test            # unit/integration tests; the fallback test uses the real Moonshine model in /tmp/opstrellis-models (OPSTRELLIS_TEST_MODELS)
cargo test -- --ignored deepgram_live      # live provider tests: DEEPGRAM_API_KEY, ASSEMBLYAI_API_KEY, OPENAI_API_KEY, LLM_*
OPSTRELLIS_BENCH_DOWNLOAD=1 cargo test --release bench_local_models -- --ignored --nocapture
cargo test --release terms_speed -- --ignored --nocapture   # cost of the tech-term pass per dictation
# Offline AI polish with the real model (download s1-mini-q4_k_m.gguf into <dir>/polish/): output and speed
OPSTRELLIS_POLISH_MODELS=<dir> cargo test --release polish_live -- --ignored --nocapture
# Engine plumbing with any small Qwen3 GGUF, random weights are fine (load, prompt, token and time caps)
OPSTRELLIS_POLISH_GGUF=<file.gguf> cargo test polish_engine -- --ignored --nocapture
# Re-mine tech-term aliases (Kokoro/Piper TTS from the sherpa-onnx tts-models release + the local STT models); review the output, then edit src-tauri/terms/tech.json
python3 ../tools/mine_aliases.py --tts DIR --models DIR --out aliases.json
```

They cover:
- Text pipeline: fillers, voice commands, "scratch that", dictionary replacements (precedence, overlaps, casing kept through capitalization/tone/AI recasing, paths and URLs left alone), spoken symbols by context (literal words kept), spoken quotes and backticks, backticks in AI prompts, snippets, tones, and URL-safe capitalization. `cargo test --release process_speed -- --ignored --nocapture` measures the whole pass with 300 dictionary entries.
- Stats: streaks and WPM.
- History privacy mode, with aggregates that survive deletion.
- App classification.
- Resampler and WAV.
- Provider request and response mapping, and the streaming protocols (Deepgram, AssemblyAI, OpenAI Realtime) against a scripted local WebSocket peer.
- LLM output guards.
- Notetaker mixing.
- Fallback from a failing provider to the real local model.

### End-to-end tests on a headless Linux desktop (`desktop/e2e`)

`env.sh` starts Xvfb, openbox and PulseAudio with a virtual microphone (`vmic`). Speech is played
into it with `paplay`. The real app binary (`npx tauri build --debug --no-bundle`) is driven with
real global hotkeys (XTest), and the tests assert what lands in another application's text field.

| Script | Scenarios |
|---|---|
| `scenarios.sh` | Push-to-talk hold · short 1.6 s hold stays push-to-talk · hands-free tap/tap (17 s) · Esc cancel inserts nothing · dictionary + snippet applied · dictionary casing kept at a sentence start and through an AI that recases everything · offline polish selected but not downloaded inserts the rule-based text · offline polish leaves terminals alone · switching windows while a dictation is processing leaves the text on the clipboard instead of pasting into the other window · paste into a real terminal (lxterminal, Ctrl+Shift+V) · fallback from an unreachable cloud provider to the local model · paste-last shortcut, also with history off · against a local OpenAI-compatible stand-in that logs every request: AI cleanup + auto action in **one** request (and off by default) · a whole-snippet dictation makes **no** request · an unreachable AI inserts the rule-based text · a terminal titled "Claude Code" gets prompt cleanup and its auto action, a plain terminal no rewrite · command mode replaces the selection · an action with a copy output leaves the text and puts the result on the clipboard · clipboard snippets stay local (whole/mixed) · action output preserves code/list indentation · an agent-named shell directory cannot enable automatic rewriting |
| `cloud_test.sh` | Starts OpsTrellis Cloud, signs up, runs the device-code flow, dictates through the cloud gateway (metered minutes > 0), then dictates locally and checks that the usage metadata synced to the server |
| `ui_test.sh` | WebDriver (tauri-driver + WebKitWebDriver) against the real app: pages render; add a dictionary entry through the UI; correcting a dictation in History auto-learns dictionary entries; a fix that does not sound like a mis-hearing is learned on the second correction and Undo removes it; the auto-action choice is saved; Settings → Test transcribes the bundled sample; Notetaker records from the virtual mic, shows a live transcript and saves the meeting note; Style → AI prompts tab, prompt style, notes and an app rule are saved; a new action with a copy output is saved; the AI Test button and Polish again report an unreachable provider and leave the text unchanged; meeting templates are listed · restarting during outcome linger keeps the pill visible · re-polishing preserves browser context and concurrent corrections · a note called Meeting prep stays in Notes · changing the primary shortcut retains the fallback toggle |

### Browser concurrency regressions

With the Vite dev server on port 1430, run from `desktop/`:

```sh
# Uses the same external Playwright installation as design_check.mjs (PW_DIR, default /tmp/pw).
CHROME=/usr/bin/chromium node e2e/review_regressions.mjs
```

These drive the real React UI with controlled IPC responses: tray toggles during pending edits,
stale save acknowledgements and failed-save rollback, closed or superseded meeting follow-ups,
and out-of-order history searches. They do not replace the native audio/hotkey/provider tests.

### Local model benchmark

Each model was downloaded with the app's downloader. The run used 4 vCPUs, int8 models, and
LibriSpeech clips 1089-134686-0000/0001 (23 s of audio). Two clips are a smoke test of accuracy,
not a full WER evaluation.

| Model | WER | Real-time factor | Load |
|---|---|---|---|
| Moonshine Tiny | 2.1% | 0.031 | 1.5 s |
| Moonshine Base | 2.1% | 0.046 | 2.5 s |
| Parakeet TDT 0.6B v2 | 2.1% | 0.049 | 5.9 s |
| Whisper Base | 3.1% | 0.133 | 1.5 s |
| SenseVoice Small | 0.0% | 0.026 | 3.7 s |

Harder audio: 180 clips, 30 each from the Open ASR Leaderboard's AMI, Earnings-22, GigaSpeech,
LibriSpeech test-other, SPGISpeech and VoxPopuli test sets, on an Apple M3 Pro. The WER here uses the
benchmark's simple normalizer, so compare rows with each other, not with leaderboard numbers.
Run it with `OPSTRELLIS_BENCH_CLIPS=<dir with trans.txt and wavs>`.

For how often developer terms come out exactly right, see the reproducible [developer-speech benchmark](BENCHMARK.md).

| Model | WER | Real-time factor | Load |
|---|---|---|---|
| Moonshine Tiny | 22.1% | 0.018 | 1.0 s |
| Parakeet TDT 0.6B v2 | 19.4% | 0.086 | 4.2 s |
| Parakeet Unified 0.6B | 18.3% | 0.089 | 3.7 s |

### Measured latency (key release → text in the target app)

Measured with the local Moonshine Tiny model on 4 vCPUs.

| Recording | Latency |
|---|---|
| 6.6 s recording | 250–500 ms |
| 16.7 s recording | 620–850 ms (with live partials) |
| Through the cloud gateway (server on the same host, 2 threads) | ~1.3 s |

### Long dictations with a local model

Local models decode each phrase when a pause (≥ 0.6 s) ends it, and any phrase that runs past
30 s without a pause, so on release only the last phrase is left. Measured with Moonshine Tiny on 4 vCPUs, using the
two LibriSpeech clips repeated with 0.6 s gaps:

| Dictation | One decode on release (before) | Phrase by phrase (now) |
|---|---|---|
| 25 s | 2.7 s, 4.5% WER | last phrase only, 3.0% WER |
| 49 s | 9.8 s, 4.5% WER | last phrase only, 3.0% WER |
| 98 s | 57 s, unusable (391 words for 264) | last phrase only, 3.0% WER |
| 196 s | 281 s, unusable (1111 words for 528) | last phrase only, 3.0% WER |

Decoding the last phrase takes about 1.1 s for the 16.7 s clip, and much less for a typical
few-second phrase. With no pauses at all, the 30 s cut keeps it at about 2 s (4.5% WER).
