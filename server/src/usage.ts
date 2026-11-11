// Metering queries, gateway usage recording, and stats for users and admins.
import { randomUUID } from 'node:crypto';
import { and, eq, gte, lt, sql } from 'drizzle-orm';
import { currentPeriod, PLANS, priceUsage, round6 } from './billing.js';
import { config } from './config.js';
import { db } from './db/index.js';
import { usageEvents, users, type Model, type User } from './db/schema.js';

type Tx = Parameters<Parameters<typeof db.transaction>[0]>[0];
type Period = { start: Date; end: Date };

const inPeriod = (userId: string, p: Period) =>
  and(eq(usageEvents.userId, userId), gte(usageEvents.createdAt, p.start), lt(usageEvents.createdAt, p.end));

/** Managed (gateway) audio seconds successfully transcribed in the period. */
export async function managedSeconds(userId: string, p: Period, tx: Tx | typeof db = db): Promise<number> {
  const [r] = await tx
    .select({ s: sql<number>`coalesce(sum(${usageEvents.audioSeconds}), 0)::float8` })
    .from(usageEvents)
    .where(and(inPeriod(userId, p), eq(usageEvents.source, 'gateway'), eq(usageEvents.kind, 'stt'), eq(usageEvents.status, 'ok')));
  return r?.s ?? 0;
}

export async function llmTokens(userId: string, p: Period): Promise<number> {
  const [r] = await db
    .select({ t: sql<number>`coalesce(sum(${usageEvents.inputTokens}::int8 + ${usageEvents.outputTokens}), 0)::float8` })
    .from(usageEvents)
    .where(and(inPeriod(userId, p), eq(usageEvents.source, 'gateway'), eq(usageEvents.kind, 'llm')));
  return r?.t ?? 0;
}

interface GatewayEvent {
  model: Model;
  status: 'ok' | 'error';
  latencyMs: number;
  audioSeconds?: number;
  words?: number;
  inputTokens?: number;
  outputTokens?: number;
}

/**
 * Records one gateway call. Successful transcriptions are priced against the period's usage under a
 * per-user advisory lock, so concurrent requests cannot both claim the same included minutes.
 */
export async function recordGatewayEvent(user: User, e: GatewayEvent) {
  const id = randomUUID();
  const seconds = e.status === 'ok' ? (e.audioSeconds ?? 0) : 0;
  const inputTokens = e.inputTokens ?? 0;
  const outputTokens = e.outputTokens ?? 0;
  const providerCostUsd =
    e.status !== 'ok'
      ? 0
      : round6((seconds / 60) * e.model.costPerMinUsd + (inputTokens * e.model.inputPer1mUsd + outputTokens * e.model.outputPer1mUsd) / 1e6);
  return db.transaction(async (tx) => {
    let billedUsd = 0;
    let overageMinutes = 0;
    if (e.model.kind === 'stt' && seconds > 0) {
      await tx.execute(sql`select pg_advisory_xact_lock(hashtextextended(${user.id}, 0))`);
      const used = await managedSeconds(user.id, currentPeriod(user), tx);
      ({ billedUsd, overageMinutes } = priceUsage(user, used, seconds));
    }
    const [row] = await tx
      .insert(usageEvents)
      .values({
        id,
        userId: user.id,
        source: 'gateway',
        mode: 'cloud',
        kind: e.model.kind,
        provider: e.model.provider,
        model: e.model.id,
        audioSeconds: seconds,
        words: e.words ?? 0,
        inputTokens,
        outputTokens,
        providerCostUsd,
        billedUsd,
        overageMinutes,
        status: e.status,
        latencyMs: e.latencyMs,
      })
      .returning();
    return row!;
  });
}

