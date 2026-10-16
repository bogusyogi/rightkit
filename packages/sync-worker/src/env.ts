export interface Env {
  /**
   * Shard databases DB_0 .. DB_{SHARD_COUNT-1}. Each (app, account) lives in exactly one, chosen by
   * a stable hash (src/shards.ts). The index signature types every DB_<n> binding.
   */
  [binding: `DB_${number}`]: D1Database | undefined;
  /** Number of shard databases in use (default 1, at most 64). Changing it needs a data migration. */
  SHARD_COUNT?: string;
  /** 'free' (default) or 'paid': picks the default save size and per-invocation query budget. */
  CF_PLAN?: string;
  /** Explicit override of the maximum records per save (1..100). */
  MAX_SAVE_RECORDS?: string;
  /** R2 bucket for ciphertext bodies above BLOB_SPILL_BYTES. Optional: without it large bodies are refused. */
  BLOBS?: R2Bucket;
  /** HS256 key shared with the account backend (secret). */
  SYNC_JWT_SECRET: string;
  /** Previous key, accepted during rotation (secret, optional). */
  SYNC_JWT_SECRET_PREVIOUS?: string;
  /** Comma-separated app ids allowed to use this worker. */
  ALLOWED_APPS: string;
  RATE_LIMIT_PER_MINUTE?: string;
  ACCOUNT_STORAGE_CAP_BYTES?: string;
  TOMBSTONE_COMPACT_DAYS?: string;
  BLOB_SPILL_BYTES?: string;
}

export interface Config {
  allowedApps: ReadonlySet<string>;
  rateLimitPerMinute: number;
  storageCapBytes: number;
  tombstoneCompactDays: number;
  blobSpillBytes: number;
  shardCount: number;
  plan: 'free' | 'paid';
  maxSaveRecords: number;
  /** D1 statements one Worker invocation may run (Cloudflare: 50 free, 1,000 paid), minus headroom. */
  queryBudget: number;
}

export const PROTOCOL = 'rightkit-sync-http/1';
export const MAX_BATCH = 100;
export const MAX_SHARDS = 64;
/** Default save size per plan: a save costs about N + 8 D1 statements and free allows 50 per invocation. */
export const FREE_MAX_SAVE = 40;
export const PAID_MAX_SAVE = 100;
export const FREE_QUERY_BUDGET = 50;
export const PAID_QUERY_BUDGET = 1000;
export const MAX_CHANGES_LIMIT = 200;
export const MAX_RECORD_BYTES = 1_048_576;
export const MAX_BODY_BYTES = 8 * 1_048_576;
export const PAGE_BYTE_BUDGET = 4 * 1_048_576;
export const MAX_ZONES_PER_ACCOUNT = 16;
export const RATE_WINDOW_MS = 60_000;
export const MAX_TOKEN_LIFETIME_S = 24 * 3600;
export const ERASE_MARKER_TTL_MS = 7 * 24 * 3600 * 1000;

function int(value: string | undefined, fallback: number, min: number): number {
  if (value === undefined || value === '') return fallback;
  const n = Number(value);
  return Number.isSafeInteger(n) && n >= min ? n : fallback;
}

export function config(env: Env): Config {
  const plan = (env.CF_PLAN ?? '').trim().toLowerCase() === 'paid' ? 'paid' : 'free';
  return {
    allowedApps: new Set(
      (env.ALLOWED_APPS ?? '')
        .split(',')
        .map((s) => s.trim())
        .filter(Boolean),
    ),
    rateLimitPerMinute: int(env.RATE_LIMIT_PER_MINUTE, 1200, 1),
    storageCapBytes: int(env.ACCOUNT_STORAGE_CAP_BYTES, 52_428_800, 0),
    tombstoneCompactDays: int(env.TOMBSTONE_COMPACT_DAYS, 90, 1),
    blobSpillBytes: int(env.BLOB_SPILL_BYTES, 65_536, 1),
    shardCount: Math.min(int(env.SHARD_COUNT, 1, 1), MAX_SHARDS),
    plan,
    maxSaveRecords: Math.min(int(env.MAX_SAVE_RECORDS, plan === 'paid' ? PAID_MAX_SAVE : FREE_MAX_SAVE, 1), MAX_BATCH),
    queryBudget: plan === 'paid' ? PAID_QUERY_BUDGET : FREE_QUERY_BUDGET,
  };
}
