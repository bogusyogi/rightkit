import { afterEach, describe, expect, it, vi } from 'vitest';
import { createDiagnosticsRecorder, type DiagnosticInput, type DiagnosticsRecorderConfig } from './recorder.js';
import { installWindowCrashCapture, type WindowCrashPayload } from './crash.js';
import type { LogsInvoke } from './index.js';

function memoryStorage(initial: Record<string, string> = {}) {
  const data = new Map(Object.entries(initial));
  return {
    data,
    getItem: vi.fn((key: string) => data.get(key) ?? null),
    setItem: vi.fn((key: string, value: string) => { data.set(key, value); }),
    removeItem: vi.fn((key: string) => { data.delete(key); }),
  };
}

function browser(idle = false) {
  const callbacks = new Map<number, () => void>();
  const events = new Map<string, (event?: any) => void>();
  const docEvents = new Map<string, () => void>();
  let next = 0;
  const doc = {
    visibilityState: 'visible',
    addEventListener: vi.fn((name: string, fn: () => void) => { docEvents.set(name, fn); }),
    removeEventListener: vi.fn((name: string) => { docEvents.delete(name); }),
  };
  const host = {
    document: doc,
    addEventListener: vi.fn((name: string, fn: (event?: any) => void) => { events.set(name, fn); }),
    removeEventListener: vi.fn((name: string) => { events.delete(name); }),
    setTimeout: vi.fn((fn: () => void, _delay: number) => { callbacks.set(++next, fn); return next; }),
    clearTimeout: vi.fn((id: number) => { callbacks.delete(id); }),
    ...(idle ? {
      requestIdleCallback: vi.fn((fn: () => void, _options: { timeout: number }) => {
        callbacks.set(++next, fn); return next;
      }),
      cancelIdleCallback: vi.fn((id: number) => { callbacks.delete(id); }),
    } : {}),
  };
  vi.stubGlobal('window', host);
  return { host, doc, callbacks, events, docEvents, run: () => {
    const pending = [...callbacks.values()];
    callbacks.clear();
    pending.forEach((fn) => fn());
  } };
}

function setup(options: Partial<DiagnosticsRecorderConfig> = {}) {
  const storage = memoryStorage();
  let time = 1000;
  const recorder = createDiagnosticsRecorder({
    storageKey: 'test.diagnostics', allowedEvents: ['open', 'close', 'window.error', 'window.unhandledrejection'],
    storage, now: () => time, ...options,
  });
  return { recorder, storage, at: (value: number) => { time = value; } };
}

afterEach(() => { vi.unstubAllGlobals(); });