/** Payload of GET /api/me (also used by the dashboard). */
export async function meSummary(user: User) {
  const period = currentPeriod(user);
  const plan = PLANS[user.plan];
  const [agg] = await db
    .select({
      managed: sql<number>`coalesce(sum(${usageEvents.audioSeconds}) filter (where ${usageEvents.source} = 'gateway' and ${usageEvents.kind} = 'stt' and ${usageEvents.status} = 'ok'), 0)::float8`,
      words: sql<number>`coalesce(sum(${usageEvents.words}), 0)::float8`,
      billed: sql<number>`coalesce(sum(${usageEvents.billedUsd}), 0)::float8`,
      overage: sql<number>`coalesce(sum(${usageEvents.overageMinutes}), 0)::float8`,
    })
    .from(usageEvents)
    .where(inPeriod(user.id, period));
  const managedMinutes = (agg?.managed ?? 0) / 60;
  return {
    user: { id: user.id, email: user.email, name: user.name, role: user.role },
    plan: user.plan,
    plan_status: user.planStatus,
    period: { start: period.start.toISOString(), end: period.end.toISOString() },
    usage: {
      managed_minutes: round6(managedMinutes),
      included_minutes: plan.includedMinutes,
      overage_minutes: agg?.overage ?? 0,
      remaining_minutes: round6(Math.max(0, plan.includedMinutes - managedMinutes)),
      words: agg?.words ?? 0,
      cost_usd: round6(agg?.billed ?? 0),
    },
    limits: {
      included_minutes: plan.includedMinutes,
      overage_per_min_usd: plan.overagePerMinUsd,
      max_overage_minutes: plan.overagePerMinUsd > 0 ? config.maxOverageMinutes : 0,
      llm_tokens: plan.llmTokensPerPeriod,
    },
    preferred_model: user.preferredStt,
    preferred_llm: user.preferredLlm,
  };
}

const shiftDay = (d: string, n: number) => new Date(Date.parse(`${d}T00:00:00Z`) + n * 86_400_000).toISOString().slice(0, 10);
const prev = (d: string) => shiftDay(d, -1);
const next = (d: string) => shiftDay(d, 1);

/** Current and longest runs of consecutive days. `days` are distinct YYYY-MM-DD strings, any order. */
export function streaks(days: string[], today: string) {
  const set = new Set(days);
  let longest = 0;
  for (const d of set) {
    if (set.has(prev(d))) continue; // not the start of a run
    let len = 1;
    for (let x = d; set.has(next(x)); x = next(x)) len++;
    longest = Math.max(longest, len);
  }
  // The current streak may end today or (if nothing yet today) yesterday.
  let current = 0;
  for (let d = set.has(today) ? today : prev(today); set.has(d); d = prev(d)) current++;
  return { current, longest };
}

export function validTimeZone(tz: string | undefined): string {
  if (!tz) return 'UTC';
  try {
    new Intl.DateTimeFormat('en-US', { timeZone: tz });
    return tz;
  } catch {
    return 'UTC';
  }
}

