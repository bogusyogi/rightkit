import { config, RATE_WINDOW_MS, type Env } from './env.js';
import { event, HttpError } from './http.js';
import { shardFor } from './shards.js';
import type { Principal } from './auth.js';

export interface AdmitOptions {
  /** Erase must stay callable after it revoked the caller's own token (retry after partial failure). */
  allowRevoked?: boolean;
}

/**
 * Per-account admission: revocation check (tokens issued at or before an account erase)
 * and a fixed-window request budget. One D1 round trip (two statements) on the account's shard,
 * which holds the account's erase marker and rate-limit counters.
 * `cost` is 1 per request, or the record count for a save.
 */
export async function admit(
  env: Env,
  p: Principal,
  cost: number,
  nowMs: number,
  options: AdmitOptions = {},
): Promise<void> {
  const db = await shardFor(env, p);
  const windowStart = Math.floor(nowMs / RATE_WINDOW_MS) * RATE_WINDOW_MS;
  const results = await db.batch([
    db.prepare('SELECT erased_at FROM erased_accounts WHERE app = ?1 AND account_id = ?2').bind(p.app, p.accountId),
    db.prepare(
      `INSERT INTO rate_limits (app, account_id, window_start, count) VALUES (?1, ?2, ?3, ?4)
       ON CONFLICT (app, account_id, window_start) DO UPDATE SET count = count + excluded.count
       RETURNING count`,
    ).bind(p.app, p.accountId, windowStart, cost),
  ]);

  const erased = (results[0]!.results[0] as { erased_at: number } | undefined)?.erased_at;
  if (!options.allowRevoked && erased !== undefined && erased >= p.iat * 1000) {
    throw new HttpError(401, 'token_revoked', 'account was erased; sign in again');
  }

  const count = (results[1]!.results[0] as { count: number } | undefined)?.count ?? cost;
  if (count > config(env).rateLimitPerMinute) {
    const retryAfter = Math.max(1, Math.ceil((windowStart + RATE_WINDOW_MS - nowMs) / 1000));
    event('sync.rate_limited', { app: p.app, retry_after_s: retryAfter });
    throw new HttpError(429, 'rate_limited', 'too many requests', retryAfter);
  }
}
