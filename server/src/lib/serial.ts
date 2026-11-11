// In-process serialisation: one call at a time per key. The server runs as a single Node process.
import { ApiError } from './http.js';

const queues = new Map<string, { tail: Promise<unknown>; size: number }>();

/**
 * Runs `fn` once every earlier call with the same key has settled. Rejects with `busy` (by default
 * 429) instead of queueing when `maxQueued` calls for the key are already waiting or running.
 */
export async function serial<T>(key: string, fn: () => Promise<T>, maxQueued = Infinity, busy?: () => ApiError): Promise<T> {
  const q = queues.get(key) ?? { tail: Promise.resolve(), size: 0 };
  if (q.size >= maxQueued) throw busy?.() ?? new ApiError(429, 'too_many_requests', 'Too many requests in flight; wait for earlier ones to finish');
  q.size++;
  queues.set(key, q);
  const run = q.tail.then(fn);
  q.tail = run.catch(() => {});
  try {
    return await run;
  } finally {
    if (--q.size === 0) queues.delete(key);
  }
}
