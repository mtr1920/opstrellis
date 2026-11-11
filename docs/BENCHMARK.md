# Developer-speech benchmark

Vendors' accuracy claims for dictation are mostly self-reported and cannot be re-run. This benchmark
is small, and everything in it is in this repository, so anyone can re-run it and check the numbers.

It answers one narrow question: **for sentences a developer would dictate to an editor, a terminal or
an AI coding agent, how often do the technical terms come out exactly right, from the raw
transcription and after OpsTrellis's rule-based cleanup?**

## What is measured

- **Test set:** [`desktop/bench/dev-speech/sentences.tsv`](../desktop/bench/dev-speech/sentences.tsv),
  60 sentences of 6-18 spoken words. 54 contain technical terms: CLI tools and flags, file names and
  paths, languages, frameworks and databases, cloud and DevOps terms, and identifiers in
  `snake_case` (7 sentences) and `camelCase` (3). 6 are plain-English control sentences with no
  technical term. Each row has `spoken` (what a person says, in plain words: "run cube control get
  pods dash n staging"), `expected` (what should be typed: `run kubectl get pods -n staging`) and
  `terms` (the technical terms in `expected` that must appear exactly; 101 in total, so 303 across the three voices).
- **Audio:** [`synthesize.sh`](../desktop/bench/dev-speech/synthesize.sh) renders every `spoken` line
  with the macOS `say` command to 16 kHz mono 16-bit WAV, once per voice.
- **Harness:** the ignored test `bench_dev_speech` in
  [`desktop/src-tauri/src/stt/local.rs`](../desktop/src-tauri/src/stt/local.rs), next to the older
  `bench_local_models`. For every local model that is installed it transcribes every clip, with the
  same `transcribe_blocking` the app uses, and scores:
  - **WER raw / after rules:** word error rate against `expected`, with the existing benchmark
    normaliser (lower case, punctuation ignored). Summed over all clips: total word errors divided by
    total reference words.
  - **Terms raw / after rules:** share of the listed terms that appear in the transcript exactly,
    case-sensitively, as whole tokens (a multi-word term such as `docker compose` must appear as those
    consecutive tokens; sentence punctuation around a token is ignored).
  - **Real-time factor:** transcription time divided by audio duration, model load excluded.
  - "After rules" is `text::process` with the options `clean_up` in `pipeline.rs` builds for a terminal
    or code editor with AI off: technical and code mode, tech terms on, fillers and voice commands on,
    English, no dictionary, no snippets, no tone, no workspace index. It is the shipped behaviour, not a
    benchmark-only variant.
  - It prints one Markdown row per model and, with `--nocapture`, the 10 worst sentences per model
    (expected, raw, processed).

Nothing in the benchmark or its scripts uses the network.

## Reproduce

```sh
# 1. Audio (macOS). Prints which voices it used or skipped; the argument is the output directory.
desktop/bench/dev-speech/synthesize.sh /tmp/dev-speech-wavs

# 2. Models: Settings -> Local models in the app, or any directory with the same layout
#    (<dir>/sherpa-onnx-moonshine-tiny-en-int8/ containing the model files and an empty .complete file).
# 3. Run the benchmark. It scores every installed local model.
cd desktop/src-tauri && OPSTRELLIS_BENCH_DEV_WAVS=/tmp/dev-speech-wavs OPSTRELLIS_TEST_MODELS=<models dir> \
  cargo test --release bench_dev_speech -- --ignored --nocapture
```

`OPSTRELLIS_BENCH_DEV_TSV` points the harness at a different sentence file (default: the committed
one). `synthesize.sh` takes `VOICES="Samantha Daniel"` to choose other installed voices. Without an
argument it writes to a new temporary directory and prints it.

### Audio on Linux (not run here)

The macOS script could not be tried on Linux, and the following was **not run** when this document
was written. [`desktop/tools/mine_aliases.py`](../desktop/tools/mine_aliases.py) already generates
speech with the Kokoro int8 and Piper LibriTTS models from the sherpa-onnx `tts-models` release
(`kokoro-int8-en-v0_19`, `vits-piper-en_US-libritts_r-medium`). The same approach works here: read the
`spoken` column of `sentences.tsv`, skip the header, call
`tts.generate(spoken, sid=<voice>, speed=1.0)` from `sherpa_onnx.OfflineTts`, and write each result with
`soundfile` as 16-bit PCM named `<voice>-<nn>.wav`, where `<nn>` is the two-digit row number starting
at 01 (the harness maps the number back to the row). Any sample rate is accepted; the harness resamples
to 16 kHz. Resulting numbers are not comparable with the macOS-voice numbers below.

## Results

Measured 2026-11-12 on an Apple M3 Pro (`sysctl -n machdep.cpu.brand_string`), 12 cores, release build.
Model: Moonshine Tiny (`moonshine-tiny-en`, int8). Voices: the macOS built-in Samantha (en_US), Daniel
(en_GB) and Karen (en_AU), 60 sentences each, 180 clips.

| model | clips | WER raw | WER after rules | terms raw | terms after rules | real-time factor |
|---|---|---|---|---|---|---|
| Moonshine Tiny | 180 | 30.3% | 23.2% | 22.4% (68/303) | 33.7% (102/303) | 0.018 |

WER and term counts were identical in two runs. The real-time factor depends on machine load: this
machine was shared with other builds, and a run under heavier load measured 0.070.

## What this does and does not show

- **The speech is synthetic.** Text-to-speech voices are clear and steady, with no accent range, no
  background noise and no hesitation. Real speech is harder, so the absolute numbers are optimistic.
  The three voices are also all English from macOS; they say "cube control" the same way every time.
- **Only local models that were installed were measured.** That is one model here, Moonshine Tiny.
  No cloud service and no other product was run, so nothing here says how OpsTrellis compares with
  any of them.
- **The set is small.** 60 sentences by 3 voices is 180 clips and 303 scored terms (101 distinct), with the same 60
  sentences repeated per voice, so the voices are not independent samples. Differences of a few
  points between two runs of different models should not be read as meaningful.
- **The term list reflects OpsTrellis's own tech pack in part.** Many listed terms (kubectl, Postgres,
  Next.js) are entries in `desktop/src-tauri/terms/tech.json`, whose aliases were mined with TTS voices
  and local models by `tools/mine_aliases.py`; others (file names, flags, identifiers, `pytest`,
  `Prometheus`) are not in the pack. So "raw vs after rules" measures what the cleanup adds on terms
  it was built for, on speech of the same synthetic kind. It does not show that OpsTrellis is more
  accurate than another product, and it does not predict the gain on real speech.
