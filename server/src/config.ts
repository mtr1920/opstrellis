// All configuration comes from the environment. Read once at startup.
import { emailSchema, passwordSchema } from './lib/http.js';

const env = process.env;

/** Environment values that could not be parsed; the server refuses to start while any exist. */
const invalidEnv: string[] = [];

export function parseBool(raw: string): boolean | null {
  const v = raw.trim().toLowerCase();
  if (['true', '1', 'yes', 'on'].includes(v)) return true;
  if (['false', '0', 'no', 'off'].includes(v)) return false;
  return null;
}

function boolEnv(name: string, fallback: boolean): boolean {
  const raw = env[name];
  if (!raw?.trim()) return fallback;
  const v = parseBool(raw);
  if (v === null) invalidEnv.push(`${name} must be true/false, 1/0, yes/no or on/off`);
  return v ?? fallback;
}

/** A number of proxies, or true/false (1/0). */
function trustProxyEnv(): number {
  const raw = env.TRUST_PROXY?.trim() ?? '';
  if (/^\d+$/.test(raw)) return Number(raw);
  const flag = raw ? parseBool(raw) : false;
  if (flag === null) invalidEnv.push('TRUST_PROXY must be the number of proxies in front of the server (or true/false)');
  return flag ? 1 : 0;
}

function intEnv(name: string, fallback: number): number {
  const raw = env[name];
  if (!raw?.trim()) return fallback;
  if (!/^\d+$/.test(raw.trim())) {
    invalidEnv.push(`${name} must be a non-negative integer`);
    return fallback;
  }
  return Number(raw.trim());
}

export const config = {
  port: Number(env.PORT ?? 8787),
  databaseUrl: env.DATABASE_URL ?? 'postgres://postgres:postgres@localhost:5432/opstrellis',
  publicUrl: (env.PUBLIC_URL ?? 'http://localhost:8787').replace(/\/$/, ''),
  /** Number of reverse proxies in front of us whose X-Forwarded-For entries we trust (0 = none). */
  trustProxy: trustProxyEnv(),
  /** Secret used to encrypt upstream provider keys at rest (hashed to 32 bytes; use 32+ random characters). */
  masterKey: env.MASTER_KEY ?? '',
  /** The admin created (or promoted) at startup and by `npm run db:seed`; when set, no signup becomes admin. */
  adminEmail: (env.ADMIN_EMAIL ?? '').trim().toLowerCase(),
  adminPassword: env.ADMIN_PASSWORD ?? '',
  allowSignup: boolEnv('ALLOW_SIGNUP', true),
  /** Accounts that may be created in any rolling 24 hours, across all clients. */
  maxSignupsPerDay: intEnv('MAX_SIGNUPS_PER_DAY', 200),
  /** Pro overage minutes allowed per billing period; 0 disables overage. */
  maxOverageMinutes: intEnv('MAX_OVERAGE_MINUTES', 3000),
  localModelDir: env.LOCAL_MODEL_DIR ?? 'models',
  webDir: env.WEB_DIR ?? 'dist/web',
  stripe: {
    secretKey: env.STRIPE_SECRET_KEY ?? '',
    webhookSecret: env.STRIPE_WEBHOOK_SECRET ?? '',
    pricePro: env.STRIPE_PRICE_PRO ?? '',
    priceOverage: env.STRIPE_PRICE_OVERAGE ?? '',
    meterEvent: env.STRIPE_METER_EVENT ?? 'opstrellis_managed_minutes',
  },
  maxAudioBytes: 25 * 1024 * 1024,
  /** How long an early-rejected upload is read and dropped before the answer goes out anyway. */
  uploadDrainMs: 30_000,
  sessionDays: 30,
};

export const stripeEnabled = () => Boolean(config.stripe.secretKey);

export const MASTER_KEY_PLACEHOLDER = 'change-me-to-a-long-random-string';

/** ADMIN_EMAIL / ADMIN_PASSWORD must pass the signup rules, or the admin could never sign in. Never echoes the password. */
export function adminError(email: string, password: string): string | null {
  if (email && !emailSchema.safeParse(email).success) return 'ADMIN_EMAIL is not a valid email address';
  if (password && !passwordSchema.safeParse(password).success) return 'ADMIN_PASSWORD must be 8 to 200 characters';
  return null;
}

type Checked = { masterKey: string; invalidEnv?: string[]; adminEmail?: string; adminPassword?: string };

/** Why the server must not start with this configuration, or null. An unset MASTER_KEY only disables saving provider keys. */
export function configError(c: Checked = { masterKey: config.masterKey, invalidEnv, adminEmail: config.adminEmail, adminPassword: config.adminPassword }): string | null {
  if (c.invalidEnv?.length) return c.invalidEnv.join('; ');
  const admin = adminError(c.adminEmail ?? '', c.adminPassword ?? '');
  if (admin) return admin;
  if (c.masterKey === MASTER_KEY_PLACEHOLDER) return 'MASTER_KEY is still the placeholder from .env.example; set a long random string (e.g. `openssl rand -base64 32`)';
  return null;
}

/** Problems worth a startup warning. A short MASTER_KEY is not refused: changing it would orphan the provider keys it encrypts. */
export function configWarnings(c: Checked = { masterKey: config.masterKey }): string[] {
  return c.masterKey && c.masterKey.length < 32 ? ['MASTER_KEY is shorter than 32 characters; a long random key is much harder to guess'] : [];
}
