import { randomUUID } from 'node:crypto';
import { eq } from 'drizzle-orm';
import { afterEach, beforeAll, describe, expect, it, vi } from 'vitest';
import { createSession, ipBucket } from '../src/auth.js';
import { config, configError, configWarnings, MASTER_KEY_PLACEHOLDER, parseBool } from '../src/config.js';
import { apiTokens, deviceCodes, sessions, users } from '../src/db/schema.js';
import { bootstrapAdmin } from '../src/db/seed.js';
import { bootstrapWarning } from '../src/routes/auth.js';
import { cookieFrom, createToken, db, req, resetDb, signup, sql } from './helpers.js';

const savedConfig = { ...config };
afterEach(() => {
  Object.assign(config, savedConfig);
  vi.useRealTimers();
});

/** Moves the clock the rate limiter reads, so window-based limits can be tested. */
const advance = (ms: number) => vi.setSystemTime(Date.now() + ms);

beforeAll(resetDb);

describe('signup / login / sessions', () => {
  it('makes the first user an admin and later users regular users', async () => {
    const first = await signup('first@example.com');
    expect(first.user.role).toBe('admin');
    const second = await signup('second@example.com');
    expect(second.user.role).toBe('user');
  });


  it('rejects JSON bodies over 64 KB, except on /api/usage', async () => {
    const big = await req('/api/auth/login', { json: { email: 'a@example.com', password: 'x'.repeat(100_000) } });
    expect(big.status).toBe(413);
    const { cookie } = await signup();
    const events = Array.from({ length: 500 }, () => ({ id: randomUUID(), provider: 'local', model: 'moonshine-tiny-en-local-model', mode: 'local', audio_seconds: 1, words: 3, created_at: new Date().toISOString() }));
    const usage = await req('/api/usage', { cookie, json: { events } });
    expect(usage.status).toBe(200);
    expect((await usage.json()).accepted).toBe(500);
  });

  it('rejects cross-site login, signup and logout (login CSRF)', async () => {
    const { email, password, cookie } = await signup();
    for (const origin of ['https://evil.example', '']) {
      const headers = { origin };
      expect((await req('/api/auth/login', { headers, json: { email, password } })).status, origin).toBe(403);
      expect((await req('/api/auth/signup', { headers, json: { email: `x-${randomUUID()}@example.com`, password } })).status, origin).toBe(403);
      expect((await req('/api/auth/logout', { headers, cookie, method: 'POST' })).status, origin).toBe(403);
    }
    expect((await req('/api/me', { cookie })).status).toBe(200); // still signed in
  });

  it('accepts only application/json bodies (a cross-site form can only send text/plain)', async () => {
    const { email, password } = await signup();
    const body = JSON.stringify({ email, password });
    for (const type of ['text/plain', 'application/x-www-form-urlencoded', 'multipart/form-data; boundary=x']) {
      const res = await req('/api/auth/login', { body, headers: { 'content-type': type } });
      expect(res.status, type).toBe(415);
      expect((await res.json()).error).toBe('unsupported_media_type');
    }
    expect((await req('/api/auth/login', { body, headers: { 'content-type': 'application/json; charset=utf-8' } })).status).toBe(200);
    // an empty body needs no type (e.g. POST /api/device/start from a script)
    expect((await req('/api/device/start', { method: 'POST' })).status).toBe(200);
  });

  it('compares the full origin against an https PUBLIC_URL, not only the host', async () => {
    const { cookie } = await signup();
    config.publicUrl = 'https://cloud.example.com';
    const headers = (origin: string) => ({ origin, host: 'cloud.example.com' });
    expect((await req('/api/tokens', { cookie, json: { name: 'x' }, headers: headers('http://cloud.example.com') })).status).toBe(403);
    expect((await req('/api/tokens', { cookie, json: { name: 'x' }, headers: headers('https://cloud.example.com') })).status).toBe(201);
    // plain-http development keeps the Host fallback
    config.publicUrl = savedConfig.publicUrl;
    expect((await req('/api/tokens', { cookie, json: { name: 'x' }, headers: { origin: 'http://127.0.0.1:5173', host: '127.0.0.1:5173' } })).status).toBe(201);
  });

  it('caps signups across all clients per rolling day', async () => {
    const [{ n }] = await sql`select count(*)::int as n from users where created_at > now() - interval '1 day'`;
    config.maxSignupsPerDay = n;
    const res = await req('/api/auth/signup', { json: { email: `cap-${randomUUID()}@example.com`, password: 'long enough pw' } });
    expect(res.status).toBe(429);
    expect((await res.json()).error).toBe('signup_limit');
    config.maxSignupsPerDay = n + 1;
    await signup();
  });

  it('closes signup with ALLOW_SIGNUP=false and tells the dashboard', async () => {
    expect(await (await req('/api/auth/signup')).json()).toEqual({ open: true });
    config.allowSignup = false;
    expect(await (await req('/api/auth/signup')).json()).toEqual({ open: false });
    const res = await req('/api/auth/signup', { json: { email: `closed-${randomUUID()}@example.com`, password: 'long enough pw' } });
    expect(res.status).toBe(403);
    expect(await res.json()).toEqual({ error: 'signup_disabled', message: 'New accounts cannot be created on this server' });
  });

  it('limits signups per client per day, below the global cap', async () => {
    vi.useFakeTimers({ toFake: ['Date'] });
    const ip = '198.51.100.200';
    const attempt = () => req('/api/auth/signup', { ip, json: { email: `day-${randomUUID()}@example.com`, password: 'long enough pw' } });
    const statuses: number[] = [];
    for (let i = 0; i < 21; i++) {
      if (i && i % 10 === 0) advance(61_000); // stay under the 10-a-minute limit
      statuses.push((await attempt()).status);
    }
    expect(statuses.slice(0, 20).every((s) => s === 201)).toBe(true);
    expect(statuses[20]).toBe(429);
    expect((await req('/api/auth/signup', { ip: '198.51.100.201', json: { email: `day-${randomUUID()}@example.com`, password: 'long enough pw' } })).status).toBe(201);
  });

  it('counts only accounts created toward the per-client daily limit', async () => {
    vi.useFakeTimers({ toFake: ['Date'] });
    const ip = '198.51.100.210';
    const { email } = await signup();
    for (let i = 0; i < 30; i++) {
      if (i && i % 9 === 0) advance(61_000);
      expect((await req('/api/auth/signup', { ip, json: { email, password: 'long enough pw' } })).status).toBe(409); // taken: not counted
    }
    advance(61_000);
    expect((await req('/api/auth/signup', { ip, json: { email: `fresh-${randomUUID()}@example.com`, password: 'long enough pw' } })).status).toBe(201);
  });

  it('a login racing a password change gets no session once the change commits', async () => {
    const s = await signup();
    let login: Promise<Response> = Promise.resolve(new Response());
    // The change is in flight: the hash is updated and the sessions deleted, but not yet committed.
    await sql.begin(async (tx) => {
      await tx`update users set password_hash = 'scrypt$changed' where id = ${s.user.id}`;
      await tx`delete from sessions where user_id = ${s.user.id}`;
      login = req('/api/auth/login', { json: { email: s.email, password: s.password } });
      await new Promise((r) => setTimeout(r, 400));
    });
    expect((await login).status).toBe(401);
    expect(await db.select().from(sessions).where(eq(sessions.userId, s.user.id))).toEqual([]);
  });

  it('creates a login session only while the verified password hash is still current', async () => {
    const s = await signup();
    const [row] = await db.select().from(users).where(eq(users.id, s.user.id));
    const expires = new Date(Date.now() + 60_000);
    expect(await createSession(s.user.id, expires, `${row!.passwordHash}x`)).toBeNull();
    expect(await createSession(s.user.id, expires, row!.passwordHash)).toEqual(expect.any(String));
    expect(await db.select().from(sessions).where(eq(sessions.userId, s.user.id))).toHaveLength(2); // signup + the current one
  });

  it('changes the password: verifies the current one, keeps this session, ends the others, keeps API tokens', async () => {
    const s = await signup();
    const token = await createToken(s.cookie);
    const other = cookieFrom(await req('/api/auth/login', { json: { email: s.email, password: s.password } }));
    const change = (json: unknown, cookie = s.cookie) => req('/api/auth/password', { cookie, json });

    let res = await change({ current: 'not my password', next: 'brand new password' });
    expect(res.status).toBe(403);
    expect((await res.json()).error).toBe('wrong_password');
    expect((await change({ current: s.password, next: 'short' })).status).toBe(400);
    expect((await req('/api/auth/password', { token, json: { current: s.password, next: 'brand new password' } })).status).toBe(403); // session only
    expect((await change({ current: s.password, next: 'brand new password' }, other)).status).toBe(200);

    expect((await req('/api/me', { cookie: other })).status).toBe(200); // the session that changed it
    expect((await req('/api/me', { cookie: s.cookie })).status).toBe(401); // every other one is gone
    expect((await req('/api/me', { token })).status).toBe(200); // tokens are not revoked
    expect((await req('/api/auth/login', { json: { email: s.email, password: s.password } })).status).toBe(401);
    expect((await req('/api/auth/login', { json: { email: s.email, password: 'brand new password' } })).status).toBe(200);
    expect(await db.select().from(sessions).where(eq(sessions.userId, s.user.id))).toHaveLength(2);
  });

  it('rate limits password changes per user', async () => {
    const s = await signup();
    const statuses = [];
    for (let i = 0; i < 6; i++) statuses.push((await req('/api/auth/password', { cookie: s.cookie, json: { current: 'wrong wrong', next: 'whatever123' } })).status);
    expect(statuses).toEqual([403, 403, 403, 403, 403, 429]);
  });

  it('rejects duplicate emails and weak passwords', async () => {
    expect((await req('/api/auth/signup', { json: { email: 'first@example.com', password: 'longenough1' } })).status).toBe(409);
    const weak = await req('/api/auth/signup', { json: { email: 'x@example.com', password: 'short' } });
    expect(weak.status).toBe(400);
    expect((await weak.json()).error).toBe('invalid_request');
  });

  it('logs in, exposes /api/me, and logs out', async () => {
    const { email, password } = await signup();
    const bad = await req('/api/auth/login', { json: { email, password: 'wrong password' } });
    expect(bad.status).toBe(401);
    const res = await req('/api/auth/login', { json: { email: email.toUpperCase(), password } });
    expect(res.status).toBe(200);
    const cookie = cookieFrom(res);
    expect(res.headers.get('set-cookie')).toMatch(/HttpOnly/i);
    expect(res.headers.get('set-cookie')).toMatch(/SameSite=Lax/i);

    const me = await (await req('/api/me', { cookie })).json();
    expect(me).toMatchObject({ user: { email }, plan: 'free', usage: { managed_minutes: 0, included_minutes: 15 } });

    expect((await req('/api/auth/logout', { cookie, method: 'POST' })).status).toBe(200);
    expect((await req('/api/me', { cookie })).status).toBe(401);
  });

  it('rejects cookie-authenticated mutations from another origin (CSRF)', async () => {
    const { cookie } = await signup();
    const res = await req('/api/tokens', { cookie, json: { name: 'x' }, headers: { origin: 'https://evil.example' } });
    expect(res.status).toBe(403);
    expect((await res.json()).error).toBe('csrf');
    // GET is fine cross-origin (read-only, and the browser blocks reading the response)
    expect((await req('/api/me', { cookie, headers: { origin: 'https://evil.example' } })).status).toBe(200);
  });

  it('rate limits login attempts per IP', async () => {
    const statuses: number[] = [];
    for (let i = 0; i < 12; i++) {
      const r = await req('/api/auth/login', { ip: '203.0.113.9', json: { email: 'nobody@example.com', password: 'whatever1' } });
      statuses.push(r.status);
    }
    expect(statuses.slice(0, 10).every((s) => s === 401)).toBe(true);
    expect(statuses.slice(10)).toEqual([429, 429]);
  });

  it('rate limits by the proxy-appended (rightmost) X-Forwarded-For entry', async () => {
    const statuses: number[] = [];
    for (let i = 0; i < 12; i++) {
      // the client controls everything left of what our proxy appended
      const headers = { 'x-forwarded-for': `198.51.100.${i}, 203.0.113.10` };
      statuses.push((await req('/api/auth/login', { headers, json: { email: 'nobody2@example.com', password: 'whatever1' } })).status);
    }
    expect(statuses.slice(10)).toEqual([429, 429]);
  });

  it('puts an IPv6 client\'s whole /64 in one bucket', async () => {
    expect(ipBucket('2001:db8:1:2::a')).toBe('2001:db8:1:2::/64');
    expect(ipBucket('2001:0db8:0001:0002:ffff:ffff:ffff:ffff')).toBe('2001:db8:1:2::/64');
    expect(ipBucket('::1')).toBe('0:0:0:0::/64');
    expect(ipBucket('2001:db8::')).toBe('2001:db8:0:0::/64');
    expect(ipBucket('::ffff:192.0.2.7')).toBe('192.0.2.7');
    expect(ipBucket('64:ff9b::192.0.2.7')).toBe('64:ff9b:0:0::/64');
    expect(ipBucket('192.0.2.7')).toBe('192.0.2.7');
    const statuses: number[] = [];
    for (let i = 0; i < 12; i++) {
      statuses.push((await req('/api/auth/login', { ip: `2001:db8:77:1::${(i + 1).toString(16)}`, json: { email: `v6-${i}@example.com`, password: 'whatever1' } })).status);
    }
    expect(statuses.slice(10)).toEqual([429, 429]);
    expect((await req('/api/auth/login', { ip: '2001:db8:77:2::1', json: { email: 'v6@example.com', password: 'whatever1' } })).status).toBe(401);
  });

  it('limits guesses per account and client, without locking the owner out from elsewhere', async () => {
    vi.useFakeTimers({ toFake: ['Date'] });
    const { email, password } = await signup();
    const attacker = '203.0.113.50';
    for (let i = 0; i < 10; i++) expect((await req('/api/auth/login', { ip: attacker, json: { email, password: `guess-${i}` } })).status).toBe(401);
    advance(61_000); // past the per-IP minute
    expect((await req('/api/auth/login', { ip: attacker, json: { email, password: 'guess-x' } })).status).toBe(429);
    // the same client can still try another account, and the owner signs in from their own address
    expect((await req('/api/auth/login', { ip: attacker, json: { email: `other-${randomUUID()}@example.com`, password: 'guess' } })).status).toBe(401);
    expect((await req('/api/auth/login', { ip: '198.51.100.77', json: { email, password } })).status).toBe(200);
  });

  it('keeps a much higher account-wide ceiling across all clients', async () => {
    const { email } = await signup();
    const statuses: number[] = [];
    for (let i = 0; i < 201; i++) statuses.push((await req('/api/auth/login', { json: { email, password: `guess-${i}` } })).status);
    expect(statuses.slice(0, 200).every((s) => s === 401)).toBe(true);
    expect(statuses[200]).toBe(429);
  });

  it('spends the same scrypt work on an unknown email as on a wrong password', async () => {
    const { email } = await signup();
    const time = async (address: string) => {
      const t0 = performance.now();
      expect((await req('/api/auth/login', { json: { email: address, password: 'wrong password' } })).status).toBe(401);
      return performance.now() - t0;
    };
    const median = (xs: number[]) => xs.sort((a, b) => a - b)[Math.floor(xs.length / 2)]!;
    const known: number[] = [];
    const unknown: number[] = [];
    for (let i = 0; i < 5; i++) {
      known.push(await time(email));
      unknown.push(await time(`missing-${randomUUID()}@example.com`));
    }
    // Without the dummy hash an unknown email costs one indexed query, far below one scrypt.
    expect(median(unknown)).toBeGreaterThan(median(known) * 0.5);
  });

  it('blocks disabled users everywhere', async () => {
    const s = await signup();
    const token = await createToken(s.cookie);
    await db.update(users).set({ disabled: true }).where(eq(users.id, s.user.id));
    expect((await req('/api/me', { token })).status).toBe(403);
    expect((await req('/api/me', { cookie: s.cookie })).status).toBe(403);
    expect((await req('/api/auth/login', { json: { email: s.email, password: s.password } })).status).toBe(403);
  });
});

