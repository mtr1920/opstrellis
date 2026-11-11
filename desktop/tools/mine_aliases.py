#!/usr/bin/env python3
"""Mine mis-hearings of built-in terms (src-tauri/terms/tech.json, or a pack with --terms) from real speech models.

Each term's spoken forms ("say") are synthesized with several quantized TTS voices (Kokoro int8 and
Piper LibriTTS from the sherpa-onnx releases), transcribed with the app's local models, and what
the models write instead of the term becomes a candidate alias:

  - seen at least twice (across voices and models), at most 4 words;
  - a single dictionary word is rejected (it would rewrite ordinary text);
  - only dictionary words -> "techAliases" (applied in code editors and terminals only);
  - otherwise -> "aliases" (applied everywhere).

Candidates are written to --out for review; nothing is applied to the terms file automatically.

  pip install sherpa-onnx soundfile numpy
  python3 tools/mine_aliases.py --tts DIR --models DIR --out aliases.json
"""
import argparse, hashlib, json, multiprocessing as mp, os, re, sys
from collections import Counter, defaultdict

import numpy as np
import sherpa_onnx
import soundfile as sf

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
TERMS = os.path.join(ROOT, "src-tauri", "terms", "tech.json")
CARRIER = "I said {} again."
EXTRACT = re.compile(r"\bsaid\b[\s,]*(.+?)[\s,.]*\bagain\b", re.I)
KOKORO_VOICES = [0, 9]  # af (US), bm_george (UK); natural but ~3 s per clip on CPU
PIPER_VOICES = [10, 120, 200, 450, 650, 800]  # LibriTTS speakers; fast


def tts_engines(tts_dir):
    k = os.path.join(tts_dir, "kokoro-int8-en-v0_19")
    p = os.path.join(tts_dir, "vits-piper-en_US-libritts_r-medium")
    kokoro = sherpa_onnx.OfflineTtsModelConfig(
        kokoro=sherpa_onnx.OfflineTtsKokoroModelConfig(
            model=f"{k}/model.int8.onnx", voices=f"{k}/voices.bin", tokens=f"{k}/tokens.txt", data_dir=f"{k}/espeak-ng-data"),
        num_threads=4)
    piper = sherpa_onnx.OfflineTtsModelConfig(
        vits=sherpa_onnx.OfflineTtsVitsModelConfig(
            model=f"{p}/en_US-libritts_r-medium.onnx", tokens=f"{p}/tokens.txt", data_dir=f"{p}/espeak-ng-data"),
        num_threads=4)
    return [("kokoro", sherpa_onnx.OfflineTts(sherpa_onnx.OfflineTtsConfig(model=kokoro)), KOKORO_VOICES),
            ("piper", sherpa_onnx.OfflineTts(sherpa_onnx.OfflineTtsConfig(model=piper)), PIPER_VOICES)]


def recognizer(models_dir, name):
    d = lambda m: os.path.join(models_dir, m)
    if name.startswith("moonshine"):
        m = d(f"sherpa-onnx-{name}-en-int8")
        return sherpa_onnx.OfflineRecognizer.from_moonshine(
            preprocessor=f"{m}/preprocess.onnx", encoder=f"{m}/encode.int8.onnx", uncached_decoder=f"{m}/uncached_decode.int8.onnx",
            cached_decoder=f"{m}/cached_decode.int8.onnx", tokens=f"{m}/tokens.txt", num_threads=1)
    if name == "whisper-base":
        m = d("sherpa-onnx-whisper-base.en")
        return sherpa_onnx.OfflineRecognizer.from_whisper(
            encoder=f"{m}/base.en-encoder.int8.onnx", decoder=f"{m}/base.en-decoder.int8.onnx", tokens=f"{m}/base.en-tokens.txt",
            language="en", task="transcribe", num_threads=1)
    m = d("sherpa-onnx-nemo-parakeet-tdt-0.6b-v2-int8")
    return sherpa_onnx.OfflineRecognizer.from_transducer(
        encoder=f"{m}/encoder.int8.onnx", decoder=f"{m}/decoder.int8.onnx", joiner=f"{m}/joiner.int8.onnx",
        tokens=f"{m}/tokens.txt", model_type="nemo_transducer", num_threads=1)


