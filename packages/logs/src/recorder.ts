export type DiagnosticKind = 'load' | 'event' | 'error' | 'rejection' | 'perf';

export interface DiagnosticInput {
  kind: DiagnosticKind;
  event: string;
  duration_ms?: number;
}

export interface DiagnosticEntry extends DiagnosticInput {
  schema_version: 1;
  ts_ms: number;
}

export type PersistenceResult = 'ok' | 'disabled' | 'unavailable' | 'failed' | 'disposed';

export interface DiagnosticsRecorderConfig {
  storageKey: string;
  /** App-owned static labels only; never derive these from user content. */
  allowedEvents: readonly string[];
  legacyKeys?: readonly string[];
  storage?: Pick<Storage, 'getItem' | 'setItem' | 'removeItem'>;
  enabled?: boolean;
  now?: () => number;
}

export interface DiagnosticsRecorder {
  record(input: DiagnosticInput): boolean;
  snapshot(): DiagnosticEntry[];
  text(): string;
  setEnabled(enabled: boolean): PersistenceResult;
  flush(): PersistenceResult;
  clear(): PersistenceResult;
  dispose(): PersistenceResult;
}

const MAX_ENTRIES = 80;
const MAX_STORAGE_BYTES = 64 * 1024;
const EVENT_PATTERN = /^[a-zA-Z0-9_.:-]+$/;

function safeInteger(value: unknown): value is number {
  return typeof value === 'number' && Number.isSafeInteger(value) && value >= 0;
}

function eventName(value: unknown): value is string {
  return typeof value === 'string' && value.length <= 96 && EVENT_PATTERN.exec(value)?.[0] === value;
}

function kindName(value: unknown): value is DiagnosticKind {
  return value === 'load' || value === 'event' || value === 'error'
    || value === 'rejection' || value === 'perf';
}

/** Count UTF-8 bytes without allocating another copy of untrusted storage text. */
function withinStorageLimit(raw: string): boolean {
  if (raw.length > MAX_STORAGE_BYTES) return false;
  let bytes = 0;
  for (let i = 0; i < raw.length; i += 1) {
    const code = raw.charCodeAt(i);
    if (code < 0x80) bytes += 1;
    else if (code < 0x800) bytes += 2;
    else if (code >= 0xd800 && code <= 0xdbff
      && i + 1 < raw.length && raw.charCodeAt(i + 1) >= 0xdc00 && raw.charCodeAt(i + 1) <= 0xdfff) {
      bytes += 4;
      i += 1;
    } else bytes += 3;
    if (bytes > MAX_STORAGE_BYTES) return false;
  }
  return true;
}