/** Per-user dictation stats. Days are computed in the caller's time zone. */
export async function userStats(userId: string, tz = 'UTC', days = 90) {
  const day = sql`to_char(${usageEvents.createdAt} at time zone ${tz}, 'YYYY-MM-DD')`;
  const mine = eq(usageEvents.userId, userId);
  // The desktop may also report its cloud dictations (to attach an app category); the gateway has
  // already counted those, so they only feed the app-category breakdown.
  const ok = and(mine, eq(usageEvents.status, 'ok'), sql`not (${usageEvents.source} = 'client' and ${usageEvents.mode} = 'cloud')`);
  const categorized = and(mine, eq(usageEvents.status, 'ok'), eq(usageEvents.kind, 'stt'), eq(usageEvents.source, 'client'));
  const [totals] = await db
    .select({
      words: sql<number>`coalesce(sum(${usageEvents.words}), 0)::float8`,
      seconds: sql<number>`coalesce(sum(${usageEvents.audioSeconds}) filter (where ${usageEvents.kind} = 'stt'), 0)::float8`,
      sttWords: sql<number>`coalesce(sum(${usageEvents.words}) filter (where ${usageEvents.kind} = 'stt' and ${usageEvents.audioSeconds} > 0), 0)::float8`,
      dictations: sql<number>`count(*) filter (where ${usageEvents.kind} = 'stt')::int`,
      cost: sql<number>`coalesce(sum(${usageEvents.providerCostUsd}), 0)::float8`,
      billed: sql<number>`coalesce(sum(${usageEvents.billedUsd}), 0)::float8`,
    })
    .from(usageEvents)
    .where(ok);
  const activeDays = await db
    .selectDistinct({ day: sql<string>`${day}` })
    .from(usageEvents)
    .where(and(ok, eq(usageEvents.kind, 'stt')));
  const [{ today } = { today: '' }] = await db.execute<{ today: string }>(
    sql`select to_char(now() at time zone ${tz}, 'YYYY-MM-DD') as today`,
  );
  const since = new Date(Date.now() - days * 86_400_000);
  const daily = await db
    .select({ day: sql<string>`${day}`, words: sql<number>`sum(${usageEvents.words})::float8`, seconds: sql<number>`sum(${usageEvents.audioSeconds})::float8` })
    .from(usageEvents)
    .where(and(ok, gte(usageEvents.createdAt, since)))
    .groupBy(sql`1`) // by position: the tz parameter makes the expression differ textually
    .orderBy(sql`1`);
  const appCat = sql<string>`coalesce(${usageEvents.appCategory}, 'other')`;
  const byApp = await db
    .select({ app_category: appCat, words: sql<number>`sum(${usageEvents.words})::float8`, seconds: sql<number>`sum(${usageEvents.audioSeconds})::float8` })
    .from(usageEvents)
    .where(categorized)
    .groupBy(appCat)
    .orderBy(sql`2 desc`);
  const byModel = await db
    .select({
      provider: usageEvents.provider,
      model: usageEvents.model,
      mode: usageEvents.mode,
      events: sql<number>`count(*)::int`,
      seconds: sql<number>`sum(${usageEvents.audioSeconds})::float8`,
      words: sql<number>`sum(${usageEvents.words})::float8`,
      tokens: sql<number>`sum(${usageEvents.inputTokens}::int8 + ${usageEvents.outputTokens})::float8`,
      cost_usd: sql<number>`sum(${usageEvents.providerCostUsd})::float8`,
      billed_usd: sql<number>`sum(${usageEvents.billedUsd})::float8`,
    })
    .from(usageEvents)
    .where(ok)
    .groupBy(usageEvents.provider, usageEvents.model, usageEvents.mode)
    .orderBy(sql`count(*) desc`);
  const t = totals!;
  return {
    time_zone: tz,
    totals: {
      words: t.words,
      dictations: t.dictations,
      audio_minutes: round6(t.seconds / 60),
      avg_wpm: t.seconds > 0 ? Math.round(t.sttWords / (t.seconds / 60)) : 0,
      cost_usd: round6(t.cost),
      billed_usd: round6(t.billed),
    },
    streak: streaks(activeDays.map((d) => d.day), today),
    daily: daily.map((d) => ({ day: d.day, words: d.words, minutes: round6(d.seconds / 60) })),
    by_app_category: byApp.map((a) => ({ app_category: a.app_category, words: a.words, minutes: round6(a.seconds / 60) })),
    by_model: byModel.map((m) => ({ ...m, minutes: round6(m.seconds / 60), cost_usd: round6(m.cost_usd), billed_usd: round6(m.billed_usd) })),
  };
}

