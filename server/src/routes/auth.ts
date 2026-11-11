// Email/password auth, API tokens, and the device-code flow for the desktop app.
import { randomInt } from 'node:crypto';
import { and, count, desc, eq, isNull, ne, sql } from 'drizzle-orm';
import { Hono } from 'hono';
import { z } from 'zod';
import { checkLimit, clientKey, createApiToken, endSession, limitKey, newApiToken, rateLimit, requireAuth, requireSameOrigin, sessionId, recordHit, startSession, type AppEnv } from '../auth.js';
import { config } from '../config.js';
import { db } from '../db/index.js';
import { apiTokens, deviceCodes, sessions, users } from '../db/schema.js';
import { hashPassword, randomToken, sha256, verifyPassword } from '../lib/crypto.js';
import { ApiError, emailSchema, iso, passwordSchema, readJson, storableText } from '../lib/http.js';

export const authRoutes = new Hono<AppEnv>();

const email = emailSchema;
const password = passwordSchema;

// ---------- signup / login / logout ----------

type Tx = Parameters<Parameters<typeof db.transaction>[0]>[0];

async function signupCounts(tx: Tx | typeof db) {
  const [row] = await tx
    .select({
      total: count(),
      admins: sql<number>`count(*) filter (where ${users.role} = 'admin')::int`,
      lastDay: sql<number>`count(*) filter (where ${users.createdAt} > now() - interval '1 day')::int`,
    })
    .from(users);
  return row ?? { total: 0, admins: 0, lastDay: 0 };
}

/**
 * Whether a signup would become the first admin: only the first account on an empty database, and
 * never when ADMIN_EMAIL is set (that admin is created at startup instead).
 */
const isBootstrap = (counts: { total: number }) => !config.adminEmail && counts.total === 0;

/** A startup warning while there is no admin and nothing configured will create one safely. */
export async function bootstrapWarning(): Promise<string | null> {
  const counts = await signupCounts(db);
  if (!config.adminEmail) {
    if (counts.total > 0) return null;
    return 'WARNING: there are no users yet, so the next account to sign up becomes ADMIN. Create yours now, or set ADMIN_EMAIL and ADMIN_PASSWORD before exposing this server.';
  }
  if (counts.admins > 0 || config.adminPassword) return null;
  return 'WARNING: ADMIN_EMAIL is set without ADMIN_PASSWORD and there is no admin yet; set ADMIN_PASSWORD so the server can create it, or run `npm run db:seed`.';
}

/** Public: lets the dashboard hide the signup form when signup is closed. */
authRoutes.get('/auth/signup', async (c) => c.json({ open: config.allowSignup || isBootstrap(await signupCounts(db)) }));

/** Accounts one client IP (IPv6: /64) may create a day, so one client cannot use up MAX_SIGNUPS_PER_DAY for everyone. */
const SIGNUPS_PER_CLIENT_PER_DAY = 20;

authRoutes.post('/auth/signup', rateLimit('signup', 10), requireSameOrigin, async (c) => {
  const body = await readJson(c, z.object({ email, password, name: storableText().trim().max(100).default('') }));
  const passwordHash = await hashPassword(body.password);
  const user = await db.transaction(async (tx) => {
    // Serialize signups so exactly one account bootstraps the admin role and the daily cap is exact.
    // Other admins come from ADMIN_EMAIL, `npm run db:seed` or the admin UI, never from an unverified signup email.
    await tx.execute(sql`select pg_advisory_xact_lock(42)`);
    const counts = await signupCounts(tx);
    const bootstrap = isBootstrap(counts);
    if (!config.allowSignup && !bootstrap) throw new ApiError(403, 'signup_disabled', 'New accounts cannot be created on this server');
    if (!bootstrap && counts.lastDay >= config.maxSignupsPerDay) {
      throw new ApiError(429, 'signup_limit', 'Too many accounts were created today; try again tomorrow');
    }
    checkLimit(c, 'signup-day', clientKey(c), SIGNUPS_PER_CLIENT_PER_DAY);
    const [existing] = await tx.select({ id: users.id }).from(users).where(eq(users.email, body.email));
    if (existing) throw new ApiError(409, 'email_taken', 'An account with this email already exists');
    const [u] = await tx.insert(users).values({ email: body.email, name: body.name, passwordHash, role: bootstrap ? 'admin' : 'user' }).returning();
    recordHit('signup-day', clientKey(c), 86_400_000);
    return u!;
  });
  await startSession(c, user.id);
  return c.json({ user: { id: user.id, email: user.email, name: user.name, role: user.role } }, 201);
});

