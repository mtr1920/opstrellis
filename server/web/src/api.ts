// Tiny fetch wrapper for the same-origin API (session cookie auth).
export class HttpError extends Error {
  constructor(
    public status: number,
    public code: string,
    message: string,
  ) {
    super(message);
  }
}

export async function api<T = any>(path: string, opts: { method?: string; json?: unknown } = {}): Promise<T> {
  const res = await fetch(path, {
    method: opts.method ?? (opts.json !== undefined ? 'POST' : 'GET'),
    headers: opts.json !== undefined ? { 'content-type': 'application/json' } : undefined,
    body: opts.json !== undefined ? JSON.stringify(opts.json) : undefined,
    credentials: 'same-origin',
  });
  const body = res.headers.get('content-type')?.includes('json') ? await res.json() : null;
  if (!res.ok) throw new HttpError(res.status, body?.error ?? 'error', body?.message ?? body?.error ?? res.statusText);
  return body as T;
}

export interface Me {
  user: { id: string; email: string; name: string; role: 'user' | 'admin' };
  plan: 'free' | 'pro';
  plan_status: string;
  period: { start: string; end: string };
  usage: { managed_minutes: number; included_minutes: number; overage_minutes: number; remaining_minutes: number; words: number; cost_usd: number };
  limits: { included_minutes: number; overage_per_min_usd: number; max_overage_minutes: number; llm_tokens: number };
  preferred_model: string | null;
  preferred_llm: string | null;
}

/** Money amount with a fixed number of decimals; tiny non-zero amounts get 4 so they don't read as $0.00. */
export const usd = (n: number, digits = 2) => {
  const d = n !== 0 && Math.abs(n) < 0.01 ? Math.max(digits, 4) : digits;
  return `$${n.toLocaleString(undefined, { minimumFractionDigits: d, maximumFractionDigits: d })}`;
};
/** Unit price: 2 to 4 decimals, e.g. $0.02, $0.0077, $0.016. */
export const price = (n: number) => `$${n.toLocaleString(undefined, { minimumFractionDigits: 2, maximumFractionDigits: 4 })}`;
export const num = (n: number, digits = 0) => n.toLocaleString(undefined, { maximumFractionDigits: digits });
/** Minutes with exactly one decimal, so table columns line up. */
export const mins = (n: number) => n.toLocaleString(undefined, { minimumFractionDigits: 1, maximumFractionDigits: 1 });
export const date = (s: string | null) => (s ? new Date(s).toLocaleDateString(undefined, { month: 'short', day: 'numeric', year: 'numeric' }) : '—');
