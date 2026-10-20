import { describe, expect, it, vi } from 'vitest';
import { createPersistedCheckCache, type CheckCacheStore } from './check-cache.js';

const isNumber = (value: unknown): value is number => typeof value === 'number';

function memoryStore(initial: string | null = null) {
  const state = { text: initial as string | null };
  const store: CheckCacheStore = {
    read: vi.fn(async () => state.text),
    write: vi.fn(async (text: string) => {
      state.text = text;
    }),
  };
  return { store, state };
}

function record(checkedAt: number, value: unknown, v = 1): string {
  return JSON.stringify({ v, checkedAt, value });
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((res) => {
    resolve = res;
  });
  return { promise, resolve };
}

describe('persisted check cache', () => {
  it('serves the persisted record inside the window and re-runs after it', async () => {
    let t = 0;
    const { store } = memoryStore();
    const cache = createPersistedCheckCache<number>({ store, minIntervalMs: 1000, now: () => t, validate: isNumber });
    let n = 0;
    const fn = vi.fn(async () => ++n);
    expect(await cache.run(fn)).toEqual({ value: 1, checkedAt: 0, source: 'fresh' });
    t = 500;
    expect(await cache.run(fn)).toEqual({ value: 1, checkedAt: 0, source: 'cache' });
    expect(fn).toHaveBeenCalledTimes(1);
    t = 1000;
    expect(await cache.run(fn)).toEqual({ value: 2, checkedAt: 1000, source: 'fresh' });
    expect(fn).toHaveBeenCalledTimes(2);
  });

  it('force bypasses the window, and reset makes the next run real', async () => {
    const { store } = memoryStore();
    const cache = createPersistedCheckCache<number>({ store, minIntervalMs: 1000, now: () => 0, validate: isNumber });
    const fn = vi.fn(async () => fn.mock.calls.length);
    await cache.run(fn);
    expect(await cache.run(fn, { force: true })).toMatchObject({ value: 2, source: 'fresh' });
    cache.reset();
    expect(await cache.run(fn)).toMatchObject({ value: 3, source: 'fresh' });
  });

  it('distrusts a record stamped beyond the clock-skew bound (clock rollback) and runs a real check', async () => {
    const now = 10_000;
    const { store } = memoryStore(record(now + 60_001, 99));
    const cache = createPersistedCheckCache<number>({ store, minIntervalMs: 1000, now: () => now, validate: isNumber });
    const fn = vi.fn(async () => 5);
    expect(await cache.run(fn)).toEqual({ value: 5, checkedAt: now, source: 'fresh' });
    expect(fn).toHaveBeenCalledTimes(1);
    expect(cache.peek()).toEqual({ checkedAt: now, value: 5 });
  });

  it('never serves a future record within the skew as cache, and does not use a beyond-skew record as last-known-good', async () => {
    const now = 10_000;
    const within = memoryStore(record(now + 30_000, 7));
    const withinCache = createPersistedCheckCache<number>({
      store: within.store, minIntervalMs: 1000, now: () => now, validate: isNumber,
    });
    const fn = vi.fn(async () => 5);
    expect(await withinCache.run(fn)).toMatchObject({ value: 5, source: 'fresh' });
    expect(fn).toHaveBeenCalledTimes(1);

    const beyond = memoryStore(record(now + 60_001, 99));
    const beyondCache = createPersistedCheckCache<number>({
      store: beyond.store, minIntervalMs: 1000, now: () => now, validate: isNumber,
    });
    await expect(beyondCache.run(async () => { throw new Error('offline'); })).rejects.toThrow('offline');
  });

  it('serves the last-known-good record on failure, with the error attached and the record kept', async () => {
    const { store, state } = memoryStore(record(0, 7));
    const cache = createPersistedCheckCache<number>({ store, minIntervalMs: 1000, now: () => 1_000_000, validate: isNumber });
    const error = new Error('network down');
    expect(await cache.run(async () => { throw error; })).toEqual({
      value: 7,
      checkedAt: 0,
      source: 'last-known-good',
      error,
    });
    expect(store.write).not.toHaveBeenCalled();
    expect(state.text).toBe(record(0, 7));
  });

  it('rethrows the original error when there is no usable record', async () => {
    const { store } = memoryStore();
    const cache = createPersistedCheckCache<number>({ store, minIntervalMs: 1000, now: () => 0, validate: isNumber });
    await expect(cache.run(async () => { throw new Error('boom'); })).rejects.toThrow('boom');
    expect(store.write).not.toHaveBeenCalled();
  });

  it('treats corrupt, wrong-version, non-finite or invalid records as absent and never deletes them', async () => {
    const absent = [
      'not json',
      record(0, 1, 2),
      JSON.stringify({ v: 1, checkedAt: '0', value: 1 }),
      record(0, 'text'),
      JSON.stringify([1, 2]),
    ];
    for (const text of absent) {
      const served = memoryStore(text);
      const cache = createPersistedCheckCache<number>({ store: served.store, minIntervalMs: 1000, now: () => 0, validate: isNumber });
      const fn = vi.fn(async () => 4);
      expect(await cache.run(fn)).toEqual({ value: 4, checkedAt: 0, source: 'fresh' });
      expect(fn).toHaveBeenCalledTimes(1);

      const failing = memoryStore(text);
      const rejecting = createPersistedCheckCache<number>({ store: failing.store, minIntervalMs: 1000, now: () => 0, validate: isNumber });
      await expect(rejecting.run(async () => { throw new Error('x'); })).rejects.toThrow('x');
      expect(failing.state.text).toBe(text);
      expect(failing.store.write).not.toHaveBeenCalled();
    }
  });

  it('treats a throwing store read as absent and never throws from read', async () => {
    const store: CheckCacheStore = {
      read: vi.fn(async () => { throw new Error('EACCES'); }),
      write: vi.fn(async () => undefined),
    };
    const cache = createPersistedCheckCache<number>({ store, minIntervalMs: 1000, now: () => 0, validate: isNumber });
    expect(await cache.run(async () => 3)).toEqual({ value: 3, checkedAt: 0, source: 'fresh' });
    expect(cache.peek()).toEqual({ checkedAt: 0, value: 3 });
  });

  it('returns a fresh result with persistError when the write fails', async () => {
    const writeError = new Error('disk full');
    const store: CheckCacheStore = {
      read: vi.fn(async () => null),
      write: vi.fn(async () => { throw writeError; }),
    };
    const cache = createPersistedCheckCache<number>({ store, minIntervalMs: 1000, now: () => 0, validate: isNumber });
    expect(await cache.run(async () => 8)).toEqual({
      value: 8,
      checkedAt: 0,
      source: 'fresh',
      persistError: writeError,
    });
  });

  it('shares one in-flight run among concurrent callers', async () => {
    const { store } = memoryStore();
    const cache = createPersistedCheckCache<number>({ store, minIntervalMs: 0, now: () => 0, validate: isNumber });
    const gate = deferred<number>();
    const fn = vi.fn(() => gate.promise);
    const a = cache.run(fn);
    const b = cache.run(fn);
    gate.resolve(7);
    expect((await a).value).toBe(7);
    expect((await b).value).toBe(7);
    expect(fn).toHaveBeenCalledTimes(1);
  });

  it('reset then falls back to the last-known-good record, which stays persisted', async () => {
    const { store, state } = memoryStore();
    const cache = createPersistedCheckCache<number>({ store, minIntervalMs: 1000, now: () => 0, validate: isNumber });
    expect(await cache.run(async () => 5)).toMatchObject({ value: 5, source: 'fresh' });
    expect(await cache.run(async () => 6)).toMatchObject({ value: 5, source: 'cache' });
    cache.reset();
    const error = new Error('offline');
    expect(await cache.run(async () => { throw error; })).toEqual({
      value: 5,
      checkedAt: 0,
      source: 'last-known-good',
      error,
    });
    expect(state.text).toBe(record(0, 5));
  });

  it('does not persist a value that fails validate and reports it as persistError', async () => {
    const { store, state } = memoryStore();
    const cache = createPersistedCheckCache<number>({ store, minIntervalMs: 1000, now: () => 0, validate: isNumber });
    const result = await cache.run(async () => 'not a number' as unknown as number);
    expect(result.source).toBe('fresh');
    expect(result.persistError).toBeInstanceOf(Error);
    expect(store.write).not.toHaveBeenCalled();
    expect(state.text).toBeNull();
  });
});
