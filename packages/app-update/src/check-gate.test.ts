import { describe, expect, it, vi } from 'vitest';
import { createCheckGate } from './check-gate.js';

describe('check gate', () => {
  it('serves the cached result inside the window and re-runs after it', async () => {
    let t = 0;
    const gate = createCheckGate<number>({ minIntervalMs: 1000, now: () => t });
    let n = 0;
    const fn = vi.fn(async () => ++n);
    expect(await gate.run(fn)).toBe(1);
    t = 500;
    expect(await gate.run(fn)).toBe(1);
    t = 1000;
    expect(await gate.run(fn)).toBe(2);
  });
  it('force bypasses the cache and reset clears it', async () => {
    const gate = createCheckGate<number>({ minIntervalMs: 1000, now: () => 0 });
    const fn = vi.fn(async () => fn.mock.calls.length);
    await gate.run(fn);
    expect(await gate.run(fn, { force: true })).toBe(2);
    gate.reset();
    expect(await gate.run(fn)).toBe(3);
  });
  it('shares one in-flight run among concurrent callers', async () => {
    const gate = createCheckGate<number>({ minIntervalMs: 0 });
    let release!: (v: number) => void;
    const fn = vi.fn(() => new Promise<number>((r) => { release = r; }));
    const a = gate.run(fn);
    const b = gate.run(fn);
    release(7);
    expect(await a).toBe(7);
    expect(await b).toBe(7);
    expect(fn).toHaveBeenCalledTimes(1);
  });
  it('does not cache failures', async () => {
    const gate = createCheckGate<number>({ minIntervalMs: 1000, now: () => 0 });
    const fn = vi.fn<() => Promise<number>>()
      .mockRejectedValueOnce(new Error('boom'))
      .mockResolvedValueOnce(5);
    await expect(gate.run(fn)).rejects.toThrow('boom');
    expect(await gate.run(fn)).toBe(5);
  });
});
