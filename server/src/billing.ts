// Plans, billing periods and the included/overage split. Pure functions; unit-tested precisely.
import { config } from './config.js';
import type { User } from './db/schema.js';

export const PLANS = {
  free: { name: 'Free', priceUsd: 0, includedMinutes: 15, overagePerMinUsd: 0, llmTokensPerPeriod: 200_000 },
  pro: { name: 'Pro', priceUsd: 12, includedMinutes: 600, overagePerMinUsd: 0.02, llmTokensPerPeriod: 5_000_000 },
} as const;
export type PlanId = keyof typeof PLANS;

type BillingUser = Pick<User, 'plan' | 'planStatus' | 'periodStart' | 'periodEnd'>;

/** The Stripe subscription period when we know it and it covers `now`, else the UTC calendar month. */
export function currentPeriod(user: BillingUser, now = new Date()): { start: Date; end: Date } {
  if (user.periodStart && user.periodEnd && user.periodStart <= now && now < user.periodEnd) {
    return { start: user.periodStart, end: user.periodEnd };
  }
  const start = new Date(Date.UTC(now.getUTCFullYear(), now.getUTCMonth(), 1));
  const end = new Date(Date.UTC(now.getUTCFullYear(), now.getUTCMonth() + 1, 1));
  return { start, end };
}

/** Overage is only allowed on a Pro plan in good standing, and not at all when the cap is 0. */
export function allowsOverage(user: BillingUser, maxOverageMinutes = config.maxOverageMinutes): boolean {
  return maxOverageMinutes > 0 && user.plan === 'pro' && (user.planStatus === 'active' || user.planStatus === 'trialing');
}

/**
 * Splits a managed transcription of `eventSeconds` given `usedSeconds` already consumed this period.
 * Overage is billed per started minute of *cumulative* overage, so the sum of `overageMinutes` over a
 * period always equals ceil(total overage seconds / 60), however the usage is chunked.
 * Arithmetic is done in integer milliseconds to avoid floating-point drift.
 */
export function splitUsage(usedSeconds: number, eventSeconds: number, includedMinutes: number) {
  const ms = (s: number) => Math.round(s * 1000);
  const included = includedMinutes * 60_000;
  const before = ms(usedSeconds);
  const after = before + ms(eventSeconds);
  const overBefore = Math.max(0, before - included);
  const overAfter = Math.max(0, after - included);
  const overageMs = overAfter - overBefore;
  return {
    includedSeconds: (after - before - overageMs) / 1000,
    overageSeconds: overageMs / 1000,
    overageMinutes: Math.ceil(overAfter / 60_000) - Math.ceil(overBefore / 60_000),
  };
}

/** Billed amount for a managed transcription (0 for anything inside the plan). */
export function priceUsage(user: BillingUser, usedSeconds: number, eventSeconds: number) {
  const plan = PLANS[user.plan];
  const split = splitUsage(usedSeconds, eventSeconds, plan.includedMinutes);
  const overageMinutes = allowsOverage(user) ? split.overageMinutes : 0;
  return { ...split, overageMinutes, billedUsd: round6(overageMinutes * plan.overagePerMinUsd) };
}

export type QuotaError = 'quota_exceeded' | 'overage_cap_reached';

/** null when the user may run a managed transcription of `requestSeconds`, else the reason. */
export function quotaError(user: BillingUser, usedSeconds: number, requestSeconds = 0, maxOverageMinutes = config.maxOverageMinutes): QuotaError | null {
  const overage = allowsOverage(user, maxOverageMinutes);
  const limit = (PLANS[user.plan].includedMinutes + (overage ? maxOverageMinutes : 0)) * 60;
  if (usedSeconds < limit && usedSeconds + requestSeconds <= limit) return null;
  return overage ? 'overage_cap_reached' : 'quota_exceeded';
}

export function quotaMessage(code: QuotaError, maxOverageMinutes = config.maxOverageMinutes): string {
  return code === 'overage_cap_reached'
    ? `This period's overage limit of ${maxOverageMinutes} minutes is reached; it resets next period, or use your own key`
    : 'Not enough managed minutes left this period; upgrade to Pro or use your own key';
}

export const round6 = (n: number) => Math.round(n * 1e6) / 1e6;
