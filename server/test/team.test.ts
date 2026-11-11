import { randomUUID } from 'node:crypto';
import { and, eq } from 'drizzle-orm';
import { beforeAll, describe, expect, it } from 'vitest';
import { orgInvites, orgLibrary, orgMembers, orgs, users } from '../src/db/schema.js';
import { MAX_LIBRARY_BYTES, purgeTombstones, SYNC_PAGE_SIZE, TOMBSTONE_TTL_MS } from '../src/routes/team.js';
import { sha256 } from '../src/lib/crypto.js';
import { db, req, resetDb, sql, userWithToken } from './helpers.js';

type U = Awaited<ReturnType<typeof userWithToken>>;

beforeAll(resetDb);

const json = (res: Response) => res.json() as Promise<any>;

async function createTeam(name = 'Acme') {
  const admin = await userWithToken();
  const res = await req('/api/team', { cookie: admin.cookie, json: { name } });
  expect(res.status).toBe(201);
  return { admin, team: (await json(res)).team as { id: string; name: string; role: string; members: number; libraryVersion: number } };
}

async function newInvite(cookie: string) {
  const res = await req('/api/team/invites', { cookie, method: 'POST' });
  expect(res.status).toBe(201);
  return (await json(res)) as { id: string; code: string; expiresAt: string };
}

async function addMember(adminCookie: string) {
  const u = await userWithToken();
  const { code } = await newInvite(adminCookie);
  expect((await req('/api/team/join', { cookie: u.cookie, json: { code } })).status).toBe(200);
  return u;
}

async function addEntry(cookie: string, kind: string, data: unknown) {
  return req('/api/team/library', { cookie, json: { kind, data } });
}

const sync = async (auth: { token?: string; cookie?: string }, since?: number | string) =>
  json(await req(`/api/library${since === undefined ? '' : `?since=${since}`}`, auth));

describe('authorisation matrix', () => {
  let admin: U, member: U, outsider: U, otherAdmin: U;
  let memberRoutes: [string, string, unknown?][];
  let adminRoutes: [string, string, unknown?][];
  let otherTeamId: string;

  beforeAll(async () => {
    ({ admin } = await createTeam('Matrix'));
    member = await addMember(admin.cookie);
    outsider = await userWithToken();
    const other = await createTeam('Other');
    otherAdmin = other.admin;
    otherTeamId = other.team.id;
    const invite = await newInvite(admin.cookie);
    const entry = await json(await addEntry(admin.cookie, 'dictionary', { phrase: 'Kubernetes', replacement: null }));
    const mid = member.user.id;
    memberRoutes = [
      ['GET', '/api/team/members'],
      ['GET', '/api/team/library'],
      ['POST', '/api/team/leave'],
    ];
    adminRoutes = [
      ['PATCH', `/api/team/members/${mid}`, { role: 'admin' }],
      ['DELETE', `/api/team/invites/${invite.id}`],
      ['DELETE', `/api/team/members/${mid}`],
      ['POST', '/api/team/invites'],
      ['GET', '/api/team/invites'],
      ['POST', '/api/team/library', { kind: 'snippet', data: { trigger: 'sig', expansion: 'Regards' } }],
      ['PUT', `/api/team/library/${entry.id}`, { data: { phrase: 'k8s', replacement: 'Kubernetes' } }],
      ['DELETE', `/api/team/library/${entry.id}`],
    ];
  });

  it('every team route is 401 anonymous', async () => {
    for (const [method, path, body] of [...memberRoutes, ...adminRoutes, ['GET', '/api/team'], ['POST', '/api/team', { name: 'x' }], ['POST', '/api/team/join', { code: 'x' }], ['GET', '/api/library']] as const) {
      expect((await req(path, { method, json: body })).status, `${method} ${path}`).toBe(401);
    }
  });

  it('session-only routes refuse API tokens (even an admin token)', async () => {
    for (const [method, path, body] of [...memberRoutes, ...adminRoutes, ['POST', '/api/team', { name: 'x' }], ['POST', '/api/team/join', { code: 'x' }]] as const) {
      for (const token of [admin.token, member.token, outsider.token]) {
        const res = await req(path, { method, token, json: body });
        expect(res.status, `${method} ${path}`).toBe(403);
        expect((await json(res)).error).toBe('session_required');
      }
    }
  });

  it('non-members get 404 no_team on member and admin routes', async () => {
    for (const [method, path, body] of [...memberRoutes, ...adminRoutes]) {
      const res = await req(path, { method, cookie: outsider.cookie, json: body });
      expect(res.status, `${method} ${path}`).toBe(404);
      expect((await json(res)).error).toBe('no_team');
    }
  });

  it('members can read the team but get 403 on admin routes', async () => {
    for (const [method, path] of memberRoutes.filter(([m]) => m === 'GET')) {
      expect((await req(path, { method, cookie: member.cookie })).status, path).toBe(200);
    }
    for (const [method, path, body] of adminRoutes) {
      const res = await req(path, { method, cookie: member.cookie, json: body });
      expect(res.status, `${method} ${path}`).toBe(403);
      expect((await json(res)).error).toBe('forbidden');
    }
  });

  it("another team's admin gets 404 for this team's member, invite and entry ids", async () => {
    for (const [method, path, body] of adminRoutes.filter(([, p]) => /\/[0-9a-f-]{36}$/.test(p))) {
      const res = await req(path, { method, cookie: otherAdmin.cookie, json: body });
      expect(res.status, `${method} ${path}`).toBe(404);
      expect((await json(res)).error).toBe('not_found');
    }
    const members = await json(await req('/api/team/members', { cookie: otherAdmin.cookie }));
    expect(members.members.map((m: any) => m.id)).toEqual([otherAdmin.user.id]);
    expect((await json(await req('/api/team/library', { cookie: otherAdmin.cookie }))).entries).toEqual([]);
    expect((await json(await req('/api/team/invites', { cookie: otherAdmin.cookie }))).invites).toEqual([]);
    expect((await sync({ token: otherAdmin.token })).team.id).toBe(otherTeamId);
  });

  it('nothing above changed the team', async () => {
    const members = await json(await req('/api/team/members', { cookie: admin.cookie }));
    expect(members.members.map((m: any) => [m.id, m.role])).toEqual([
      [admin.user.id, 'admin'],
      [member.user.id, 'member'],
    ]);
    expect((await json(await req('/api/team/invites', { cookie: admin.cookie }))).invites).toHaveLength(2); // addMember's and the matrix's
    const lib = await json(await req('/api/team/library', { cookie: admin.cookie }));
    expect(lib.entries.map((e: any) => e.data)).toEqual([{ phrase: 'Kubernetes', replacement: null }]);
    expect(lib.version).toBe(1);
  });

  it('admins can use every admin route', async () => {
    for (const [method, path, body] of adminRoutes) {
      const res = await req(path, { method, cookie: admin.cookie, json: body });
      expect(res.status, `${method} ${path}`).toBeLessThan(300);
    }
  });

  it('malformed ids are 404', async () => {
    for (const [method, path] of [
      ['PATCH', '/api/team/members/nope'],
      ['DELETE', '/api/team/members/1'],
      ['DELETE', '/api/team/invites/abc'],
      ['PUT', '/api/team/library/abc'],
      ['DELETE', '/api/team/library/abc'],
    ]) {
      expect((await req(path!, { method, cookie: admin.cookie, json: { role: 'member', data: {} } })).status, path).toBe(404);
    }
  });

  it('cookie mutations from another origin are rejected (CSRF)', async () => {
    const res = await req('/api/team/library', { cookie: admin.cookie, headers: { origin: 'https://evil.example' }, json: { kind: 'snippet', data: { trigger: 'a', expansion: 'b' } } });
    expect(res.status).toBe(403);
    expect((await json(res)).error).toBe('csrf');
  });

  it('API tokens work for GET /api/team and GET /api/library', async () => {
    const m = await addMember(admin.cookie); // the matrix member was removed above
    const t = await json(await req('/api/team', { token: m.token }));
    expect(t.team).toMatchObject({ name: 'Matrix', role: 'member' });
    expect((await sync({ token: m.token })).team.name).toBe('Matrix');
  });

  it('disabled users are rejected by the auth middleware', async () => {
    const u = await addMember(admin.cookie);
    await db.update(users).set({ disabled: true }).where(eq(users.id, u.user.id));
    expect((await req('/api/library', { token: u.token })).status).toBe(403);
    expect((await req('/api/team', { token: u.token })).status).toBe(403);
    expect((await req('/api/team/members', { cookie: u.cookie })).status).toBe(403);
  });
});

