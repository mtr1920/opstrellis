// Batch speech-to-text adapters. Each takes a WAV (or other audio) buffer and returns text.
import { existsSync, readdirSync } from 'node:fs';
import { createRequire } from 'node:module';
import path from 'node:path';
import { config } from './config.js';
import { parseWav, wavToMonoFloat } from './lib/audio.js';
import { ApiError } from './lib/http.js';
import { serial } from './lib/serial.js';
import { baseUrl } from './providers.js';

export interface SttRequest {
  audio: Buffer;
  filename: string;
  mimeType: string;
  upstreamModel: string;
  language?: string;
  prompt?: string;
  key: string | null;
}
export interface SttResult {
  text: string;
  language?: string;
}

export class UpstreamError extends Error {}

const TIMEOUT_MS = 60_000;

async function call(url: string, init: RequestInit): Promise<any> {
  const res = await fetch(url, { ...init, signal: AbortSignal.timeout(TIMEOUT_MS) });
  const body = await res.text();
  if (!res.ok) throw new UpstreamError(`${new URL(url).host} ${res.status}: ${body.slice(0, 300)}`);
  try {
    return JSON.parse(body);
  } catch {
    throw new UpstreamError(`${new URL(url).host}: invalid JSON response`);
  }
}

function requireKey(key: string | null, provider: string): string {
  if (!key) throw new UpstreamError(`no API key configured for ${provider}`);
  return key;
}

/** Vocabulary hints: the prompt as a comma/newline separated list of terms. */
export function keyterms(prompt?: string, max = 100): string[] {
  return (prompt ?? '')
    .split(/[,;\n]/)
    .map((t) => t.trim())
    .filter((t) => t && t.length <= 100)
    .slice(0, max);
}

const blob = (r: SttRequest) => new Blob([new Uint8Array(r.audio)], { type: r.mimeType });

// ---------- OpenAI-compatible (OpenAI, Groq) ----------
function openaiCompatible(provider: string, defaultBase: string) {
  return async (r: SttRequest): Promise<SttResult> => {
    const form = new FormData();
    form.set('file', blob(r), r.filename);
    form.set('model', r.upstreamModel);
    form.set('response_format', 'json');
    if (r.language) form.set('language', r.language);
    if (r.prompt) form.set('prompt', r.prompt);
    const json = await call(`${baseUrl(provider, defaultBase)}/audio/transcriptions`, {
      method: 'POST',
      headers: { authorization: `Bearer ${requireKey(r.key, provider)}` },
      body: form,
    });
    return { text: String(json.text ?? ''), language: json.language };
  };
}

// ---------- Deepgram pre-recorded /v1/listen ----------
async function deepgram(r: SttRequest): Promise<SttResult> {
  const q = new URLSearchParams({ model: r.upstreamModel, smart_format: 'true', punctuate: 'true' });
  if (r.language) q.set('language', r.language);
  else q.set('detect_language', 'true');
  for (const t of keyterms(r.prompt)) q.append('keyterm', t);
  const json = await call(`${baseUrl('deepgram', 'https://api.deepgram.com')}/v1/listen?${q}`, {
    method: 'POST',
    headers: { authorization: `Token ${requireKey(r.key, 'deepgram')}`, 'content-type': r.mimeType },
    body: new Uint8Array(r.audio),
  });
  const channel = json.results?.channels?.[0];
  return { text: channel?.alternatives?.[0]?.transcript ?? '', language: channel?.detected_language ?? r.language };
}

