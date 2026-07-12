import { describe, expect, it, vi } from 'vitest';
import {
  RIGHTKIT_LOGS_CLEAR_COMMAND,
  RIGHTKIT_LOGS_COLLECT_COMMAND,
  RIGHTKIT_LOGS_WRITE_COMMAND,
  createLogsClient,
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
