import { describe, expect, it, vi } from 'vitest';
import { assertPaintWithin, awaitPaintScript, beginPaintScript, measurePaint } from './paint.js';
import type { PaintMetrics, QaBrowser } from './paint.js';

function fakeBrowser(asyncResult: unknown): QaBrowser & { scripts: string[]; asyncScripts: string[] } {
  const scripts: string[] = [];
  const asyncScripts: string[] = [];
  return {
    scripts,
    asyncScripts,
    async execute<T>(script: string): Promise<T> {
      scripts.push(script);
      return true as unknown as T;
    },
    async executeAsync<T>(script: string): Promise<T> {
      asyncScripts.push(script);
      return asyncResult as T;
    },
  };
}

describe('paint measurement', () => {
  it('marks the start before running the trigger, so no mutation is missed', async () => {
    const browser = fakeBrowser({ durationMs: 42, settleMs: 10, mutations: 3, animationFrames: true });
    const order: string[] = [];
    const trigger = vi.fn(async () => { order.push('trigger'); });
    const originalExecute = browser.execute.bind(browser);
    browser.execute = async <T>(script: string): Promise<T> => {
      if (script.includes('marks.set')) order.push('begin');
      return originalExecute<T>(script);
    };

    await measurePaint(browser, trigger, { requestId: 'req-1' });

    expect(order).toEqual(['begin', 'trigger']);
    expect(trigger).toHaveBeenCalledOnce();
  });

  it('reports the painted duration, not the trigger duration', async () => {
    const browser = fakeBrowser({ durationMs: 137.4, settleMs: 20, mutations: 12, animationFrames: true });
    const metrics = await measurePaint(browser, async () => {}, { requestId: 'req-2' });

    expect(metrics).toMatchObject({ requestId: 'req-2', durationMs: 137.4, mutations: 12, animationFrames: true });
  });

  it('waits two animation frames rather than one', () => {
    const script = awaitPaintScript('req-3', 100, 5_000);
    expect(script.match(/requestAnimationFrame/g)).toHaveLength(2);
    expect(script).toMatch(/requestAnimationFrame\(function \(\) \{ requestAnimationFrame/);
  });

  it('reports honestly when the document is hidden and frames never fire', () => {
    expect(awaitPaintScript('req-4', 100, 5_000)).toMatch(/document\.hidden.*finish\(false\)/s);
  });

  it('scopes the mutation observer to the requested subtree', () => {
    expect(beginPaintScript('req-5', '#viewer')).toContain('document.querySelector("#viewer")');
    expect(beginPaintScript('req-5')).toContain('document.documentElement');
  });

  it('fails loudly when the page could not settle instead of returning a number', async () => {
    const browser = fakeBrowser({ error: 'paint did not settle within 5000ms; 900 mutations, last 2ms ago' });
    await expect(measurePaint(browser, async () => {}, { requestId: 'req-6' }))
      .rejects.toThrow(/paint measurement failed for req-6.*did not settle/s);
  });

  it('carries the evidence into a budget failure', () => {
    const metrics: PaintMetrics = { requestId: 'req-7', durationMs: 812.6, settleMs: 400, mutations: 51, animationFrames: true };
    expect(() => assertPaintWithin(metrics, 500, 'open document')).toThrow(/open document painted in 813ms, over its 500ms budget.*51 DOM mutations.*req-7/s);
    expect(assertPaintWithin(metrics, 1_000, 'open document')).toBe(metrics);
  });
});
