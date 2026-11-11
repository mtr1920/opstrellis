import { randomUUID } from 'node:crypto';
import { createApp } from '../src/app.js';
import { db, sql } from '../src/db/index.js';
import { seedCatalog } from '../src/db/seed.js';

export const app = createApp();
export const ORIGIN = 'http://localhost:8787';

interface Opts {
  method?: string;
  json?: unknown;
  body?: BodyInit;
  cookie?: string;
  token?: string;
  headers?: Record<string, string>;
  ip?: string;
}

/** Calls the app in-process. Each call gets a fresh client IP unless one is given (rate limits). */
export function req(path: string, o: Opts = {}) {
  // Like a browser on the dashboard, every request carries our Origin unless a test overrides it ('' = none).
  const headers: Record<string, string> = { 'x-forwarded-for': o.ip ?? `10.${rnd()}.${rnd()}.${rnd()}`, origin: ORIGIN, ...o.headers };
  if (o.cookie) headers.cookie = o.cookie;
  if (o.token) headers.authorization = `Bearer ${o.token}`;
  let body = o.body;
  if (o.json !== undefined) {
    body = JSON.stringify(o.json);
    headers['content-type'] = 'application/json';
  }
  return app.request(path, { method: o.method ?? (body ? 'POST' : 'GET'), headers, body });
}
const rnd = () => Math.floor(Math.random() * 250);

export function cookieFrom(res: Response): string {
  const set = res.headers.get('set-cookie') ?? '';
  return set.split(';')[0]!;
}

export async function signup(email = `u-${randomUUID()}@example.com`, password = 'correct horse battery') {
  const res = await req('/api/auth/signup', { json: { email, password, name: 'Test' } });
  if (res.status !== 201) throw new Error(`signup failed: ${res.status} ${await res.text()}`);
  const { user } = (await res.json()) as { user: { id: string; email: string; role: string } };
  return { cookie: cookieFrom(res), user, email, password };
}

export async function createToken(cookie: string, name = 'test') {
  const res = await req('/api/tokens', { cookie, json: { name } });
  return ((await res.json()) as { token: string }).token;
}

/** A signed-up user with an API token. */
export async function userWithToken(email?: string) {
  const s = await signup(email);
  return { ...s, token: await createToken(s.cookie) };
}

export async function resetDb() {
  await sql`truncate users, sessions, api_tokens, device_codes, usage_events, models, provider_keys, orgs cascade`;
  await seedCatalog();
}

export { db, sql };

// ---------- hand-built WAV files ----------

export function riffChunk(id: string, body: Buffer) {
  const head = Buffer.alloc(8);
  head.write(id, 0, 'ascii');
  head.writeUInt32LE(body.length, 4);
  return Buffer.concat([head, body, Buffer.alloc(body.length % 2)]);
}

export function fmtChunk(sampleRate: number, channels = 1, bits = 16, format = 1) {
  const b = Buffer.alloc(16);
  b.writeUInt16LE(format, 0);
  b.writeUInt16LE(channels, 2);
  b.writeUInt32LE(sampleRate, 4);
  b.writeUInt32LE(((sampleRate * channels * bits) / 8) % 2 ** 32, 8);
  b.writeUInt16LE((channels * bits) / 8, 12);
  b.writeUInt16LE(bits, 14);
  return riffChunk('fmt ', b);
}

export function riffWave(...chunks: Buffer[]) {
  return Buffer.concat([Buffer.from('RIFF'), Buffer.alloc(4), Buffer.from('WAVE'), ...chunks]);
}
