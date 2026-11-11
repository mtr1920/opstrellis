// Admin-only: users, model catalog, upstream provider keys, global stats.
import { and, count, desc, eq, gte, ilike, or, sql } from 'drizzle-orm';
import { Hono } from 'hono';
import { z } from 'zod';
import { requireAuth, type AppEnv } from '../auth.js';
import { currentPeriod } from '../billing.js';
import { listModels } from '../catalog.js';
import { db } from '../db/index.js';
import { models, sessions, usageEvents, users, type User } from '../db/schema.js';
import { ApiError, iso, parse, readJson, storableText } from '../lib/http.js';
import { deleteProviderKey, isProvider, listProviderKeys, setProviderKey } from '../providers.js';
import { adminStats, meSummary, userStats } from '../usage.js';

export const admin = new Hono<AppEnv>();
// Sessions only: an API token (e.g. from a device sign-in an admin was tricked into approving)
// must never carry admin rights.
admin.use('*', requireAuth({ role: 'admin', sessionOnly: true }));

admin.get('/stats', async (c) => c.json(await adminStats()));

// ---------- users ----------

const userJson = (u: User) => ({
  id: u.id,
  email: u.email,
  name: u.name,
  role: u.role,
  plan: u.plan,
  plan_status: u.planStatus,
  disabled: u.disabled,
  stripe_customer_id: u.stripeCustomerId,
  created_at: iso(u.createdAt),
});

admin.get('/users', async (c) => {
  const q = parse(
    z.object({
      q: storableText().trim().max(100).optional(),
      page: z.coerce.number().int().min(1).default(1),
      page_size: z.coerce.number().int().min(1).max(100).default(25),
    }),
    c.req.query(),
  );
  const like = q.q ? `%${q.q.replace(/[%_\\]/g, '\\$&')}%` : null;
  const where = like ? or(ilike(users.email, like), ilike(users.name, like)) : undefined;
  const monthStart = new Date(Date.UTC(new Date().getUTCFullYear(), new Date().getUTCMonth(), 1));
  const [rows, [total]] = await Promise.all([
    db
      .select({
        user: users,
        minutes: sql<number>`coalesce((select sum(${usageEvents.audioSeconds}) from ${usageEvents}
          where ${usageEvents.userId} = "users"."id" and ${usageEvents.source} = 'gateway' and ${usageEvents.status} = 'ok'
          and ${usageEvents.createdAt} >= ${monthStart.toISOString()}), 0)::float8 / 60`,
      })
      .from(users)
      .where(where)
      .orderBy(desc(users.createdAt))
      .limit(q.page_size)
      .offset((q.page - 1) * q.page_size),
    db.select({ n: count() }).from(users).where(where),
  ]);
  return c.json({
    users: rows.map((r) => ({ ...userJson(r.user), managed_minutes_this_month: Math.round(r.minutes * 100) / 100 })),
    total: total?.n ?? 0,
    page: q.page,
    page_size: q.page_size,
  });
});

async function getUser(id: string): Promise<User> {
  if (!z.uuid().safeParse(id).success) throw new ApiError(404, 'not_found');
  const [u] = await db.select().from(users).where(eq(users.id, id));
  if (!u) throw new ApiError(404, 'not_found');
  return u;
}

admin.get('/users/:id', async (c) => {
  const u = await getUser(c.req.param('id'));
  const period = currentPeriod(u);
  const recent = await db
    .select()
    .from(usageEvents)
    .where(and(eq(usageEvents.userId, u.id), gte(usageEvents.createdAt, period.start)))
    .orderBy(desc(usageEvents.createdAt))
    .limit(50);
  return c.json({
    user: userJson(u),
    summary: await meSummary(u),
    stats: await userStats(u.id),
    recent_events: recent.map((e) => ({
      id: e.id,
      source: e.source,
      mode: e.mode,
      kind: e.kind,
      model: e.model,
      audio_seconds: e.audioSeconds,
      words: e.words,
      tokens: e.inputTokens + e.outputTokens,
      provider_cost_usd: e.providerCostUsd,
      billed_usd: e.billedUsd,
      status: e.status,
      created_at: iso(e.createdAt),
    })),
  });
});