describe('API tokens', () => {
  it('creates, uses, lists and revokes tokens', async () => {
    const { cookie } = await signup();
    const created = await (await req('/api/tokens', { cookie, json: { name: 'laptop' } })).json();
    expect(created.token).toMatch(/^utt_/);
    expect(created.prefix).toBe(created.token.slice(0, 10));

    expect((await req('/api/me', { token: created.token })).status).toBe(200);
    const list = await (await req('/api/tokens', { cookie })).json();
    expect(list.tokens).toHaveLength(1);
    expect(JSON.stringify(list)).not.toContain(created.token); // only the prefix is ever shown
    expect(list.tokens[0].last_used_at).not.toBeNull();

    // A token cannot mint or list tokens.
    expect((await req('/api/tokens', { token: created.token, json: { name: 'x' } })).status).toBe(403);

    expect((await req(`/api/tokens/${created.id}`, { cookie, method: 'DELETE' })).status).toBe(200);
    expect((await req('/api/me', { token: created.token })).status).toBe(401);
  });

  it("cannot revoke another user's token", async () => {
    const a = await signup();
    const b = await signup();
    const created = await (await req('/api/tokens', { cookie: a.cookie, json: { name: 'a' } })).json();
    expect((await req(`/api/tokens/${created.id}`, { cookie: b.cookie, method: 'DELETE' })).status).toBe(404);
    expect((await req('/api/me', { token: created.token })).status).toBe(200);
  });

  it('rejects unknown bearer tokens with an OpenAI-style error on /v1', async () => {
    const res = await req('/v1/models', { token: 'utt_nope' });
    expect(res.status).toBe(401);
    expect((await res.json()).error).toMatchObject({ type: 'unauthorized' });
  });
});

