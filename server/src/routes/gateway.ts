// OpenAI-compatible gateway: /v1/audio/transcriptions, /v1/chat/completions, /v1/models.
import { Hono, type Context } from 'hono';
import { bodyLimit } from 'hono/body-limit';
import { z } from 'zod';
import { rateLimit, requireAuth, type AppEnv } from '../auth.js';
import { currentPeriod, PLANS, quotaError, quotaMessage } from '../billing.js';
import { listModels, modelChain } from '../catalog.js';
import { config } from '../config.js';
import type { Model } from '../db/schema.js';
import { countWords, normalizeAudio } from '../lib/audio.js';
import { apiError, ApiError, jsonBodyLimit, readJson } from '../lib/http.js';
import { serial } from '../lib/serial.js';
import { LLM_ADAPTERS } from '../llm.js';
import { getProviderKey } from '../providers.js';
import { STT_ADAPTERS } from '../stt.js';
import { reportOverage } from '../stripe.js';
import { llmTokens, managedSeconds, recordGatewayEvent } from '../usage.js';

export const gateway = new Hono<AppEnv>();
gateway.use('*', requireAuth());
// Per user, so a single account cannot exhaust the shared provider rate limits.
gateway.use('*', rateLimit('v1', 120, 60_000, (c) => c.get('user').id));

/** Requests per user and kind that may be running or queued at once; they run one at a time. */
const MAX_IN_FLIGHT = 3;

/** Clips are billed for at least this long, so floods of tiny clips are not free. */
const MIN_BILLED_SECONDS = 1;

/** Yields each model in the chain that has an adapter and credentials (local needs none). */
async function* attempts(chain: Model[], adapters: Record<string, unknown>) {
  for (const model of chain) {
    if (!adapters[model.provider]) continue;
    let key: string | null = null;
    if (model.provider !== 'local') {
      try {
        key = await getProviderKey(model.provider);
      } catch (e) {
        // e.g. a saved key that MASTER_KEY can no longer decrypt
        console.warn(`skipping ${model.id}: cannot read the ${model.provider} key: ${(e as Error).message}`);
        continue;
      }
      if (!key) continue; // not configured: skip silently
    }
    yield { model, key };
  }
}

const FORMATS = ['json', 'text', 'verbose_json'] as const;

const MAX_AUDIO_BODY = config.maxAudioBytes + 1024 * 1024;
const audioBodyLimit = bodyLimit({ maxSize: MAX_AUDIO_BODY, onError: (c) => apiError(c, 413, 'file_too_large', 'Audio is limited to 25 MB') });

const draining = new Map<string, number>();

/**
 * Reads and drops an unread upload (up to the size limit and config.uploadDrainMs, without keeping it),
 * so a client that is still sending gets the error response instead of a reset connection. At most
 * MAX_IN_FLIGHT drains per user; beyond that, or past the deadline, the answer goes out undrained.
 */
async function discardBody(c: Context, userId: string) {
  const body = c.req.raw.body;
  if (!body || c.req.raw.bodyUsed || Number(c.req.header('content-length')) > MAX_AUDIO_BODY) return;
  const running = draining.get(userId) ?? 0;
  if (running >= MAX_IN_FLIGHT) return;
  draining.set(userId, running + 1);
  const reader = body.getReader();
  const deadline = setTimeout(() => reader.cancel().catch(() => {}), config.uploadDrainMs);
  let size = 0;
  try {
    for (;;) {
      const { done, value } = await reader.read();
      if (done) return;
      size += value.byteLength;
      if (size > MAX_AUDIO_BODY) return void (await reader.cancel());
    }
  } catch {
    // the client went away
  } finally {
    clearTimeout(deadline);
    if (draining.get(userId) === 1) draining.delete(userId);
    else draining.set(userId, draining.get(userId)! - 1);
  }
}

