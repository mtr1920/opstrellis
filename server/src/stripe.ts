// Stripe: Checkout, Customer Portal, webhook state sync, and metered overage reporting.
import { and, eq, gt, isNotNull, isNull, lt, ne, or, sql } from 'drizzle-orm';
import Stripe from 'stripe';
import { config, stripeEnabled } from './config.js';
import { db } from './db/index.js';
import { usageEvents, users, type User } from './db/schema.js';

let client: Stripe | null = null;
/** Webhook verification works offline, so a placeholder key is fine when billing is not configured. */
export function stripe(): Stripe {
  client ??= new Stripe(config.stripe.secretKey || 'sk_test_unconfigured');
  return client;
}
let injected = false;
/** For tests: point the SDK at a local fake API (also enables meter reporting without a key). */
export function setStripeClient(c: Stripe | null) {
  client = c;
  injected = c !== null;
}

const idOf = (v: string | { id: string } | null | undefined) => (typeof v === 'string' ? v : (v?.id ?? null));

async function ensureCustomer(user: User): Promise<string> {
  if (user.stripeCustomerId) return user.stripeCustomerId;
  const customer = await stripe().customers.create({ email: user.email, name: user.name || undefined, metadata: { user_id: user.id } });
  await db.update(users).set({ stripeCustomerId: customer.id }).where(eq(users.id, user.id));
  return customer.id;
}

export async function createCheckoutUrl(user: User): Promise<string> {
  const { pricePro, priceOverage } = config.stripe;
  const session = await stripe().checkout.sessions.create({
    mode: 'subscription',
    customer: await ensureCustomer(user),
    client_reference_id: user.id,
    // Metered prices take no quantity; usage arrives through the Billing Meter.
    line_items: [{ price: pricePro, quantity: 1 }, ...(priceOverage ? [{ price: priceOverage }] : [])],
    subscription_data: { metadata: { user_id: user.id } },
    allow_promotion_codes: true,
    success_url: `${config.publicUrl}/billing?checkout=success`,
    cancel_url: `${config.publicUrl}/billing?checkout=cancel`,
  });
  return session.url!;
}

export async function createPortalUrl(user: User): Promise<string> {
  const session = await stripe().billingPortal.sessions.create({
    customer: await ensureCustomer(user),
    return_url: `${config.publicUrl}/billing`,
  });
  return session.url;
}

/** Verifies the signature and returns the event; throws on a bad signature. */
export function verifyWebhook(payload: string, signature: string): Stripe.Event {
  return stripe().webhooks.constructEvent(payload, signature, config.stripe.webhookSecret);
}

async function findUser(customerId: string | null, userId?: string | null): Promise<User | undefined> {
  if (customerId) {
    const [u] = await db.select().from(users).where(eq(users.stripeCustomerId, customerId));
    if (u) return u;
  }
  if (userId) {
    const [u] = await db.select().from(users).where(eq(users.id, userId)).catch(() => []);
    return u;
  }
}

/** past_due keeps Pro (without overage) while Stripe retries the payment; unpaid and the rest are Free. */
const PAID_STATUSES = new Set(['active', 'trialing', 'past_due']);

/** With STRIPE_PRICE_PRO set, only a subscription to that price is Pro (not another product in the same account). */
const isProSubscription = (sub: Stripe.Subscription) =>
  !config.stripe.pricePro || (sub.items?.data ?? []).some((i) => i.price?.id === config.stripe.pricePro);
const toDate = (s: number | undefined | null) => (s ? new Date(s * 1000) : null);

/**
 * Writes `set` unless the user already has a newer event applied, or `subscriptionId` was deleted.
 * Webhooks arrive out of order and are retried, so a late "active" must not revive a cancelled plan.
 * Event times have one-second resolution. With `incomplete` (a state that only ever precedes Pro), a
 * same-second event cannot replace Pro on this subscription, so a "created" processed after "active"
 * does not drop a paying customer. Without `stamp` the event is not recorded as the latest one.
 */
async function applyEvent(event: Stripe.Event, userId: string, subscriptionId: string | null, set: Partial<User>, opts: { incomplete?: boolean; stamp?: boolean } = {}) {
  const at = new Date(event.created * 1000);
  const tieAllowed = opts.incomplete && subscriptionId ? or(ne(users.plan, 'pro'), sql`${users.stripeSubscriptionId} is distinct from ${subscriptionId}`) : undefined;
  await db
    .update(users)
    .set(opts.stamp === false ? set : { ...set, stripeEventAt: at })
    .where(
      and(
        eq(users.id, userId),
        or(isNull(users.stripeEventAt), lt(users.stripeEventAt, at), and(eq(users.stripeEventAt, at), tieAllowed)),
        subscriptionId ? or(isNull(users.stripeEndedSubscriptionId), ne(users.stripeEndedSubscriptionId, subscriptionId)) : undefined,
      ),
    );
}