def transcribe_all(args):
    models_dir, name, clips = args
    rec = recognizer(models_dir, name)
    out = []
    for clip in clips:
        samples, rate = sf.read(clip["wav"], dtype="float32")
        s = rec.create_stream()
        s.accept_waveform(rate, samples)
        rec.decode_stream(s)
        out.append({**clip, "model": name, "text": s.result.text.strip()})
    return out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--tts", required=True)
    ap.add_argument("--models", required=True)
    ap.add_argument("--work", default="/tmp/opstrellis-mine")
    ap.add_argument("--out", required=True)
    ap.add_argument("--limit", type=int, help="only the first N terms (smoke test)")
    ap.add_argument("--terms", default=TERMS, help="tech.json (default) or a term pack file, e.g. terms/packs/finance.json")
    a = ap.parse_args()

    data = json.load(open(a.terms))
    terms = (data["terms"] if isinstance(data, dict) else data)[: a.limit]
    os.makedirs(a.work, exist_ok=True)
    clips = []
    for engine, tts, voices in tts_engines(a.tts):
        for t in terms:
            for say in t.get("say") or [t["term"]]:
                for sid in voices:
                    wav = os.path.join(a.work, f"{engine}-{sid}-{hashlib.md5(say.encode()).hexdigest()[:12]}.wav")
                    if not os.path.exists(wav):
                        audio = tts.generate(CARRIER.format(say), sid=sid, speed=1.0)
                        sf.write(wav, np.array(audio.samples, dtype=np.float32), audio.sample_rate)
                    clips.append({"term": t["term"], "say": say, "voice": f"{engine}-{sid}", "wav": wav})
        print(f"synthesized {engine}: {len(clips)} clips", file=sys.stderr)

    models = ["moonshine-tiny", "moonshine-base", "whisper-base", "parakeet"]
    with mp.Pool(len(models)) as pool:
        results = [r for rs in pool.map(transcribe_all, [(a.models, m, clips) for m in models]) for r in rs]

    words = {w.strip().lower() for w in open("/usr/share/dict/words") if w.strip()}
    norm_terms = {t["term"].lower() for t in terms}
    seen, total, correct = defaultdict(Counter), Counter(), Counter()
    for r in results:
        total[r["term"]] += 1
        m = EXTRACT.search(r["text"])
        if not m:
            continue
        heard = re.sub(r"\s+", " ", m.group(1)).strip(" ,.;:!?\"'")
        if heard.lower() == r["term"].lower():
            correct[r["term"]] += 1
        else:
            seen[r["term"]][heard.lower()] += 1

    report = []
    for t in terms:
        term = t["term"]
        aliases, tech, rejected = [], [], []
        for heard, n in seen[term].most_common():
            ws = heard.replace("-", " ").split()
            why = None
            if n < 2:
                why = "seen once"
            elif not ws or len(ws) > 4 or len(heard) > 40:
                why = "too long"
            elif heard in norm_terms:
                why = "another term"
            elif any(ch.isdigit() for ch in heard) and not any(ch.isdigit() for ch in term):
                why = "digits"
            elif len(ws) == 1 and ws[0].strip(".") in words:
                why = "single word"
            if why:
                rejected.append([heard, n, why])
            elif all(w.strip(".,") in words for w in ws):
                tech.append([heard, n])
            else:
                aliases.append([heard, n])
        report.append({"term": term, "correct": f"{correct[term]}/{total[term]}", "aliases": aliases, "techAliases": tech, "rejected": rejected})
    json.dump(report, open(a.out, "w"), indent=1, ensure_ascii=False)
    print(f"wrote {a.out}", file=sys.stderr)


if __name__ == "__main__":
    main()
