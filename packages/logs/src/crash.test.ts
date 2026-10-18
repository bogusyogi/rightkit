import { describe, expect, it, vi } from 'vitest';
import {
  RIGHTKIT_CRASH_WINDOW_ERROR_COMMAND,
  installWindowCrashCapture,
  scrubWindowCrashText,
  type WindowCrashTarget,
} from './crash.js';

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
    expect(reports[0].message).toBe('non-error rejection');
    expect(reports[1].message).not.toContain('abcdefghijkl1234');
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

// Subset of the shared JS/Rust parity vectors (scrub_parity.rs / index.test.ts)
// that the window-crash scrubber also guarantees.
describe('scrubWindowCrashText parity subset', () => {
  const vectors: Array<[string, string]> = [
    ['Authorization: Bearer abcdefghijklmnop failed', 'Authorization: Bearer [redacted:credential] failed'],
    ['contact person@example.com now', 'contact [redacted:email] now'],
    [
      'GET https://user:pw@example.com/v1/items?key=abc&x=1#frag failed',
      'GET https://example.com/v1/items?[redacted] failed',
    ],
    [
      'token eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.c2lnbmF0dXJlLXBhcnQ done',
      'token [redacted:credential] done',
    ],
    ['key ' + 'sk-' + 'a1b2' + 'c3d4e5f6g7h8 used', 'key [redacted:credential] used'],
    ['crash at /Users/ada/app/src/main.rs:4', 'crash at /Users/<user>/app/src/main.rs:4'],
  ];
  it.each(vectors)('scrubs %s', (input, expected) => {
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
