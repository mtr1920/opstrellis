// Teams: membership, invite codes, and the shared library that every member's desktop app syncs.
import { and, asc, count, desc, eq, gt, lte, ne, sql } from 'drizzle-orm';
import { Hono } from 'hono';
import { z } from 'zod';
import { limitKey, rateLimit, requireAuth, type AppEnv } from '../auth.js';
import { db } from '../db/index.js';
import { orgInvites, orgLibrary, orgMembers, orgs, users } from '../db/schema.js';
import { randomToken, sha256 } from '../lib/crypto.js';
import { ApiError, iso, parse, readJson, storableText } from '../lib/http.js';

export const team = new Hono<AppEnv>();

export const INVITE_TTL_MS = 7 * 86_400_000;
export const MAX_LIBRARY_ENTRIES = 2000;
/** Rows per GET /api/library response. */
export const SYNC_PAGE_SIZE = 500;
/** Tombstones older than this are purged; a desktop that has not synced for longer starts over. */
export const TOMBSTONE_TTL_MS = 30 * 86_400_000;

type Tx = Parameters<Parameters<typeof db.transaction>[0]>[0];
type Kind = (typeof orgLibrary.$inferSelect)['kind'];

/** Live library bytes per team (UTF-8 of JSON-serialised `data`): a full sync stays far below the desktop's 16 MB cap. */
export const MAX_LIBRARY_BYTES = 6 * 1024 * 1024;
const dataSize = (data: Record<string, unknown>) => Buffer.byteLength(JSON.stringify(data));

const text = (max: number) => storableText().trim().min(1).max(max);
// The same rules the desktop applies to synced entries, so an admin sees the error instead of desktops skipping the entry.
const CONTROL = /\p{Cc}/u;
const CONTROL_BUT_NEWLINE_TAB = /[^\P{Cc}\n\t]/u;
const line = (max: number) => text(max).refine((s) => !CONTROL.test(s), 'Must not contain control characters (such as line breaks)');
const multiline = (max: number) => text(max).refine((s) => !CONTROL_BUT_NEWLINE_TAB.test(s), 'Must not contain control characters other than line breaks and tabs');
const DATA: Record<Kind, z.ZodType<Record<string, unknown>>> = {
  dictionary: z.strictObject({ phrase: line(100), replacement: line(200).nullable() }),
  snippet: z.strictObject({
    trigger: line(100),
    // Desktops refuse team snippets that would type the user's clipboard wherever someone says the trigger.
    expansion: multiline(5000).refine((s) => !s.includes('{clipboard}'), 'Team snippets cannot use the {clipboard} placeholder'),
  }),
  action: z.strictObject({ name: line(60), prompt: multiline(2000), output: z.enum(['replace', 'copy']) }),
};
/** Live entries of a kind are unique by this field, compared as `libraryKey()`. */
const KEY: Record<Kind, string> = { dictionary: 'phrase', snippet: 'trigger', action: 'name' };

/** Case- and compatibility-insensitive form of an entry's unique field, computed here so it does not depend on the database collation. */
export const libraryKey = (kind: Kind, data: Record<string, unknown>) => String(data[KEY[kind]]).normalize('NFKC').toLowerCase().trim();
const KINDS = Object.keys(DATA) as [Kind, ...Kind[]];

const sessionOnly = requireAuth({ sessionOnly: true });

// ---------- membership helpers: the team is always the caller's own, from their membership row ----------

async function membership(userId: string) {
  const [m] = await db.select({ orgId: orgMembers.orgId, role: orgMembers.role }).from(orgMembers).where(eq(orgMembers.userId, userId));
  return m ?? null;
}

function check(m: { orgId: string; role: 'admin' | 'member' } | null | undefined, admin: boolean) {
  if (!m) throw new ApiError(404, 'no_team', 'You are not in a team');
  if (admin && m.role !== 'admin') throw new ApiError(403, 'forbidden', 'Team admins only');
  return m;
}

const requireMember = async (userId: string, admin = false) => check(await membership(userId), admin);