- **Two sentence types are out of reach of the rules by design.** `camelCase` identifiers spoken as
  words ("get user by I D" for `getUserById`) can only be written as code when the workspace index
  knows the name, and the benchmark has no workspace. Single-dash flags ("dash n") are not turned into
  `-n` by the shipped rules, which join `dash dash <word>`, `dot`, `slash`, `underscore` and
  `tilde slash`. Both lower the "after rules" score and are left in on purpose.
- **Failures are visible.** In the 10 worst sentences some transcripts are empty (`Daniel-05`,
  `Daniel-17`, `Karen-10`) and one is a run of `-f` (`Samantha-10`); the model's own output, not the
  rules, is the limit there. `expected` is exactly what should be typed in a terminal, so a rule that
  writes a capitalised product name where a lower-case command belongs counts as a miss: the tech pack
  maps "S S H" to `SSH`, while the expected command is `ssh`.
- **Case-sensitive, whole-token term matching is strict.** `Kubernetes,` counts, `kubernetes` does not.
  A model that gets every word right but writes it in another case scores zero on that term.

## Adding rows

- **Other local models:** install them with the app's downloader (Settings -> Local models, or the
  existing `OPSTRELLIS_BENCH_DOWNLOAD=1 ... bench_local_models` switch described in
  [TESTING.md](TESTING.md), which downloads into `OPSTRELLIS_TEST_MODELS`). `bench_dev_speech` then scores
  every installed model in the models directory and prints one row each; it never downloads.
- **Other products:** transcribe the same WAVs with the product, save the transcripts, and score them
  the same way, either with the helper functions in `bench_dev_speech` (word error rate, whole-token
  case-sensitive term match) or an equivalent that follows the definitions above. Report the raw
  output of the product, and say which voices, settings and version were used. Do not change the
  sentence file to suit a product.
- **Other voices or languages:** `VOICES=... synthesize.sh` for other macOS voices; for other TTS
  engines write `<voice>-<nn>.wav` files as described above.
- **More sentences:** append rows to `sentences.tsv` (three tab-separated columns, header kept). The
  WAV number is the row number, so regenerate all audio after changing the file.