describe('create / join / leave', () => {
  it('GET /api/team is null outside a team', async () => {
    const u = await userWithToken();
    expect(await json(await req('/api/team', { cookie: u.cookie }))).toEqual({ team: null });
    expect(await json(await req('/api/team', { token: u.token }))).toEqual({ team: null });
  });

  it('creates a team with the caller as admin and validates the name', async () => {
    const u = await userWithToken();
    expect((await req('/api/team', { cookie: u.cookie, json: { name: '   ' } })).status).toBe(400);
    expect((await req('/api/team', { cookie: u.cookie, json: { name: 'x'.repeat(81) } })).status).toBe(400);
    expect((await req('/api/team', { cookie: u.cookie, json: {} })).status).toBe(400);
    const res = await req('/api/team', { cookie: u.cookie, json: { name: '  Design team  ' } });
    expect(res.status).toBe(201);
    const { team } = await json(res);
    expect(team).toEqual({ id: expect.any(String), name: 'Design team', role: 'admin', members: 1, libraryVersion: 0 });
    expect(await json(await req('/api/team', { token: u.token }))).toEqual({ team });

    const before = (await db.select().from(orgs)).length;
    const again = await req('/api/team', { cookie: u.cookie, json: { name: 'Second' } });
    expect(again.status).toBe(409);
    expect((await json(again)).error).toBe('already_in_team');
    expect((await db.select().from(orgs)).length).toBe(before); // rolled back
  });

  it('joins by code; 409 when already in a team', async () => {
    const { admin, team } = await createTeam();
    const { code } = await newInvite(admin.cookie);
    const u = await userWithToken();
    const res = await req('/api/team/join', { cookie: u.cookie, json: { code: ` ${code} ` } });
    expect(res.status).toBe(200);
    expect((await json(res)).team).toEqual({ id: team.id, name: 'Acme', role: 'member', members: 2, libraryVersion: 0 });
    expect((await req('/api/team/join', { cookie: u.cookie, json: { code } })).status).toBe(409);
    expect((await req('/api/team/join', { cookie: admin.cookie, json: { code } })).status).toBe(409);
    // codes are multi-use until they expire
    expect((await req('/api/team/join', { cookie: (await userWithToken()).cookie, json: { code } })).status).toBe(200);
  });

  it('answers unknown, expired and revoked codes identically', async () => {
    const { admin } = await createTeam();
    const expired = await newInvite(admin.cookie);
    await db.update(orgInvites).set({ expiresAt: new Date(Date.now() - 1000) }).where(eq(orgInvites.id, expired.id));
    const revoked = await newInvite(admin.cookie);
    expect((await req(`/api/team/invites/${revoked.id}`, { method: 'DELETE', cookie: admin.cookie })).status).toBe(200);
    const u = await userWithToken();
    const bodies = [];
    for (const code of ['not-a-real-code', expired.code, revoked.code]) {
      const res = await req('/api/team/join', { cookie: u.cookie, json: { code } });
      expect(res.status).toBe(404);
      bodies.push(await res.text());
    }
    expect(new Set(bodies).size).toBe(1);
    expect(JSON.parse(bodies[0]!).error).toBe('invalid_code');
    expect(await json(await req('/api/team', { cookie: u.cookie }))).toEqual({ team: null });
  });

  it('rate limits join attempts per user, after the session check', async () => {
    const u = await userWithToken();
    const statuses = [];
    for (let i = 0; i < 12; i++) statuses.push((await req('/api/team/join', { cookie: u.cookie, json: { code: `guess-${i}` } })).status);
    expect(statuses.slice(0, 10).every((s) => s === 404)).toBe(true);
    expect(statuses.slice(10)).toEqual([429, 429]);
    // anonymous floods from one address do not lock out a signed-in user there
    for (let i = 0; i < 12; i++) expect((await req('/api/team/join', { ip: '203.0.113.77', json: { code: 'x' } })).status).toBe(401);
    expect((await req('/api/team/join', { ip: '203.0.113.77', cookie: (await userWithToken()).cookie, json: { code: 'x' } })).status).toBe(404);
  });

  it('a join that waited behind a revocation rejects the revoked code', async () => {
    const { admin, team } = await createTeam();
    const inv = await newInvite(admin.cookie);
    const u = await userWithToken();
    let join: Promise<Response> = Promise.resolve(new Response());
    // Hold the team row the way revocation does, revoke inside, and let the join queue behind it.
    await sql.begin(async (tx) => {
      await tx`select 1 from orgs where id = ${team.id} for update`;
      join = req('/api/team/join', { cookie: u.cookie, json: { code: inv.code } });
      await new Promise((r) => setTimeout(r, 300));
      await tx`delete from org_invites where id = ${inv.id}`;
    });
    const res = await join;
    expect(res.status).toBe(404);
    expect((await json(res)).error).toBe('invalid_code');
  });

  it('members leave freely; the last admin cannot leave while others remain', async () => {
    const { admin, team } = await createTeam();
    const m1 = await addMember(admin.cookie);
    const m2 = await addMember(admin.cookie);
    expect((await req('/api/team/leave', { method: 'POST', cookie: m1.cookie })).status).toBe(200);
    expect(await json(await req('/api/team', { cookie: m1.cookie }))).toEqual({ team: null });

    const blocked = await req('/api/team/leave', { method: 'POST', cookie: admin.cookie });
    expect(blocked.status).toBe(409);
    expect(await json(blocked)).toMatchObject({ error: 'last_admin', message: expect.stringMatching(/admin/) });

    await req(`/api/team/members/${m2.user.id}`, { method: 'PATCH', cookie: admin.cookie, json: { role: 'admin' } });
    expect((await req('/api/team/leave', { method: 'POST', cookie: admin.cookie })).status).toBe(200);
    expect((await json(await req('/api/team', { cookie: m2.cookie }))).team).toMatchObject({ id: team.id, role: 'admin', members: 1 });
    expect((await req('/api/team/leave', { method: 'POST', cookie: m1.cookie })).status).toBe(404);
  });

  it('the only member leaving deletes the team, its library and invites', async () => {
    const { admin, team } = await createTeam();
    await addEntry(admin.cookie, 'snippet', { trigger: 'addr', expansion: '1 Main St' });
    const { code } = await newInvite(admin.cookie);
    expect((await req('/api/team/leave', { method: 'POST', cookie: admin.cookie })).status).toBe(200);
    expect(await db.select().from(orgs).where(eq(orgs.id, team.id))).toEqual([]);
    expect(await db.select().from(orgLibrary).where(eq(orgLibrary.orgId, team.id))).toEqual([]);
    expect(await db.select().from(orgInvites).where(eq(orgInvites.orgId, team.id))).toEqual([]);
    expect((await req('/api/team/join', { cookie: (await userWithToken()).cookie, json: { code } })).status).toBe(404);
    // and the former admin can start over
    expect((await req('/api/team', { cookie: admin.cookie, json: { name: 'Again' } })).status).toBe(201);
  });
});

