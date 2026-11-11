import { randomUUID } from 'node:crypto';
import { existsSync, readFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import { and, eq, ne } from 'drizzle-orm';
import { afterEach, beforeAll, describe, expect, it, vi } from 'vitest';
import { config } from '../src/config.js';
import { models, providerKeys, usageEvents, users } from '../src/db/schema.js';
import { parseWav } from '../src/lib/audio.js';
import { deleteProviderKey, setProviderKey } from '../src/providers.js';
import { LOCAL_WINDOW_SECONDS, MAX_LOCAL_JOBS, STT_ADAPTERS } from '../src/stt.js';
import { app, db, fmtChunk, ORIGIN, req, resetDb, riffChunk, riffWave, userWithToken } from './helpers.js';

const WAV_PATH = 'models/sherpa-onnx-moonshine-tiny-en-int8/test_wavs/0.wav';
const WAV_SECONDS = 6.625;
if (!existsSync(WAV_PATH)) throw new Error(`Missing ${WAV_PATH}: run scripts/fetch-model.sh first`);
const wav = readFileSync(WAV_PATH);
const pcm = parseWav(wav)!.data; // 16 kHz mono PCM16 samples of the test clip

function transcribe(token: string, fields: Record<string, string> = {}, file: Blob = new Blob([wav], { type: 'audio/wav' })) {
  const form = new FormData();
  form.set('file', file, 'speech.wav');
  for (const [k, v] of Object.entries(fields)) form.set(k, v);
  return req('/v1/audio/transcriptions', { token, method: 'POST', body: form });
}

const eventsOf = (userId: string) => db.select().from(usageEvents).where(eq(usageEvents.userId, userId));

/** Pretend the user already transcribed `seconds` through the gateway this period. */
async function seedManaged(userId: string, seconds: number) {
  await db.insert(usageEvents).values({ id: randomUUID(), userId, source: 'gateway', mode: 'cloud', kind: 'stt', provider: 'local', model: 'local/moonshine-tiny-en', audioSeconds: seconds });
}

const savedEnv = { ...process.env };
beforeAll(async () => {
  await resetDb();
  delete process.env.OPENAI_API_KEY; // make "no key configured" deterministic
});
afterEach(async () => {
  process.env = { ...savedEnv };
  delete process.env.OPENAI_API_KEY;
  await deleteProviderKey('openai');
});

describe('POST /v1/audio/transcriptions with the self-hosted model', () => {
  it('transcribes test_wavs/0.wav with the real local sherpa-onnx model and meters it', async () => {
    const { token, user } = await userWithToken();
    const res = await transcribe(token, { model: 'local/moonshine-tiny-en' });
    expect(res.status).toBe(200);
    expect(res.headers.get('x-opstrellis-model')).toBe('local/moonshine-tiny-en');
    const body = await res.json();
    expect(Object.keys(body)).toEqual(['text']);
    expect(body.text.toLowerCase()).toContain('yellow lamps');

    const [e] = await eventsOf(user.id);
    expect(e).toMatchObject({ source: 'gateway', mode: 'cloud', kind: 'stt', provider: 'local', model: 'local/moonshine-tiny-en', status: 'ok', providerCostUsd: 0, billedUsd: 0 });
    expect(e!.audioSeconds).toBeCloseTo(WAV_SECONDS, 3);
    expect(e!.words).toBe(body.text.split(/\s+/).length);
    expect(e!.latencyMs).toBeGreaterThan(0);

    const me = await (await req('/api/me', { token })).json();
    expect(me.usage.managed_minutes).toBeCloseTo(WAV_SECONDS / 60, 5);
  });

  it('supports text and verbose_json response formats', async () => {
    const { token } = await userWithToken();
    const text = await transcribe(token, { model: 'local/moonshine-tiny-en', response_format: 'text' });
    expect(text.headers.get('content-type')).toMatch(/text\/plain/);
    expect((await text.text()).toLowerCase()).toContain('yellow lamps');

    const verbose = await (await transcribe(token, { model: 'local/moonshine-tiny-en', response_format: 'verbose_json', language: 'en' })).json();
    expect(verbose).toMatchObject({ task: 'transcribe', duration: WAV_SECONDS, language: 'en', model: 'local/moonshine-tiny-en' });
    expect((await transcribe(token, { response_format: 'srt' })).status).toBe(400);
  });

  it('routes "auto" to the preferred model', async () => {
    const { token } = await userWithToken();
    await req('/api/me', { token, method: 'PATCH', json: { preferred_model: 'local/moonshine-tiny-en' } });
    const res = await transcribe(token, { model: 'auto' });
    expect(res.headers.get('x-opstrellis-model')).toBe('local/moonshine-tiny-en');
  });
});

/** Replaces the recognizer's decodeAsync for the duration of `fn`. */
async function withDecode(wrap: (original: (...a: unknown[]) => Promise<unknown>) => (this: unknown, ...a: unknown[]) => Promise<unknown>, fn: () => Promise<void>) {
  const proto = createRequire(import.meta.url)('sherpa-onnx-node').OfflineRecognizer.prototype;
  const original = proto.decodeAsync;
  proto.decodeAsync = wrap(original);
  try {
    await fn();
  } finally {
    proto.decodeAsync = original;
  }
}

describe('long audio on the self-hosted model', () => {
  it('decodes clips over 30 s in windows and joins the text of every window', async () => {
    // The clip, padded with silence to exactly one window, then the clip again: 36.6 s, two windows.
    const window = Buffer.alloc(LOCAL_WINDOW_SECONDS * 16000 * 2);
    pcm.copy(window);
    const long = riffWave(fmtChunk(16000), riffChunk('data', Buffer.concat([window, pcm])));
    let decodes = 0;
    await withDecode(
      (original) =>
        function (this: unknown, ...args: unknown[]) {
          decodes++;
          return original.apply(this, args);
        },
      async () => {
        const { token, user } = await userWithToken();
        const res = await transcribe(token, { model: 'local/moonshine-tiny-en' }, new Blob([long], { type: 'audio/wav' }));
        expect(res.status).toBe(200);
        const text = ((await res.json()).text as string).toLowerCase();
        expect(text.match(/yellow lamps/g)).toHaveLength(2);
        expect((await eventsOf(user.id))[0]!.audioSeconds).toBeCloseTo(LOCAL_WINDOW_SECONDS + WAV_SECONDS, 3);
      },
    );
    expect(decodes).toBe(2);
  });

  it('refuses with 503 when too many local decodes are waiting', async () => {
    let release!: () => void;
    const gate = new Promise<void>((r) => (release = r));
    await withDecode(
      (original) =>
        async function (this: unknown, ...args: unknown[]) {
          await gate;
          return original.apply(this, args);
        },
      async () => {
        const run = () => STT_ADAPTERS.local!({ audio: wav, filename: 'a.wav', mimeType: 'audio/wav', upstreamModel: 'sherpa-onnx-moonshine-tiny-en-int8', key: null });
        const queued = Array.from({ length: MAX_LOCAL_JOBS }, run);
        await new Promise((r) => setTimeout(r, 50));
        await expect(run()).rejects.toMatchObject({ status: 503, code: 'local_model_busy' });
        // through the gateway the same rejection reaches the client as is (local is the last model in the chain)
        const { token, user } = await userWithToken();
        const res = await transcribe(token, { model: 'local/moonshine-tiny-en' });
        expect(res.status).toBe(503);
        expect((await res.json()).error.type).toBe('local_model_busy');
        expect(await eventsOf(user.id)).toHaveLength(0);
        release();
        expect((await Promise.all(queued)).every((r) => r.text.toLowerCase().includes('yellow lamps'))).toBe(true);
      },
    );
  });
});

/** A multipart upload whose body never finishes until `end()` is called. */
function stalledUpload(token: string) {
  let controller!: ReadableStreamDefaultController<Uint8Array>;
  const body = new ReadableStream<Uint8Array>({ start: (c) => void (controller = c) });
  const res = app.request('/v1/audio/transcriptions', {
    method: 'POST',
    headers: { authorization: `Bearer ${token}`, 'content-type': 'multipart/form-data; boundary=xyz', origin: ORIGIN, 'x-forwarded-for': '10.9.9.9' },
    body,
    duplex: 'half',
  } as RequestInit);
  return { res: Promise.resolve(res), end: () => controller.close() };
}

describe('admission before the upload is read', () => {
  it('takes the in-flight slot before reading the body', async () => {
    const { token } = await userWithToken();
    const stalled = [stalledUpload(token), stalledUpload(token), stalledUpload(token)];
    await new Promise((r) => setTimeout(r, 50));
    const res = await transcribe(token, { model: 'local/moonshine-tiny-en' });
    expect(res.status).toBe(429);
    for (const s of stalled) s.end();
    expect((await Promise.all(stalled.map((s) => s.res))).map((r) => r.status)).toEqual([400, 400, 400]);
    expect((await transcribe(token, { model: 'local/moonshine-tiny-en' })).status).toBe(200);
  });

  it('drains a stalled early-rejected upload only until the deadline, and at most 3 at a time per user', async () => {
    const saved = config.uploadDrainMs;
    config.uploadDrainMs = 600;
    try {
      const { token, user } = await userWithToken();
      await seedManaged(user.id, 15 * 60);
      const t0 = Date.now();
      const stalled = [stalledUpload(token), stalledUpload(token), stalledUpload(token)];
      await new Promise((r) => setTimeout(r, 50));
      // a fourth is answered at once, without draining
      const fourth = stalledUpload(token);
      expect((await fourth.res).status).toBe(402);
      expect(Date.now() - t0).toBeLessThan(400);
      // the three that declared a body and sent nothing are answered at the deadline
      expect((await Promise.all(stalled.map((s) => s.res))).map((r) => r.status)).toEqual([402, 402, 402]);
      expect(Date.now() - t0).toBeGreaterThanOrEqual(550);
    } finally {
      config.uploadDrainMs = saved;
    }
  });

  it('rejects an exhausted quota before parsing the body', async () => {
    const { token, user } = await userWithToken();
    await seedManaged(user.id, 15 * 60);
    const s = stalledUpload(token);
    s.end(); // an empty, unparsable multipart body: a 400 if it were parsed first
    expect((await s.res).status).toBe(402);
  });

  it('reads and drops the upload of an early rejection, so a real client still sending gets the JSON error', async () => {
    const { serve } = await import('@hono/node-server');
    const http = await import('node:http');
    const server = serve({ fetch: app.fetch, port: 0, hostname: '127.0.0.1' });
    await new Promise<void>((r) => server.once('listening', () => r()));
    const { port } = server.address() as { port: number };
    let serverClosedSocket = false;
    server.on('connection', (socket) => socket.on('close', () => (serverClosedSocket = true)));
    try {
      const { token, user } = await userWithToken();
      await seedManaged(user.id, 15 * 60);
      // 8 MB sent in 64 KB chunks over about a second: longer than Hono's 500 ms drain before it resets the socket
      const chunk = Buffer.alloc(64 * 1024, 1);
      const chunks = 128;
      let uploaded!: Promise<boolean>;
      const result = await new Promise<{ status: number; body: string }>((resolve, reject) => {
        const r = http.request(
          { host: '127.0.0.1', port, method: 'POST', path: '/v1/audio/transcriptions', headers: { authorization: `Bearer ${token}`, 'content-type': 'multipart/form-data; boundary=xyz', 'content-length': chunk.length * chunks } },
          (res) => {
            let body = '';
            res.on('data', (d) => (body += d));
            res.on('end', () => resolve({ status: res.statusCode!, body }));
          },
        );
        r.on('error', reject);
        uploaded = new Promise((done, fail) => {
          r.on('finish', () => done(serverClosedSocket));
          r.on('error', fail);
        });
        let sent = 0;
        const next = () => {
          if (sent === chunks) return void r.end();
          sent++;
          r.write(chunk, () => setTimeout(next, 8));
        };
        next();
      });
      // Without draining, the server dropped the connection about 500 ms in, while the client was still sending.
      expect(await uploaded).toBe(false);
      expect(result.status).toBe(402);
      expect(JSON.parse(result.body).error.type).toBe('quota_exceeded');
    } finally {
      server.close();
    }
  });
});

describe('fallback', () => {
  it('skips an upstream with no key configured and falls back to local', async () => {
    const { token, user } = await userWithToken();
    const res = await transcribe(token, { model: 'openai/gpt-4o-mini-transcribe', response_format: 'verbose_json' });
    expect(res.status).toBe(200);
    const body = await res.json();
    expect(body.model).toBe('local/moonshine-tiny-en');
    expect(body.text.toLowerCase()).toContain('yellow lamps');
    expect((await eventsOf(user.id)).map((e) => e.status)).toEqual(['ok']);
  });

  it('records the failed attempt when an upstream is unreachable, then falls back', async () => {
    await setProviderKey('openai', 'sk-test-not-a-real-key');
    process.env.OPENAI_BASE_URL = 'http://127.0.0.1:9/v1'; // nothing listens here
    const { token, user } = await userWithToken();
    const res = await transcribe(token, { model: 'openai/gpt-4o-mini-transcribe' });
    expect(res.status).toBe(200);
    expect(res.headers.get('x-opstrellis-model')).toBe('local/moonshine-tiny-en');
    const events = await eventsOf(user.id);
    const failed = events.filter((e) => e.status === 'error');
    // every OpenAI model in the chain is tried (same unreachable host), none is billed
    expect(failed.map((e) => e.model).sort()).toEqual(['openai/gpt-4o-mini-transcribe', 'openai/gpt-4o-transcribe', 'openai/whisper-1']);
    expect(failed.every((e) => e.audioSeconds === 0 && e.billedUsd === 0)).toBe(true);
    expect(events.filter((e) => e.status === 'ok').map((e) => e.model)).toEqual(['local/moonshine-tiny-en']);
  });

  it('skips a provider whose saved key cannot be decrypted (e.g. MASTER_KEY rotated)', async () => {
    await setProviderKey('openai', 'sk-test-not-a-real-key');
    await db.update(providerKeys).set({ ciphertext: Buffer.alloc(40, 7).toString('base64') }).where(eq(providerKeys.provider, 'openai'));
    const { token } = await userWithToken();
    const res = await transcribe(token, { model: 'openai/gpt-4o-mini-transcribe' });
    expect(res.status).toBe(200);
    expect(res.headers.get('x-opstrellis-model')).toBe('local/moonshine-tiny-en');
  });

  it('returns 502 when every model in the chain fails, logging "stt failed" only', async () => {
    await setProviderKey('openai', 'sk-test-not-a-real-key');
    process.env.OPENAI_BASE_URL = 'http://127.0.0.1:9/v1';
    await db.update(models).set({ enabled: false }).where(and(eq(models.kind, 'stt'), ne(models.provider, 'openai')));
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});
    try {
      const { token } = await userWithToken();
      const res = await transcribe(token, { model: 'openai/whisper-1' });
      expect(res.status).toBe(502);
      expect((await res.json()).error.type).toBe('upstream_error');
      const lines = warn.mock.calls.map((c) => String(c[0]));
      expect(lines.filter((l) => l.startsWith('stt failed:'))).toHaveLength(1);
      expect(lines.some((l) => l.startsWith('llm failed:'))).toBe(false);
    } finally {
      warn.mockRestore();
      await db.update(models).set({ enabled: true });
    }
  });

  it('logs "llm failed" when every chat model fails', async () => {
    await setProviderKey('openai', 'sk-test-not-a-real-key');
    process.env.OPENAI_BASE_URL = 'http://127.0.0.1:9/v1';
    await db.update(models).set({ enabled: false }).where(and(eq(models.kind, 'llm'), ne(models.provider, 'openai')));
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});
    try {
      const { token } = await userWithToken();
      const res = await req('/v1/chat/completions', { token, json: { model: 'openai/gpt-4o-mini', messages: [{ role: 'user', content: 'hi' }] } });
      expect(res.status).toBe(502);
      const lines = warn.mock.calls.map((c) => String(c[0]));
      expect(lines.filter((l) => l.startsWith('llm failed:'))).toHaveLength(1);
      expect(lines.some((l) => l.startsWith('stt failed:'))).toBe(false);
    } finally {
      warn.mockRestore();
      await db.update(models).set({ enabled: true });
    }
  });

  it('rejects unparsable audio before calling any upstream', async () => {
    const { token, user } = await userWithToken();
    const garbage = new Blob([Buffer.from('RIFF\0\0\0\0WAVEjunk')], { type: 'audio/wav' });
    const res = await transcribe(token, { model: 'local/moonshine-tiny-en' }, garbage);
    expect(res.status).toBe(400);
    expect((await res.json()).error.type).toBe('unsupported_audio');
    expect(await eventsOf(user.id)).toHaveLength(0);
  });
});

