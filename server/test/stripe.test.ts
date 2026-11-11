import { eq } from 'drizzle-orm';
import Stripe from 'stripe';
import { afterEach, beforeAll, describe, expect, it } from 'vitest';
import { config } from '../src/config.js';
import { users } from '../src/db/schema.js';
import { db, req, resetDb, signup } from './helpers.js';

const SECRET = 'whsec_test_secret';
const stripe = new Stripe('sk_test_unused');

/** Posts an event to the real webhook endpoint with a valid Stripe signature. */
function sendEvent(type: string, object: Record<string, unknown>, secret = SECRET, created = Math.floor(Date.now() / 1000)) {
  const payload = JSON.stringify({ id: `evt_${Math.random().toString(36).slice(2)}`, object: 'event', type, data: { object }, api_version: '2025-09-30.clover', created });
  const signature = stripe.webhooks.generateTestHeaderString({ payload, secret });
  return req('/api/stripe/webhook', { method: 'POST', body: payload, headers: { 'stripe-signature': signature, 'content-type': 'application/json' } });
}
const userRow = async (id: string) => (await db.select().from(users).where(eq(users.id, id)))[0]!;

beforeAll(resetDb);

describe('Stripe webhook', () => {
  it('rejects bad or missing signatures', async () => {
    expect((await sendEvent('checkout.session.completed', {}, 'whsec_wrong')).status).toBe(400);
    const res = await req('/api/stripe/webhook', { method: 'POST', body: '{}', headers: { 'stripe-signature': 't=1,v1=abc' } });
    expect(res.status).toBe(400);
    expect((await res.json()).error).toBe('invalid_signature');
  });

  it('walks a user through checkout -> renewal -> payment failure -> cancellation', async () => {
    const { user, cookie } = await signup();
    // A period around now: one that is already over falls back to the calendar month.
    const start = Math.floor(Date.now() / 1000) - 10 * 86_400;
    const end = start + 30 * 86_400;
    const startIso = new Date(start * 1000).toISOString();
    const endIso = new Date(end * 1000).toISOString();

    // 1. checkout completes -> Pro
    let res = await sendEvent('checkout.session.completed', { id: 'cs_1', object: 'checkout.session', mode: 'subscription', client_reference_id: user.id, customer: 'cus_123', subscription: 'sub_123' });
    expect(res.status).toBe(200);
    expect(await userRow(user.id)).toMatchObject({ plan: 'pro', planStatus: 'active', stripeCustomerId: 'cus_123', stripeSubscriptionId: 'sub_123' });

    // 2. subscription created/updated -> billing period mirrored (period on items, API >= 2025-03-31)
    const sub = { id: 'sub_123', object: 'subscription', customer: 'cus_123', status: 'active', metadata: { user_id: user.id }, items: { data: [{ current_period_start: start, current_period_end: end }] } };
    expect((await sendEvent('customer.subscription.updated', sub)).status).toBe(200);
    const u = await userRow(user.id);
    expect(u.periodStart?.toISOString()).toBe(startIso);
    expect(u.periodEnd?.toISOString()).toBe(endIso);
    const me = await (await req('/api/me', { cookie })).json();
    expect(me.period).toEqual({ start: startIso, end: endIso });
    expect(me.usage.included_minutes).toBe(600);

    // 3. payment fails -> past_due (still Pro, but overage is no longer allowed)
    await sendEvent('invoice.payment_failed', { id: 'in_1', object: 'invoice', customer: 'cus_123' });
    expect(await userRow(user.id)).toMatchObject({ plan: 'pro', planStatus: 'past_due' });

    // 4. recovered
    await sendEvent('customer.subscription.updated', sub);
    expect(await userRow(user.id)).toMatchObject({ plan: 'pro', planStatus: 'active' });

    // 5. an event for some other, older subscription is ignored
    await sendEvent('customer.subscription.deleted', { ...sub, id: 'sub_old', status: 'canceled' });
    expect((await userRow(user.id)).plan).toBe('pro');

    // 6. cancelled -> Free
    await sendEvent('customer.subscription.deleted', { ...sub, status: 'canceled' });
    expect(await userRow(user.id)).toMatchObject({ plan: 'free', planStatus: 'canceled', stripeSubscriptionId: null, periodStart: null });
  });

  it('ignores late or retried events for a deleted subscription', async () => {
    const { user } = await signup();
    const t = Math.floor(Date.now() / 1000);
    const sub = { id: 'sub_late', object: 'subscription', customer: 'cus_late', status: 'active', metadata: { user_id: user.id }, items: { data: [] } };
    const checkout = { id: 'cs_late', object: 'checkout.session', mode: 'subscription', client_reference_id: user.id, customer: 'cus_late', subscription: 'sub_late' };
    await sendEvent('checkout.session.completed', checkout, SECRET, t - 100);
    await sendEvent('customer.subscription.deleted', { ...sub, status: 'canceled' }, SECRET, t);
    const free = { plan: 'free', planStatus: 'canceled', stripeSubscriptionId: null };
    expect(await userRow(user.id)).toMatchObject(free);

    // an older "active" update delivered late, a retried checkout, and a same-second update: all ignored
    await sendEvent('customer.subscription.updated', sub, SECRET, t - 50);
    expect(await userRow(user.id)).toMatchObject(free);
    await sendEvent('checkout.session.completed', checkout, SECRET, t - 100);
    expect(await userRow(user.id)).toMatchObject(free);
    await sendEvent('customer.subscription.updated', sub, SECRET, t);
    expect(await userRow(user.id)).toMatchObject(free);

    // a new subscription still works
    await sendEvent('customer.subscription.created', { ...sub, id: 'sub_new' }, SECRET, t + 10);
    expect(await userRow(user.id)).toMatchObject({ plan: 'pro', planStatus: 'active', stripeSubscriptionId: 'sub_new' });
  });

  it('downgrades unpaid-incomplete subscriptions and ignores unknown customers', async () => {
    const { user } = await signup();
    await sendEvent('customer.subscription.created', { id: 'sub_9', object: 'subscription', customer: 'cus_9', status: 'incomplete', metadata: { user_id: user.id }, items: { data: [] } });
    expect(await userRow(user.id)).toMatchObject({ plan: 'free', stripeCustomerId: 'cus_9' });
    expect((await sendEvent('invoice.payment_failed', { id: 'in_x', object: 'invoice', customer: 'cus_unknown' })).status).toBe(200);
    expect((await sendEvent('customer.created', { id: 'cus_z', object: 'customer' })).status).toBe(200);
  });
});