describe('members', () => {
  it('lists members with id, email, name and role', async () => {
    const { admin } = await createTeam();
    const m = await addMember(admin.cookie);
    const { members } = await json(await req('/api/team/members', { cookie: m.cookie }));
    expect(members).toEqual([
      { id: admin.user.id, email: admin.email, name: 'Test', role: 'admin' },
      { id: m.user.id, email: m.email, name: 'Test', role: 'member' },
    ]);
  });

  it('promotes, demotes and removes; the last admin cannot be demoted or removed', async () => {
    const { admin } = await createTeam();
    const m = await addMember(admin.cookie);
    const self = `/api/team/members/${admin.user.id}`;
    let res = await req(self, { method: 'PATCH', cookie: admin.cookie, json: { role: 'member' } });
    expect(res.status).toBe(409);
    expect((await json(res)).error).toBe('last_admin');
    expect((await req(self, { method: 'DELETE', cookie: admin.cookie })).status).toBe(409);
    expect((await req(`/api/team/members/${m.user.id}`, { method: 'PATCH', cookie: admin.cookie, json: { role: 'owner' } })).status).toBe(400);

    res = await req(`/api/team/members/${m.user.id}`, { method: 'PATCH', cookie: admin.cookie, json: { role: 'admin' } });
    expect(res.status).toBe(200);
    expect((await json(res)).member).toEqual({ id: m.user.id, email: m.email, name: 'Test', role: 'admin' });
    // now there are two admins, so either can be demoted
    expect((await req(self, { method: 'PATCH', cookie: m.cookie, json: { role: 'member' } })).status).toBe(200);
    expect((await req(`/api/team/members/${m.user.id}`, { method: 'PATCH', cookie: admin.cookie, json: { role: 'member' } })).status).toBe(403);
    await req(self, { method: 'PATCH', cookie: m.cookie, json: { role: 'admin' } });

    expect((await req(`/api/team/members/${m.user.id}`, { method: 'DELETE', cookie: admin.cookie })).status).toBe(200);
    expect(await json(await req('/api/team', { token: m.token }))).toEqual({ team: null });
    expect((await req(`/api/team/members/${m.user.id}`, { method: 'DELETE', cookie: admin.cookie })).status).toBe(404);
    expect((await req(`/api/team/members/${randomUUID()}`, { method: 'PATCH', cookie: admin.cookie, json: { role: 'admin' } })).status).toBe(404);
  });

  it('removing a member revokes every invite, so the removed member cannot rejoin', async () => {
    const { admin } = await createTeam();
    const { code } = await newInvite(admin.cookie);
    const m = await userWithToken();
    expect((await req('/api/team/join', { cookie: m.cookie, json: { code } })).status).toBe(200);
    await newInvite(admin.cookie);
    expect((await req(`/api/team/members/${m.user.id}`, { method: 'DELETE', cookie: admin.cookie })).status).toBe(200);
    expect((await json(await req('/api/team/invites', { cookie: admin.cookie }))).invites).toEqual([]);
    expect((await req('/api/team/join', { cookie: m.cookie, json: { code } })).status).toBe(404);
    expect(await sync({ token: m.token })).toMatchObject({ team: null, entries: [] });
  });

  it("demoting an admin revokes the invites that admin created, and only those", async () => {
    const { admin } = await createTeam();
    const m = await addMember(admin.cookie);
    await req(`/api/team/members/${m.user.id}`, { method: 'PATCH', cookie: admin.cookie, json: { role: 'admin' } });
    const theirs = await newInvite(m.cookie);
    const mine = await newInvite(admin.cookie);
    expect((await req(`/api/team/members/${m.user.id}`, { method: 'PATCH', cookie: admin.cookie, json: { role: 'member' } })).status).toBe(200);
    const left = (await json(await req('/api/team/invites', { cookie: admin.cookie }))).invites.map((i: any) => i.id);
    expect(left).toContain(mine.id);
    expect(left).not.toContain(theirs.id);
    expect((await req('/api/team/join', { cookie: (await userWithToken()).cookie, json: { code: theirs.code } })).status).toBe(404);
  });

  it('leaving revokes the invites the leaver created, and only those', async () => {
    const { admin } = await createTeam();
    const m = await addMember(admin.cookie);
    await req(`/api/team/members/${m.user.id}`, { method: 'PATCH', cookie: admin.cookie, json: { role: 'admin' } });
    const theirs = await newInvite(m.cookie);
    const mine = await newInvite(admin.cookie);
    expect((await req('/api/team/leave', { method: 'POST', cookie: m.cookie })).status).toBe(200);
    const left = (await json(await req('/api/team/invites', { cookie: admin.cookie }))).invites.map((i: any) => i.id);
    expect(left).toContain(mine.id);
    expect(left).not.toContain(theirs.id);
    expect((await req('/api/team/join', { cookie: (await userWithToken()).cookie, json: { code: theirs.code } })).status).toBe(404);
  });

  it('counts only active admins in the last-admin checks', async () => {
    const { admin } = await createTeam();
    const m = await addMember(admin.cookie);
    const other = await addMember(admin.cookie);
    await req(`/api/team/members/${m.user.id}`, { method: 'PATCH', cookie: admin.cookie, json: { role: 'admin' } });
    await db.update(users).set({ disabled: true }).where(eq(users.id, m.user.id));
    // the disabled admin does not count, so the active one cannot step down, be removed or leave
    expect((await req(`/api/team/members/${admin.user.id}`, { method: 'PATCH', cookie: admin.cookie, json: { role: 'member' } })).status).toBe(409);
    expect((await req('/api/team/leave', { method: 'POST', cookie: admin.cookie })).status).toBe(409);
    // but the disabled admin can be demoted
    expect((await req(`/api/team/members/${m.user.id}`, { method: 'PATCH', cookie: admin.cookie, json: { role: 'member' } })).status).toBe(200);
    expect((await req(`/api/team/members/${other.user.id}`, { method: 'DELETE', cookie: admin.cookie })).status).toBe(200);
  });

  it('serialises concurrent demotions so one admin always remains', async () => {
    const { admin } = await createTeam();
    const m = await addMember(admin.cookie);
    await req(`/api/team/members/${m.user.id}`, { method: 'PATCH', cookie: admin.cookie, json: { role: 'admin' } });
    const results = await Promise.all([
      req(`/api/team/members/${m.user.id}`, { method: 'PATCH', cookie: admin.cookie, json: { role: 'member' } }),
      req(`/api/team/members/${admin.user.id}`, { method: 'PATCH', cookie: m.cookie, json: { role: 'member' } }),
    ]);
    // the second request sees its caller already demoted by the first
    expect(results.map((r) => r.status).sort()).toEqual([200, 403]);
    const { members } = await json(await req('/api/team/members', { cookie: admin.cookie }));
    expect(members.filter((x: any) => x.role === 'admin')).toHaveLength(1);
  });
});

