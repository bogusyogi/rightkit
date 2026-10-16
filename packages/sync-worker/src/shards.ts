import { config, MAX_SHARDS, type Env } from './env.js';
import type { Principal } from './auth.js';

export interface Shard {
  index: number;
  db: D1Database;
}

const encoder = new TextEncoder();

/**
 * Stable shard index for one tenant: first 32 bits (big endian) of SHA-256("app|account") mod count.
 * App ids come from ALLOWED_APPS and account ids match [A-Za-z0-9._:@-], so "|" cannot occur in
 * either and the pair is unambiguous. The result depends only on (app, account, count): changing the
 * count re-routes tenants and needs a data migration (see README, "Sharding").
 */
export async function shardIndex(app: string, accountId: string, count: number): Promise<number> {
  if (count <= 1) return 0;
  const digest = new Uint8Array(await crypto.subtle.digest('SHA-256', encoder.encode(`${app}|${accountId}`)));
  const n = ((digest[0]! << 24) | (digest[1]! << 16) | (digest[2]! << 8) | digest[3]!) >>> 0;
  return n % count;
}

export function shardBinding(env: Env, index: number): D1Database {
  const db = env[`DB_${index}`];
  if (!db) {
    // Content-free; the router logs only the error name.
    const e = new Error(`D1 binding DB_${index} is missing but SHARD_COUNT requires it`);
    e.name = 'ShardConfigError';
    throw e;
  }
  return db;
}

export function shardCount(env: Env): number {
  return Math.min(config(env).shardCount, MAX_SHARDS);
}

/** The one database that holds everything for this (app, account). */
export async function shardFor(env: Env, p: Principal): Promise<D1Database> {
  return shardBinding(env, await shardIndex(p.app, p.accountId, shardCount(env)));
}

/** Every shard, in index order. Used by erase and the scheduled jobs. */
export function allShards(env: Env): Shard[] {
  const out: Shard[] = [];
  for (let i = 0; i < shardCount(env); i++) out.push({ index: i, db: shardBinding(env, i) });
  return out;
}

/** Counts D1 statements against a per-invocation limit (50 on the free plan, 1,000 on paid). */
export class QueryBudget {
  constructor(public left: number) {}
  take(n: number): void {
    this.left -= n;
  }
}
