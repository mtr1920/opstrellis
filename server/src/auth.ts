// Authentication (session cookie or Bearer API token), authorization, CSRF and rate limiting.
import { getConnInfo } from '@hono/node-server/conninfo';
import { isIPv6 } from 'node:net';
import { and, eq, gt, lt, sql } from 'drizzle-orm';
import type { Context, MiddlewareHandler } from 'hono';
import { deleteCookie, getCookie, setCookie } from 'hono/cookie';
import { config } from './config.js';
import { db } from './db/index.js';
import { apiTokens, deviceCodes, orgInvites, sessions, users, type User } from './db/schema.js';
import { randomToken, sha256 } from './lib/crypto.js';
import { ApiError, apiError } from './lib/http.js';

export type AppEnv = { Variables: { user: User; via: 'session' | 'token' } };

export const SESSION_COOKIE = 'opstrellis_session';
export const TOKEN_PREFIX = 'utt_';

// ---------- sessions ----------

/**
 * Stores a new session. With `verifiedHash`, only while the user's stored hash is still that one, in the
 * same statement: a login checked against a password that was changed (or reset) since gets no session.
 * FOR SHARE waits for an uncommitted change of the user row and then re-checks the hash against it.
 */
export async function createSession(userId: string, expiresAt: Date, verifiedHash?: string): Promise<string | null> {
  const value = randomToken();
  const id = sha256(value);
  if (verifiedHash === undefined) {
    await db.insert(sessions).values({ id, userId, expiresAt });
    return value;
  }
  const rows = await db.execute(sql`
    insert into ${sessions} (id, user_id, expires_at)
    select ${id}, ${userId}, ${expiresAt.toISOString()}
    where exists (select 1 from ${users} where ${users.id} = ${userId} and ${users.passwordHash} = ${verifiedHash} for share)
    returning id`);
  return rows.length ? value : null;
}

/** Starts a session and sets its cookie; false when `verifiedHash` is no longer the user's hash. */
export async function startSession(c: Context, userId: string, verifiedHash?: string): Promise<boolean> {
  const expiresAt = new Date(Date.now() + config.sessionDays * 86_400_000);
  const value = await createSession(userId, expiresAt, verifiedHash);
  if (!value) return false;
  setCookie(c, SESSION_COOKIE, value, {
    httpOnly: true,
    sameSite: 'Lax',
    secure: config.publicUrl.startsWith('https://'),
    path: '/',
    expires: expiresAt,
  });
  return true;
}

/** The id (hash) of the request's session cookie, if any. */
export function sessionId(c: Context): string | null {
  const value = getCookie(c, SESSION_COOKIE);
  return value ? sha256(value) : null;
}

export async function endSession(c: Context) {
  const value = getCookie(c, SESSION_COOKIE);
  if (value) await db.delete(sessions).where(eq(sessions.id, sha256(value)));
  deleteCookie(c, SESSION_COOKIE, { path: '/' });
}

/** Deletes expired sessions, device codes and team invites (run periodically). */
export async function purgeExpired() {
  const now = new Date();
  await db.delete(sessions).where(lt(sessions.expiresAt, now));
  await db.delete(deviceCodes).where(lt(deviceCodes.expiresAt, now));
  await db.delete(orgInvites).where(lt(orgInvites.expiresAt, now));
}

// ---------- API tokens ----------

export const newApiToken = () => TOKEN_PREFIX + randomToken(24);

/** Stores an API token; the plain value is returned once and only its hash is stored. */
export async function createApiToken(userId: string, name: string, token = newApiToken(), tx: Pick<typeof db, 'insert'> = db) {
  const [row] = await tx
    .insert(apiTokens)
    .values({ userId, name, prefix: token.slice(0, TOKEN_PREFIX.length + 6), tokenHash: sha256(token) })
    .returning();
  return { token, row: row! };
}

async function userFromRequest(c: Context): Promise<{ user: User; via: 'session' | 'token' } | null> {
  const header = c.req.header('authorization');
  if (header?.toLowerCase().startsWith('bearer ')) {
    const hash = sha256(header.slice(7).trim());
    const [row] = await db
      .select({ user: users, tokenId: apiTokens.id, lastUsedAt: apiTokens.lastUsedAt })
      .from(apiTokens)
      .innerJoin(users, eq(users.id, apiTokens.userId))
      .where(eq(apiTokens.tokenHash, hash));
    if (!row) return null;
    // Informational; written at most once a minute per token to keep requests cheap.
    if (!row.lastUsedAt || Date.now() - row.lastUsedAt.getTime() > 60_000) {
      await db.update(apiTokens).set({ lastUsedAt: new Date() }).where(eq(apiTokens.id, row.tokenId));
    }
    return { user: row.user, via: 'token' };
  }
  const cookie = getCookie(c, SESSION_COOKIE);
  if (!cookie) return null;
  const [row] = await db
    .select({ user: users })
    .from(sessions)
    .innerJoin(users, eq(users.id, sessions.userId))
    .where(and(eq(sessions.id, sha256(cookie)), gt(sessions.expiresAt, new Date())));
  return row ? { user: row.user, via: 'session' } : null;
}

const SAFE_METHODS = new Set(['GET', 'HEAD', 'OPTIONS']);

/**
 * Cookie-authenticated mutations must come from our own origin. SameSite=Lax already blocks most
 * cross-site requests; the Origin check covers the rest. Bearer requests are not CSRF-prone.
 */