/**
 * The caller's membership, read after locking their team row for the transaction. Every membership
 * change and library write goes through here, so they serialise per team and a role check cannot
 * race a concurrent demotion. Team routes lock the team row before any member, invite or entry row
 * (join takes it FOR SHARE), so they cannot deadlock with each other.
 */
async function lockTeam(tx: Tx, userId: string, admin = false) {
  const [locked] = await tx
    .select({ id: orgs.id })
    .from(orgs)
    .where(eq(orgs.id, sql`(select ${orgMembers.orgId} from ${orgMembers} where ${orgMembers.userId} = ${userId})`))
    .for('update');
  if (!locked) return check(null, admin);
  const [m] = await tx
    .select({ orgId: orgMembers.orgId, role: orgMembers.role })
    .from(orgMembers)
    .where(and(eq(orgMembers.userId, userId), eq(orgMembers.orgId, locked.id)));
  return check(m, admin);
}

async function teamJson(orgId: string, role: 'admin' | 'member') {
  const [row] = await db
    .select({ id: orgs.id, name: orgs.name, libraryVersion: orgs.libraryVersion, members: count(orgMembers.userId) })
    .from(orgs)
    .innerJoin(orgMembers, eq(orgMembers.orgId, orgs.id))
    .where(eq(orgs.id, orgId))
    .groupBy(orgs.id);
  if (!row) throw new ApiError(404, 'no_team', 'You are not in a team');
  return { id: row.id, name: row.name, role, members: row.members, libraryVersion: row.libraryVersion };
}

/** 404 for anything that is not a uuid, so a malformed id looks like a missing one. */
function uuidParam(value: string) {
  const r = z.uuid().safeParse(value);
  if (!r.success) throw new ApiError(404, 'not_found');
  return r.data;
}

/** Active (not disabled) admins of the team other than `userId`. */
async function otherActiveAdmins(tx: Tx, orgId: string, userId: string) {
  const [row] = await tx
    .select({ n: count() })
    .from(orgMembers)
    .innerJoin(users, eq(users.id, orgMembers.userId))
    .where(and(eq(orgMembers.orgId, orgId), eq(orgMembers.role, 'admin'), eq(users.disabled, false), ne(orgMembers.userId, userId)));
  return row?.n ?? 0;
}

async function assertNotLastAdmin(tx: Tx, orgId: string, userId: string) {
  if (!(await otherActiveAdmins(tx, orgId, userId))) throw new ApiError(409, 'last_admin', 'A team needs at least one admin. Make another member an admin first');
}

// ---------- team ----------

team.get('/team', requireAuth(), async (c) => {
  const m = await membership(c.get('user').id);
  return c.json({ team: m ? await teamJson(m.orgId, m.role) : null });
});

team.post('/team', sessionOnly, async (c) => {
  const { name } = await readJson(c, z.object({ name: text(80) }));
  const userId = c.get('user').id;
  const orgId = await db.transaction(async (tx) => {
    const [org] = await tx.insert(orgs).values({ name }).returning({ id: orgs.id });
    const joined = await tx.insert(orgMembers).values({ userId, orgId: org!.id, role: 'admin' }).onConflictDoNothing().returning();
    if (!joined.length) throw new ApiError(409, 'already_in_team', 'You are already in a team. Leave it first');
    return org!.id;
  });
  return c.json({ team: await teamJson(orgId, 'admin') }, 201);
});

const INVALID_CODE = () => new ApiError(404, 'invalid_code', 'This invite code is invalid or has expired');

/** One indexed lookup by hash whatever the code; unknown, expired and revoked codes share one answer. */
const findInvite = (tx: Tx | typeof db, codeHash: string, orgId?: string) =>
  tx
    .select({ orgId: orgInvites.orgId })
    .from(orgInvites)
    .where(and(eq(orgInvites.codeHash, codeHash), gt(orgInvites.expiresAt, new Date()), orgId ? eq(orgInvites.orgId, orgId) : undefined));

