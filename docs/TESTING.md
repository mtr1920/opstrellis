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
# Local polish bake-off: every installed model in <dir>/polish/ on desktop/bench/polish/cases.tsv (below)
OPSTRELLIS_POLISH_MODELS=<dir> [OPSTRELLIS_POLISH_ONLY=id,id] [OPSTRELLIS_POLISH_OUT=<file>] cargo test --release bench_polish -- --ignored --nocapture
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

`stt/live.rs` commits a dictation piece by piece at pauses while it is recorded, so on release only
the last piece is left (and often not even that: a draft of it is decoded once speech stops). The
ignored test `bench_live_dictation` replays long dictations as the recording thread would see them
(a tick every 100 ms; a decode still running when the key is released counts as waiting) through
the loop that shipped before (`old`: a cut at every ≥ 0.6 s pause by loudness, a preview of the last
5 s), through `stt/live.rs` (`new`), and as one pass over the whole dictation on release (`whole`).
Each dictation is five consecutive sentences of one voice joined with pauses of 0.25–1.2 s, released
0.4 s after the last word; `quiet` is the same audio at -24 dB, `noisy` with noise at 10 dB SNR.

```sh
desktop/bench/dev-speech/synthesize.sh /tmp/dev-wavs
TSV=desktop/bench/names/sentences.tsv desktop/bench/dev-speech/synthesize.sh /tmp/names-wavs
cd desktop/src-tauri
OPSTRELLIS_BENCH_DEV_WAVS=/tmp/dev-wavs OPSTRELLIS_TEST_MODELS=<models dir> \
  cargo test --release bench_live_dictation -- --ignored --nocapture
# Names and brands said as written, with every term of the file as the vocabulary (hotwords):
OPSTRELLIS_BENCH_DEV_TSV=$PWD/../bench/names/sentences.tsv OPSTRELLIS_BENCH_DEV_WAVS=/tmp/names-wavs \
  OPSTRELLIS_BENCH_VOCAB=terms OPSTRELLIS_TEST_MODELS=<models dir> cargo test --release bench_live_dictation -- --ignored --nocapture
```

Parakeet Unified 0.6B (int8) on an Apple M-series Mac, 36 dev-speech dictations (704 s) and 18
names dictations (334 s). "As written" counts casing and punctuation too; "after rules" is
`text::process` as in `bench_dev_speech`. The release time includes waiting for a decode that was
still running when the key was released.

| Set | Loop | WER as written | WER after rules | Terms exact | Release → text (median / p95) |
|---|---|---|---|---|---|
| dev, clean | old | 54.9% | 9.7% | 51.8% | 602 / 1201 ms |
| dev, clean | new | 54.4% | 9.7% | 52.5% | 481 / 852 ms |
| dev, clean | whole | 53.5% | 8.8% | 53.5% | 2101 / 2353 ms |
| dev, quiet | old | 55.5% | 10.0% | 52.5% | 456 / 607 ms |
| dev, quiet | new | 54.0% | 9.7% | 56.1% | 723 / 1002 ms |
| dev, noisy | old | 53.9% | 11.2% | 50.2% | 1375 / 2502 ms |
| dev, noisy | new | 54.0% | 11.2% | 51.5% | 673 / 1112 ms |
| names, clean | old | 18.3% | 10.5% | 45.5% | 557 / 649 ms |
| names, clean | new | 17.3% | 11.6% | 47.9% | 479 / 746 ms |
| names, noisy | old | 19.4% | 14.2% | 44.2% | 1726 / 2419 ms |
| names, noisy | new | 20.1% | 14.0% | 41.2% | 710 / 1542 ms |
| names + vocabulary, clean | old | 18.3% | 9.6% | 47.9% | 585 / 832 ms |
| names + vocabulary, clean | new | 12.0% | 8.0% | 73.9% | 458 / 1135 ms |
| names + vocabulary, noisy | old | 19.4% | 13.6% | 46.1% | 1839 / 2246 ms |
| names + vocabulary, noisy | new | 14.9% | 9.1% | 66.1% | 798 / 2059 ms |

The old loop cannot use the vocabulary (its "after rules" column still gains from the product names
added to the tech pack). The vocabulary rows were measured with hotword strength 1.0, the rest
before the product names were added. With previews on (dev, clean, first voice), the new
preview took back 28.1 words per dictation where the old one took back 44.4
(`OPSTRELLIS_BENCH_DETAIL=1` prints them). The decoding thread is busier while recording (0.43 s
per second of audio with previews, old 0.22 s), mostly from drafts.