describe('diagnostics recorder', () => {
  it('evicts oldest entries, returns copies, deduplicates consecutive kind+event below 1000 ms', () => {
    browser();
    const { recorder, at } = setup();
    expect(recorder.record({ kind: 'perf', event: 'open', duration_ms: 1 })).toBe(true);
    at(1999);
    expect(recorder.record({ kind: 'perf', event: 'open', duration_ms: 2 })).toBe(false);
    at(2000);
    expect(recorder.record({ kind: 'perf', event: 'open', duration_ms: 2 })).toBe(true);
    expect(recorder.record({ kind: 'event', event: 'open' })).toBe(true);
    for (let i = 0; i < 85; i += 1) {
      at(3000 + i * 1000);
      recorder.record({ kind: 'event', event: i % 2 ? 'open' : 'close' });
    }
    const copy = recorder.snapshot();
    expect(copy).toHaveLength(80);
    expect(copy[0].ts_ms).toBe(8000);
    copy[0].event = 'mutated';
    copy.pop();
    expect(recorder.snapshot()).toHaveLength(80);
    expect(recorder.snapshot()[0].event).toBe('open');
    recorder.dispose();
  });

  it('accepts backward clock movement & deduplicates relative to newly accepted timestamp', () => {
    browser();
    const { recorder, at } = setup();
    at(5000);
    expect(recorder.record({ kind: 'event', event: 'open' })).toBe(true);
    at(1000);
    expect(recorder.record({ kind: 'event', event: 'open' })).toBe(true);
    at(1999);
    expect(recorder.record({ kind: 'event', event: 'open' })).toBe(false);
    at(2000);
    expect(recorder.record({ kind: 'event', event: 'open' })).toBe(true);
    expect(recorder.snapshot().map((entry) => entry.ts_ms)).toEqual([5000, 1000, 2000]);
    recorder.dispose();
  });

  it('rejects invalid input before storage & projects only fixed allowed fields', () => {
    browser();
    const { recorder, storage } = setup();
    const invalid: unknown[] = [null, {}, { kind: 'error', event: 'unknown' },
      { kind: 'event', event: ' open' }, { kind: 'event', event: 'open\n' }, { kind: 'other', event: 'open' },
      { kind: 'event', event: 'open', duration_ms: 2 },
      { kind: 'perf', event: 'open', duration_ms: -1 },
      { kind: 'perf', event: 'open', duration_ms: 0.5 },
      { kind: 'perf', event: 'open', duration_ms: Infinity },
      { kind: 'perf', event: 'open', duration_ms: Number.MAX_SAFE_INTEGER + 1 },
      { get kind() { throw new Error('private'); }, event: 'open' }];
    for (const input of invalid) expect(recorder.record(input as DiagnosticInput)).toBe(false);
    expect(storage.getItem).not.toHaveBeenCalled();
    expect(storage.setItem).not.toHaveBeenCalled();
    recorder.record({ kind: 'perf', event: 'open', duration_ms: 0, detail: 'private',
      content: 'private', source: '/private', stack: 'private', message: 'private' } as DiagnosticInput);
    recorder.flush();
    expect(recorder.snapshot()).toEqual([{ schema_version: 1, ts_ms: 1000, kind: 'perf', event: 'open', duration_ms: 0 }]);
    expect(recorder.text()).not.toContain('private');
    expect(storage.data.get('test.diagnostics')).not.toContain('private');
    recorder.dispose();
  });

  it('handles invalid & throwing clocks without touching storage', () => {
    for (const now of [() => NaN, () => -1, () => 0.1, () => Number.MAX_SAFE_INTEGER + 1,
      () => { throw new Error('private'); }]) {
      const { recorder, storage } = setup({ now });
      expect(recorder.record({ kind: 'load', event: 'open' })).toBe(false);
      expect(storage.getItem).not.toHaveBeenCalled();
      expect(storage.setItem).not.toHaveBeenCalled();
    }
  });

  it('validates config with fixed errors & copies allowlists', () => {
    const base = { storageKey: 'key', allowedEvents: ['open'] };
    for (const bad of [{ storageKey: '' }, { allowedEvents: ['private text'] }, { allowedEvents: ['open\n'] },
      { allowedEvents: ['x'.repeat(97)] }, { legacyKeys: ['key'] },
      { enabled: 'private' }, { now: 1 }, { storage: {} }]) {
      expect(() => createDiagnosticsRecorder({ ...base, ...bad } as DiagnosticsRecorderConfig)).toThrow(TypeError);
      expect(() => createDiagnosticsRecorder({ ...base, ...bad } as DiagnosticsRecorderConfig)).not.toThrow('private');
    }
    const allowedEvents = ['open'];
    const { recorder } = setup({ allowedEvents });
    allowedEvents.push('close');
    expect(recorder.record({ kind: 'event', event: 'close' })).toBe(false);
    recorder.dispose();
  });

  it('restores only projected valid schema entries, capped to last 80', () => {
    const entries = Array.from({ length: 90 }, (_, i) => ({
      schema_version: 1, ts_ms: i, kind: 'perf', event: 'open', duration_ms: i, detail: 'private',
    }));
    const storage = memoryStorage({ key: JSON.stringify({ schema_version: 1, entries: [
      { schema_version: 1, ts_ms: -1, kind: 'event', event: 'open' },
      { schema_version: 2, ts_ms: 0, kind: 'event', event: 'open' },
      ...entries,
      { schema_version: 1, ts_ms: 0, kind: 'event', event: 'unknown' },
      { schema_version: 1, ts_ms: 0, kind: 'perf', event: 'open', duration_ms: NaN },
    ] }) });
    const { recorder } = setup({ storageKey: 'key', storage });
    expect(recorder.snapshot()).toHaveLength(80);
    expect(recorder.snapshot()[0].ts_ms).toBe(10);
    expect(recorder.text()).not.toContain('private');
    recorder.flush();
    expect(storage.data.get('key')).not.toContain('private');
    recorder.dispose();
  });

  it('migrates legacy timestamps & labels only after successful sanitized write', () => {
    const storage = memoryStorage({ old: JSON.stringify([
      { t: '2026-01-01T00:00:00.000Z', kind: 'event', msg: 'open', detail: 'private' },
      { t: 'invalid', kind: 'event', msg: 'open' },
      { t: '2026-01-01T00:00:00.000Z', kind: 'error', msg: 'private text' },
    ]) });
    const { recorder } = setup({ storageKey: 'key', legacyKeys: ['old'], storage });
    expect(recorder.snapshot()).toEqual([{ schema_version: 1, ts_ms: Date.parse('2026-01-01T00:00:00Z'), kind: 'event', event: 'open' }]);
    storage.setItem.mockImplementationOnce(() => { throw new Error(); });
    expect(recorder.flush()).toBe('failed');
    expect(storage.removeItem).not.toHaveBeenCalled();
    expect(recorder.flush()).toBe('ok');
    expect(storage.data.has('old')).toBe(false);
    expect(storage.data.get('key')).not.toContain('private');
    recorder.dispose();
  });

  it('never falls back from malformed current data & bounds input bytes before parsing', () => {
    const legacy = JSON.stringify([{ t: '2026-01-01', kind: 'event', msg: 'open' }]);
    for (const raw of ['{bad', '', JSON.stringify({ schema_version: 2, entries: [] }),
      ' '.repeat(65537), JSON.stringify(['é'.repeat(33000)]), JSON.stringify(Array(22000).fill(0))]) {
      const storage = memoryStorage({ key: raw, old: legacy });
      const { recorder } = setup({ storageKey: 'key', legacyKeys: ['old'], storage });
      expect(recorder.snapshot()).toEqual([]);
      expect(storage.getItem.mock.calls).toEqual([['key']]);
      recorder.dispose();
    }
  });

  it('coalesces one idle flush & flushes load/error/rejection immediately', () => {
    const view = browser(true);
    const { recorder, storage } = setup();
    recorder.record({ kind: 'event', event: 'open' });
    recorder.record({ kind: 'event', event: 'close' });
    expect(view.host.requestIdleCallback).toHaveBeenCalledTimes(1);
    expect(view.host.requestIdleCallback).toHaveBeenCalledWith(expect.any(Function), { timeout: 2000 });
    expect(storage.setItem).not.toHaveBeenCalled();
    view.run();
    expect(storage.setItem).toHaveBeenCalledTimes(1);
    for (const kind of ['load', 'error', 'rejection'] as const) {
      recorder.record({ kind, event: 'open' });
    }
    expect(storage.setItem).toHaveBeenCalledTimes(4);
    recorder.dispose();
  });

  it('uses one 1000 ms fallback timer & flushes pagehide/hidden only', () => {
    const view = browser();
    const { recorder, storage } = setup();
    recorder.record({ kind: 'event', event: 'open' });
    recorder.record({ kind: 'event', event: 'close' });
    expect(view.host.setTimeout).toHaveBeenCalledOnce();
    expect(view.host.setTimeout).toHaveBeenCalledWith(expect.any(Function), 1000);
    view.docEvents.get('visibilitychange')!();
    expect(storage.setItem).not.toHaveBeenCalled();
    view.events.get('pagehide')!();
    expect(storage.setItem).toHaveBeenCalledOnce();
    expect(view.callbacks.size).toBe(0);
    recorder.record({ kind: 'event', event: 'open' });
    view.doc.visibilityState = 'hidden';
    view.docEvents.get('visibilitychange')!();
    expect(storage.setItem).toHaveBeenCalledTimes(2);
    recorder.dispose();
    expect(view.events.size + view.docEvents.size).toBe(0);
  });

  it('flushes accepted history once on disable, blocks writes, retains history & resets dedup on enable', () => {
    const view = browser();
    const { recorder, storage } = setup();
    recorder.record({ kind: 'event', event: 'open' });
    const canceled = [...view.callbacks.values()][0];
    expect(recorder.setEnabled(false)).toBe('ok');
    expect(storage.setItem).toHaveBeenCalledOnce();
    expect(view.callbacks.size).toBe(0);
    expect(recorder.record({ kind: 'error', event: 'close' })).toBe(false);
    expect(recorder.flush()).toBe('disabled');
    expect(recorder.setEnabled(false)).toBe('disabled');
    view.events.get('pagehide')!();
    canceled();
    expect(storage.setItem).toHaveBeenCalledOnce();
    expect(recorder.snapshot()).toHaveLength(1);
    expect(recorder.setEnabled(true)).toBe('ok');
    expect(recorder.record({ kind: 'event', event: 'open' })).toBe(true);
    recorder.dispose();
  });

  it('retains dirty ring after failure, retries only on explicit/lifecycle/record work', () => {
    const view = browser();
    const { recorder, storage } = setup();
    storage.setItem.mockImplementationOnce(() => { throw new Error('quota'); });
    recorder.record({ kind: 'error', event: 'open' });
    expect(recorder.snapshot()).toHaveLength(1);
    expect(view.callbacks.size).toBe(0);
    view.events.get('pagehide')!();
    expect(storage.setItem).toHaveBeenCalledTimes(2);
    storage.setItem.mockImplementationOnce(() => { throw new Error(); });
    recorder.record({ kind: 'error', event: 'close' });
    recorder.record({ kind: 'event', event: 'open' });
    view.run();
    expect(storage.setItem).toHaveBeenCalledTimes(4);
    recorder.dispose();
  });

  it('does not retry a failed disable flush until reenabled', () => {
    const view = browser();
    const { recorder, storage } = setup();
    recorder.record({ kind: 'event', event: 'open' });
    storage.setItem.mockImplementationOnce(() => { throw new Error(); });
    expect(recorder.setEnabled(false)).toBe('failed');
    expect(recorder.flush()).toBe('disabled');
    view.events.get('pagehide')!();
    expect(storage.setItem).toHaveBeenCalledOnce();
    recorder.setEnabled(true);
    expect(recorder.flush()).toBe('ok');
    recorder.dispose();
  });

  it('clear works disabled, attempts every key & reports any removal failure', () => {
    const view = browser();
    const { recorder, storage } = setup({ legacyKeys: ['old', 'older'] });
    recorder.record({ kind: 'event', event: 'open' });
    recorder.setEnabled(false);
    storage.removeItem.mockClear();
    storage.removeItem.mockImplementationOnce(() => { throw new Error(); });
    expect(recorder.clear()).toBe('failed');
    expect(storage.removeItem.mock.calls).toEqual([['test.diagnostics'], ['old'], ['older']]);
    expect(recorder.snapshot()).toEqual([]);
    expect(view.callbacks.size).toBe(0);
    expect(recorder.clear()).toBe('ok');
    recorder.dispose();
  });

  it('reports partial migration cleanup failure without dropping retry state', () => {
    browser();
    const { recorder, storage } = setup({ legacyKeys: ['old', 'older'] });
    recorder.record({ kind: 'event', event: 'open' });
    storage.removeItem.mockImplementationOnce(() => { throw new Error(); });
    expect(recorder.flush()).toBe('failed');
    expect(storage.removeItem.mock.calls).toEqual([['old'], ['older']]);
    expect(recorder.flush()).toBe('ok');
    recorder.dispose();
  });

  it('supports absent window & throwing localStorage getter', () => {
    vi.stubGlobal('window', undefined);
    const recorder = createDiagnosticsRecorder({ storageKey: 'key', allowedEvents: ['open'] });
    expect(recorder.record({ kind: 'event', event: 'open' })).toBe(true);
    expect(recorder.flush()).toBe('unavailable');
    expect(recorder.clear()).toBe('unavailable');
    expect(recorder.dispose()).toBe('unavailable');
    const view = browser();
    Object.defineProperty(view.host, 'localStorage', { get() { throw new Error(); } });
    const blocked = createDiagnosticsRecorder({ storageKey: 'key', allowedEvents: ['open'] });
    expect(blocked.flush()).toBe('unavailable');
    blocked.dispose();
  });

  it('never overwrites unreadable history & merges sanitized recovered history with pending entries', () => {
    browser();
    const { recorder, storage } = setup();
    storage.data.set('test.diagnostics', JSON.stringify({ schema_version: 1, entries: [
      { schema_version: 1, ts_ms: 0, kind: 'event', event: 'close', detail: 'private' },
    ] }));
    storage.getItem.mockImplementation(() => { throw new Error('denied'); });
    expect(recorder.record({ kind: 'load', event: 'open' })).toBe(true);
    expect(recorder.flush()).toBe('failed');
    expect(storage.setItem).not.toHaveBeenCalled();
    expect(storage.data.get('test.diagnostics')).toContain('close');
    expect(recorder.snapshot()).toHaveLength(1);
    storage.getItem.mockImplementation((key) => storage.data.get(key) ?? null);
    expect(recorder.flush()).toBe('ok');
    expect(recorder.snapshot().map((entry) => entry.event)).toEqual(['close', 'open']);
    expect(storage.data.get('test.diagnostics')).not.toContain('private');
    recorder.dispose();
  });

  it('dispose flushes once, removes lifecycle listeners & makes future operations inert', () => {
    const view = browser();
    const { recorder, storage } = setup();
    recorder.record({ kind: 'event', event: 'open' });
    expect(recorder.dispose()).toBe('ok');
    expect(storage.setItem).toHaveBeenCalledOnce();
    expect(view.callbacks.size + view.events.size + view.docEvents.size).toBe(0);
    expect(recorder.snapshot()).toEqual([]);
    expect(recorder.text()).toBe('[]');
    expect(recorder.record({ kind: 'event', event: 'open' })).toBe(false);
    for (const operation of [recorder.flush, recorder.clear, recorder.dispose, () => recorder.setEnabled(true)]) {
      expect(operation()).toBe('disposed');
    }
    expect(storage.setItem).toHaveBeenCalledOnce();
  });

  it('cleans up successful registration when another lifecycle target throws', () => {
    const view = browser();
    view.doc.addEventListener.mockImplementation(() => { throw new Error('unsupported'); });
    const { recorder } = setup();
    recorder.record({ kind: 'event', event: 'open' });
    expect(view.events.has('pagehide')).toBe(true);
    expect(recorder.dispose()).toBe('ok');
    expect(view.events.size + view.docEvents.size + view.callbacks.size).toBe(0);
  });

  it('composes one existing crash capture with fixed labels & one privacy gate for both destinations', () => {
    const view = browser();
    const { recorder } = setup();
    let enabled = true;
    const nativeInvoke = vi.fn(async (_command: string, _args?: Record<string, unknown>) => undefined);
    const invoke: LogsInvoke = async <T>(command: string, args?: Record<string, unknown>) => {
      if (!enabled) return undefined as T;
      const report = args?.report as WindowCrashPayload;
      recorder.record(report.kind === 'error'
        ? { kind: 'error', event: 'window.error' }
        : { kind: 'rejection', event: 'window.unhandledrejection' });
      return nativeInvoke(command, args) as Promise<T>;
    };
    const off = installWindowCrashCapture({ invoke, target: view.host });
    expect([...view.events.keys()].sort()).toEqual(['error', 'pagehide', 'unhandledrejection']);
    (view.events.get('error') as unknown as (event: unknown) => void)({ message: 'private document' });
    expect(recorder.snapshot()[0].event).toBe('window.error');
    expect(recorder.text()).not.toContain('private');
    expect(nativeInvoke).toHaveBeenCalledOnce();
    enabled = false;
    recorder.setEnabled(false);
    (view.events.get('unhandledrejection') as unknown as (event: unknown) => void)({ reason: 'private' });
    expect(nativeInvoke).toHaveBeenCalledOnce();
    expect(recorder.snapshot()).toHaveLength(1);
    off();
    recorder.dispose();
    expect(view.events.size).toBe(0);
  });
});