// Keyed by user after the session check, so anonymous clients sharing an IP cannot lock others out.
team.post('/team/join', sessionOnly, rateLimit('team-join', 10, 60_000, (c) => c.get('user').id), async (c) => {
  const { code } = await readJson(c, z.object({ code: z.string().trim().min(1).max(200) }));
  const userId = c.get('user').id;
  if (await membership(userId)) throw new ApiError(409, 'already_in_team', 'You are already in a team. Leave it first');
  const codeHash = sha256(code);
  const [found] = await findInvite(db, codeHash);
  if (!found) throw INVALID_CODE();
  const orgId = await db.transaction(async (tx) => {
    // Lock the team row (shared, so joins run in parallel) the way revocation and team deletion do,
    // then re-read the invite: a join that waited behind a revoke or a deletion sees it gone.
    const [org] = await tx.select({ id: orgs.id }).from(orgs).where(eq(orgs.id, found.orgId)).for('share');
    if (!org) throw INVALID_CODE();
    const [invite] = await findInvite(tx, codeHash, org.id);
    if (!invite) throw INVALID_CODE();
    const joined = await tx.insert(orgMembers).values({ userId, orgId: org.id, role: 'member' }).onConflictDoNothing().returning();
    if (!joined.length) throw new ApiError(409, 'already_in_team', 'You are already in a team. Leave it first');
    return org.id;
  });
  return c.json({ team: await teamJson(orgId, 'member') });
});

team.post('/team/leave', sessionOnly, async (c) => {
  const userId = c.get('user').id;
  await db.transaction(async (tx) => {
    const m = await lockTeam(tx, userId);
    if (m.role === 'admin') {
      const [row] = await tx.select({ members: count() }).from(orgMembers).where(eq(orgMembers.orgId, m.orgId));
      if (row!.members > 1 && !(await otherActiveAdmins(tx, m.orgId, userId))) {
        throw new ApiError(409, 'last_admin', 'You are the only admin. Make another member an admin before leaving');
      }
    }
    // Codes this user made must not keep working after they are gone.
    await tx.delete(orgInvites).where(and(eq(orgInvites.orgId, m.orgId), eq(orgInvites.createdBy, userId)));
    // The org_members_drop_empty_org trigger deletes the team when this was its only member.
    await tx.delete(orgMembers).where(eq(orgMembers.userId, userId));
  });
  return c.json({ ok: true });
});

// ---------- members ----------

const memberColumns = { id: users.id, email: users.email, name: users.name, role: orgMembers.role };

team.get('/team/members', sessionOnly, async (c) => {
  const m = await requireMember(c.get('user').id);
  const members = await db
    .select(memberColumns)
    .from(orgMembers)
    .innerJoin(users, eq(users.id, orgMembers.userId))
    .where(eq(orgMembers.orgId, m.orgId))
    .orderBy(asc(orgMembers.role), asc(orgMembers.createdAt));
  return c.json({ members });
});

/** A member of the caller's own team, else 404 (also for valid ids from other teams). */
async function targetMember(tx: Tx, orgId: string, userId: string) {
  const [t] = await tx
    .select({ role: orgMembers.role })
    .from(orgMembers)
    .where(and(eq(orgMembers.userId, userId), eq(orgMembers.orgId, orgId)));
  if (!t) throw new ApiError(404, 'not_found');
  return t;
}

team.patch('/team/members/:userId', sessionOnly, async (c) => {
  const targetId = uuidParam(c.req.param('userId'));
  const { role } = await readJson(c, z.object({ role: z.enum(['admin', 'member']) }));
  const member = await db.transaction(async (tx) => {
    const m = await lockTeam(tx, c.get('user').id, true);
    const t = await targetMember(tx, m.orgId, targetId);
    if (t.role === 'admin' && role === 'member') {
      await assertNotLastAdmin(tx, m.orgId, targetId);
      await tx.delete(orgInvites).where(and(eq(orgInvites.orgId, m.orgId), eq(orgInvites.createdBy, targetId)));
    }
    await tx.update(orgMembers).set({ role }).where(and(eq(orgMembers.userId, targetId), eq(orgMembers.orgId, m.orgId)));
    const [row] = await tx.select(memberColumns).from(orgMembers).innerJoin(users, eq(users.id, orgMembers.userId)).where(eq(orgMembers.userId, targetId));
    return row!;
  });
  return c.json({ member });
});