describe('invites', () => {
  it('returns the code once, stores only its hash, lists and revokes', async () => {
    const { admin, team } = await createTeam();
    const before = Date.now();
    const inv = await newInvite(admin.cookie);
    expect(inv.code).toMatch(/^[A-Za-z0-9_-]{22,}$/); // >= 128 bits, URL-safe
    expect(Date.parse(inv.expiresAt) - before).toBeGreaterThan(7 * 86_400_000 - 60_000);
    expect(Date.parse(inv.expiresAt) - before).toBeLessThanOrEqual(7 * 86_400_000 + 60_000);
    const second = await newInvite(admin.cookie);
    expect(second.code).not.toBe(inv.code);

    const [row] = await sql`select row_to_json(i)::text as t, code_hash from org_invites i where id = ${inv.id}`;
    expect(row!.code_hash).toBe(sha256(inv.code));
    expect(row!.t).not.toContain(inv.code);
    const dump = await sql`select row_to_json(i)::text as t from org_invites i where org_id = ${team.id}`;
    expect(dump.some((r) => r.t.includes(inv.code))).toBe(false);

    const list = await req('/api/team/invites', { cookie: admin.cookie });
    const text = await list.text();
    expect(text).not.toContain(inv.code);
    expect(JSON.parse(text).invites).toEqual([
      { id: second.id, expiresAt: second.expiresAt, createdAt: expect.any(String) },
      { id: inv.id, expiresAt: inv.expiresAt, createdAt: expect.any(String) },
    ]);

    await db.update(orgInvites).set({ expiresAt: new Date(Date.now() - 1) }).where(eq(orgInvites.id, second.id));
    expect((await json(await req('/api/team/invites', { cookie: admin.cookie }))).invites.map((i: any) => i.id)).toEqual([inv.id]);

    expect((await req(`/api/team/invites/${inv.id}`, { method: 'DELETE', cookie: admin.cookie })).status).toBe(200);
    expect((await req(`/api/team/invites/${inv.id}`, { method: 'DELETE', cookie: admin.cookie })).status).toBe(404);
    expect((await json(await req('/api/team/invites', { cookie: admin.cookie }))).invites).toEqual([]);
  });
});