/** Checked when the email is unknown, so that path costs one scrypt like a wrong password does. */
const DUMMY_HASH = 'scrypt$16384$8$1$m9rOYxyX4S-5-w60bK5cng$CNwXsZPrVb1i7YbZlYZcWbpqkKa0_4OrQqqq4Yy2yd-o3RG4OJ064rDEwPr5P-pmUoeJo-oq4J00kld4L5U6eA';

authRoutes.post('/auth/login', rateLimit('login', 10), requireSameOrigin, async (c) => {
  const body = await readJson(c, z.object({ email, password: z.string().max(200) }));
  // Tight per account and client, so guessing one password is slow; the account-wide ceiling is far
  // higher, so others cannot lock the owner out from their own addresses.
  limitKey(c, 'login-email-ip', `${body.email}|${clientKey(c)}`, 10, 15 * 60_000);
  limitKey(c, 'login-email', body.email, 200, 15 * 60_000);
  const [user] = await db.select().from(users).where(eq(users.email, body.email));
  const valid = await verifyPassword(body.password, user?.passwordHash ?? DUMMY_HASH);
  // Same error for unknown email and wrong password.
  if (!user || !valid) throw new ApiError(401, 'invalid_credentials', 'Wrong email or password');
  if (user.disabled) throw new ApiError(403, 'account_disabled', 'This account is disabled');
  if (!(await startSession(c, user.id, user.passwordHash))) throw new ApiError(401, 'invalid_credentials', 'Wrong email or password');
  return c.json({ user: { id: user.id, email: user.email, name: user.name, role: user.role } });
});

authRoutes.post('/auth/logout', requireSameOrigin, async (c) => {
  await endSession(c);
  return c.json({ ok: true });
});

/** Signs out every other session; API tokens stay valid (they are revoked on the tokens page). */
authRoutes.post('/auth/password', requireAuth({ sessionOnly: true }), rateLimit('password', 5, 15 * 60_000, (c) => c.get('user').id), async (c) => {
  const body = await readJson(c, z.object({ current: z.string().max(200), next: password }));
  const user = c.get('user');
  if (!(await verifyPassword(body.current, user.passwordHash))) throw new ApiError(403, 'wrong_password', 'Your current password is not correct');
  const passwordHash = await hashPassword(body.next);
  const keep = sessionId(c)!;
  await db.transaction(async (tx) => {
    await tx.update(users).set({ passwordHash }).where(eq(users.id, user.id));
    await tx.delete(sessions).where(and(eq(sessions.userId, user.id), ne(sessions.id, keep)));
  });
  return c.json({ ok: true });
});

// ---------- API tokens (dashboard only) ----------

const tokenJson = (t: typeof apiTokens.$inferSelect) => ({
  id: t.id,
  name: t.name,
  prefix: t.prefix,
  last_used_at: iso(t.lastUsedAt),
  created_at: iso(t.createdAt),
});

authRoutes.get('/tokens', requireAuth({ sessionOnly: true }), async (c) => {
  const rows = await db.select().from(apiTokens).where(eq(apiTokens.userId, c.get('user').id)).orderBy(desc(apiTokens.createdAt));
  return c.json({ tokens: rows.map(tokenJson) });
});

authRoutes.post('/tokens', requireAuth({ sessionOnly: true }), async (c) => {
  const { name } = await readJson(c, z.object({ name: storableText().trim().min(1).max(100) }));
  const { token, row } = await createApiToken(c.get('user').id, name);
  return c.json({ token, ...tokenJson(row) }, 201);
});

authRoutes.delete('/tokens/:id', requireAuth({ sessionOnly: true }), async (c) => {
  const id = z.uuid().safeParse(c.req.param('id'));
  if (!id.success) throw new ApiError(404, 'not_found');
  const deleted = await db
    .delete(apiTokens)
    .where(and(eq(apiTokens.id, id.data), eq(apiTokens.userId, c.get('user').id)))
    .returning({ id: apiTokens.id });
  if (!deleted.length) throw new ApiError(404, 'not_found');
  return c.json({ ok: true });
});

// ---------- device-code flow ----------

const DEVICE_TTL_MS = 10 * 60_000;
const POLL_INTERVAL_S = 5;
const USER_CODE_ALPHABET = 'BCDFGHJKLMNPQRSTVWXZ'; // no vowels (no words), no lookalikes