admin.patch('/users/:id', async (c) => {
  const u = await getUser(c.req.param('id'));
  const body = await readJson(
    c,
    z.object({ role: z.enum(['user', 'admin']).optional(), plan: z.enum(['free', 'pro']).optional(), disabled: z.boolean().optional() }),
  );
  if (u.id === c.get('user').id && (body.role === 'user' || body.disabled)) {
    throw new ApiError(400, 'cannot_modify_self', 'You cannot demote or disable your own account');
  }
  const patch = { ...body, ...(body.plan ? { planStatus: 'active' } : {}) };
  if (!Object.keys(patch).length) return c.json({ user: userJson(u) });
  const [updated] = await db.update(users).set(patch).where(eq(users.id, u.id)).returning();
  if (body.disabled) await db.delete(sessions).where(eq(sessions.userId, u.id));
  return c.json({ user: userJson(updated!) });
});

// ---------- model catalog ----------

const modelFields = {
  kind: z.enum(['stt', 'llm']),
  provider: storableText().trim().min(1).max(50),
  upstream_model: storableText().trim().min(1).max(200),
  display_name: storableText().trim().min(1).max(100),
  streaming: z.boolean().default(false),
  price_per_min_usd: z.number().min(0).default(0),
  cost_per_min_usd: z.number().min(0).default(0),
  input_per_1m_usd: z.number().min(0).default(0),
  output_per_1m_usd: z.number().min(0).default(0),
  enabled: z.boolean().default(true),
  fallback_order: z.number().int().min(0).max(10_000).default(100),
};

type ModelInput = Partial<z.infer<z.ZodObject<typeof modelFields>>>;
const toRow = (b: ModelInput) =>
  Object.fromEntries(
    Object.entries({
      kind: b.kind,
      provider: b.provider,
      upstreamModel: b.upstream_model,
      displayName: b.display_name,
      streaming: b.streaming,
      pricePerMinUsd: b.price_per_min_usd,
      costPerMinUsd: b.cost_per_min_usd,
      inputPer1mUsd: b.input_per_1m_usd,
      outputPer1mUsd: b.output_per_1m_usd,
      enabled: b.enabled,
      fallbackOrder: b.fallback_order,
    }).filter(([, v]) => v !== undefined),
  );

/** An id from the URL; one Postgres could not compare (e.g. with NUL) cannot exist, so it is a 404. */
function storedId(raw: string) {
  if (!storableText().safeParse(raw).success) throw new ApiError(404, 'not_found');
  return raw;
}

const modelJson = (m: typeof models.$inferSelect) => ({
  id: m.id,
  kind: m.kind,
  provider: m.provider,
  upstream_model: m.upstreamModel,
  display_name: m.displayName,
  streaming: m.streaming,
  price_per_min_usd: m.pricePerMinUsd,
  cost_per_min_usd: m.costPerMinUsd,
  input_per_1m_usd: m.inputPer1mUsd,
  output_per_1m_usd: m.outputPer1mUsd,
  enabled: m.enabled,
  fallback_order: m.fallbackOrder,
});

admin.get('/models', async (c) => c.json({ models: (await listModels()).map(modelJson) }));

admin.post('/models', async (c) => {
  const body = await readJson(c, z.object({ id: z.string().trim().regex(/^[a-z0-9-]+\/[\w.:-]+$/, 'id must look like provider/model'), ...modelFields }));
  const [row] = await db
    .insert(models)
    .values({ id: body.id, ...toRow(body) } as typeof models.$inferInsert)
    .onConflictDoNothing()
    .returning();
  if (!row) throw new ApiError(409, 'exists', 'A model with this id already exists');
  return c.json({ model: modelJson(row) }, 201);
});

admin.patch('/models/:id{.+}', async (c) => {
  const body = await readJson(c, z.object(modelFields).partial());
  const row = toRow(body);
  if (!Object.keys(row).length) throw new ApiError(400, 'invalid_request', 'Nothing to update');
  const [updated] = await db.update(models).set(row).where(eq(models.id, storedId(c.req.param('id')))).returning();
  if (!updated) throw new ApiError(404, 'not_found');
  return c.json({ model: modelJson(updated) });
});

admin.delete('/models/:id{.+}', async (c) => {
  const deleted = await db.delete(models).where(eq(models.id, storedId(c.req.param('id')))).returning();
  if (!deleted.length) throw new ApiError(404, 'not_found');
  return c.json({ ok: true });
});

// ---------- upstream provider keys ----------

admin.get('/providers', async (c) => c.json({ providers: await listProviderKeys() }));

admin.put('/providers/:provider', async (c) => {
  const provider = c.req.param('provider');
  if (!isProvider(provider)) throw new ApiError(404, 'not_found');
  const { key } = await readJson(c, z.object({ key: storableText().trim().min(8).max(500) }));
  await setProviderKey(provider, key);
  return c.json({ ok: true });
});

admin.delete('/providers/:provider', async (c) => {
  await deleteProviderKey(storedId(c.req.param('provider')));
  return c.json({ ok: true });
});