describe('which subscriptions are Pro', () => {
  const saved = { ...config.stripe };
  afterEach(() => {
    Object.assign(config.stripe, saved);
  });
  const sub = (userId: string, o: Record<string, unknown> = {}) => ({
    id: `sub_${Math.random().toString(36).slice(2)}`,
    object: 'subscription',
    customer: `cus_${userId}`,
    status: 'active',
    metadata: { user_id: userId },
    items: { data: [{ price: { id: 'price_pro' } }] },
    ...o,
  });
  const t0 = Math.floor(Date.now() / 1000);

  it('treats unpaid as Free but keeps it linked, so checkout sends the user to the portal; past_due keeps Pro', async () => {
    const { user, cookie } = await signup();
    const s = sub(user.id);
    await sendEvent('customer.subscription.created', s, SECRET, t0);
    expect((await userRow(user.id)).plan).toBe('pro');
    await sendEvent('customer.subscription.updated', { ...s, status: 'past_due' }, SECRET, t0 + 1);
    expect(await userRow(user.id)).toMatchObject({ plan: 'pro', planStatus: 'past_due' });
    await sendEvent('customer.subscription.updated', { ...s, status: 'unpaid' }, SECRET, t0 + 2);
    expect(await userRow(user.id)).toMatchObject({ plan: 'free', planStatus: 'unpaid', stripeSubscriptionId: s.id });

    Object.assign(config.stripe, { secretKey: 'sk_test_unused', pricePro: 'price_pro' });
    const res = await req('/api/billing/checkout', { cookie, method: 'POST' });
    expect(res.status).toBe(409);
    expect((await res.json()).error).toBe('already_subscribed');
    // paying the unpaid invoice revives the same subscription
    await sendEvent('customer.subscription.updated', s, SECRET, t0 + 3);
    expect(await userRow(user.id)).toMatchObject({ plan: 'pro', planStatus: 'active', stripeSubscriptionId: s.id });
  });

  it('with STRIPE_PRICE_PRO set, only a subscription to that price is Pro, and checkout alone grants nothing', async () => {
    config.stripe.pricePro = 'price_pro';
    const { user } = await signup();
    await sendEvent('checkout.session.completed', { id: 'cs_p', object: 'checkout.session', mode: 'subscription', client_reference_id: user.id, customer: `cus_${user.id}`, subscription: 'sub_other' }, SECRET, t0);
    expect((await userRow(user.id)).plan).toBe('free');
    await sendEvent('customer.subscription.created', sub(user.id, { id: 'sub_other', items: { data: [{ price: { id: 'price_cheap' } }] } }), SECRET, t0 + 1);
    expect((await userRow(user.id)).plan).toBe('free');
    const pro = sub(user.id, { items: { data: [{ price: { id: 'price_overage' } }, { price: { id: 'price_pro' } }] } });
    await sendEvent('customer.subscription.created', pro, SECRET, t0 + 2);
    expect(await userRow(user.id)).toMatchObject({ plan: 'pro', stripeSubscriptionId: pro.id });
    // switching the tracked subscription to another price in the portal drops Pro
    await sendEvent('customer.subscription.updated', { ...pro, items: { data: [{ price: { id: 'price_cheap' } }] } }, SECRET, t0 + 3);
    expect((await userRow(user.id)).plan).toBe('free');
  });

  it('with STRIPE_PRICE_PRO set, a second subscription to another product leaves a Pro subscriber alone', async () => {
    config.stripe.pricePro = 'price_pro';
    const { user } = await signup();
    const pro = sub(user.id);
    await sendEvent('customer.subscription.created', pro, SECRET, t0);
    const other = sub(user.id, { items: { data: [{ price: { id: 'price_cheap' } }] } });
    for (const [type, status] of [['customer.subscription.created', 'active'], ['customer.subscription.updated', 'unpaid'], ['customer.subscription.deleted', 'canceled']] as const) {
      await sendEvent(type, { ...other, status }, SECRET, t0 + 5);
      expect(await userRow(user.id), type).toMatchObject({ plan: 'pro', planStatus: 'active', stripeSubscriptionId: pro.id });
    }
  });

  it('a same-second payment failure does not hide the unpaid event that follows it', async () => {
    const { user } = await signup();
    const s = sub(user.id);
    await sendEvent('customer.subscription.created', s, SECRET, t0);
    await sendEvent('invoice.payment_failed', { id: 'in_w1', object: 'invoice', customer: s.customer }, SECRET, t0 + 1);
    expect(await userRow(user.id)).toMatchObject({ plan: 'pro', planStatus: 'past_due' });
    await sendEvent('customer.subscription.updated', { ...s, status: 'unpaid' }, SECRET, t0 + 1);
    expect(await userRow(user.id)).toMatchObject({ plan: 'free', planStatus: 'unpaid' });
  });

  it('a second Pro subscription replaces the tracked one only once it is active', async () => {
    const { user } = await signup();
    const a = sub(user.id);
    await sendEvent('customer.subscription.created', a, SECRET, t0);
    // a second checkout tab whose payment failed
    const b = sub(user.id);
    await sendEvent('customer.subscription.created', { ...b, status: 'incomplete' }, SECRET, t0 + 1);
    await sendEvent('customer.subscription.updated', { ...b, status: 'incomplete_expired' }, SECRET, t0 + 2);
    expect(await userRow(user.id)).toMatchObject({ plan: 'pro', planStatus: 'active', stripeSubscriptionId: a.id });
    // one that is paid takes over
    const c = sub(user.id);
    await sendEvent('customer.subscription.created', c, SECRET, t0 + 3);
    expect(await userRow(user.id)).toMatchObject({ plan: 'pro', stripeSubscriptionId: c.id });
  });

  it('a same-second event cannot downgrade a Pro state of the same subscription', async () => {
    const { user } = await signup();
    const s = sub(user.id);
    // "updated: active" processed before the "created: incomplete" Stripe sent in the same second
    await sendEvent('customer.subscription.updated', s, SECRET, t0);
    await sendEvent('customer.subscription.created', { ...s, status: 'incomplete' }, SECRET, t0);
    expect(await userRow(user.id)).toMatchObject({ plan: 'pro', planStatus: 'active', stripeSubscriptionId: s.id });
    // a strictly newer downgrade still applies, and a same-second deletion always does
    await sendEvent('customer.subscription.updated', { ...s, status: 'incomplete_expired' }, SECRET, t0 + 1);
    expect((await userRow(user.id)).plan).toBe('free');
    const again = sub(user.id);
    await sendEvent('customer.subscription.created', again, SECRET, t0 + 2);
    await sendEvent('customer.subscription.deleted', { ...again, status: 'canceled' }, SECRET, t0 + 2);
    expect(await userRow(user.id)).toMatchObject({ plan: 'free', planStatus: 'canceled' });
  });
});