team.delete('/team/members/:userId', sessionOnly, async (c) => {
  const targetId = uuidParam(c.req.param('userId'));
  await db.transaction(async (tx) => {
    const m = await lockTeam(tx, c.get('user').id, true);
    const t = await targetMember(tx, m.orgId, targetId);
    if (t.role === 'admin') await assertNotLastAdmin(tx, m.orgId, targetId);
    // Invite codes are multi-use: any of them would let the removed member straight back in.
    await tx.delete(orgInvites).where(eq(orgInvites.orgId, m.orgId));
    await tx.delete(orgMembers).where(and(eq(orgMembers.userId, targetId), eq(orgMembers.orgId, m.orgId)));
  });
  return c.json({ ok: true });
});

// ---------- invites ----------

team.post('/team/invites', sessionOnly, async (c) => {
  const userId = c.get('user').id;
  const code = randomToken(16); // 128 bits, base64url
  const row = await db.transaction(async (tx) => {
    const m = await lockTeam(tx, userId, true);
    const [r] = await tx
      .insert(orgInvites)
      .values({ orgId: m.orgId, codeHash: sha256(code), expiresAt: new Date(Date.now() + INVITE_TTL_MS), createdBy: userId })
      .returning();
    return r!;
  });
  return c.json({ id: row.id, code, expiresAt: iso(row.expiresAt) }, 201);
});

team.get('/team/invites', sessionOnly, async (c) => {
  const m = await requireMember(c.get('user').id, true);
  const rows = await db
    .select({ id: orgInvites.id, expiresAt: orgInvites.expiresAt, createdAt: orgInvites.createdAt })
    .from(orgInvites)
    .where(and(eq(orgInvites.orgId, m.orgId), gt(orgInvites.expiresAt, new Date())))
    .orderBy(desc(orgInvites.createdAt));
  return c.json({ invites: rows.map((r) => ({ id: r.id, expiresAt: iso(r.expiresAt), createdAt: iso(r.createdAt) })) });
});

team.delete('/team/invites/:id', sessionOnly, async (c) => {
  const id = uuidParam(c.req.param('id'));
  const deleted = await db.transaction(async (tx) => {
    const m = await lockTeam(tx, c.get('user').id, true);
    return tx
      .delete(orgInvites)
      .where(and(eq(orgInvites.id, id), eq(orgInvites.orgId, m.orgId)))
      .returning({ id: orgInvites.id });
  });
  if (!deleted.length) throw new ApiError(404, 'not_found');
  return c.json({ ok: true });
});

// ---------- library (edited in the dashboard by team admins) ----------

const entryJson = (e: typeof orgLibrary.$inferSelect) => ({ id: e.id, kind: e.kind, data: e.data, updatedAt: iso(e.updatedAt) });

/** Next library version; the caller holds the team row lock (lockTeam), so versions are distinct and commit in order. */
async function bumpVersion(tx: Tx, orgId: string) {
  const [row] = await tx
    .update(orgs)
    .set({ libraryVersion: sql`${orgs.libraryVersion} + 1` })
    .where(eq(orgs.id, orgId))
    .returning({ v: orgs.libraryVersion });
  return row!.v;
}

/** Maps a violation of the live-key unique index to 409; anything else is rethrown. */
function duplicateAs409(kind: Kind) {
  return (e: unknown): never => {
    const cause = (e as { cause?: { code?: string; constraint_name?: string } }).cause ?? (e as { code?: string; constraint_name?: string });
    if (cause?.code === '23505' && cause.constraint_name === 'org_library_live_key_idx') {
      throw new ApiError(409, 'duplicate', `An entry with this ${KEY[kind]} already exists`);
    }
    throw e;
  };
}

