export const RIGHTKIT_LOGS_WRITE_COMMAND = 'rightkit_logs_write';
export const RIGHTKIT_LOGS_COLLECT_COMMAND = 'rightkit_logs_collect';
export const RIGHTKIT_LOGS_CLEAR_COMMAND = 'rightkit_logs_clear';

export type LogLevel = 'trace' | 'debug' | 'info' | 'warn' | 'error';
export type LogFieldValue = string | number | boolean | null | LogFieldValue[] | { [key: string]: LogFieldValue };
export type LogFields = Record<string, unknown>;

export interface StructuredLogEvent {
  schema_version: 1;
  ts_ms: number;
  level: LogLevel;
  event: string;
  [field: string]: unknown;
}

export type LogsInvoke = <T = unknown>(
  command: string,
  args?: Record<string, unknown>,
) => Promise<T>;

export interface LogsClientConfig {
  /** Tauri invoke adapter. Injected so this package has no Tauri or network dependency. */
  invoke: LogsInvoke;
  /** Closed, app-owned set of content-free field names allowed onto disk. */
  allowedFields: readonly string[];
  /** Delivery queue bound. Oldest events are dropped with a content-free marker. */
  maxBufferedEvents?: number;
  /** Flush cadence. Set to 0 when the host flushes explicitly. */
  flushIntervalMs?: number;
  /** Injectable clock for deterministic tests. */
  now?: () => number;
}

export interface LogsClient {
  log(level: LogLevel, event: string, fields?: LogFields): void;
  trace(event: string, fields?: LogFields): void;
  debug(event: string, fields?: LogFields): void;
  info(event: string, fields?: LogFields): void;
  warn(event: string, fields?: LogFields): void;
  error(event: string, fields?: LogFields): void;
  flush(): Promise<void>;
  collectDiagnostics<T = unknown>(): Promise<T>;
  clear(): Promise<void>;
}

const RESERVED_FIELDS = new Set(['schema_version', 'ts_ms', 'level', 'event']);
const SENSITIVE_EXACT = new Set([
  'api_key', 'authorization', 'body', 'clipboard', 'completion', 'content',
  'cookie', 'message', 'password', 'prompt', 'recognized_text', 'refresh_token',
  'transcript', 'window_title',
]);
const SENSITIVE_FRAGMENTS = ['secret', 'password', 'api_key', 'token'];

function sensitiveKey(key: string): boolean {
  const normalized = key.toLowerCase();
  return SENSITIVE_EXACT.has(normalized)
    || SENSITIVE_FRAGMENTS.some((fragment) => normalized.includes(fragment));
}

function sanitizeValue(value: unknown, allowed: Set<string>): LogFieldValue | undefined {
  if (value === null || typeof value === 'number' || typeof value === 'boolean') return value;
  if (typeof value === 'string') return value.slice(0, 200);
  if (Array.isArray(value)) {
    return value
      .slice(0, 16)
      .map((item) => sanitizeValue(item, allowed))
      .filter((item): item is LogFieldValue => item !== undefined);
  }
  if (typeof value === 'object') {
    const output: Record<string, LogFieldValue> = {};
    for (const [key, child] of Object.entries(value as Record<string, unknown>)) {
      if (!allowed.has(key) || RESERVED_FIELDS.has(key) || sensitiveKey(key)) continue;
      const sanitized = sanitizeValue(child, allowed);
      if (sanitized !== undefined) output[key] = sanitized;
    }
    return output;
  }
  return undefined;
}

function sanitizeFields(fields: LogFields | undefined, allowed: Set<string>): Record<string, LogFieldValue> {
  const output: Record<string, LogFieldValue> = {};
  if (!fields) return output;
  for (const [key, value] of Object.entries(fields)) {
    if (!allowed.has(key) || RESERVED_FIELDS.has(key) || sensitiveKey(key)) continue;
    const sanitized = sanitizeValue(value, allowed);
    if (sanitized !== undefined) output[key] = sanitized;
  }
  return output;
}

function safeEventName(event: string): string {
  const normalized = event.trim().slice(0, 96);
  return /^[a-zA-Z0-9_.:-]+$/.test(normalized) ? normalized : 'invalid_event_name';
}

export function createLogsClient(config: LogsClientConfig): LogsClient {
  const allowedFields = new Set(config.allowedFields);
  const maxBufferedEvents = Math.max(1, config.maxBufferedEvents ?? 256);
  const flushIntervalMs = Math.max(0, config.flushIntervalMs ?? 250);
  const now = config.now ?? Date.now;
  let queue: StructuredLogEvent[] = [];
  let dropped = 0;
  let timer: ReturnType<typeof setTimeout> | undefined;
  let activeFlush: Promise<void> | undefined;

  function cancelTimer(): void {
    if (timer !== undefined) clearTimeout(timer);
    timer = undefined;
  }

  function scheduleFlush(): void {
    if (flushIntervalMs === 0 || timer !== undefined) return;
    timer = setTimeout(() => {
      timer = undefined;
      void flush();
    }, flushIntervalMs);
  }

  function log(level: LogLevel, event: string, fields?: LogFields): void {
    if (queue.length >= maxBufferedEvents) {
      queue.shift();
      dropped += 1;
    }
    queue.push({
      schema_version: 1,
      ts_ms: now(),
      level,
      event: safeEventName(event),
      ...sanitizeFields(fields, allowedFields),
    });
    scheduleFlush();
  }

  async function flushBatch(): Promise<void> {
    cancelTimer();
    if (queue.length === 0 && dropped === 0) return;
    const events = queue;
    queue = [];
    if (dropped > 0) {
      events.unshift({
        schema_version: 1,
        ts_ms: now(),
        level: 'warn',
        event: 'logs_dropped',
        dropped_count: dropped,
      });
      dropped = 0;
    }
    try {
      await config.invoke(RIGHTKIT_LOGS_WRITE_COMMAND, { events });
    } catch {
      // Local logging must never break the user action that emitted the event.
    }
  }

  function flush(): Promise<void> {
    if (activeFlush) return activeFlush;
    activeFlush = flushBatch().finally(() => {
      activeFlush = undefined;
      if (queue.length > 0) scheduleFlush();
    });
    return activeFlush;
  }

  async function collectDiagnostics<T = unknown>(): Promise<T> {
    await flush();
    return config.invoke<T>(RIGHTKIT_LOGS_COLLECT_COMMAND, undefined);
  }

  async function clear(): Promise<void> {
    cancelTimer();
    queue = [];
    dropped = 0;
    await config.invoke(RIGHTKIT_LOGS_CLEAR_COMMAND, undefined);
  }

  return {
    log,
    trace: (event, fields) => log('trace', event, fields),
    debug: (event, fields) => log('debug', event, fields),
    info: (event, fields) => log('info', event, fields),
    warn: (event, fields) => log('warn', event, fields),
    error: (event, fields) => log('error', event, fields),
    flush,
    collectDiagnostics,
    clear,
  };
}
export * from './crash.js';