/** No browser access, listeners, or storage work occurs until this factory is called. */
export function createDiagnosticsRecorder(config: DiagnosticsRecorderConfig): DiagnosticsRecorder {
  if (!config || typeof config.storageKey !== 'string' || config.storageKey.length === 0) {
    throw new TypeError('Invalid diagnostics storage key');
  }
  if (!Array.isArray(config.allowedEvents) || ![...config.allowedEvents].every(eventName)) {
    throw new TypeError('Invalid diagnostics event allowlist');
  }
  if (config.legacyKeys !== undefined && (!Array.isArray(config.legacyKeys)
    || ![...config.legacyKeys].every((key) => typeof key === 'string' && key.length > 0 && key !== config.storageKey))) {
    throw new TypeError('Invalid diagnostics legacy keys');
  }
  if (config.enabled !== undefined && typeof config.enabled !== 'boolean') {
    throw new TypeError('Invalid diagnostics enabled value');
  }
  if (config.now !== undefined && typeof config.now !== 'function') {
    throw new TypeError('Invalid diagnostics clock');
  }
  if (config.storage !== undefined) {
    try {
      if (!config.storage || typeof config.storage.getItem !== 'function'
        || typeof config.storage.setItem !== 'function' || typeof config.storage.removeItem !== 'function') {
        throw new Error();
      }
    } catch {
      throw new TypeError('Invalid diagnostics storage adapter');
    }
  }

  const storageKey = config.storageKey;
  const allowed = new Set(config.allowedEvents);
  const legacyKeys = [...new Set(config.legacyKeys ?? [])];
  const now = config.now ?? Date.now;
  const suppliedStorage = config.storage;
  let enabled = config.enabled ?? true;
  let disposed = false;
  let loaded = false;
  let ring: DiagnosticEntry[] = [];
  let dirty = false;
  let legacyRemoved = legacyKeys.length === 0;
  let lastKey: string | undefined;
  let lastAt = 0;
  let cancelScheduled: (() => void) | undefined;
  let host: Window | undefined;
  let doc: Document | undefined;
  try { if (typeof window !== 'undefined') host = window; } catch { /* unavailable host */ }
  try { doc = host?.document; } catch { /* unavailable document */ }

  function storage(): DiagnosticsRecorderConfig['storage'] {
    if (suppliedStorage !== undefined) return suppliedStorage;
    try { return host?.localStorage; } catch { return undefined; }
  }

  function project(value: unknown, ts: unknown, legacy = false): DiagnosticEntry | undefined {
    if (!value || typeof value !== 'object' || !safeInteger(ts)) return undefined;
    const input = value as Record<string, unknown>;
    const kind = input.kind;
    const event = legacy ? input.msg : input.event;
    if (!kindName(kind) || !eventName(event) || !allowed.has(event)) return undefined;
    const duration = legacy ? undefined : input.duration_ms;
    if (duration !== undefined && (kind !== 'perf' || !safeInteger(duration))) return undefined;
    return duration === undefined
      ? { schema_version: 1, ts_ms: ts, kind, event }
      : { schema_version: 1, ts_ms: ts, kind, event, duration_ms: duration };
  }

  function restore(): PersistenceResult {
    if (loaded) return 'ok';
    const target = storage();
    if (!target) return 'unavailable';
    let raw: string | null;
    try {
      raw = target.getItem(storageKey);
      if (raw === null) {
        for (const key of legacyKeys) {
          raw = target.getItem(key);
          if (raw !== null) break;
        }
      }
    } catch { return 'failed'; }
    // A failed read remains unresolved; pending accepted entries stay in memory
    // until a successful read can recover existing history before any write.
    loaded = true;
    try {
      if (raw === null || !withinStorageLimit(raw)) return 'ok';
      const parsed: unknown = JSON.parse(raw);
      const legacy = Array.isArray(parsed);
      let entries: unknown[];
      if (legacy) entries = parsed;
      else if (parsed && typeof parsed === 'object'
        && (parsed as Record<string, unknown>).schema_version === 1
        && Array.isArray((parsed as Record<string, unknown>).entries)) {
        entries = (parsed as { entries: unknown[] }).entries;
      } else return 'ok';
      const restored: DiagnosticEntry[] = [];
      for (const value of entries) {
        if (!value || typeof value !== 'object') continue;
        const item = value as Record<string, unknown>;
        if (!legacy && item.schema_version !== 1) continue;
        const ts = legacy && typeof item.t === 'string' ? Date.parse(item.t) : legacy ? undefined : item.ts_ms;
        const entry = project(value, ts, legacy);
        if (entry) {
          restored.push(entry);
          if (restored.length > MAX_ENTRIES) restored.shift();
        }
      }
      ring = [...restored, ...ring].slice(-MAX_ENTRIES);
      // Rewrite only projected data, including removal of unknown persisted fields.
      dirty = true;
    } catch { /* malformed data is empty; pending accepted entries remain intact */ }
    return 'ok';
  }

  function cancel(): void {
    const pending = cancelScheduled;
    cancelScheduled = undefined;
    try { pending?.(); } catch { /* callbacks also check enabled/disposed */ }
  }

  function flush(): PersistenceResult {
    if (disposed) return 'disposed';
    cancel();
    if (!enabled) return 'disabled';
    const restored = restore();
    if (restored !== 'ok') return restored;
    const target = storage();
    if (!target) return 'unavailable';
    if (!dirty) return 'ok';
    try {
      target.setItem(storageKey, JSON.stringify({ schema_version: 1, entries: ring }));
      if (!legacyRemoved) {
        let failed = false;
        for (const key of legacyKeys) {
          try { target.removeItem(key); } catch { failed = true; }
        }
        if (failed) return 'failed';
        legacyRemoved = true;
      }
      dirty = false;
      return 'ok';
    } catch { return 'failed'; }
  }

  function schedule(): void {
    if (cancelScheduled || !enabled || disposed) return;
    if (!host) { flush(); return; }
    // Invalidate canceled callbacks even if the host cannot cancel them.
    let active = true;
    const run = () => {
      if (!active) return;
      cancelScheduled = undefined;
      if (enabled && !disposed) flush();
    };
    try {
      if (typeof host.requestIdleCallback === 'function' && typeof host.cancelIdleCallback === 'function') {
        const id = host.requestIdleCallback(run, { timeout: 2000 });
        cancelScheduled = () => { active = false; host?.cancelIdleCallback(id); };
      } else {
        const id = host.setTimeout(run, 1000);
        cancelScheduled = () => { active = false; host?.clearTimeout(id); };
      }
    } catch { active = false; /* retain dirty data for explicit/lifecycle/record retry */ }
  }

  function record(input: DiagnosticInput): boolean {
    if (!enabled || disposed) return false;
    let entry: DiagnosticEntry | undefined;
    try {
      // Validate labels and duration before consulting storage or modifying memory.
      entry = project(input, 0);
      if (!entry) return false;
      const ts = now();
      if (!safeInteger(ts)) return false;
      entry.ts_ms = ts;
    } catch { return false; }
    const key = `${entry.kind}:${entry.event}`;
    const delta = entry.ts_ms - lastAt;
    if (key === lastKey && delta >= 0 && delta < 1000) {
      if (dirty) schedule();
      return false;
    }
    restore();
    lastKey = key;
    lastAt = entry.ts_ms;
    ring.push(entry);
    if (ring.length > MAX_ENTRIES) ring.shift();
    dirty = true;
    if (entry.kind === 'error' || entry.kind === 'rejection' || entry.kind === 'load') flush();
    else schedule();
    return true;
  }

  function snapshot(): DiagnosticEntry[] {
    if (disposed) return [];
    restore();
    return ring.map((entry) => ({ ...entry }));
  }

  function setEnabled(value: boolean): PersistenceResult {
    if (disposed) return 'disposed';
    if (typeof value !== 'boolean') throw new TypeError('Invalid diagnostics enabled value');
    if (!value) {
      const result = enabled ? flush() : 'disabled';
      cancel();
      enabled = false;
      return result;
    }
    if (!enabled) { lastKey = undefined; lastAt = 0; }
    enabled = true;
    return 'ok';
  }

  function clear(): PersistenceResult {
    if (disposed) return 'disposed';
    cancel();
    loaded = true;
    ring = [];
    dirty = false;
    lastKey = undefined;
    lastAt = 0;
    const target = storage();
    if (!target) return 'unavailable';
    let failed = false;
    for (const key of [storageKey, ...legacyKeys]) {
      try { target.removeItem(key); } catch { failed = true; }
    }
    legacyRemoved = !failed;
    return failed ? 'failed' : 'ok';
  }

  const onPageHide = () => { flush(); };
  const onVisibility = () => { if (doc?.visibilityState === 'hidden') flush(); };
  try { host?.addEventListener('pagehide', onPageHide); } catch { /* optional host */ }
  try { doc?.addEventListener('visibilitychange', onVisibility); } catch { /* optional host */ }

  function dispose(): PersistenceResult {
    if (disposed) return 'disposed';
    const result = enabled ? flush() : 'disabled';
    disposed = true;
    cancel();
    try { host?.removeEventListener('pagehide', onPageHide); } catch { /* optional host */ }
    try { doc?.removeEventListener('visibilitychange', onVisibility); } catch { /* optional host */ }
    ring = [];
    dirty = false;
    lastKey = undefined;
    return result;
  }

  return { record, snapshot, text: () => JSON.stringify(snapshot(), null, 2), setEnabled, flush, clear, dispose };
}