describe('quota and billing', () => {
  it('returns 402 for a free user over 15 minutes', async () => {
    const { token, user } = await userWithToken();
    await seedManaged(user.id, 15 * 60);
    const res = await transcribe(token, { model: 'local/moonshine-tiny-en' });
    expect(res.status).toBe(402);
    expect((await res.json()).error).toMatchObject({ type: 'quota_exceeded' });
  });

  it('rejects a free request that would go past the allowance', async () => {
    const { token, user } = await userWithToken();
    await seedManaged(user.id, 15 * 60 - 1);
    const res = await transcribe(token, { model: 'local/moonshine-tiny-en' });
    expect(res.status).toBe(402);
    expect(await eventsOf(user.id)).toHaveLength(1); // just the seeded usage
  });

  it('checks the free allowance atomically across parallel requests', async () => {
    const { token, user } = await userWithToken();
    await seedManaged(user.id, 15 * 60 - 15); // room for two 6.625 s clips, not three
    const results = await Promise.all(Array.from({ length: 3 }, () => transcribe(token, { model: 'local/moonshine-tiny-en' })));
    expect(results.map((r) => r.status).sort()).toEqual([200, 200, 402]);
    const me = await (await req('/api/me', { token })).json();
    expect(me.usage.managed_minutes).toBeLessThanOrEqual(15);
  });

  it('caps in-flight gateway requests per user', async () => {
    const { token, user } = await userWithToken();
    await db.update(users).set({ plan: 'pro' }).where(eq(users.id, user.id));
    const results = await Promise.all(Array.from({ length: 4 }, () => transcribe(token, { model: 'local/moonshine-tiny-en' })));
    expect(results.map((r) => r.status).sort()).toEqual([200, 200, 200, 429]);
  });

  it('bills Pro overage per started minute beyond 600', async () => {
    const { token, user } = await userWithToken();
    await db.update(users).set({ plan: 'pro' }).where(eq(users.id, user.id));
    await seedManaged(user.id, 600 * 60 - 3); // 3 s left; the 6.625 s clip goes 3.625 s over
    const res = await transcribe(token, { model: 'local/moonshine-tiny-en' });
    expect(res.status).toBe(200);
    const [e] = await db.select().from(usageEvents).where(and(eq(usageEvents.userId, user.id), eq(usageEvents.provider, 'local'), eq(usageEvents.status, 'ok'))).orderBy(usageEvents.createdAt).offset(1);
    expect(e).toMatchObject({ overageMinutes: 1, billedUsd: 0.02 });
    // the next clip stays inside the already-billed overage minute
    await transcribe(token, { model: 'local/moonshine-tiny-en' });
    const me = await (await req('/api/me', { token })).json();
    expect(me.plan).toBe('pro');
    expect(me.usage).toMatchObject({ included_minutes: 600, overage_minutes: 1, cost_usd: 0.02 });
  });

  it('stops Pro overage at MAX_OVERAGE_MINUTES with its own error', async () => {
    const saved = config.maxOverageMinutes;
    config.maxOverageMinutes = 2;
    try {
      const { token, user } = await userWithToken();
      await db.update(users).set({ plan: 'pro' }).where(eq(users.id, user.id));
      await seedManaged(user.id, 602 * 60 - 3); // 3 s left before the cap
      const res = await transcribe(token, { model: 'local/moonshine-tiny-en' });
      expect(res.status).toBe(402);
      const { error } = await res.json();
      expect(error.type).toBe('overage_cap_reached');
      expect(error.message).toMatch(/2 minutes/);
      expect((await (await req('/api/me', { token })).json()).limits.max_overage_minutes).toBe(2);
      expect((await (await req('/api/billing', { token })).json()).max_overage_minutes).toBe(2);
    } finally {
      config.maxOverageMinutes = saved;
    }
  });

  it('never double-counts included minutes under concurrent requests', async () => {
    const { token, user } = await userWithToken();
    await db.update(users).set({ plan: 'pro' }).where(eq(users.id, user.id));
    await seedManaged(user.id, 600 * 60 - 60);
    const batch = (n: number) => Promise.all(Array.from({ length: n }, () => transcribe(token, { model: 'local/moonshine-tiny-en' })));
    expect((await batch(3)).every((r) => r.status === 200)).toBe(true);
    // 3 x 6.625 s = 19.875 s, all within the last included minute
    const me = await (await req('/api/me', { token })).json();
    expect(me.usage.overage_minutes).toBe(0);
    for (const n of [3, 3, 1]) expect((await batch(n)).every((r) => r.status === 200)).toBe(true);
    // 10 x 6.625 = 66.25 s total -> 6.25 s over -> exactly one billed minute
    expect((await (await req('/api/me', { token })).json()).usage).toMatchObject({ overage_minutes: 1, cost_usd: 0.02 });
  });
});