// ---------- AssemblyAI: upload -> create transcript -> poll ----------
async function assemblyai(r: SttRequest): Promise<SttResult> {
  const base = `${baseUrl('assemblyai', 'https://api.assemblyai.com')}/v2`;
  const authorization = requireKey(r.key, 'assemblyai');
  const upload = await call(`${base}/upload`, {
    method: 'POST',
    headers: { authorization, 'content-type': 'application/octet-stream' },
    body: new Uint8Array(r.audio),
  });
  const terms = keyterms(r.prompt);
  const created = await call(`${base}/transcript`, {
    method: 'POST',
    headers: { authorization, 'content-type': 'application/json' },
    body: JSON.stringify({
      audio_url: upload.upload_url,
      speech_model: r.upstreamModel,
      ...(r.language ? { language_code: r.language } : { language_detection: true }),
      ...(terms.length ? { keyterms_prompt: terms } : {}),
    }),
  });
  const deadline = Date.now() + TIMEOUT_MS;
  for (let t = created; ; ) {
    if (t.status === 'completed') return { text: t.text ?? '', language: t.language_code };
    if (t.status === 'error') throw new UpstreamError(`assemblyai: ${t.error}`);
    if (Date.now() > deadline) throw new UpstreamError('assemblyai: timed out');
    await new Promise((res) => setTimeout(res, 500));
    t = await call(`${base}/transcript/${created.id}`, { headers: { authorization } });
  }
}

// ---------- ElevenLabs Scribe ----------
async function elevenlabs(r: SttRequest): Promise<SttResult> {
  const form = new FormData();
  form.set('file', blob(r), r.filename);
  form.set('model_id', r.upstreamModel);
  if (r.language) form.set('language_code', r.language);
  const json = await call(`${baseUrl('elevenlabs', 'https://api.elevenlabs.io')}/v1/speech-to-text`, {
    method: 'POST',
    headers: { 'xi-api-key': requireKey(r.key, 'elevenlabs') },
    body: form,
  });
  return { text: json.text ?? '', language: json.language_code };
}

/** Google and Azure need a BCP-47 locale; a bare "en" becomes "en-US". */
const locale = (lang?: string) => (!lang ? 'en-US' : lang.includes('-') ? lang : lang === 'en' ? 'en-US' : lang);

// ---------- Google Cloud Speech v1 speech:recognize (API key) ----------
async function google(r: SttRequest): Promise<SttResult> {
  const wav = parseWav(r.audio);
  if (!wav || wav.bitsPerSample !== 16) throw new UpstreamError('google: requires 16-bit PCM WAV');
  const terms = keyterms(r.prompt, 500);
  const key = encodeURIComponent(requireKey(r.key, 'google'));
  const json = await call(`${baseUrl('google', 'https://speech.googleapis.com')}/v1/speech:recognize?key=${key}`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({
      config: {
        encoding: 'LINEAR16',
        sampleRateHertz: wav.sampleRate,
        audioChannelCount: wav.channels,
        languageCode: locale(r.language),
        model: r.upstreamModel,
        enableAutomaticPunctuation: true,
        ...(terms.length ? { speechContexts: [{ phrases: terms }] } : {}),
      },
      audio: { content: r.audio.toString('base64') },
    }),
  });
  const text = (json.results ?? []).map((x: any) => x.alternatives?.[0]?.transcript ?? '').join(' ');
  return { text: text.replace(/\s+/g, ' ').trim(), language: json.results?.[0]?.languageCode };
}

// ---------- Azure Speech short-audio REST ----------
async function azure(r: SttRequest): Promise<SttResult> {
  const wav = parseWav(r.audio);
  if (!wav) throw new UpstreamError('azure: requires WAV audio');
  const region = process.env.AZURE_SPEECH_REGION || 'eastus';
  const q = new URLSearchParams({ language: locale(r.language), format: 'simple' });
  const host = baseUrl('azure', `https://${region}.stt.speech.microsoft.com`);
  const json = await call(`${host}/speech/recognition/${r.upstreamModel}/cognitiveservices/v1?${q}`, {
    method: 'POST',
    headers: {
      'ocp-apim-subscription-key': requireKey(r.key, 'azure'),
      'content-type': `audio/wav; codecs=audio/pcm; samplerate=${wav.sampleRate}`,
      accept: 'application/json',
    },
    body: new Uint8Array(r.audio),
  });
  if (json.RecognitionStatus !== 'Success') throw new UpstreamError(`azure: ${json.RecognitionStatus}`);
  return { text: json.DisplayText ?? '', language: locale(r.language) };
}