describe('library', () => {
  let admin: U, member: U;
  beforeAll(async () => {
    ({ admin } = await createTeam('Library'));
    member = await addMember(admin.cookie);
  });

  it('creates, updates and deletes entries of every kind', async () => {
    let res = await addEntry(admin.cookie, 'dictionary', { phrase: '  OpsTrellis ', replacement: null });
    expect(res.status).toBe(201);
    const dict = await json(res);
    expect(dict).toEqual({ id: expect.any(String), kind: 'dictionary', data: { phrase: 'OpsTrellis', replacement: null }, updatedAt: expect.any(String) });

    res = await addEntry(admin.cookie, 'snippet', { trigger: 'sig', expansion: 'Best,\nMaya' });
    expect(res.status).toBe(201);
    const snip = await json(res);
    res = await addEntry(admin.cookie, 'action', { name: 'Formal', prompt: 'Rewrite formally', output: 'replace' });
    expect(res.status).toBe(201);
    const action = await json(res);

    res = await req(`/api/team/library/${snip.id}`, { method: 'PUT', cookie: admin.cookie, json: { data: { trigger: 'sig', expansion: 'Thanks,\nMaya' } } });
    expect(res.status).toBe(200);
    expect(await json(res)).toMatchObject({ id: snip.id, kind: 'snippet', data: { trigger: 'sig', expansion: 'Thanks,\nMaya' } });
    // kind is immutable: the data is validated against the stored kind and a sent kind is ignored
    res = await req(`/api/team/library/${snip.id}`, { method: 'PUT', cookie: admin.cookie, json: { kind: 'action', data: { name: 'x', prompt: 'y', output: 'copy' } } });
    expect(res.status).toBe(400);

    expect((await req(`/api/team/library/${action.id}`, { method: 'DELETE', cookie: admin.cookie })).status).toBe(200);
    expect((await req(`/api/team/library/${action.id}`, { method: 'DELETE', cookie: admin.cookie })).status).toBe(404);
    expect((await req(`/api/team/library/${action.id}`, { method: 'PUT', cookie: admin.cookie, json: { data: { name: 'Formal', prompt: 'p', output: 'copy' } } })).status).toBe(404);

    const lib = await json(await req('/api/team/library', { cookie: member.cookie }));
    expect(lib.version).toBe(5);
    expect(lib.entries.map((e: any) => e.id).sort()).toEqual([dict.id, snip.id].sort());
    const [tomb] = await db.select().from(orgLibrary).where(eq(orgLibrary.id, action.id));
    expect(tomb).toMatchObject({ deleted: true, data: {}, version: 5 });
  });

  it('rejects what desktops would skip: control characters and {clipboard}', async () => {
    const bad: [string, unknown][] = [
      ['dictionary', { phrase: 'two\nlines', replacement: null }],
      ['dictionary', { phrase: 'tab\there', replacement: null }],
      ['dictionary', { phrase: 'ok', replacement: 'bell\u0007' }],
      ['dictionary', { phrase: 'c1\u0085', replacement: null }],
      ['snippet', { trigger: 'a\rb', expansion: 'x' }],
      ['snippet', { trigger: 'cr', expansion: 'line\r\nbreak' }],
      ['snippet', { trigger: 'esc', expansion: 'x\u001b[31m' }],
      ['snippet', { trigger: 'clip', expansion: 'Paste: {clipboard}' }],
      ['action', { name: 'del\u007f', prompt: 'p', output: 'copy' }],
      ['action', { name: 'n', prompt: 'p\u000bq', output: 'copy' }],
    ];
    for (const [kind, data] of bad) {
      const res = await addEntry(admin.cookie, kind, data);
      expect(res.status, JSON.stringify([kind, data])).toBe(400);
    }
    // newlines and tabs are fine where the desktop allows them, and so is a near-miss placeholder
    expect((await addEntry(admin.cookie, 'snippet', { trigger: 'multi', expansion: 'a\n\tb {Clipboard} {clipboard' })).status).toBe(201);
    expect((await addEntry(admin.cookie, 'action', { name: 'Tabbed', prompt: 'step 1\n\tstep 2', output: 'replace' })).status).toBe(201);
    expect((await addEntry(admin.cookie, 'dictionary', { phrase: 'naïve café', replacement: 'Naïve Café' })).status).toBe(201);
  });

  it('caps the live library at 6 MB of JSON text, on create and on edit', async () => {
    const { admin: a, team } = await createTeam('Heavy');
    const size = (d: unknown) => Buffer.byteLength(JSON.stringify(d));
    const entry = { trigger: 'edge', expansion: 'é'.repeat(1000) }; // 2 bytes a character
    // everything else in the team takes the rest of the budget
    await db.insert(orgLibrary).values({ orgId: team.id, kind: 'dictionary', key: 'filler', data: { phrase: 'filler', replacement: null }, size: MAX_LIBRARY_BYTES - size(entry), version: 0 });
    const res = await addEntry(a.cookie, 'snippet', entry);
    expect(res.status).toBe(201); // exactly at the limit
    const created = await json(res);
    const over = await addEntry(a.cookie, 'dictionary', { phrase: 'x', replacement: null });
    expect(over.status).toBe(409);
    expect(await json(over)).toMatchObject({ error: 'library_too_large', message: expect.stringMatching(/6 MB/) });
    // an edit is measured without the entry's old text: growing by one byte is refused, shrinking is fine
    expect((await req(`/api/team/library/${created.id}`, { method: 'PUT', cookie: a.cookie, json: { data: { ...entry, expansion: `${entry.expansion}x` } } })).status).toBe(409);
    expect((await req(`/api/team/library/${created.id}`, { method: 'PUT', cookie: a.cookie, json: { data: { ...entry, expansion: 'short' } } })).status).toBe(200);
    // deleting frees its share
    await req(`/api/team/library/${created.id}`, { method: 'DELETE', cookie: a.cookie });
    expect((await addEntry(a.cookie, 'dictionary', { phrase: 'x', replacement: null })).status).toBe(201);
  });

  it('validates data strictly', async () => {
    const bad: [string, unknown][] = [
      ['dictionary', { phrase: 'a', replacement: null, extra: 1 }],
      ['dictionary', { phrase: '', replacement: null }],
      ['dictionary', { phrase: '   ', replacement: 'x' }],
      ['dictionary', { phrase: 'x'.repeat(101), replacement: null }],
      ['dictionary', { phrase: 'a', replacement: 'x'.repeat(201) }],
      ['dictionary', { phrase: 'a', replacement: '' }],
      ['dictionary', { phrase: 'a' }],
      ['snippet', { trigger: 'a', expansion: 'x'.repeat(5001) }],
      ['snippet', { trigger: 'x'.repeat(101), expansion: 'b' }],
      ['snippet', { trigger: 'a' }],
      ['action', { name: 'x'.repeat(61), prompt: 'p', output: 'copy' }],
      ['action', { name: 'n', prompt: 'x'.repeat(2001), output: 'copy' }],
      ['action', { name: 'n', prompt: 'p', output: 'paste' }],
      ['action', 'not an object'],
      ['template', { name: 'n' }],
    ];
    bad.push(['dictionary', { phrase: 'nul\u0000byte', replacement: null }], ['snippet', { trigger: 'lone', expansion: 'half \ud83d pair' }], ['action', { name: '\udc00x', prompt: 'p', output: 'copy' }]);
    for (const [kind, data] of bad) {
      const res = await addEntry(admin.cookie, kind, data);
      expect(res.status, JSON.stringify([kind, data])).toBe(400);
      expect((await json(res)).error).toBe('invalid_request');
    }
    expect((await addEntry(admin.cookie, 'snippet', { trigger: 'max', expansion: 'x'.repeat(5000) })).status).toBe(201);
    expect((await addEntry(admin.cookie, 'action', { name: 'n'.repeat(60), prompt: 'p'.repeat(2000), output: 'copy' })).status).toBe(201);
    expect((await addEntry(admin.cookie, 'snippet', { trigger: 'emoji', expansion: 'a full pair \ud83d\ude00 is fine' })).status).toBe(201);
    expect((await req('/api/team', { cookie: (await userWithToken()).cookie, json: { name: 'bad\u0000name' } })).status).toBe(400);
    const big = await addEntry(admin.cookie, 'snippet', { trigger: 'big', expansion: 'x'.repeat(70_000) });
    expect(big.status).toBe(413);
  });

  it('keeps live keys unique per kind, case-insensitively', async () => {
    expect((await addEntry(admin.cookie, 'dictionary', { phrase: 'GitHub', replacement: null })).status).toBe(201);
    const dup = await addEntry(admin.cookie, 'dictionary', { phrase: ' github ', replacement: 'GitHub' });
    expect(dup.status).toBe(409);
    expect((await json(dup)).error).toBe('duplicate');
    // same key in another kind is fine
    expect((await addEntry(admin.cookie, 'snippet', { trigger: 'github', expansion: 'https://github.com' })).status).toBe(201);

    const other = await json(await addEntry(admin.cookie, 'dictionary', { phrase: 'GitLab', replacement: null }));
    let res = await req(`/api/team/library/${other.id}`, { method: 'PUT', cookie: admin.cookie, json: { data: { phrase: 'GITHUB', replacement: null } } });
    expect(res.status).toBe(409);
    res = await req(`/api/team/library/${other.id}`, { method: 'PUT', cookie: admin.cookie, json: { data: { phrase: 'gitlab', replacement: 'GitLab' } } });
    expect(res.status).toBe(200); // renaming its own key's case is not a duplicate

    await req(`/api/team/library/${other.id}`, { method: 'DELETE', cookie: admin.cookie });
    expect((await addEntry(admin.cookie, 'dictionary', { phrase: 'GitLab', replacement: null })).status).toBe(201);
  });

  it('compares keys after Unicode NFKC normalisation, backed by a unique index', async () => {
    // U+FB01 is the "fi" ligature; NFKC turns it into "fi", and full-width letters into ASCII
    expect((await addEntry(admin.cookie, 'dictionary', { phrase: 'ﬁnance', replacement: null })).status).toBe(201);
    expect((await addEntry(admin.cookie, 'dictionary', { phrase: 'FINANCE', replacement: null })).status).toBe(409);
    expect((await addEntry(admin.cookie, 'snippet', { trigger: 'ＡＢＣ', expansion: 'x' })).status).toBe(201);
    expect((await addEntry(admin.cookie, 'snippet', { trigger: 'abc', expansion: 'y' })).status).toBe(409);
    const [row] = await db.select().from(orgLibrary).where(and(eq(orgLibrary.kind, 'snippet'), eq(orgLibrary.key, 'abc')));
    expect(row!.data).toEqual({ trigger: 'ＡＢＣ', expansion: 'x' });
    // the database itself refuses a second live row with the same key; a tombstone frees it
    await expect(db.insert(orgLibrary).values({ orgId: row!.orgId, kind: 'snippet', key: 'abc', data: { trigger: 'abc', expansion: 'z' }, version: 0 })).rejects.toThrow();
    await req(`/api/team/library/${row!.id}`, { method: 'DELETE', cookie: admin.cookie });
    const [tomb] = await db.select().from(orgLibrary).where(eq(orgLibrary.id, row!.id));
    expect(tomb!.key).toBeNull();
    expect((await addEntry(admin.cookie, 'snippet', { trigger: 'abc', expansion: 'y' })).status).toBe(201);
  });

  it('rate limits library writes per user', async () => {
    const { admin: a } = await createTeam('Busy');
    const statuses = [];
    for (let i = 0; i < 62; i++) statuses.push((await addEntry(a.cookie, 'dictionary', { phrase: `w${i}`, replacement: null })).status);
    expect(statuses.slice(0, 60).every((s) => s === 201)).toBe(true);
    expect(statuses.slice(60)).toEqual([429, 429]);
  });

  it('caps a team at 2000 live entries', async () => {
    const { admin: a, team } = await createTeam('Big');
    const rows = Array.from({ length: 1999 }, (_, i) => ({ orgId: team.id, kind: 'dictionary' as const, data: { phrase: `w${i}`, replacement: null }, version: 0 }));
    await db.insert(orgLibrary).values(rows);
    await db.insert(orgLibrary).values({ orgId: team.id, kind: 'dictionary', data: {}, version: 0, deleted: true });
    expect((await addEntry(a.cookie, 'dictionary', { phrase: 'last', replacement: null })).status).toBe(201);
    const full = await addEntry(a.cookie, 'snippet', { trigger: 'one-more', expansion: 'x' });
    expect(full.status).toBe(409);
    expect((await json(full)).error).toBe('library_full');
    const v = (await db.select().from(orgs).where(eq(orgs.id, team.id)))[0]!.libraryVersion;
    expect(v).toBe(1); // the refused write did not bump the version
  });

  it('gives concurrent writes distinct, increasing versions', async () => {
    const { admin: a, team } = await createTeam('Concurrent');
    const results = await Promise.all(Array.from({ length: 20 }, (_, i) => addEntry(a.cookie, 'snippet', { trigger: `t${i}`, expansion: `e${i}` })));
    expect(results.every((r) => r.status === 201)).toBe(true);
    const versions = (await db.select({ v: orgLibrary.version }).from(orgLibrary).where(eq(orgLibrary.orgId, team.id))).map((r) => r.v).sort((x, y) => x - y);
    expect(versions).toEqual(Array.from({ length: 20 }, (_, i) => i + 1));
    expect((await db.select().from(orgs).where(eq(orgs.id, team.id)))[0]!.libraryVersion).toBe(20);
    // duplicates racing each other: exactly one wins
    const dups = await Promise.all(Array.from({ length: 5 }, () => addEntry(a.cookie, 'action', { name: 'Same', prompt: 'p', output: 'copy' })));
    expect(dups.map((r) => r.status).sort()).toEqual([201, 409, 409, 409, 409]);
  });
});