describe('audio validation', () => {
  const rejected = async (file: Buffer) => {
    const { token, user } = await userWithToken();
    const res = await transcribe(token, { model: 'local/moonshine-tiny-en' }, new Blob([file], { type: 'audio/wav' }));
    expect(res.status).toBe(400);
    expect((await res.json()).error.type).toBe('unsupported_audio');
    expect(await eventsOf(user.id)).toHaveLength(0);
  };

  it('rejects WAVs whose headers misstate the duration (duplicate data or fmt chunks)', async () => {
    // a 2-byte data chunk followed by the real one would bill ~0 s for the whole clip
    await rejected(riffWave(fmtChunk(16000), riffChunk('data', Buffer.alloc(2)), riffChunk('data', pcm)));
    // a second fmt chunk with a huge sample rate would do the same
    await rejected(riffWave(fmtChunk(16000), fmtChunk(0xfffffff0), riffChunk('data', pcm)));
  });

  it('rejects sample rates outside 8-48 kHz and unsupported encodings', async () => {
    // 10 Hz audio made the local model resample to gigabytes
    await rejected(riffWave(fmtChunk(1000), riffChunk('data', pcm.subarray(0, 4000))));
    await rejected(riffWave(fmtChunk(16000, 1, 8), riffChunk('data', pcm)));
    await rejected(riffWave(fmtChunk(16000, 6), riffChunk('data', pcm)));
  });

  it('bills a tiny clip as one second', async () => {
    const { token, user } = await userWithToken();
    const tiny = riffWave(fmtChunk(16000), riffChunk('data', pcm.subarray(0, 32)));
    const res = await transcribe(token, { model: 'local/moonshine-tiny-en' }, new Blob([tiny], { type: 'audio/wav' }));
    expect(res.status).toBe(200);
    expect((await eventsOf(user.id))[0]!.audioSeconds).toBe(1);
  });

  it('limits each user to 120 requests a minute, whatever their IP', async () => {
    const { token } = await userWithToken();
    for (let i = 0; i < 120; i++) expect((await req('/v1/models', { token })).status).toBe(200);
    expect((await req('/v1/models', { token })).status).toBe(429);
    expect((await req('/v1/models', { token: (await userWithToken()).token })).status).toBe(200);
  });

  it('bills stereo audio by its real duration', async () => {
    const { token, user } = await userWithToken();
    const stereo = Buffer.alloc(pcm.length * 2);
    for (let i = 0; i < pcm.length / 2; i++) {
      stereo.writeInt16LE(pcm.readInt16LE(i * 2), i * 4);
      stereo.writeInt16LE(pcm.readInt16LE(i * 2), i * 4 + 2);
    }
    const file = riffWave(riffChunk('LIST', Buffer.from('INFOjunk')), fmtChunk(16000, 2), riffChunk('data', stereo));
    const res = await transcribe(token, { model: 'local/moonshine-tiny-en' }, new Blob([file], { type: 'audio/wav' }));
    expect(res.status).toBe(200);
    expect((await res.json()).text.toLowerCase()).toContain('yellow lamps');
    expect((await eventsOf(user.id))[0]!.audioSeconds).toBeCloseTo(WAV_SECONDS, 3);
  });

  it('runs one local decode at a time', async () => {
    const proto = createRequire(import.meta.url)('sherpa-onnx-node').OfflineRecognizer.prototype;
    const original = proto.decodeAsync;
    let active = 0;
    let peak = 0;
    proto.decodeAsync = async function (this: unknown, ...args: unknown[]) {
      peak = Math.max(peak, ++active);
      try {
        return await original.apply(this, args);
      } finally {
        active--;
      }
    };
    try {
      const run = () => STT_ADAPTERS.local!({ audio: wav, filename: 'a.wav', mimeType: 'audio/wav', upstreamModel: 'sherpa-onnx-moonshine-tiny-en-int8', key: null });
      const results = await Promise.all([run(), run(), run()]);
      expect(results.every((r) => r.text.toLowerCase().includes('yellow lamps'))).toBe(true);
      expect(peak).toBe(1);
    } finally {
      proto.decodeAsync = original;
    }
  });
});