/** 409 when the team's live entries, with `adding` bytes more (and without `exceptId`), would exceed MAX_LIBRARY_BYTES. */
async function assertWithinBudget(tx: Tx, orgId: string, adding: number, exceptId?: string) {
  const [row] = await tx
    .select({ bytes: sql<number>`coalesce(sum(${orgLibrary.size}), 0)::float8` })
    .from(orgLibrary)
    .where(and(eq(orgLibrary.orgId, orgId), eq(orgLibrary.deleted, false), exceptId ? ne(orgLibrary.id, exceptId) : undefined));
  if ((row?.bytes ?? 0) + adding > MAX_LIBRARY_BYTES) {
    throw new ApiError(409, 'library_too_large', 'The team library is limited to 6 MB of text in total; shorten or delete entries first');
  }
}

async function liveEntry(tx: Tx, orgId: string, id: string) {
  const [e] = await tx
    .select()
    .from(orgLibrary)
    .where(and(eq(orgLibrary.id, id), eq(orgLibrary.orgId, orgId), eq(orgLibrary.deleted, false)));
  if (!e) throw new ApiError(404, 'not_found');
  return e;
}

team.get('/team/library', sessionOnly, async (c) => {
  const m = await requireMember(c.get('user').id);
  const [[org], entries] = await Promise.all([
    db.select({ version: orgs.libraryVersion }).from(orgs).where(eq(orgs.id, m.orgId)),
    db
      .select()
      .from(orgLibrary)
      .where(and(eq(orgLibrary.orgId, m.orgId), eq(orgLibrary.deleted, false)))
      .orderBy(asc(orgLibrary.version)),
  ]);
  return c.json({ version: org?.version ?? 0, entries: entries.map(entryJson) });
});

// Every write leaves a row (deletes a tombstone), so writes are limited per user.
const libraryWriteLimit = rateLimit('library-write', 60, 60_000, (c) => c.get('user').id);

team.post('/team/library', sessionOnly, libraryWriteLimit, async (c) => {
  const body = await readJson(c, z.object({ kind: z.enum(KINDS), data: z.unknown() }));
  const data = parse(DATA[body.kind], body.data);
  const userId = c.get('user').id;
  const entry = await db.transaction(async (tx) => {
    const m = await lockTeam(tx, userId, true);
    const [live] = await tx
      .select({ n: count() })
      .from(orgLibrary)
      .where(and(eq(orgLibrary.orgId, m.orgId), eq(orgLibrary.deleted, false)));
    if ((live?.n ?? 0) >= MAX_LIBRARY_ENTRIES) throw new ApiError(409, 'library_full', `A team library holds at most ${MAX_LIBRARY_ENTRIES} entries`);
    const size = dataSize(data);
    await assertWithinBudget(tx, m.orgId, size);
    const version = await bumpVersion(tx, m.orgId);
    const [row] = await tx
      .insert(orgLibrary)
      .values({ orgId: m.orgId, kind: body.kind, data, key: libraryKey(body.kind, data), size, version, updatedBy: userId })
      .returning()
      .catch(duplicateAs409(body.kind));
    return row!;
  });
  return c.json(entryJson(entry), 201);
});

team.put('/team/library/:id', sessionOnly, libraryWriteLimit, async (c) => {
  const id = uuidParam(c.req.param('id'));
  const body = await readJson(c, z.object({ data: z.unknown() }));
  const userId = c.get('user').id;
  const entry = await db.transaction(async (tx) => {
    const m = await lockTeam(tx, userId, true);
    const existing = await liveEntry(tx, m.orgId, id);
    const data = parse(DATA[existing.kind], body.data);
    const size = dataSize(data);
    await assertWithinBudget(tx, m.orgId, size, id);
    const version = await bumpVersion(tx, m.orgId);
    const [row] = await tx
      .update(orgLibrary)
      .set({ data, key: libraryKey(existing.kind, data), size, version, updatedAt: new Date(), updatedBy: userId })
      .where(eq(orgLibrary.id, id))
      .returning()
      .catch(duplicateAs409(existing.kind));
    return row!;
  });
  return c.json(entryJson(entry));
});

