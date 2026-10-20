/** Persisted record format version. Any other version in the store is treated as absent. */
const RECORD_VERSION = 1;

/** Host-owned storage for the check record. The package never touches the filesystem itself. */
export interface CheckCacheStore {
  /** Returns the stored text, or null when nothing is stored. A rejection is treated as absent. */
  read(): Promise<string | null>;
  /** Replaces the stored text. A rejection never fails the check; it is reported as `persistError`. */
  write(text: string): Promise<void>;
}

export interface PersistedCheckCacheOptions<T> {
  store: CheckCacheStore;
  /** Minimum ms between real checks. A persisted record younger than this is served as the cache. */
  minIntervalMs: number;
  /** Injectable clock for tests and hosts with their own time source. */
  now?: () => number;
  /** Validates a stored value. A value that fails (or throws) is treated as absent. */
  validate: (value: unknown) => value is T;
  /** How far ahead of `now()` a record's `checkedAt` may be before it is distrusted. Default 60000. */
  maxFutureSkewMs?: number;
}

export interface PersistedCheckResult<T> {
  value: T;
  checkedAt: number;
  /** 'fresh': a real check ran and its result was stored. 'cache': a stored record inside the window. 'last-known-good': a real check failed and a stored record was served. */
  source: 'fresh' | 'cache' | 'last-known-good';
  /** The check error. Present only when `source` is 'last-known-good'. */
  error?: unknown;
  /** The store write error. Present only when a fresh result could not be persisted. */
  persistError?: unknown;
}

export interface PersistedCheckCache<T> {
  /** Serves the cache inside the window, otherwise runs `fn`. Concurrent non-forced calls share one run. */
  run(fn: () => Promise<T>, options?: { force?: boolean }): Promise<PersistedCheckResult<T>>;
  /** The next run is a real check. The persisted record is kept, so last-known-good still works. */
  reset(): void;
  /**
   * Synchronous view of the record this instance last read or wrote successfully. It does not touch the
   * store, so it is null until the first run. A record distrusted by the clock-rollback guard is reported as null.
   */
  peek(): { checkedAt: number; value: T } | null;
}

interface StoredRecord<T> {
  checkedAt: number;
  value: T;
}

/**
 * Persisted update-check cache with a last-known-good fallback. The store holds JSON
 * `{ "v": 1, "checkedAt": number, "value": unknown }`.
 *
 * - Each run reads the store. A record is served as source 'cache' only when the run is not forced and
 *   `0 <= now() - checkedAt < minIntervalMs`.
 * - Clock-rollback guard: a record with `checkedAt > now() + maxFutureSkewMs` (the clock moved backwards)
 *   is not trusted at all. The run performs a real check, and the record is neither served nor used as
 *   last-known-good. A record only slightly ahead, within the skew, is never served as cache either.
 * - Unparseable text, a wrong version, a non-finite `checkedAt` or a value failing `validate` is absent.
 *   Nothing is ever deleted from the store.
 * - When `fn` rejects and a trusted record exists (any age), the result is that record with
 *   source 'last-known-good' and the error attached. Otherwise the original error is rethrown.
 *   Failures are never written.
 * - A successful result is written with `store.write`. A value that fails `validate` is not written.
 *   A write failure still returns source 'fresh', with `persistError` set.
 */
export function createPersistedCheckCache<T>(options: PersistedCheckCacheOptions<T>): PersistedCheckCache<T> {
  const now = options.now ?? (() => Date.now());
  const maxFutureSkewMs = options.maxFutureSkewMs ?? 60_000;
  let inflight: Promise<PersistedCheckResult<T>> | null = null;
  let bypassNext = false;
  let lastRecord: StoredRecord<T> | null = null;

  /** Clock-rollback guard: a record stamped beyond `at + maxFutureSkewMs` is not trusted. */
  const trusted = (record: StoredRecord<T> | null, at: number): StoredRecord<T> | null =>
    record && record.checkedAt <= at + maxFutureSkewMs ? record : null;

  function parseRecord(text: unknown): StoredRecord<T> | null {
    if (typeof text !== 'string') return null;
    let parsed: unknown;
    try {
      parsed = JSON.parse(text);
    } catch {
      return null;
    }
    if (typeof parsed !== 'object' || parsed === null || Array.isArray(parsed)) return null;
    const { v, checkedAt, value } = parsed as Record<string, unknown>;
    if (v !== RECORD_VERSION) return null;
    if (typeof checkedAt !== 'number' || !Number.isFinite(checkedAt)) return null;
    let valid: boolean;
    try {
      valid = options.validate(value);
    } catch {
      return null;
    }
    return valid ? { checkedAt, value: value as T } : null;
  }

  async function readRecord(): Promise<StoredRecord<T> | null> {
    let text: string | null;
    try {
      text = await options.store.read();
    } catch {
      return null;
    }
    const record = parseRecord(text);
    if (record) lastRecord = record;
    return record;
  }

  async function execute(fn: () => Promise<T>, skipCache: boolean): Promise<PersistedCheckResult<T>> {
    const before = await readRecord();
    const startedAt = now();
    const usable = trusted(before, startedAt);
    if (!skipCache && usable) {
      const age = startedAt - usable.checkedAt;
      if (age >= 0 && age < options.minIntervalMs) {
        return { value: usable.value, checkedAt: usable.checkedAt, source: 'cache' };
      }
    }

    let value: T;
    try {
      value = await fn();
    } catch (error) {
      if (usable) {
        return { value: usable.value, checkedAt: usable.checkedAt, source: 'last-known-good', error };
      }
      throw error;
    }

    const checkedAt = now();
    let persistFailed = false;
    let persistError: unknown;
    try {
      if (!options.validate(value)) throw new Error('check result failed validate(); not persisted');
      await options.store.write(JSON.stringify({ v: RECORD_VERSION, checkedAt, value }));
      lastRecord = { checkedAt, value };
    } catch (error) {
      persistFailed = true;
      persistError = error;
    }
    return persistFailed
      ? { value, checkedAt, source: 'fresh', persistError }
      : { value, checkedAt, source: 'fresh' };
  }

  return {
    run(fn, runOptions) {
      if (!runOptions?.force && inflight) return inflight;
      const skipCache = runOptions?.force === true || bypassNext;
      bypassNext = false;
      const started: Promise<PersistedCheckResult<T>> = execute(fn, skipCache).finally(() => {
        if (inflight === started) inflight = null;
      });
      inflight = started;
      return started;
    },
    reset() {
      bypassNext = true;
    },
    peek() {
      const record = trusted(lastRecord, now());
      return record ? { checkedAt: record.checkedAt, value: record.value } : null;
    },
  };
}
