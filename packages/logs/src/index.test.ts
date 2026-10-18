import { describe, expect, it, vi } from 'vitest';
import {
  RIGHTKIT_LOGS_CLEAR_COMMAND,
  RIGHTKIT_LOGS_COLLECT_COMMAND,
  RIGHTKIT_LOGS_WRITE_COMMAND,
  createLogsClient,
  scrubLogText,
} from './index.js';

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

// Shared JS/Rust parity vectors: identical pairs run in
// `crates/rightkit-logs/tests/scrub_parity.rs` against Rust `scrub_text`.
describe('scrubLogText parity with Rust scrub_text', () => {
  const vectors: Array<[string, string]> = [
    ['Authorization: Bearer abcdefghijklmnop failed', 'Authorization: Bearer [redacted:credential] failed'],
    ['contact person@example.com now', 'contact [redacted:email] now'],
    [
      'GET https://user:pw@example.com/v1/items?key=abc&x=1#frag failed',
      'GET https://example.com/v1/items?[redacted] failed',
    ],
    ['callback code=4/abc&state=xyz failed', 'callback code=[redacted]&state=[redacted] failed'],
    [
      'token eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.c2lnbmF0dXJlLXBhcnQ done',
      'token [redacted:credential] done',
    ],
    ['key ' + 'sk-' + 'a1b2' + 'c3d4e5f6g7h8 used', 'key [redacted:credential] used'],
    ['crash at /Users/ada/app/src/main.rs:4', 'crash at /Users/<user>/app/src/main.rs:4'],
  ];
  it.each(vectors)('scrubs %s', (input, expected) => {
    expect(scrubLogText(input)).toBe(expected);
  });
});