team.delete('/team/library/:id', sessionOnly, libraryWriteLimit, async (c) => {
  const id = uuidParam(c.req.param('id'));
  const userId = c.get('user').id;
  await db.transaction(async (tx) => {
    const m = await lockTeam(tx, userId, true);
    await liveEntry(tx, m.orgId, id);
    const version = await bumpVersion(tx, m.orgId);
    await tx
      .update(orgLibrary)
      .set({ data: {}, key: null, size: 0, deleted: true, version, updatedAt: new Date(), updatedBy: userId })
      .where(eq(orgLibrary.id, id));
  });
  return c.json({ ok: true });
});

// ---------- desktop sync ----------

const NO_TEAM = { team: null, version: 0, entries: [], more: false, reset: false };

/**
 * Library changes after `since`, oldest first. A full sync (since 0, or a `reset`) is one response with
 * every live entry (at most MAX_LIBRARY_ENTRIES) and the current version. An incremental sync includes
 * tombstones and is paged: with `more`, `version` is the last entry's and the client calls again with it.
 */
team.get('/library', requireAuth(), rateLimit('library', 60, 60_000, (c) => c.get('user').id), async (c) => {
  const q = parse(z.object({ since: z.string().regex(/^\d{1,15}$/, 'since must be a non-negative integer').optional() }), c.req.query());
  const requested = Number(q.since ?? 0);
  const m = await membership(c.get('user').id);
  if (!m) return c.json(NO_TEAM);
  // One snapshot for the team row and the entries, so the version, floor and rows agree with each other.
  const body = await db.transaction(
    async (tx) => {
      const [org] = await tx
        .select({ id: orgs.id, name: orgs.name, version: orgs.libraryVersion, floor: orgs.tombstoneFloor })
        .from(orgs)
        .where(eq(orgs.id, m.orgId));
      if (!org) return NO_TEAM;
      // Below the floor some deletes were purged; above the version the server lost writes (e.g. a restore).
      const reset = requested > 0 && (requested < org.floor || requested > org.version);
      const full = reset || requested === 0;
      // A full response can be large, so it has its own, tighter limit.
      if (full) limitKey(c, 'library-full', c.get('user').id, 10);
      const columns = { id: orgLibrary.id, kind: orgLibrary.kind, data: orgLibrary.data, deleted: orgLibrary.deleted, version: orgLibrary.version };
      const inTeam = and(eq(orgLibrary.orgId, org.id), lte(orgLibrary.version, org.version));
      const rows = full
        ? await tx.select(columns).from(orgLibrary).where(and(inTeam, eq(orgLibrary.deleted, false))).orderBy(asc(orgLibrary.version))
        : await tx
            .select(columns)
            .from(orgLibrary)
            .where(and(inTeam, gt(orgLibrary.version, requested)))
            .orderBy(asc(orgLibrary.version))
            .limit(SYNC_PAGE_SIZE + 1);
      const more = !full && rows.length > SYNC_PAGE_SIZE;
      const page = more ? rows.slice(0, SYNC_PAGE_SIZE) : rows;
      return {
        team: { id: org.id, name: org.name },
        version: more ? page[page.length - 1]!.version : org.version,
        entries: page.map(({ version: _, ...e }) => e),
        more,
        reset,
      };
    },
    { isolationLevel: 'repeatable read', accessMode: 'read only' },
  );
  return c.json(body);
});

/** Deletes tombstones older than TOMBSTONE_TTL_MS and raises each team's floor to the highest version purged. */
export async function purgeTombstones(now = new Date()) {
  await db.execute(sql`
    with purged as (
      delete from ${orgLibrary} where ${orgLibrary.deleted} and ${orgLibrary.updatedAt} < ${new Date(now.getTime() - TOMBSTONE_TTL_MS).toISOString()}
      returning ${orgLibrary.orgId} as org_id, ${orgLibrary.version} as version
    ), floors as (select org_id, max(version) as v from purged group by org_id)
    update ${orgs} set tombstone_floor = greatest(${orgs.tombstoneFloor}, floors.v) from floors where ${orgs.id} = floors.org_id`);
}