Moonshine Tiny has no word timestamps, so it gets plain pieces of at least 8 s ending at pauses of
≥ 0.6 s. After rules: clean 22.2% → 17.1%, quiet 26.4% → 18.9%, noisy 29.1% → 31.3%. In noise the
old loop found no pause by loudness and decoded each dictation in one pass.

Hotword strength (`OPSTRELLIS_HOTWORDS_SCORE`) on the names clips one by one (`bench_dev_speech`
with `OPSTRELLIS_BENCH_VOCAB=terms`): none 12.8% WER and 37.6% of terms; 1.0 (shipped) 7.4% and
68.5%; 1.5 9.4% and 71.5%; 2.5 16.7% and 78.8% (words that weren't said turn into terms); 3.5
unusable. On `desktop/bench/products/sentences.tsv` (AI tools and developer products, plus control
sentences with look-alike words: cloud, code, cursor, codecs) 1.5 repeated a hotword ("Claude Claude
Claude"), wrote "Codex Claude" for "Codex CLI" and dropped the end of a sentence; 1.0 did none of
that. There, at 1.0, the vocabulary took WER after rules from 4.8% to 3.0% and terms from 61.1% to
80.0%, with the control sentences unchanged.

These are synthetic voices (macOS `say`), so pauses and pronunciation are cleaner than people's;
compare rows with each other.

### Local AI polish models (bake-off)

`bench_polish` runs [`desktop/bench/polish/cases.tsv`](../desktop/bench/polish/cases.tsv) through
each on-device model as the app does: cleanup models with the prompt they were trained on,
instruction models with the cloud cleanup prompt (and an action's prompt for the action cases). The
36 cleanup cases are Parakeet-style text after the rule pass: self-corrections, repeats, lists,
numbers, emails, grammar, AI prompts with code names, questions and instructions that must not be
answered, and text that is already right and must come back unchanged ("left alone"). The 9 action
cases (Prompt engineer, Make formal, Shorten, Bullet points) are scored on the facts they must keep;
their outputs were also read by hand. Apple M-series Mac, Metal, Q4_K_M or Q8_0 files.

| Model | Kind | Exact | WER as written | WER | Left alone | Words lost | Action facts kept | Median / p95 |
|---|---|---|---|---|---|---|---|---|
| **Spokenly Cleanup 1** (shipped, recommended) | cleanup | 57% | 16% | 8% | 100% | 5 | - | 70 / 221 ms |
| **S1-mini** (shipped) | cleanup | 51% | 13% | 7% | 70% | 9 | - | 88 / 190 ms |
| SpeakoFlow Mini 0.8B | cleanup | 41% | 20% | 11% | 70% | 8 | - | 276 / 678 ms |
| **Qwen3.5 4B** (shipped, recommended for actions) | instruction | 49% | 15% | 9% | 60% | 6 | 90% | 1112 / 2727 ms |
| Qwen3.5 2B | instruction | 32% | 28% | 22% | 80% | 13 | 90% | 499 / 1246 ms |
| Qwen3.5 0.8B | instruction | 27% | 33% | 26% | 70% | 23 | 87% | 258 / 1017 ms |
| Ministral 3 3B | instruction | 11% | 49% | 50% | 20% | 43 | 81% | 769 / 7463 ms |

- S1-mini dropped "Translate this to French." (it acted on the text), turned a plain list of four
  items into bullets and added line breaks to short emails. Spokenly Cleanup 1 changed nothing that
  was right, but leaves spoken symbols ("dot ts") to the rule pass after it.
- Qwen3.5 4B rewords correct text when only cleaning up ("has been moved"), but is the only model
  that ran the actions properly: a structured prompt from rambling speech, a formal rewrite, a real
  shortening, real bullet points. Qwen3.5 2B mostly just cleaned up the action cases; Ministral 3 3B
  added content ("use pg_dump") and commentary.
- Granite 4.0 1B outputs only "!" on Metal with this llama.cpp (fine on the CPU), so it isn't offered.
- Prompt-lookup drafting gave the same outputs byte for byte and cut S1-mini from 122 to 88 ms and
  Spokenly Cleanup 1 from 126 to 70 ms at the median (p95 311 → 190 ms and 401 → 221 ms). Qwen3.5 is
  a hybrid model whose cache can't be trimmed, so it decodes without drafts.

