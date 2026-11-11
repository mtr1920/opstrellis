import { describe, expect, it } from 'vitest';
import { currentPeriod, priceUsage, quotaError, quotaMessage, splitUsage } from '../src/billing.js';
import { streaks } from '../src/usage.js';

const pro = { plan: 'pro' as const, planStatus: 'active', periodStart: null, periodEnd: null };
const free = { plan: 'free' as const, planStatus: 'active', periodStart: null, periodEnd: null };

describe('splitUsage', () => {
  it('is fully included below the allowance', () => {
    expect(splitUsage(0, 90, 15)).toEqual({ includedSeconds: 90, overageSeconds: 0, overageMinutes: 0 });
    expect(splitUsage(800, 100, 15)).toEqual({ includedSeconds: 100, overageSeconds: 0, overageMinutes: 0 });
  });

  it('lands exactly on the allowance without overage', () => {
    expect(splitUsage(840, 60, 15)).toEqual({ includedSeconds: 60, overageSeconds: 0, overageMinutes: 0 });
  });

  it('splits an event that straddles the allowance', () => {
    // 14.5 min used + 2 min event, 15 included -> 0.5 included-min, 1.5 overage-min, billed 2 started minutes
    expect(splitUsage(870, 120, 15)).toEqual({ includedSeconds: 30, overageSeconds: 90, overageMinutes: 2 });
  });

  it('bills per started minute of cumulative overage', () => {
    // Already 10 s into overage (minute 1 billed). +40 s stays within minute 1, +20 s crosses into minute 2.
    expect(splitUsage(910, 40, 15).overageMinutes).toBe(0);
    expect(splitUsage(950, 20, 15).overageMinutes).toBe(1);
  });

  it('sums to ceil(total overage) regardless of chunking', () => {
    const included = 60; // minutes; the chunks below total ~64.6 min
    let used = 0;
    let billed = 0;
    const chunks = [3599.7, 0.4, 12.345, 33.3, 59.99, 0.01, 7, 120.5, 0.001, 45];
    for (const c of chunks) {
      billed += splitUsage(used, c, included).overageMinutes;
      used += c;
    }
    const overSec = used - included * 60;
    expect(billed).toBe(Math.ceil(Math.round(overSec * 1000) / 60_000));
    expect(billed).toBe(5);
  });

  it('has no floating point drift at the boundary', () => {
    // 0.1 + 0.2 style inputs landing exactly on 15:00
    expect(splitUsage(899.9, 0.1, 15).overageMinutes).toBe(0);
    expect(splitUsage(899.9, 0.1, 15).overageSeconds).toBe(0);
  });
});

describe('priceUsage and quota', () => {
  it('charges Pro overage at $0.02 per minute', () => {
    const p = priceUsage(pro, 600 * 60 - 30, 150); // 2 min overage
    expect(p.overageMinutes).toBe(2);
    expect(p.billedUsd).toBe(0.04);
    expect(priceUsage(pro, 0, 600).billedUsd).toBe(0);
  });

  it('never bills the free plan or a past-due Pro plan', () => {
    expect(priceUsage(free, 15 * 60, 60)).toMatchObject({ overageMinutes: 0, billedUsd: 0 });
    expect(priceUsage({ ...pro, planStatus: 'past_due' }, 600 * 60, 60).billedUsd).toBe(0);
  });

  it('blocks free users once the allowance is used', () => {
    expect(quotaError(free, 14.9 * 60)).toBeNull();
    expect(quotaError(free, 15 * 60)).toBe('quota_exceeded');
    expect(quotaError(pro, 3000 * 60)).toBeNull();
    expect(quotaError({ ...pro, planStatus: 'past_due' }, 600 * 60)).toBe('quota_exceeded');
  });

  it('caps Pro overage per period (MAX_OVERAGE_MINUTES, default 3000)', () => {
    const cap = (600 + 3000) * 60;
    expect(quotaError(pro, cap - 10, 10)).toBeNull();
    expect(quotaError(pro, cap - 10, 11)).toBe('overage_cap_reached');
    expect(quotaError(pro, cap)).toBe('overage_cap_reached');
    expect(quotaError(pro, 700 * 60, 0, 50)).toBe('overage_cap_reached');
    expect(quotaError(pro, 649 * 60, 60, 50)).toBeNull();
    expect(quotaMessage('overage_cap_reached', 50)).toMatch(/50 minutes/);
    expect(quotaMessage('quota_exceeded')).toMatch(/upgrade/);
  });

  it('MAX_OVERAGE_MINUTES=0 disables overage: Pro stops at its included minutes', () => {
    expect(quotaError(pro, 600 * 60, 0, 0)).toBe('quota_exceeded');
    expect(quotaError(pro, 599 * 60, 60, 0)).toBeNull();
    expect(quotaError(pro, 599 * 60, 61, 0)).toBe('quota_exceeded');
  });
});

describe('currentPeriod', () => {
  it('uses the UTC calendar month by default', () => {
    const p = currentPeriod(free, new Date('2026-02-14T23:59:00Z'));
    expect(p.start.toISOString()).toBe('2026-02-01T00:00:00.000Z');
    expect(p.end.toISOString()).toBe('2026-03-01T00:00:00.000Z');
  });

  it('uses the Stripe period when it covers now', () => {
    const periodStart = new Date('2026-02-10T08:00:00Z');
    const periodEnd = new Date('2026-03-10T08:00:00Z');
    expect(currentPeriod({ ...pro, periodStart, periodEnd }, new Date('2026-03-01T00:00:00Z')).start).toEqual(periodStart);
    // stale period -> calendar month
    expect(currentPeriod({ ...pro, periodStart, periodEnd }, new Date('2026-04-02T00:00:00Z')).start.toISOString()).toBe(
      '2026-04-01T00:00:00.000Z',
    );
  });
});

describe('streaks', () => {
  it('computes current and longest streaks', () => {
    const days = ['2026-01-01', '2026-01-02', '2026-01-03', '2026-01-04', '2026-01-10', '2026-01-11', '2026-01-12'];
    expect(streaks(days, '2026-01-12')).toEqual({ current: 3, longest: 4 });
    // nothing yet today: streak still alive from yesterday
    expect(streaks(days, '2026-01-13')).toEqual({ current: 3, longest: 4 });
    // missed a day: broken
    expect(streaks(days, '2026-01-14')).toEqual({ current: 0, longest: 4 });
    expect(streaks([], '2026-01-14')).toEqual({ current: 0, longest: 0 });
  });

  it('handles month and year boundaries', () => {
    expect(streaks(['2025-12-30', '2025-12-31', '2026-01-01', '2026-02-28', '2026-03-01'], '2026-03-01')).toEqual({
      current: 2,
      longest: 3,
    });
  });
});