describe('GET /api/library (desktop sync)', () => {
  it('returns an empty library without a team', async () => {
    const u = await userWithToken();
    const none = { team: null, version: 0, entries: [], more: false, reset: false };
    expect(await sync({ token: u.token })).toEqual(none);
    expect(await sync({ cookie: u.cookie }, 5)).toEqual(none);
  });

  it('syncs incrementally, including tombstones, in version order; a full sync lists live entries only', async () => {
    const { admin, team } = await createTeam('Sync');
    const member = await addMember(admin.cookie);
    const a = await json(await addEntry(admin.cookie, 'dictionary', { phrase: 'Alpha', replacement: null }));
    const b = await json(await addEntry(admin.cookie, 'snippet', { trigger: 'b', expansion: 'Bravo' }));
    const c = await json(await addEntry(admin.cookie, 'action', { name: 'C', prompt: 'Charlie', output: 'copy' }));

    const full = await sync({ token: member.token }, 0);
    expect(full).toEqual({
      team: { id: team.id, name: 'Sync' },
      version: 3,
      entries: [
        { id: a.id, kind: 'dictionary', data: { phrase: 'Alpha', replacement: null }, deleted: false },
        { id: b.id, kind: 'snippet', data: { trigger: 'b', expansion: 'Bravo' }, deleted: false },
        { id: c.id, kind: 'action', data: { name: 'C', prompt: 'Charlie', output: 'copy' }, deleted: false },
      ],
      more: false,
      reset: false,
    });
    expect(await sync({ token: member.token })).toEqual(full); // since defaults to 0

    await req(`/api/team/library/${a.id}`, { method: 'PUT', cookie: admin.cookie, json: { data: { phrase: 'Alpha', replacement: 'ALPHA' } } });
    await req(`/api/team/library/${b.id}`, { method: 'DELETE', cookie: admin.cookie });
    const delta = await sync({ token: member.token }, 3);
    expect(delta.version).toBe(5);
    expect(delta.entries).toEqual([
      { id: a.id, kind: 'dictionary', data: { phrase: 'Alpha', replacement: 'ALPHA' }, deleted: false },
      { id: b.id, kind: 'snippet', data: {}, deleted: true },
    ]);
    expect((await sync({ token: member.token }, 4)).entries.map((e: any) => e.id)).toEqual([b.id]);
    expect(await sync({ token: member.token }, 5)).toEqual({ team: { id: team.id, name: 'Sync' }, version: 5, entries: [], more: false, reset: false });
    // the full sync leaves out the tombstone
    expect((await sync({ token: member.token }, 0)).entries.map((e: any) => [e.id, e.deleted])).toEqual([
      [c.id, false],
      [a.id, false],
    ]);
    // a cursor ahead of the server (e.g. after a database restore) restarts as a full sync
    expect(await sync({ token: member.token }, 999)).toMatchObject({ version: 5, reset: true, more: false, entries: [{ id: c.id }, { id: a.id }] });
  });

  /** Rows written straight to the table, with distinct versions after the team's current one. */
  async function bulkEntries(teamId: string, n: number, from = 0) {
    await db.insert(orgLibrary).values(
      Array.from({ length: n }, (_, i) => ({ orgId: teamId, kind: 'dictionary' as const, key: `p${from + i}`, data: { phrase: `p${from + i}`, replacement: null }, version: from + i + 1 })),
    );
    await db.update(orgs).set({ libraryVersion: from + n }).where(eq(orgs.id, teamId));
  }

  it('answers a full sync of more than 500 live entries in one response, also after a reset', async () => {
    const { admin, team } = await createTeam('Big sync');
    const member = await addMember(admin.cookie);
    await bulkEntries(team.id, 600);
    // one entry deleted, its tombstone purged, the floor raised past the 500th version
    const [victim] = await db.select().from(orgLibrary).where(and(eq(orgLibrary.orgId, team.id), eq(orgLibrary.key, 'p550')));
    await req(`/api/team/library/${victim!.id}`, { method: 'DELETE', cookie: admin.cookie }); // v601
    await db.update(orgLibrary).set({ updatedAt: new Date(Date.now() - TOMBSTONE_TTL_MS - 60_000) }).where(eq(orgLibrary.id, victim!.id));
    await purgeTombstones();
    expect((await db.select().from(orgs).where(eq(orgs.id, team.id)))[0]!.tombstoneFloor).toBe(601);

    const full = await sync({ token: member.token }, 0);
    expect(full).toMatchObject({ version: 601, more: false, reset: false });
    expect(full.entries).toHaveLength(599);
    // a cursor below the floor gets the same single full response, so a client can never loop
    const reset = await sync({ token: member.token }, 500);
    expect(reset).toMatchObject({ version: 601, more: false, reset: true });
    expect(reset.entries).toHaveLength(599);
    // and the next request from its version is an ordinary, empty incremental one
    expect(await sync({ token: member.token }, reset.version)).toMatchObject({ entries: [], more: false, reset: false });
  });

  it('pages an incremental sync, still exact when entries change between pages', async () => {
    const { admin, team } = await createTeam('Paged');
    const member = await addMember(admin.cookie);
    const first = await json(await addEntry(admin.cookie, 'snippet', { trigger: 'first', expansion: 'v1' })); // v1, the client has this
    const local = new Map<string, any>([[first.id, { kind: 'snippet', data: first.data }]]);
    await bulkEntries(team.id, SYNC_PAGE_SIZE * 2 + 10, 1); // v2..v1011

    let since = 1;
    const cursors: number[] = [];
    for (let page = 0; ; page++) {
      const res = await sync({ token: member.token }, since);
      expect(res.reset).toBe(false);
      expect(res.entries.length).toBeLessThanOrEqual(SYNC_PAGE_SIZE);
      for (const e of res.entries) (e.deleted ? local.delete(e.id) : local.set(e.id, { kind: e.kind, data: e.data }));
      cursors.push(res.version);
      since = res.version;
      if (page === 0) {
        // between pages: edit an entry already synced, delete one not yet sent, add one
        const [seen] = await db.select().from(orgLibrary).where(and(eq(orgLibrary.orgId, team.id), eq(orgLibrary.key, 'p5')));
        const [unseen] = await db.select().from(orgLibrary).where(and(eq(orgLibrary.orgId, team.id), eq(orgLibrary.key, 'p900')));
        expect((await req(`/api/team/library/${seen!.id}`, { method: 'PUT', cookie: admin.cookie, json: { data: { phrase: 'p5', replacement: 'five' } } })).status).toBe(200);
        expect((await req(`/api/team/library/${unseen!.id}`, { method: 'DELETE', cookie: admin.cookie })).status).toBe(200);
        expect((await addEntry(admin.cookie, 'dictionary', { phrase: 'late', replacement: null })).status).toBe(201);
      }
      if (!res.more) break;
    }
    // page 2 skips p900's old version (it moved past the end), so it ends one version later
    expect(cursors.slice(0, 2)).toEqual([SYNC_PAGE_SIZE + 1, SYNC_PAGE_SIZE * 2 + 2]);
    expect(cursors.at(-1)).toBe(SYNC_PAGE_SIZE * 2 + 11 + 3);
    // the client now holds exactly the server's live entries
    const live = await db.select().from(orgLibrary).where(and(eq(orgLibrary.orgId, team.id), eq(orgLibrary.deleted, false)));
    expect(new Map(live.map((e) => [e.id, { kind: e.kind, data: e.data }]))).toEqual(local);
  });

  it('limits full syncs more tightly than incremental ones', async () => {
    const { admin } = await createTeam('Full limit');
    const member = await addMember(admin.cookie);
    const statuses = [];
    for (let i = 0; i < 11; i++) statuses.push((await req('/api/library?since=0', { token: member.token })).status);
    expect(statuses.slice(0, 10).every((s) => s === 200)).toBe(true);
    expect(statuses[10]).toBe(429);
    await addEntry(admin.cookie, 'dictionary', { phrase: 'x', replacement: null });
    expect((await req('/api/library?since=1', { token: member.token })).status).toBe(200);
  });

  it('purges old tombstones, raises the floor, and resets clients below it', async () => {
    const { admin, team } = await createTeam('Purge');
    const member = await addMember(admin.cookie);
    const a = await json(await addEntry(admin.cookie, 'dictionary', { phrase: 'Old', replacement: null })); // v1
    const b = await json(await addEntry(admin.cookie, 'dictionary', { phrase: 'Keep', replacement: null })); // v2
    await req(`/api/team/library/${a.id}`, { method: 'DELETE', cookie: admin.cookie }); // v3: tombstone
    const c = await json(await addEntry(admin.cookie, 'snippet', { trigger: 'gone', expansion: 'x' })); // v4
    await req(`/api/team/library/${c.id}`, { method: 'DELETE', cookie: admin.cookie }); // v5: recent tombstone
    await db.update(orgLibrary).set({ updatedAt: new Date(Date.now() - TOMBSTONE_TTL_MS - 60_000) }).where(eq(orgLibrary.id, a.id));

    await purgeTombstones();
    expect(await db.select().from(orgLibrary).where(eq(orgLibrary.id, a.id))).toEqual([]);
    expect((await db.select().from(orgLibrary).where(eq(orgLibrary.id, c.id))).length).toBe(1); // too recent
    expect((await db.select().from(orgs).where(eq(orgs.id, team.id)))[0]!.tombstoneFloor).toBe(3);

    // below the floor the delete of "Old" is gone, so the client must start over
    const reset = await sync({ token: member.token }, 2);
    expect(reset).toMatchObject({ reset: true, more: false, version: 5 });
    expect(reset.entries.map((e: any) => e.id)).toEqual([b.id]);
    // at or above the floor the incremental sync is still exact
    expect(await sync({ token: member.token }, 3)).toMatchObject({ reset: false, version: 5, entries: [{ id: c.id, deleted: true }] });
    // a second purge never lowers the floor
    await purgeTombstones();
    expect((await db.select().from(orgs).where(eq(orgs.id, team.id)))[0]!.tombstoneFloor).toBe(3);
  });

  it('rejects an invalid since', async () => {
    const u = await userWithToken();
    for (const since of ['-1', 'abc', '1.5', '', '1e3', '9999999999999999']) {
      const res = await req(`/api/library?since=${encodeURIComponent(since)}`, { token: u.token });
      expect(res.status, since).toBe(400);
      expect((await json(res)).error).toBe('invalid_request');
    }
  });

  it('is rate limited per user', async () => {
    const u = await userWithToken();
    const statuses = [];
    for (let i = 0; i < 62; i++) statuses.push((await req('/api/library', { token: u.token })).status);
    expect(statuses.slice(0, 60).every((s) => s === 200)).toBe(true);
    expect(statuses.slice(60)).toEqual([429, 429]);
    expect((await req('/api/library', { token: (await userWithToken()).token })).status).toBe(200);
  });
});

