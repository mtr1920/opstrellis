// Account, catalog, usage ingestion, stats and billing endpoints for the desktop app and dashboard.
import { and, eq } from 'drizzle-orm';
import { Hono } from 'hono';
import { z } from 'zod';
import { rateLimit, requireAuth, type AppEnv } from '../auth.js';
import { PLANS } from '../billing.js';
import { catalogEntry, listModels } from '../catalog.js';
import { config, stripeEnabled } from '../config.js';
import { db } from '../db/index.js';
import { models, usageEvents, users } from '../db/schema.js';
import { ApiError, jsonBodyLimit, readJson, storableText } from '../lib/http.js';
import { createCheckoutUrl, createPortalUrl, handleStripeEvent, verifyWebhook } from '../stripe.js';
import { meSummary, userStats, validTimeZone } from '../usage.js';

export const api = new Hono<AppEnv>();

api.get('/me', requireAuth(), async (c) => c.json(await meSummary(c.get('user'))));

/** Checks that a preferred model exists, is enabled and has the right kind. */
async function checkModel(id: string | null | undefined, kind: 'stt' | 'llm') {
  if (!id) return;
  const [m] = await db.select().from(models).where(and(eq(models.id, id), eq(models.kind, kind), eq(models.enabled, true)));
  if (!m) throw new ApiError(400, 'model_not_found', `"${id}" is not an enabled ${kind} model`);
}

api.patch('/me', requireAuth(), async (c) => {
  const body = await readJson(
    c,
    z.object({
      name: storableText().trim().max(100).optional(),
      preferred_model: storableText().max(100).nullable().optional(),
      preferred_llm: storableText().max(100).nullable().optional(),
    }),
  );
  await checkModel(body.preferred_model, 'stt');
  await checkModel(body.preferred_llm, 'llm');
  const patch = { name: body.name, preferredStt: body.preferred_model, preferredLlm: body.preferred_llm };
  if (Object.values(patch).every((v) => v === undefined)) return c.json(await meSummary(c.get('user')));
  const [user] = await db
    .update(users)
    .set(patch)
    .where(eq(users.id, c.get('user').id))
    .returning();
  return c.json(await meSummary(user!));
});

api.get('/catalog', async (c) => {
  const rows = await listModels({ enabledOnly: true });
  return c.json({
    models: rows.map(catalogEntry),
    plans: Object.entries(PLANS).map(([id, p]) => ({
      id,
      name: p.name,
      price_usd: p.priceUsd,
      included_minutes: p.includedMinutes,
      overage_per_min_usd: p.overagePerMinUsd,
    })),
  });
});

// ---------- usage ingestion (BYOK / local / cloud metadata from the desktop app) ----------

const INT4_MAX = 2_147_483_647;

const usageEvent = z.strictObject({
  id: z.uuid(),
  provider: storableText().trim().min(1).max(50),
  model: storableText().trim().min(1).max(100),
  mode: z.enum(['byok', 'local', 'cloud']),
  kind: z.enum(['stt', 'llm']).default('stt'),
  audio_seconds: z.number().min(0).max(24 * 3600).default(0),
  words: z.number().int().min(0).max(1_000_000).default(0),
  input_tokens: z.number().int().min(0).max(INT4_MAX).default(0),
  output_tokens: z.number().int().min(0).max(INT4_MAX).default(0),
  app_category: storableText().trim().max(50).nullish(),
  provider_cost_usd: z.number().min(0).max(1000).nullish(),
  created_at: z.iso.datetime({ offset: true }),
});

// A desktop flushes up to 500 events once a minute and after dictations, far below this.
api.post('/usage', requireAuth(), rateLimit('usage', 30, 60_000, (c) => c.get('user').id), jsonBodyLimit(1024 * 1024), async (c) => {
  // strictObject rejects unknown keys, so transcript text can never be smuggled in.
  const { events } = await readJson(c, z.object({ events: z.array(usageEvent).max(500) }));
  const userId = c.get('user').id;
  const now = Date.now();
  const rows = events.map((e) => ({
    id: e.id,
    userId,
    source: 'client' as const,
    mode: e.mode,
    kind: e.kind,
    provider: e.provider,
    model: e.model,
    audioSeconds: e.audio_seconds,
    words: e.words,
    inputTokens: e.input_tokens,
    outputTokens: e.output_tokens,
    appCategory: e.app_category ?? null,
    providerCostUsd: e.provider_cost_usd ?? 0,
    // Clamp clock skew: no events from the future.
    createdAt: new Date(Math.min(Date.parse(e.created_at), now)),
  }));
  const inserted = rows.length
    ? await db.insert(usageEvents).values(rows).onConflictDoNothing({ target: usageEvents.id }).returning({ id: usageEvents.id })
    : [];
  return c.json({ accepted: inserted.length, duplicates: rows.length - inserted.length });
});

api.get('/stats', requireAuth(), async (c) => {
  return c.json(await userStats(c.get('user').id, validTimeZone(c.req.query('tz'))));
});

// ---------- billing ----------

api.get('/billing', requireAuth(), async (c) => {
  const user = c.get('user');
  return c.json({
    mode: stripeEnabled() ? 'stripe' : 'manual',
    plan: user.plan,
    plan_status: user.planStatus,
    has_customer: Boolean(user.stripeCustomerId),
    max_overage_minutes: config.maxOverageMinutes,
    plans: PLANS,
  });
});

function requireStripe() {
  if (!stripeEnabled()) throw new ApiError(400, 'billing_manual', 'Billing is managed manually by an administrator');
}

api.post('/billing/checkout', requireAuth({ sessionOnly: true }), async (c) => {
  requireStripe();
  if (!config.stripe.pricePro) throw new ApiError(500, 'misconfigured', 'STRIPE_PRICE_PRO is not set');
  const user = c.get('user');
  // Also for an unpaid subscription (plan Free), which can still be paid in the portal and revive.
  if (user.stripeSubscriptionId) throw new ApiError(409, 'already_subscribed', 'Manage your plan in the billing portal');
  return c.json({ url: await createCheckoutUrl(user) });
});

api.post('/billing/portal', requireAuth({ sessionOnly: true }), async (c) => {
  requireStripe();
  return c.json({ url: await createPortalUrl(c.get('user')) });
});

api.post('/stripe/webhook', jsonBodyLimit(1024 * 1024), async (c) => {
  if (!config.stripe.webhookSecret) throw new ApiError(400, 'billing_manual', 'Stripe webhooks are not configured');
  const payload = await c.req.text();
  let event;
  try {
    event = verifyWebhook(payload, c.req.header('stripe-signature') ?? '');
  } catch {
    throw new ApiError(400, 'invalid_signature');
  }
  await handleStripeEvent(event);
  return c.json({ received: true });
});