/** Applies a webhook event to our users table. Idempotent: it only ever sets state. */
export async function handleStripeEvent(event: Stripe.Event): Promise<void> {
  switch (event.type) {
    case 'checkout.session.completed': {
      const s = event.data.object;
      // The session carries no line items (and a payment link can set client_reference_id), so with a
      // Pro price configured the subscription events, which list their prices, decide the plan.
      if (s.mode !== 'subscription' || config.stripe.pricePro) return;
      const user = await findUser(idOf(s.customer), s.client_reference_id ?? s.metadata?.user_id);
      if (!user) return;
      const subscriptionId = idOf(s.subscription);
      await applyEvent(event, user.id, subscriptionId, { plan: 'pro', planStatus: 'active', stripeCustomerId: idOf(s.customer), stripeSubscriptionId: subscriptionId });
      return;
    }
    case 'customer.subscription.created':
    case 'customer.subscription.updated':
    case 'customer.subscription.deleted': {
      const sub = event.data.object;
      const user = await findUser(idOf(sub.customer), sub.metadata?.user_id);
      if (!user) return;
      const deleted = event.type === 'customer.subscription.deleted';
      const active = !deleted && PAID_STATUSES.has(sub.status) && isProSubscription(sub);
      // Another subscription replaces the tracked one only when it is itself an active Pro one, so an
      // older, failed or second-checkout subscription cannot knock a paying subscriber to Free.
      if (user.stripeSubscriptionId && user.stripeSubscriptionId !== sub.id && !active) return;
      // A subscription to another product must not touch the plan unless it is the one we track.
      if (!isProSubscription(sub) && sub.id !== user.stripeSubscriptionId) return;
      // An unpaid subscription can still be paid and revive, so it stays linked (checkout then sends the user to the portal).
      const linked = active || (!deleted && sub.status === 'unpaid');
      // Since API 2025-03-31 the period lives on the subscription items.
      const item = sub.items?.data?.[0] as { current_period_start?: number; current_period_end?: number } | undefined;
      const legacy = sub as unknown as { current_period_start?: number; current_period_end?: number };
      await applyEvent(event, user.id, sub.id, {
        plan: active ? 'pro' : 'free',
        planStatus: deleted ? 'canceled' : sub.status,
        stripeCustomerId: idOf(sub.customer),
        stripeSubscriptionId: linked ? sub.id : null,
        ...(deleted ? { stripeEndedSubscriptionId: sub.id } : {}),
        periodStart: active ? toDate(item?.current_period_start ?? legacy.current_period_start) : null,
        periodEnd: active ? toDate(item?.current_period_end ?? legacy.current_period_end) : null,
      }, { incomplete: !deleted && sub.status === 'incomplete' });
      return;
    }
    case 'invoice.payment_failed': {
      const user = await findUser(idOf(event.data.object.customer));
      // Not stamped: the subscription event Stripe sends in the same second (e.g. unpaid) must still apply.
      if (user?.plan === 'pro') await applyEvent(event, user.id, null, { planStatus: 'past_due' }, { stamp: false });
      return;
    }
  }
}

/**
 * Sends unreported overage minutes to the Stripe Billing Meter. The usage event id is the meter
 * event identifier, so retries (and concurrent runs) never double-charge.
 */
export async function reportOverage(): Promise<number> {
  if (!stripeEnabled() && !injected) return 0;
  const pending = await db
    .select({ id: usageEvents.id, minutes: usageEvents.overageMinutes, at: usageEvents.createdAt, customer: users.stripeCustomerId })
    .from(usageEvents)
    .innerJoin(users, eq(users.id, usageEvents.userId))
    .where(and(gt(usageEvents.overageMinutes, 0), isNull(usageEvents.meterReportedAt), isNotNull(users.stripeCustomerId)))
    .limit(500);
  let sent = 0;
  for (const e of pending) {
    try {
      await stripe().billing.meterEvents.create({
        event_name: config.stripe.meterEvent,
        identifier: e.id,
        timestamp: Math.floor(e.at.getTime() / 1000),
        payload: { stripe_customer_id: e.customer!, value: String(e.minutes) },
      });
      await db.update(usageEvents).set({ meterReportedAt: new Date() }).where(eq(usageEvents.id, e.id));
      sent++;
    } catch (err) {
      console.error(`stripe meter event ${e.id} failed:`, (err as Error).message);
    }
  }
  return sent;
}