describe('device-code flow', () => {
  it('runs start -> pending -> approve -> token (once)', async () => {
    const start = await (await req('/api/device/start', { json: { device_name: 'Work laptop' } })).json();
    expect(start).toMatchObject({ interval: 5, expires_in: 600 });
    expect(start.user_code).toMatch(/^[A-Z]{4}-[A-Z]{4}$/);
    expect(start.verification_url).toBe(`http://localhost:8787/device?code=${start.user_code}`);

    const pending = await req('/api/device/poll', { json: { device_code: start.device_code } });
    expect(pending.status).toBe(428);
    expect(await pending.json()).toEqual({ error: 'authorization_pending' });

    const { cookie } = await signup();
    const info = await (await req(`/api/device/${start.user_code.toLowerCase().replace('-', '')}`, { cookie })).json();
    expect(info.device_name).toBe('Work laptop');
    expect((await req('/api/device/approve', { cookie, json: { user_code: start.user_code } })).status).toBe(200);
    // already approved -> code no longer usable
    expect((await req('/api/device/approve', { cookie, json: { user_code: start.user_code } })).status).toBe(404);

    const ok = await req('/api/device/poll', { json: { device_code: start.device_code } });
    expect(ok.status).toBe(200);
    const { token } = await ok.json();
    expect(token).toMatch(/^utt_/);
    expect((await (await req('/api/me', { token })).json()).user.email).toBeTruthy();
    const tokens = await (await req('/api/tokens', { cookie })).json();
    expect(tokens.tokens.map((t: { name: string }) => t.name)).toEqual(['Work laptop']);

    // the token is handed out once
    expect((await req('/api/device/poll', { json: { device_code: start.device_code } })).status).toBe(400);
  });

  it('returns 400 expired_token for expired codes and approval requires a session', async () => {
    const start = await (await req('/api/device/start', { json: {} })).json();
    await db.update(deviceCodes).set({ expiresAt: new Date(Date.now() - 1000) }).where(eq(deviceCodes.userCode, start.user_code));
    const res = await req('/api/device/poll', { json: { device_code: start.device_code } });
    expect(res.status).toBe(400);
    expect((await res.json()).error).toBe('expired_token');

    const s = await signup();
    const token = await createToken(s.cookie);
    const fresh = await (await req('/api/device/start', { json: {} })).json();
    expect((await req('/api/device/approve', { token, json: { user_code: fresh.user_code } })).status).toBe(403);
  });

  it('approves a code once under concurrent approvals (no orphan token)', async () => {
    const start = await (await req('/api/device/start', { json: {} })).json();
    const { cookie } = await signup();
    // Hold the row so all three approvals pass the initial lookup before any of them writes.
    let pending: Promise<Response>[] = [];
    await sql.begin(async (tx) => {
      await tx`select 1 from device_codes where user_code = ${start.user_code} for update`;
      pending = [1, 2, 3].map(() => req('/api/device/approve', { cookie, json: { user_code: start.user_code } }));
      await new Promise((r) => setTimeout(r, 300));
    });
    const approvals = await Promise.all(pending);
    expect(approvals.map((r) => r.status).sort()).toEqual([200, 404, 404]);
    expect((await (await req('/api/tokens', { cookie })).json()).tokens).toHaveLength(1);
    expect((await req('/api/device/poll', { json: { device_code: start.device_code } })).status).toBe(200);
  });

  it('rate limits code lookups and decisions per user', async () => {
    const { cookie } = await signup();
    const lookups = [];
    for (let i = 0; i < 31; i++) lookups.push((await req(`/api/device/BCDF-GHJ${'KLMNPQRSTVWXZBCDFGHJKLMNPQRSTVWXZ'[i]}`, { cookie })).status);
    expect(lookups.slice(0, 30).every((s) => s === 404)).toBe(true);
    expect(lookups[30]).toBe(429);
    const decisions = [];
    for (let i = 0; i < 11; i++) decisions.push((await req(i % 2 ? '/api/device/deny' : '/api/device/approve', { cookie, json: { user_code: 'BCDF-GHJK' } })).status);
    expect(decisions.slice(0, 10).every((s) => s === 404)).toBe(true);
    expect(decisions[10]).toBe(429);
  });

  it('deny removes the request', async () => {
    const start = await (await req('/api/device/start', { json: {} })).json();
    const { cookie } = await signup();
    expect((await req('/api/device/deny', { cookie, json: { user_code: start.user_code } })).status).toBe(200);
    expect((await req('/api/device/poll', { json: { device_code: start.device_code } })).status).toBe(400);
  });
});

