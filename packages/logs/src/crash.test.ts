import { describe, expect, it, vi } from 'vitest';
import {
  RIGHTKIT_CRASH_WINDOW_ERROR_COMMAND,
  installWindowCrashCapture,
  scrubWindowCrashText,
  type WindowCrashTarget,
} from './crash.js';
import parityVectors from '../../../crates/rightkit-logs/tests/scrub-parity.v1.json';

interface ParityVector { name: string; input?: string; inputParts?: string[]; expected: string; windowCrash?: boolean }
const parityInput = (v: ParityVector): string => v.input ?? (v.inputParts ?? []).join('');

function fakeTarget() {
  const handlers = new Map<string, (e: unknown) => void>();
  const target: WindowCrashTarget = {
    addEventListener: (t, l) => void handlers.set(t, l),
    removeEventListener: (t) => void handlers.delete(t),
  };
  return { target, handlers };
}

describe('installWindowCrashCapture', () => {
  it('installs nothing until called and uninstalls cleanly', () => {
    const { target, handlers } = fakeTarget();
    expect(handlers.size).toBe(0);
    const off = installWindowCrashCapture({ invoke: vi.fn(async () => undefined), target });
    expect(handlers.size).toBe(2);
    off();
    expect(handlers.size).toBe(0);
  });

  it('scrubs, caps and sends an error event', () => {
    const invoke = vi.fn(async () => undefined);
    const { target, handlers } = fakeTarget();
    installWindowCrashCapture({ invoke, target, redactValues: ['my-db-key'], maxMessageChars: 40 });
    handlers.get('error')!({
      error: new Error('failed my-db-key for bob@example.com ' + 'x'.repeat(200)),
      filename: 'app://localhost/assets/index.js?token=abc',
      lineno: 7,
      colno: 3,
    });
    expect(invoke).toHaveBeenCalledOnce();
    const [command, args] = invoke.mock.calls[0] as unknown as [string, { report: any }];
    expect(command).toBe(RIGHTKIT_CRASH_WINDOW_ERROR_COMMAND);
    expect(args.report.kind).toBe('error');
    expect(args.report.message.length).toBeLessThanOrEqual(40);
    expect(args.report.message).not.toContain('my-db-key');
    expect(args.report.source).toBe('index.js:7:3');
  });

  it('records rejections and tolerates non-error reasons', () => {
    const invoke = vi.fn(async () => undefined);
    const { target, handlers } = fakeTarget();
    installWindowCrashCapture({ invoke, target });
    handlers.get('unhandledrejection')!({ reason: { weird: true } });
    handlers.get('unhandledrejection')!({ reason: 'Bearer abcdefghijkl1234' });
    const reports = invoke.mock.calls.map((c) => (c as unknown as [string, { report: any }])[1].report);
    expect(reports[0].message).toBe('{"weird":true}');
    expect(reports[1].message).not.toContain('abcdefghijkl1234');
  });

  it('preserves structured codes & describes primitive rejection reasons', () => {
    const invoke = vi.fn(async () => undefined);
    const { target, handlers } = fakeTarget();
    installWindowCrashCapture({ invoke, target, maxPerWindow: 20 });
    for (const reason of [{ code: 'E_OPEN', message: 'Cannot open' }, 42, false, null, undefined, 12n]) {
      handlers.get('unhandledrejection')!({ reason });
    }
    const messages = invoke.mock.calls.map((call) => (call as unknown as [string, { report: any }])[1].report.message);
    expect(messages).toEqual(['E_OPEN: Cannot open', '42', 'false', 'null', 'undefined', '12n']);
  });

  it('formats cycles & nested own data without executing getters or conversion hooks', () => {
    const invoke = vi.fn(async () => undefined);
    const { target, handlers } = fakeTarget();
    installWindowCrashCapture({ invoke, target });
    const accessor = vi.fn(() => { throw new Error('getter called'); });
    const toJSON = vi.fn(() => { throw new Error('toJSON called'); });
    const toString = vi.fn(() => { throw new Error('toString called'); });
    const reason = Object.assign(Object.create({ inherited: 'exclude' }), {
      nested: { status: 'failed' }, toJSON, toString,
    });
    reason.self = reason;
    Object.defineProperty(reason, 'danger', { enumerable: true, get: accessor });
    handlers.get('unhandledrejection')!({ reason });
    const report = (invoke.mock.calls[0] as unknown as [string, { report: any }])[1].report;
    expect(report.message).toContain('"nested":{"status":"failed"}');
    expect(report.message).toContain('[circular]');
    expect(report.message).toContain('[accessor]');
    expect(report.message).not.toContain('inherited');
    expect(accessor).not.toHaveBeenCalled();
    expect(toJSON).not.toHaveBeenCalled();
    expect(toString).not.toHaveBeenCalled();
  });

  it('contains throwing Error properties, event getters & hostile proxies', () => {
    const invoke = vi.fn(async () => undefined);
    const { target, handlers } = fakeTarget();
    installWindowCrashCapture({ invoke, target, maxPerWindow: 20 });
    const error = new Error('hidden');
    void error.stack;
    for (const key of ['name', 'message', 'stack']) {
      Object.defineProperty(error, key, { get() { throw new Error('private'); } });
    }
    const proxy = new Proxy({}, {
      ownKeys() { throw new Error('private'); },
      getOwnPropertyDescriptor() { throw new Error('private'); },
      getPrototypeOf() { throw new Error('private'); },
      get() { throw new Error('private'); },
    });
    const revoked = Proxy.revocable({}, {});
    revoked.revoke();
    for (const reason of [error, proxy, revoked.proxy]) {
      expect(() => handlers.get('unhandledrejection')!({ reason })).not.toThrow();
    }
    expect(() => handlers.get('error')!(proxy)).not.toThrow();
    expect(() => handlers.get('unhandledrejection')!(proxy)).not.toThrow();
    expect(invoke).toHaveBeenCalledTimes(5);
    expect(JSON.stringify(invoke.mock.calls)).not.toContain('private');
  });

  it('bounds width, depth, nodes & raw string admission with whole-value markers', () => {
    const invoke = vi.fn(async () => undefined);
    const { target, handlers } = fakeTarget();
    installWindowCrashCapture({ invoke, target, maxMessageChars: 100000, maxPerWindow: 20 });
    const wide = Object.fromEntries(Array.from({ length: 20 }, (_, i) => [`item${i}`, i]));
    const deep = { a: { b: { c: { d: 'unvisited' } } } };
    const nodes = Object.fromEntries(Array.from({ length: 8 }, (_, i) => [`group${i}`,
      Object.fromEntries(Array.from({ length: 8 }, (_, j) => [`child${j}`, j]))]));
    for (const reason of [wide, deep, nodes, 'raw-secret'.repeat(8000)]) {
      handlers.get('unhandledrejection')!({ reason });
    }
    const messages = invoke.mock.calls.map((call) => (call as unknown as [string, { report: any }])[1].report.message);
    expect(messages[0]).toContain('[width limit]');
    expect(messages[0]).not.toContain('item8');
    expect(messages[1]).toContain('[depth limit]');
    expect(messages[1]).not.toContain('unvisited');
    expect(messages[2]).toContain('[node limit]');
    expect(messages[3]).toBe('[oversized value]');
    expect(messages[3]).not.toContain('raw-secret');
    expect(messages.every((message) => message.length < 65536)).toBe(true);
  });

  it('scrubs intact values before escaping or truncating, including oversize & source edges', () => {
    const invoke = vi.fn(async () => undefined);
    const { target, handlers } = fakeTarget();
    const secret = 'private"\\line\nsecret';
    installWindowCrashCapture({ invoke, target, redactValues: [secret], maxMessageChars: 45, maxPerWindow: 20 });
    handlers.get('unhandledrejection')!({ reason: { nested: secret } });
    handlers.get('unhandledrejection')!({ reason: 'x'.repeat(40) + secret });
    handlers.get('unhandledrejection')!({ reason: 'x'.repeat(65530) + secret });
    handlers.get('error')!({ message: 'failure', filename: 'z'.repeat(115) + secret });
    const reports = invoke.mock.calls.map((call) => (call as unknown as [string, { report: any }])[1].report);
    expect(reports[0].message).toBe('{"nested":"[redacted]"}');
    expect(reports[1].message).toBe('x'.repeat(40) + '[reda');
    expect(reports[2].message).toBe('[oversized value]');
    expect(reports[3].source).toBe('z'.repeat(115) + '[reda');
    expect(JSON.stringify(reports)).not.toContain('private');
  });

  it('rate limits per window and in total', () => {
    const invoke = vi.fn(async () => undefined);
    const { target, handlers } = fakeTarget();
    let t = 0;
    installWindowCrashCapture({ invoke, target, maxPerWindow: 2, windowMs: 100, maxTotal: 3, now: () => t });
    for (let i = 0; i < 5; i++) handlers.get('error')!({ message: 'm' });
    expect(invoke).toHaveBeenCalledTimes(2);
    t = 200;
    for (let i = 0; i < 5; i++) handlers.get('error')!({ message: 'm' });
    expect(invoke).toHaveBeenCalledTimes(3);
  });

  it('never throws when invoke fails', async () => {
    const { target, handlers } = fakeTarget();
    installWindowCrashCapture({ invoke: () => { throw new Error('boom'); }, target });
    expect(() => handlers.get('error')!({ message: 'x' })).not.toThrow();
    const { target: t2, handlers: h2 } = fakeTarget();
    installWindowCrashCapture({ invoke: async () => { throw new Error('boom'); }, target: t2 });
    expect(() => h2.get('error')!({ message: 'x' })).not.toThrow();
  });
});

// The `windowCrash` subset of the shared JS/Rust parity vectors (`crates/rightkit-logs/tests/scrub-parity.v1.json`, also run by
// `index.test.ts` against `scrubLogText` and by `scrub_parity.rs` against Rust `scrub_text`) that the window-crash
// scrubber also guarantees.
describe('scrubWindowCrashText parity subset', () => {
  const subset = parityVectors.vectors.filter((v) => v.windowCrash === true);
  it('the subset is not empty', () => {
    expect(subset.length).toBeGreaterThanOrEqual(6);
  });
  it.each(subset.map((v) => [v.name, parityInput(v), v.expected] as const))('%s', (_name, input, expected) => {
    expect(scrubWindowCrashText(input)).toBe(expected);
  });
});

describe('scrubWindowCrashText', () => {
  it('removes home paths, jwt and long hex', () => {
    const out = scrubWindowCrashText('/Users/bob/x eyJabcdefghi.jklmnopqr.stu ' + 'a'.repeat(40));
    expect(out).not.toContain('bob');
    expect(out).not.toContain('eyJ');
    expect(out).not.toContain('aaaaaaaa');
  });
});
