import type { Context } from 'hono';
import { bodyLimit } from 'hono/body-limit';
import type { ContentfulStatusCode } from 'hono/utils/http-status';
import { z } from 'zod';

/** Thrown from handlers; rendered by the app's onError via apiError(). */
export class ApiError extends Error {
  constructor(
    public status: ContentfulStatusCode,
    public code: string,
    message?: string,
  ) {
    super(message ?? code);
  }
}

/** Errors in the shape each API family expects: OpenAI-style under /v1, flat elsewhere. */
export function apiError(c: Context, status: ContentfulStatusCode, code: string, message?: string) {
  if (c.req.path.startsWith('/v1/')) {
    return c.json({ error: { message: message ?? code, type: code, code } }, status);
  }
  return c.json(message ? { error: code, message } : { error: code }, status);
}

const UNPAIRED_SURROGATE = /[\uD800-\uDBFF](?![\uDC00-\uDFFF])|(?<![\uD800-\uDBFF])[\uDC00-\uDFFF]/;

/** A string Postgres can store: text and jsonb reject NUL, and jsonb rejects unpaired surrogates (a 500 otherwise). */
export const storableText = () =>
  z.string().refine((s) => !s.includes('\u0000') && !UNPAIRED_SURROGATE.test(s), 'Contains a NUL character or an unpaired surrogate');

export const emailSchema = z.string().trim().toLowerCase().pipe(z.email()).pipe(z.string().max(254));
export const passwordSchema = z.string().min(8, 'Password must be at least 8 characters').max(200);

export function parse<T extends z.ZodType>(schema: T, value: unknown): z.infer<T> {
  const r = schema.safeParse(value);
  if (!r.success) throw new ApiError(400, 'invalid_request', z.prettifyError(r.error));
  return r.data;
}

/** JSON request bodies only: a cross-site HTML form can send text/plain without a preflight, never application/json. */
export async function readJson<T extends z.ZodType>(c: Context, schema: T): Promise<z.infer<T>> {
  const text = await c.req.text();
  const type = c.req.header('content-type')?.split(';')[0]!.trim().toLowerCase();
  if (text && type !== 'application/json') throw new ApiError(415, 'unsupported_media_type', 'Send the body as application/json');
  let raw: unknown;
  try {
    raw = text ? JSON.parse(text) : {};
  } catch {
    throw new ApiError(400, 'invalid_json', 'Body must be JSON');
  }
  return parse(schema, raw);
}

/** Request body size limit that answers 413 in the API's error shape. */
export const jsonBodyLimit = (maxSize: number) =>
  bodyLimit({ maxSize, onError: (c) => apiError(c, 413, 'payload_too_large', 'Request body is too large') });

export const iso = (d: Date | null | undefined) => (d ? d.toISOString() : null);