/** Global view for admins: users, activity, and provider cost vs billed revenue. */
export async function adminStats(now = new Date()) {
  const month = { start: new Date(Date.UTC(now.getUTCFullYear(), now.getUTCMonth(), 1)), end: now };
  const [u] = await db
    .select({
      total: sql<number>`count(*)::int`,
      pro: sql<number>`count(*) filter (where ${users.plan} = 'pro' and ${users.planStatus} in ('active','trialing'))::int`,
      disabled: sql<number>`count(*) filter (where ${users.disabled})::int`,
      newThisMonth: sql<number>`count(*) filter (where ${users.createdAt} >= ${month.start.toISOString()})::int`,
    })
    .from(users);
  const thisMonth = and(gte(usageEvents.createdAt, month.start), lt(usageEvents.createdAt, month.end));
  const [m] = await db
    .select({
      active: sql<number>`count(distinct ${usageEvents.userId})::int`,
      managedSeconds: sql<number>`coalesce(sum(${usageEvents.audioSeconds}) filter (where ${usageEvents.source} = 'gateway' and ${usageEvents.status} = 'ok'), 0)::float8`,
    })
    .from(usageEvents)
    .where(thisMonth);
  const monthCol = sql<string>`to_char(date_trunc('month', ${usageEvents.createdAt} at time zone 'UTC'), 'YYYY-MM')`;
  const gateway = eq(usageEvents.source, 'gateway');
  const monthly = await db
    .select({
      month: monthCol,
      managed_seconds: sql<number>`coalesce(sum(${usageEvents.audioSeconds}) filter (where ${usageEvents.status} = 'ok'), 0)::float8`,
      provider_cost_usd: sql<number>`sum(${usageEvents.providerCostUsd})::float8`,
      billed_usd: sql<number>`sum(${usageEvents.billedUsd})::float8`,
      active_users: sql<number>`count(distinct ${usageEvents.userId})::int`,
    })
    .from(usageEvents)
    .where(and(gateway, gte(usageEvents.createdAt, new Date(Date.UTC(now.getUTCFullYear(), now.getUTCMonth() - 11, 1)))))
    .groupBy(monthCol)
    .orderBy(monthCol);
  const perModel = await db
    .select({
      model: usageEvents.model,
      provider: usageEvents.provider,
      kind: usageEvents.kind,
      requests: sql<number>`count(*)::int`,
      errors: sql<number>`count(*) filter (where ${usageEvents.status} = 'error')::int`,
      seconds: sql<number>`sum(${usageEvents.audioSeconds})::float8`,
      tokens: sql<number>`sum(${usageEvents.inputTokens}::int8 + ${usageEvents.outputTokens})::float8`,
      provider_cost_usd: sql<number>`sum(${usageEvents.providerCostUsd})::float8`,
      billed_usd: sql<number>`sum(${usageEvents.billedUsd})::float8`,
      avg_latency_ms: sql<number>`coalesce(avg(${usageEvents.latencyMs}) filter (where ${usageEvents.status} = 'ok'), 0)::int`,
    })
    .from(usageEvents)
    .where(and(gateway, thisMonth))
    .groupBy(usageEvents.model, usageEvents.provider, usageEvents.kind)
    .orderBy(sql`count(*) desc`);
  const subscriptionRevenue = (u?.pro ?? 0) * PLANS.pro.priceUsd;
  const cur = monthly.find((r) => r.month === month.start.toISOString().slice(0, 7));
  return {
    users: { total: u?.total ?? 0, pro: u?.pro ?? 0, disabled: u?.disabled ?? 0, new_this_month: u?.newThisMonth ?? 0, active_this_month: m?.active ?? 0 },
    this_month: {
      managed_minutes: round6((m?.managedSeconds ?? 0) / 60),
      provider_cost_usd: round6(cur?.provider_cost_usd ?? 0),
      usage_billed_usd: round6(cur?.billed_usd ?? 0),
      subscription_revenue_usd: subscriptionRevenue,
      revenue_usd: round6(subscriptionRevenue + (cur?.billed_usd ?? 0)),
      margin_usd: round6(subscriptionRevenue + (cur?.billed_usd ?? 0) - (cur?.provider_cost_usd ?? 0)),
    },
    monthly: monthly.map((r) => ({
      month: r.month,
      managed_minutes: round6(r.managed_seconds / 60),
      provider_cost_usd: round6(r.provider_cost_usd),
      billed_usd: round6(r.billed_usd),
      active_users: r.active_users,
    })),
    per_model: perModel.map((r) => ({ ...r, minutes: round6(r.seconds / 60), provider_cost_usd: round6(r.provider_cost_usd), billed_usd: round6(r.billed_usd) })),
  };
}