describe('admin bootstrap and closed signup on an empty database', () => {
  beforeAll(resetDb);

  it('warns at startup while the first signup would become admin', async () => {
    expect(await bootstrapWarning()).toMatch(/becomes ADMIN/);
    config.adminEmail = 'ops@example.com';
    expect(await bootstrapWarning()).toMatch(/ADMIN_PASSWORD/);
    config.adminPassword = 'operator password';
    expect(await bootstrapWarning()).toBeNull();
  });

  it('with ADMIN_EMAIL set no signup becomes admin; startup creates that admin once', async () => {
    config.adminEmail = 'ops@example.com';
    expect((await signup('first@example.com')).user.role).toBe('user'); // first account, but not promoted
    expect(await bootstrapAdmin('ops@example.com', 'operator password')).toBe('created');
    expect(await bootstrapAdmin('ops@example.com', 'operator password')).toBeNull(); // an admin exists
    expect(await bootstrapAdmin('ops@example.com', '')).toBeNull();
    expect((await req('/api/auth/login', { json: { email: 'ops@example.com', password: 'operator password' } })).status).toBe(200);
  });

  it('startup promotes an existing account with the db:seed reset (password, sessions, tokens)', async () => {
    await resetDb();
    config.adminEmail = 'ops@example.com';
    const squatter = await signup('ops@example.com');
    const token = await createToken(squatter.cookie);
    expect(squatter.user.role).toBe('user');
    expect(await bootstrapAdmin('ops@example.com', 'operator password')).toMatch(/^promoted/);
    expect((await db.select().from(users).where(eq(users.id, squatter.user.id)))[0]!.role).toBe('admin');
    expect(await db.select().from(sessions).where(eq(sessions.userId, squatter.user.id))).toEqual([]);
    expect(await db.select().from(apiTokens).where(eq(apiTokens.userId, squatter.user.id))).toEqual([]);
    expect((await req('/api/me', { token })).status).toBe(401);
    expect((await req('/api/auth/login', { json: { email: 'ops@example.com', password: squatter.password } })).status).toBe(401);
  });

  it('with signup closed, still lets the first admin sign up (not when ADMIN_EMAIL is set)', async () => {
    await resetDb();
    config.allowSignup = false;
    expect(await (await req('/api/auth/signup')).json()).toEqual({ open: true });
    expect((await signup('owner@example.com')).user.role).toBe('admin');
    expect(await (await req('/api/auth/signup')).json()).toEqual({ open: false });
    expect((await req('/api/auth/signup', { json: { email: 'late@example.com', password: 'long enough pw' } })).status).toBe(403);

    await resetDb();
    config.adminEmail = 'ops@example.com';
    expect(await (await req('/api/auth/signup')).json()).toEqual({ open: false });
    expect((await req('/api/auth/signup', { json: { email: 'ops@example.com', password: 'long enough pw' } })).status).toBe(403);
  });
});

