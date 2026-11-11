// Default catalog (inserted when missing on every migrate) and an optional admin from env.
import { eq, sql as q } from 'drizzle-orm';
import { adminError, config } from '../config.js';
import { pathToFileURL } from 'node:url';
import { hashPassword } from '../lib/crypto.js';
import { db, sql } from './index.js';
import { apiTokens, deviceCodes, models, orgMembers, sessions, users } from './schema.js';

type NewModel = typeof models.$inferInsert;
const perHour = (usd: number) => usd / 60;

/** Prices as published by each provider (USD). price_per_min is our reference charge. */
export const DEFAULT_MODELS: NewModel[] = [
  // Speech-to-text
  { id: 'groq/whisper-large-v3-turbo', kind: 'stt', provider: 'groq', upstreamModel: 'whisper-large-v3-turbo', displayName: 'Groq Whisper Large v3 Turbo', costPerMinUsd: perHour(0.04), fallbackOrder: 10 },
  { id: 'deepgram/nova-3', kind: 'stt', provider: 'deepgram', upstreamModel: 'nova-3', displayName: 'Deepgram Nova-3', streaming: true, costPerMinUsd: 0.0077, fallbackOrder: 15 },
  { id: 'openai/gpt-4o-mini-transcribe', kind: 'stt', provider: 'openai', upstreamModel: 'gpt-4o-mini-transcribe', displayName: 'OpenAI GPT-4o mini Transcribe', streaming: true, costPerMinUsd: 0.003, fallbackOrder: 20 },
  { id: 'openai/gpt-4o-transcribe', kind: 'stt', provider: 'openai', upstreamModel: 'gpt-4o-transcribe', displayName: 'OpenAI GPT-4o Transcribe', streaming: true, costPerMinUsd: 0.006, fallbackOrder: 30 },
  { id: 'assemblyai/universal', kind: 'stt', provider: 'assemblyai', upstreamModel: 'universal', displayName: 'AssemblyAI Universal', streaming: true, costPerMinUsd: perHour(0.15), fallbackOrder: 40 },
  { id: 'elevenlabs/scribe_v1', kind: 'stt', provider: 'elevenlabs', upstreamModel: 'scribe_v1', displayName: 'ElevenLabs Scribe v1', costPerMinUsd: perHour(0.22), fallbackOrder: 50 },
  { id: 'openai/whisper-1', kind: 'stt', provider: 'openai', upstreamModel: 'whisper-1', displayName: 'OpenAI Whisper', costPerMinUsd: 0.006, fallbackOrder: 60 },
  { id: 'google/latest_short', kind: 'stt', provider: 'google', upstreamModel: 'latest_short', displayName: 'Google Cloud Speech (short)', costPerMinUsd: 0.016, fallbackOrder: 70 },
  { id: 'azure/short-audio', kind: 'stt', provider: 'azure', upstreamModel: 'conversation', displayName: 'Azure Speech (short audio)', costPerMinUsd: perHour(1), fallbackOrder: 80 },
  { id: 'local/moonshine-tiny-en', kind: 'stt', provider: 'local', upstreamModel: 'sherpa-onnx-moonshine-tiny-en-int8', displayName: 'Moonshine Tiny (self-hosted)', costPerMinUsd: 0, pricePerMinUsd: 0, fallbackOrder: 90 },
  // LLMs (USD per 1M tokens)
  { id: 'openai/gpt-4o-mini', kind: 'llm', provider: 'openai', upstreamModel: 'gpt-4o-mini', displayName: 'OpenAI GPT-4o mini', streaming: true, inputPer1mUsd: 0.15, outputPer1mUsd: 0.6, fallbackOrder: 10 },
  { id: 'groq/llama-3.3-70b-versatile', kind: 'llm', provider: 'groq', upstreamModel: 'llama-3.3-70b-versatile', displayName: 'Groq Llama 3.3 70B', streaming: true, inputPer1mUsd: 0.59, outputPer1mUsd: 0.79, fallbackOrder: 20 },
  { id: 'anthropic/claude-haiku-4-5', kind: 'llm', provider: 'anthropic', upstreamModel: 'claude-haiku-4-5', displayName: 'Claude Haiku 4.5', streaming: true, inputPer1mUsd: 1, outputPer1mUsd: 5, fallbackOrder: 30 },
].map((m) => ({ pricePerMinUsd: m.kind === 'stt' ? 0.02 : 0, ...m }) as NewModel);

export async function seedCatalog() {
  await db.insert(models).values(DEFAULT_MODELS).onConflictDoNothing();
}

/**
 * Creates (or promotes) the admin given by ADMIN_EMAIL / ADMIN_PASSWORD. Signup emails are unverified,
 * so an existing account may have been registered by someone else: promoting it resets its password,
 * revokes its sessions, API tokens and device sign-ins, and takes it out of its team.
 */
export async function seedAdmin(email = config.adminEmail, password = config.adminPassword, opts: { onlyIfNoAdmin?: boolean } = {}): Promise<string | null> {
  if (!email || !password) throw new Error('Set ADMIN_EMAIL and ADMIN_PASSWORD (an existing account gets this password)');
  const invalid = adminError(email, password);
  if (invalid) throw new Error(invalid);
  const normalized = email.trim().toLowerCase();
  const passwordHash = await hashPassword(password);
  // A failed query's message lists its parameters, which include the password hash: report only the code.
  return db.transaction(async (tx) => {
    // The signup lock: no account can be created with this email while we look.
    await tx.execute(q`select pg_advisory_xact_lock(42)`);
    if (opts.onlyIfNoAdmin) {
      const [admin] = await tx.select({ id: users.id }).from(users).where(eq(users.role, 'admin')).limit(1);
      if (admin) return null;
    }
    const [existing] = await tx.select({ id: users.id }).from(users).where(eq(users.email, normalized)).for('update');
    if (existing) {
      await tx.update(users).set({ role: 'admin', passwordHash }).where(eq(users.id, existing.id));
      await tx.delete(sessions).where(eq(sessions.userId, existing.id));
      await tx.delete(apiTokens).where(eq(apiTokens.userId, existing.id));
      await tx.delete(deviceCodes).where(eq(deviceCodes.userId, existing.id));
      await tx.delete(orgMembers).where(eq(orgMembers.userId, existing.id));
      return 'promoted (password reset; sessions, API tokens, device sign-ins and team membership removed)';
    }
    await tx.insert(users).values({ email: normalized, passwordHash, role: 'admin' });
    return 'created';
  }).catch((e: { cause?: { code?: string }; code?: string }) => {
    throw new Error(`Could not create or promote the admin account (database error ${e.cause?.code ?? e.code ?? 'unknown'})`);
  });
}

/** At startup: with ADMIN_EMAIL and ADMIN_PASSWORD set and no admin yet, creates or promotes that account. */
export async function bootstrapAdmin(email = config.adminEmail, password = config.adminPassword): Promise<string | null> {
  if (!email || !password) return null;
  return seedAdmin(email, password, { onlyIfNoAdmin: true });
}

if (import.meta.url === pathToFileURL(process.argv[1] ?? '').href) {
  Promise.all([seedCatalog(), seedAdmin()])
    .then(([, r]) => console.log(`catalog seeded; admin ${r}`))
    .catch((e) => {
      console.error(e.message);
      process.exitCode = 1;
    })
    .finally(() => sql.end());
}