// ---------- Local (self-hosted) sherpa-onnx Moonshine ----------
const require = createRequire(import.meta.url);
const recognizers = new Map<string, Promise<any>>();

function findFile(dir: string, re: RegExp): string {
  const name = readdirSync(dir).find((f) => re.test(f));
  if (!name) throw new UpstreamError(`local: ${re} not found in ${dir}`);
  return path.join(dir, name);
}

/** `upstreamModel` is a model directory name under LOCAL_MODEL_DIR (or an absolute path). */
export function localModelDir(upstreamModel: string) {
  return path.resolve(config.localModelDir, upstreamModel);
}

function recognizer(dir: string): Promise<any> {
  const cached = recognizers.get(dir);
  if (cached) return cached;
  if (!existsSync(dir)) throw new UpstreamError(`local: model not installed at ${dir} (run scripts/fetch-model.sh)`);
  const sherpa = require('sherpa-onnx-node');
  const created: Promise<any> = sherpa.OfflineRecognizer.createAsync({
    featConfig: { sampleRate: 16000, featureDim: 80 },
    modelConfig: {
      moonshine: {
        preprocessor: findFile(dir, /^preprocess.*\.onnx$/),
        encoder: findFile(dir, /^encode.*\.onnx$/),
        uncachedDecoder: findFile(dir, /^uncached_decode.*\.onnx$/),
        cachedDecoder: findFile(dir, /^cached_decode.*\.onnx$/),
      },
      tokens: findFile(dir, /^tokens\.txt$/),
      numThreads: Number(process.env.LOCAL_NUM_THREADS ?? 2),
      provider: 'cpu',
      debug: 0,
    },
  });
  created.catch(() => recognizers.delete(dir)); // allow a retry after a failed load
  recognizers.set(dir, created);
  return created;
}

/** Moonshine's memory grows with the square of the clip length, so longer audio is decoded in windows of this size. */
export const LOCAL_WINDOW_SECONDS = 30;
/** Local decodes running or waiting across all users; each waiting one holds its upload in memory. */
export const MAX_LOCAL_JOBS = 4;

async function local(r: SttRequest): Promise<SttResult> {
  const wav = parseWav(r.audio);
  if (!wav) throw new UpstreamError('local: requires WAV audio (16 kHz mono PCM16 recommended)');
  const rec = await recognizer(localModelDir(r.upstreamModel));
  // One decode at a time: each holds the resampled audio and model activations in memory.
  return serial(
    'local-stt',
    async () => {
      const samples = wavToMonoFloat(wav);
      const size = LOCAL_WINDOW_SECONDS * wav.sampleRate;
      const parts: string[] = [];
      // Plain cuts at the window boundary: a word spoken across it may be split or lost.
      for (let at = 0; at < samples.length; at += size) {
        const stream = rec.createStream();
        stream.acceptWaveform({ samples: samples.slice(at, at + size), sampleRate: wav.sampleRate });
        const text = String((await rec.decodeAsync(stream)).text ?? '').trim();
        if (text) parts.push(text);
      }
      return { text: parts.join(' '), language: 'en' };
    },
    MAX_LOCAL_JOBS,
    () => new ApiError(503, 'local_model_busy', 'The self-hosted model is busy; try again shortly'),
  );
}

export const STT_ADAPTERS: Record<string, (r: SttRequest) => Promise<SttResult>> = {
  openai: openaiCompatible('openai', 'https://api.openai.com/v1'),
  groq: openaiCompatible('groq', 'https://api.groq.com/openai/v1'),
  deepgram,
  assemblyai,
  elevenlabs,
  google,
  azure,
  local,
};