function userCode() {
  const pick = () => Array.from({ length: 4 }, () => USER_CODE_ALPHABET[randomInt(USER_CODE_ALPHABET.length)]).join('');
  return `${pick()}-${pick()}`;
}
const normalizeUserCode = (s: string) => {
  const raw = s.toUpperCase().replace(/[^A-Z]/g, '');
  return raw.length === 8 ? `${raw.slice(0, 4)}-${raw.slice(4)}` : raw;
};

authRoutes.post('/device/start', rateLimit('device-start', 20), async (c) => {
  const body = await readJson(
    c,
    z.object({ device_name: storableText().trim().max(100).optional(), client_name: storableText().trim().max(100).optional() }),
  );
  const deviceCode = randomToken();
  const code = userCode();
  await db.insert(deviceCodes).values({
    id: sha256(deviceCode),
    userCode: code,
    deviceName: body.device_name || body.client_name || 'OpsTrellis desktop',
    expiresAt: new Date(Date.now() + DEVICE_TTL_MS),
  });
  return c.json({
    device_code: deviceCode,
    user_code: code,
    verification_url: `${config.publicUrl}/device?code=${code}`,
    interval: POLL_INTERVAL_S,
    expires_in: DEVICE_TTL_MS / 1000,
  });
});

authRoutes.post('/device/poll', rateLimit('device-poll', 60), async (c) => {
  const { device_code } = await readJson(c, z.object({ device_code: z.string().min(1).max(200) }));
  const id = sha256(device_code);
  const [row] = await db.select().from(deviceCodes).where(eq(deviceCodes.id, id));
  if (!row || row.expiresAt < new Date()) {
    if (row) await db.delete(deviceCodes).where(eq(deviceCodes.id, id));
    return c.json({ error: 'expired_token', message: 'The code expired or is unknown; start again' }, 400);
  }
  if (!row.token) return c.json({ error: 'authorization_pending' }, 428);
  // Hand out the token exactly once.
  const [claimed] = await db.delete(deviceCodes).where(eq(deviceCodes.id, id)).returning({ token: deviceCodes.token });
  if (!claimed?.token) return c.json({ error: 'expired_token' }, 400);
  return c.json({ token: claimed.token });
});

async function pendingDevice(code: string) {
  const [row] = await db.select().from(deviceCodes).where(eq(deviceCodes.userCode, normalizeUserCode(code)));
  if (!row || row.expiresAt < new Date() || row.userId) throw new ApiError(404, 'not_found', 'This code is invalid, expired, or already used');
  return row;
}

// Per user: guessing another device's code needs an account, and new accounts are capped per day.
const deviceLookupLimit = rateLimit('device-lookup', 30, 60_000, (c) => c.get('user').id);
const deviceDecideLimit = rateLimit('device-decide', 10, 60_000, (c) => c.get('user').id);

authRoutes.get('/device/:code', requireAuth({ sessionOnly: true }), deviceLookupLimit, async (c) => {
  const row = await pendingDevice(c.req.param('code'));
  return c.json({ user_code: row.userCode, device_name: row.deviceName, expires_at: iso(row.expiresAt) });
});

authRoutes.post('/device/approve', requireAuth({ sessionOnly: true }), deviceDecideLimit, async (c) => {
  const { user_code } = await readJson(c, z.object({ user_code: z.string().max(20) }));
  const row = await pendingDevice(user_code);
  const user = c.get('user');
  await db.transaction(async (tx) => {
    // Claim the code first; a concurrent approval of the same code then matches no row.
    const token = newApiToken();
    const [claimed] = await tx
      .update(deviceCodes)
      .set({ userId: user.id, token })
      .where(and(eq(deviceCodes.id, row.id), isNull(deviceCodes.userId)))
      .returning({ id: deviceCodes.id });
    if (!claimed) throw new ApiError(404, 'not_found', 'This code is invalid, expired, or already used');
    await createApiToken(user.id, row.deviceName, token, tx);
  });
  return c.json({ ok: true, device_name: row.deviceName });
});

authRoutes.post('/device/deny', requireAuth({ sessionOnly: true }), deviceDecideLimit, async (c) => {
  const { user_code } = await readJson(c, z.object({ user_code: z.string().max(20) }));
  const row = await pendingDevice(user_code);
  await db.delete(deviceCodes).where(eq(deviceCodes.id, row.id));
  return c.json({ ok: true });
});