export function sameOrigin(c: Context): boolean {
  const origin = c.req.header('origin') || c.req.header('referer');
  if (!origin) return false;
  try {
    const u = new URL(origin);
    const pub = new URL(config.publicUrl).origin;
    // Behind https the Host header alone would also accept the same host over plain http.
    if (pub.startsWith('https://')) return u.origin === pub;
    return u.host === c.req.header('host') || u.origin === pub;
  } catch {
    return false;
  }
}

/** For cookie-setting routes that run before any session exists (login, signup, logout). */
export const requireSameOrigin: MiddlewareHandler = async (c, next) => {
  if (!sameOrigin(c)) return apiError(c, 403, 'csrf', 'Cross-origin request rejected');
  await next();
};

/**
 * Requires a signed-in, enabled user. `role: 'admin'` additionally requires an admin;
 * `sessionOnly` refuses API tokens (e.g. so a leaked token cannot mint new tokens).
 */
export function requireAuth(opts: { role?: 'admin'; sessionOnly?: boolean } = {}): MiddlewareHandler<AppEnv> {
  return async (c, next) => {
    const auth = await userFromRequest(c);
    if (!auth) return apiError(c, 401, 'unauthorized', 'Sign in or pass a valid API token');
    if (auth.user.disabled) return apiError(c, 403, 'account_disabled', 'This account is disabled');
    if (auth.via === 'session' && !SAFE_METHODS.has(c.req.method) && !sameOrigin(c)) {
      return apiError(c, 403, 'csrf', 'Cross-origin request rejected');
    }
    if (opts.sessionOnly && auth.via !== 'session') {
      return apiError(c, 403, 'session_required', 'Use the dashboard for this action');
    }
    if (opts.role === 'admin' && auth.user.role !== 'admin') return apiError(c, 403, 'forbidden', 'Admins only');
    c.set('user', auth.user);
    c.set('via', auth.via);
    await next();
  };
}

// ---------- rate limiting (in-memory, fixed window, per process) ----------

const buckets = new Map<string, { count: number; resetAt: number }>();

export function clientIp(c: Context): string {
  if (config.trustProxy) {
    // Each trusted proxy appends the address it saw, so the client is the Nth entry from the right;
    // anything further left is whatever the client chose to send.
    const fwd = (c.req.header('x-forwarded-for') ?? '').split(',').map((s) => s.trim()).filter(Boolean);
    const ip = fwd[Math.max(0, fwd.length - config.trustProxy)];
    if (ip) return ip;
  }
  try {
    return getConnInfo(c).remote.address ?? 'unknown';
  } catch {
    return 'unknown';
  }
}

/** Rate-limit key for an address: IPv6 clients usually control a whole /64, so they share one bucket. */
export function ipBucket(ip: string): string {
  if (!isIPv6(ip)) return ip;
  const mapped = /^::ffff:(\d+\.\d+\.\d+\.\d+)$/i.exec(ip);
  if (mapped) return mapped[1]!;
  const groups = (s: string) => (s ? s.split(':').flatMap((g) => (g.includes('.') ? ['0', '0'] : [g])) : []);
  const [head = '', tail] = ip.split('%')[0]!.split('::');
  const h = groups(head);
  const t = tail === undefined ? [] : groups(tail);
  const full = tail === undefined ? h : [...h, ...Array<string>(8 - h.length - t.length).fill('0'), ...t];
  return `${full.slice(0, 4).map((g) => parseInt(g, 16).toString(16)).join(':')}::/64`;
}

export const clientKey = (c: Context) => ipBucket(clientIp(c));

/** Counts one hit on `key`; returns the seconds until its window resets once `limit` is exceeded, else null. */
function hit(key: string, limit: number, windowMs: number): number | null {
  const now = Date.now();
  let b = buckets.get(key);
  if (!b || b.resetAt <= now) {
    b = { count: 0, resetAt: now + windowMs };
    buckets.set(key, b);
  }
  return ++b.count > limit ? Math.ceil((b.resetAt - now) / 1000) : null;
}

/** Fixed-window limit per client IP (IPv6: per /64), or per whatever `keyOf` returns (e.g. the signed-in user). */
export function rateLimit(name: string, limit: number, windowMs = 60_000, keyOf: (c: Context) => string = clientKey): MiddlewareHandler {
  return async (c, next) => {
    const retryAfter = hit(`${name}:${keyOf(c)}`, limit, windowMs);
    if (retryAfter !== null) {
      c.header('Retry-After', String(retryAfter));
      return apiError(c, 429, 'rate_limited', 'Too many requests, slow down');
    }
    await next();
  };
}

/** The same limit for a key only known inside the handler (e.g. the email in a login body); throws 429. */
/** For limits on successes only: refuses once `limit` hits were recorded with `recordHit` in the window. */
export function checkLimit(c: Context, name: string, key: string, limit: number) {
  const b = buckets.get(`${name}:${key}`);
  if (b && b.resetAt > Date.now() && b.count >= limit) {
    c.header('Retry-After', String(Math.ceil((b.resetAt - Date.now()) / 1000)));
    throw new ApiError(429, 'rate_limited', 'Too many requests, slow down');
  }
}

export function recordHit(name: string, key: string, windowMs: number) {
  hit(`${name}:${key}`, Infinity, windowMs);
}

export function limitKey(c: Context, name: string, key: string, limit: number, windowMs = 60_000) {
  const retryAfter = hit(`${name}:${key}`, limit, windowMs);
  if (retryAfter !== null) {
    c.header('Retry-After', String(retryAfter));
    throw new ApiError(429, 'rate_limited', 'Too many requests, slow down');
  }
}

// Drop expired buckets so the map cannot grow without bound.
setInterval(() => {
  const now = Date.now();
  for (const [k, b] of buckets) if (b.resetAt <= now) buckets.delete(k);
}, 60_000).unref();