describe('gateway validation', () => {
  it('rejects bad requests', async () => {
    const { token, user } = await userWithToken();
    expect((await transcribe(token, { model: 'nope/model' })).status).toBe(400);
    const notAudio = await transcribe(token, { model: 'local/moonshine-tiny-en' }, new Blob(['hello world'], { type: 'text/plain' }));
    expect(notAudio.status).toBe(400);
    const noFile = await req('/v1/audio/transcriptions', { token, method: 'POST', body: new FormData() });
    expect(noFile.status).toBe(400);
    expect((await transcribe('utt_invalid')).status).toBe(401);
    await db.update(users).set({ disabled: true }).where(eq(users.id, user.id));
    expect((await transcribe(token)).status).toBe(403);
  });

  it('rejects audio over 25 MB with 413', async () => {
    const { token } = await userWithToken();
    const big = new Blob([new Uint8Array(26 * 1024 * 1024)], { type: 'audio/wav' });
    expect((await transcribe(token, {}, big)).status).toBe(413);
  });

  it('lists enabled models at /v1/models', async () => {
    const { token } = await userWithToken();
    const res = await (await req('/v1/models', { token })).json();
    expect(res.object).toBe('list');
    expect(res.data.map((m: any) => m.id)).toContain('local/moonshine-tiny-en');
    expect(res.data.find((m: any) => m.id === 'openai/gpt-4o-mini')).toMatchObject({ object: 'model', owned_by: 'openai', kind: 'llm' });
  });
});