describe('configuration', () => {
  it('refuses only the placeholder MASTER_KEY; a short one is a warning', () => {
    expect(configError({ masterKey: MASTER_KEY_PLACEHOLDER })).toMatch(/placeholder/);
    expect(configError({ masterKey: 'short-but-existing' })).toBeNull();
    expect(configWarnings({ masterKey: 'short-but-existing' })).toEqual([expect.stringMatching(/shorter than 32/)]);
    expect(configWarnings({ masterKey: 'x'.repeat(32) })).toEqual([]);
    expect(configError({ masterKey: '' })).toBeNull();
    expect(configWarnings({ masterKey: '' })).toEqual([]);
  });

  it('refuses ADMIN_EMAIL / ADMIN_PASSWORD the admin could never sign in with, without echoing the password', () => {
    expect(configError({ masterKey: '', adminEmail: 'ops@example.com', adminPassword: 'long enough pw' })).toBeNull();
    expect(configError({ masterKey: '', adminEmail: 'not an email', adminPassword: 'long enough pw' })).toBe('ADMIN_EMAIL is not a valid email address');
    const short = configError({ masterKey: '', adminEmail: 'ops@example.com', adminPassword: 'secret7' });
    expect(short).toBe('ADMIN_PASSWORD must be 8 to 200 characters');
    expect(short).not.toContain('secret7');
  });

  it('parses TRUST_PROXY strictly', async () => {
    const saved = { ...process.env };
    try {
      for (const [raw, expected] of [['2', 2], ['true', 1], ['off', 0], ['', 0]] as const) {
        process.env.TRUST_PROXY = raw;
        vi.resetModules();
        const m = await import('../src/config.js');
        expect(m.config.trustProxy, raw).toBe(expected);
        expect(m.configError({ masterKey: '' })).toBeNull();
      }
      process.env.TRUST_PROXY = 'nginx';
      vi.resetModules();
      expect((await import('../src/config.js')).configError()).toMatch(/TRUST_PROXY/);
    } finally {
      process.env = saved;
      vi.resetModules();
    }
  });

  it('parses booleans strictly and refuses to start on anything else', async () => {
    for (const v of ['true', 'TRUE', '1', 'yes', 'On']) expect(parseBool(v), v).toBe(true);
    for (const v of ['false', 'False', '0', 'no', 'OFF']) expect(parseBool(v), v).toBe(false);
    for (const v of ['', 'maybe', 'disabled', 'n']) expect(parseBool(v), v).toBeNull();
    const saved = { ...process.env };
    try {
      process.env.ALLOW_SIGNUP = 'off';
      vi.resetModules();
      const off = await import('../src/config.js');
      expect(off.config.allowSignup).toBe(false);
      expect(off.configError()).toBeNull();
      process.env.ALLOW_SIGNUP = 'maybe';
      process.env.MAX_SIGNUPS_PER_DAY = 'lots';
      vi.resetModules();
      const bad = await import('../src/config.js');
      expect(bad.configError()).toMatch(/ALLOW_SIGNUP.*MAX_SIGNUPS_PER_DAY/);
    } finally {
      process.env = saved;
      vi.resetModules();
    }
  });
});
