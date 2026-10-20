import { describe, expect, it, vi } from 'vitest';
import {
  RIGHTKIT_LOGS_CLEAR_COMMAND,
  RIGHTKIT_LOGS_COLLECT_COMMAND,
  RIGHTKIT_LOGS_WRITE_COMMAND,
  createLogsClient,
  scrubLogText,
} from './index.js';
import parityVectors from '../../../crates/rightkit-logs/tests/scrub-parity.v1.json';

interface ParityVector { name: string; input?: string; inputParts?: string[]; expected: string; windowCrash?: boolean }
const parityInput = (v: ParityVector): string => v.input ?? (v.inputParts ?? []).join('');

describe('createLogsClient', () => {
  it('flushes bounded structured events through the standard local command', async () => {
    const invoke = vi.fn(async () => undefined);
    let now = 100;
    const logs = createLogsClient({
      invoke,
      allowedFields: ['surface', 'count'],
      now: () => now++,
      flushIntervalMs: 0,
    });

    logs.info('app_started', { surface: 'shell', count: 2, transcript: 'private' });
    logs.error('render_failed', { surface: 'preview', unknown: 'drop me' });
    await logs.flush();

    expect(invoke).toHaveBeenCalledOnce();
    expect(invoke).toHaveBeenCalledWith(RIGHTKIT_LOGS_WRITE_COMMAND, {
      events: [
        { schema_version: 1, ts_ms: 100, level: 'info', event: 'app_started', surface: 'shell', count: 2 },
        { schema_version: 1, ts_ms: 101, level: 'error', event: 'render_failed', surface: 'preview' },
      ],
    });
  });

  it('drops the oldest queued item at capacity and emits a content-free marker', async () => {
    const invoke = vi.fn(async () => undefined);
    const logs = createLogsClient({
      invoke,
      allowedFields: [],
      maxBufferedEvents: 2,
      now: () => 42,
      flushIntervalMs: 0,
    });

    logs.info('one');
    logs.info('two');
    logs.info('three');
    await logs.flush();

    const events = invoke.mock.calls[0]?.[1]?.events as Array<Record<string, unknown>>;
    expect(events.map((event) => event.event)).toEqual(['logs_dropped', 'two', 'three']);
    expect(events[0]).toMatchObject({ level: 'warn', dropped_count: 1 });
  });

  it('collects and clears through the shared command contract', async () => {
    const invoke = vi.fn(async (command: string) => command === RIGHTKIT_LOGS_COLLECT_COMMAND ? { schema_version: 1 } : undefined);
    const logs = createLogsClient({ invoke, allowedFields: [], flushIntervalMs: 0 });

    await expect(logs.collectDiagnostics()).resolves.toEqual({ schema_version: 1 });
    await logs.clear();

    expect(invoke).toHaveBeenCalledWith(RIGHTKIT_LOGS_COLLECT_COMMAND, undefined);
    expect(invoke).toHaveBeenCalledWith(RIGHTKIT_LOGS_CLEAR_COMMAND, undefined);
  });

  it('drops credential-shaped keys but preserves correlation ids, and scrubs strings', async () => {
    const invoke = vi.fn(async () => undefined);
    const logs = createLogsClient({
      invoke,
      allowedFields: ['session_id', 'request_id', 'trace_id', 'credential', 'private_key', 'session_secret', 'detail'],
      flushIntervalMs: 0,
    });
    logs.info('sync', {
      session_id: 'sess-123',
      request_id: 'req-9',
      trace_id: 'tr-1',
      credential: 'cred-value-123',
      private_key: 'pem-value-123',
      session_secret: 'shh-secret-value',
      detail: 'failed for ada@example.com with Bearer abcdefghijklmnop',
    });
    await logs.flush();
    const event = (invoke.mock.calls[0]?.[1]?.events as Array<Record<string, unknown>>)[0]!;
    expect(event.session_id).toBe('sess-123');
    expect(event.request_id).toBe('req-9');
    expect(event.trace_id).toBe('tr-1');
    expect(event.credential).toBeUndefined();
    expect(event.private_key).toBeUndefined();
    expect(event.session_secret).toBeUndefined();
    expect(event.detail).toBe('failed for [redacted:email] with Bearer [redacted:credential]');
  });

  it('never propagates local write failures into the app', async () => {
    const logs = createLogsClient({
      invoke: vi.fn(async () => { throw new Error('disk unavailable'); }),
      allowedFields: [],
      flushIntervalMs: 0,
    });
    logs.info('app_started');
    await expect(logs.flush()).resolves.toBeUndefined();
  });
});

// Shared JS/Rust parity vectors: `crates/rightkit-logs/tests/scrub-parity.v1.json` is the single source. The Rust suite
// (`crates/rightkit-logs/tests/scrub_parity.rs`) runs every entry against `scrub_text`; this suite runs every
// entry against `scrubLogText`, and `crash.test.ts` runs the `windowCrash` subset against `scrubWindowCrashText`.
describe('scrubLogText parity with Rust scrub_text', () => {
  it('the shared vector file is well formed', () => {
    expect(parityVectors.schema).toBe(1);
    expect(parityVectors.vectors.length).toBeGreaterThanOrEqual(30);
    const names = parityVectors.vectors.map((v) => v.name);
    expect(new Set(names).size).toBe(names.length);
    for (const v of parityVectors.vectors) {
      expect(typeof v.expected).toBe('string');
      expect(typeof v.input === 'string' || Array.isArray(v.inputParts)).toBe(true);
    }
  });
  it.each(parityVectors.vectors.map((v) => [v.name, parityInput(v), v.expected] as const))('%s', (_name, input, expected) => {
    expect(scrubLogText(input)).toBe(expected);
  });
});