describe('user deletion', () => {
  it("removes the user's membership and keeps the team while others remain", async () => {
    const { admin, team } = await createTeam();
    const m = await addMember(admin.cookie);
    await db.delete(users).where(eq(users.id, m.user.id));
    expect(await db.select().from(orgMembers).where(eq(orgMembers.userId, m.user.id))).toEqual([]);
    expect((await json(await req('/api/team', { cookie: admin.cookie }))).team).toMatchObject({ id: team.id, members: 1 });
  });

  it('db:seed promotion takes the account out of its team and drops its device sign-ins', async () => {
    const { seedAdmin } = await import('../src/db/seed.js');
    const { deviceCodes } = await import('../src/db/schema.js');
    const { admin } = await createTeam();
    const squatter = await addMember(admin.cookie);
    await db.insert(deviceCodes).values({ id: randomUUID(), userCode: 'BCDF-GHJK', deviceName: 'x', userId: squatter.user.id, token: 'utt_pending', expiresAt: new Date(Date.now() + 60_000) });
    expect(await seedAdmin(squatter.email, 'operator chosen password')).toMatch(/^promoted/);
    expect(await db.select().from(orgMembers).where(eq(orgMembers.userId, squatter.user.id))).toEqual([]);
    expect(await db.select().from(deviceCodes).where(eq(deviceCodes.userId, squatter.user.id))).toEqual([]);
  });

  it('promotes the longest-standing remaining member when the only admin is deleted', async () => {
    const { admin, team } = await createTeam();
    const first = await addMember(admin.cookie);
    const second = await addMember(admin.cookie);
    await db.delete(users).where(eq(users.id, admin.user.id));
    const roles = await db.select({ userId: orgMembers.userId, role: orgMembers.role }).from(orgMembers).where(eq(orgMembers.orgId, team.id));
    expect(Object.fromEntries(roles.map((r) => [r.userId, r.role]))).toEqual({ [first.user.id]: 'admin', [second.user.id]: 'member' });
    expect((await json(await req('/api/team', { cookie: first.cookie }))).team).toMatchObject({ role: 'admin', members: 2 });
  });

  it('does not promote anyone while another admin remains', async () => {
    const { admin, team } = await createTeam();
    const co = await addMember(admin.cookie);
    const m = await addMember(admin.cookie);
    await req(`/api/team/members/${co.user.id}`, { method: 'PATCH', cookie: admin.cookie, json: { role: 'admin' } });
    await db.delete(users).where(eq(users.id, admin.user.id));
    const [row] = await db.select().from(orgMembers).where(and(eq(orgMembers.orgId, team.id), eq(orgMembers.userId, m.user.id)));
    expect(row!.role).toBe('member');
  });

  it('promotes the longest-standing active member when the only admin is disabled', async () => {
    const { admin, team } = await createTeam();
    const inactive = await addMember(admin.cookie);
    const active = await addMember(admin.cookie);
    await db.update(users).set({ disabled: true }).where(eq(users.id, inactive.user.id));
    await db.update(users).set({ disabled: true }).where(eq(users.id, admin.user.id));
    const roles = await db.select({ userId: orgMembers.userId, role: orgMembers.role }).from(orgMembers).where(eq(orgMembers.orgId, team.id));
    expect(Object.fromEntries(roles.map((r) => [r.userId, r.role]))).toEqual({ [admin.user.id]: 'admin', [inactive.user.id]: 'member', [active.user.id]: 'admin' });
  });

  it('deletes a team (library, invites) when its last member is deleted', async () => {
    const { admin, team } = await createTeam();
    await addEntry(admin.cookie, 'dictionary', { phrase: 'Orphan', replacement: null });
    await newInvite(admin.cookie);
    await db.delete(users).where(eq(users.id, admin.user.id));
    expect(await db.select().from(orgs).where(eq(orgs.id, team.id))).toEqual([]);
    expect(await db.select().from(orgLibrary).where(eq(orgLibrary.orgId, team.id))).toEqual([]);
    expect(await db.select().from(orgInvites).where(eq(orgInvites.orgId, team.id))).toEqual([]);
  });

  it('deletes the team when its last two members are deleted concurrently', async () => {
    const { admin, team } = await createTeam();
    const m = await addMember(admin.cookie);
    // Hold the first deletion open so the second runs while the first member still looks present.
    let second: Promise<unknown> = Promise.resolve();
    await sql.begin(async (tx) => {
      await tx`delete from users where id = ${admin.user.id}`;
      second = db.delete(users).where(eq(users.id, m.user.id)).execute();
      await new Promise((r) => setTimeout(r, 300));
    });
    await second;
    expect(await db.select().from(orgMembers).where(eq(orgMembers.orgId, team.id))).toEqual([]);
    expect(await db.select().from(orgs).where(eq(orgs.id, team.id))).toEqual([]);
  });
});
