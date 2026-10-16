export interface Env {
  DB: D1Database;
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
}

export const PROTOCOL = 'rightkit-sync-http/1';
export const MAX_BATCH = 100;
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
  };
}