describe('webhook body limit', () => {
  it('answers 413 (not 500) for a body over 1 MB', async () => {
    const res = await req('/api/stripe/webhook', { method: 'POST', body: 'x'.repeat(1024 * 1024 + 1), headers: { 'stripe-signature': 't=1,v1=abc', 'content-type': 'application/json' } });
    expect(res.status).toBe(413);
    expect((await res.json()).error).toBe('payload_too_large');
  });
});

describe.skipIf(Boolean(process.env.STRIPE_SECRET_KEY))('billing endpoints without Stripe keys', () => {
  it('reports manual billing and refuses checkout', async () => {
    const { cookie } = await signup();
    expect((await (await req('/api/billing', { cookie })).json()).mode).toBe('manual');
    const res = await req('/api/billing/checkout', { cookie, method: 'POST' });
    expect(res.status).toBe(400);
    expect((await res.json()).error).toBe('billing_manual');
  });
});

describe('metered overage reporting', () => {
  it('sends only unreported overage events, keyed by usage event id', async () => {
    const { createServer } = await import('node:http');
    const { randomUUID } = await import('node:crypto');
    const { usageEvents } = await import('../src/db/schema.js');
    const { reportOverage, setStripeClient } = await import('../src/stripe.js');

    const received: URLSearchParams[] = [];
    const server = createServer((rq, rs) => {
      let body = '';
      rq.on('data', (d) => (body += d));
      rq.on('end', () => {
        if (rq.url === '/v1/billing/meter_events') received.push(new URLSearchParams(body));
        rs.setHeader('content-type', 'application/json');
        rs.end(JSON.stringify({ object: 'billing.meter_event', event_name: 'opstrellis_managed_minutes' }));
      });
    });
    await new Promise<void>((r) => server.listen(0, '127.0.0.1', r));
    const port = (server.address() as { port: number }).port;
    setStripeClient(new Stripe('sk_test_fake', { host: '127.0.0.1', port, protocol: 'http' }));
    try {
      const { user } = await signup();
      await db.update(users).set({ plan: 'pro', stripeCustomerId: 'cus_meter' }).where(eq(users.id, user.id));
      const base = { userId: user.id, source: 'gateway' as const, mode: 'cloud' as const, provider: 'local', model: 'local/moonshine-tiny-en', audioSeconds: 60 };
      const overId = randomUUID();
      await db.insert(usageEvents).values([
        { ...base, id: overId, overageMinutes: 2, billedUsd: 0.04 },
        { ...base, id: randomUUID(), overageMinutes: 0 },
      ]);
      expect(await reportOverage()).toBe(1);
      expect(received).toHaveLength(1);
      expect(received[0]!.get('event_name')).toBe('opstrellis_managed_minutes');
      expect(received[0]!.get('identifier')).toBe(overId);
      expect(received[0]!.get('payload[stripe_customer_id]')).toBe('cus_meter');
      expect(received[0]!.get('payload[value]')).toBe('2');
      // already reported -> nothing more to send
      expect(await reportOverage()).toBe(0);
    } finally {
      setStripeClient(null);
      server.close();
    }
  });
});