gateway.post(
  '/audio/transcriptions',
  async (c) => {
    const user = c.get('user');
    let reading = false;
    // The in-flight slot is taken before the upload is read (the size limit runs inside it, as it buffers
    // chunked bodies), so a user cannot hold more than MAX_IN_FLIGHT uploads; quota check and metering
    // run under the same per-user lock.
    try {
      return await serial(
        `stt:${user.id}`,
        async () => {
          const used = await managedSeconds(user.id, currentPeriod(user));
          const exhausted = quotaError(user, used);
          if (exhausted) throw new ApiError(402, exhausted, quotaMessage(exhausted));
          reading = true;
          const tooLarge = await audioBodyLimit(c, async () => {});
          if (tooLarge) return tooLarge;

          const form = await c.req.formData().catch(() => {
            throw new ApiError(400, 'invalid_request', 'Expected multipart/form-data');
          });
          const file = form.get('file');
          if (!(file instanceof File)) throw new ApiError(400, 'invalid_request', 'Missing "file"');
          if (file.size > config.maxAudioBytes) throw new ApiError(413, 'file_too_large', 'Audio is limited to 25 MB');
          const field = (k: string) => {
            const v = form.get(k);
            return typeof v === 'string' && v.trim() ? v.trim() : undefined;
          };
          const format = (field('response_format') ?? 'json') as (typeof FORMATS)[number];
          if (!FORMATS.includes(format)) throw new ApiError(400, 'invalid_request', `response_format must be one of ${FORMATS.join(', ')}`);
          const requested = field('model') ?? 'auto';
          const language = field('language');
          const prompt = field('prompt');

          // Upstreams get a WAV rebuilt from the parsed samples, so they decode exactly what is billed.
          const audio = normalizeAudio(Buffer.from(await file.arrayBuffer()));
          if (!audio) throw new ApiError(400, 'unsupported_audio', 'Send WAV audio (PCM16 or 32-bit float, mono or stereo, 8-48 kHz), ideally 16 kHz mono PCM16');
          const { duration } = audio;
          const billed = Math.max(duration, MIN_BILLED_SECONDS);

          const chain = await modelChain('stt', requested, user.preferredStt);
          if (!chain.length) throw new ApiError(400, 'model_not_found', `Unknown or disabled model "${requested}"`);

          const quota = quotaError(user, await managedSeconds(user.id, currentPeriod(user)), billed);
          if (quota) throw new ApiError(402, quota, quotaMessage(quota));

          let lastError = 'no provider is configured for this model';
          let busy: ApiError | null = null;
          for await (const { model, key } of attempts(chain, STT_ADAPTERS)) {
            const t0 = Date.now();
            try {
              const r = await STT_ADAPTERS[model.provider]!({
                audio: audio.wav,
                filename: 'audio.wav',
                mimeType: 'audio/wav',
                upstreamModel: model.upstreamModel,
                language,
                prompt,
                key,
              });
              const event = await recordGatewayEvent(user, {
                model,
                status: 'ok',
                latencyMs: Date.now() - t0,
                audioSeconds: billed,
                words: countWords(r.text),
              });
              if (event.overageMinutes > 0) reportOverage().catch((e) => console.error(e));
              c.header('x-opstrellis-model', model.id);
              if (format === 'text') return c.text(r.text);
              if (format === 'json') return c.json({ text: r.text });
              return c.json({ task: 'transcribe', language: r.language ?? language ?? null, duration, text: r.text, model: model.id });
            } catch (e) {
              // A full local queue is not an upstream failure: try the next model, else report it as is.
              if (e instanceof ApiError) {
                busy = e;
                continue;
              }
              lastError = `${model.id}: ${(e as Error).message}`;
              console.warn(`stt fallback: ${lastError}`);
              await recordGatewayEvent(user, { model, status: 'error', latencyMs: Date.now() - t0 });
            }
          }
          if (busy) throw busy;
          console.warn(`stt failed: ${lastError}`);
          throw new ApiError(502, 'upstream_error', 'All providers for this model failed; try again later');
        },
        MAX_IN_FLIGHT,
      );
    } catch (e) {
      if (!reading) await discardBody(c, user.id);
      throw e;
    }
  },
);

const MAX_OUTPUT_TOKENS = 4096;

// Only these fields reach the upstream (unknown ones such as n, tools or service_tier are dropped).
const chatSchema = z.object({
  model: z.string().default('auto'),
  messages: z.array(z.object({ role: z.string(), content: z.unknown() })).min(1),
  stream: z.boolean().optional(),
  temperature: z.number().min(0).max(2).optional(),
  top_p: z.number().min(0).max(1).optional(),
  max_tokens: z.number().int().positive().optional(),
  stop: z.union([z.string(), z.array(z.string()).max(4)]).optional(),
  response_format: z.looseObject({ type: z.string() }).optional(),
});

gateway.post('/chat/completions', jsonBodyLimit(1024 * 1024), async (c) => {
  const user = c.get('user');
  const { model: requested, stream, max_tokens, ...params } = await readJson(c, chatSchema);
  if (stream) throw new ApiError(400, 'invalid_request', 'stream: true is not supported; use stream: false');
  const chain = await modelChain('llm', requested, user.preferredLlm);
  if (!chain.length) throw new ApiError(400, 'model_not_found', `Unknown or disabled model "${requested}"`);

  // Quota check and metering run under one per-user lock, so parallel requests cannot all pass the check.
  return serial(
    `llm:${user.id}`,
    async () => {
      const remaining = PLANS[user.plan].llmTokensPerPeriod - (await llmTokens(user.id, currentPeriod(user)));
      if (remaining <= 0) throw new ApiError(402, 'quota_exceeded', 'LLM token allowance for this period is used up');
      // Input tokens count against the allowance too: estimate them (about 4 bytes a token) so one
      // large prompt can't run far past what is left.
      const outputLeft = remaining - Math.ceil(JSON.stringify(params).length / 4);
      if (outputLeft <= 0) throw new ApiError(402, 'quota_exceeded', 'This request is larger than the LLM token allowance left for this period');
      const body = { ...params, n: 1, max_tokens: Math.min(max_tokens ?? MAX_OUTPUT_TOKENS, MAX_OUTPUT_TOKENS, outputLeft) };

      let lastError = 'no provider is configured for this model';
      for await (const { model, key } of attempts(chain, LLM_ADAPTERS)) {
        const t0 = Date.now();
        try {
          const r = await LLM_ADAPTERS[model.provider]!({ body, upstreamModel: model.upstreamModel, key });
          await recordGatewayEvent(user, { model, status: 'ok', latencyMs: Date.now() - t0, inputTokens: r.inputTokens, outputTokens: r.outputTokens });
          c.header('x-opstrellis-model', model.id);
          return c.json({ ...r.response, model: model.id });
        } catch (e) {
          lastError = `${model.id}: ${(e as Error).message}`;
          console.warn(`llm fallback: ${lastError}`);
          await recordGatewayEvent(user, { model, status: 'error', latencyMs: Date.now() - t0 });
        }
      }
      console.warn(`llm failed: ${lastError}`);
      throw new ApiError(502, 'upstream_error', 'All providers for this model failed; try again later');
    },
    MAX_IN_FLIGHT,
  );
});

gateway.get('/models', async (c) => {
  const rows = await listModels({ enabledOnly: true });
  return c.json({
    object: 'list',
    data: rows.map((m) => ({ id: m.id, object: 'model', created: 0, owned_by: m.provider, kind: m.kind, streaming: m.streaming })),
  });
});
