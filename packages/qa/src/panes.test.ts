import { describe, expect, it } from 'vitest';
import { assertActivePane, readPaneGeometry, waitForPaneResize } from './panes.js';
import type { PaneGeometry, QaGeometryBrowser } from './panes.js';

function pane(overrides: Partial<PaneGeometry> = {}): PaneGeometry {
  return {
    selector: '.editor-pane',
    x: 0,
    y: 0,
    width: 800,
    height: 600,
    visible: true,
    active: true,
    matches: 1,
    viewport: { width: 1280, height: 800, devicePixelRatio: 1 },
    ...overrides,
  };
}

function browserReturning(readings: Array<PaneGeometry | null>): QaGeometryBrowser {
  let index = 0;
  return {
    async execute<T>(): Promise<T> {
      const reading = readings[Math.min(index, readings.length - 1)];
      index += 1;
      if (!reading) return null as unknown as T;
      const { selector: _selector, ...rest } = reading;
      return rest as unknown as T;
    },
  };
}

describe('active pane geometry', () => {
  it('rejects a pane that is in the DOM but not visible', () => {
    expect(() => assertActivePane(pane({ visible: false, width: 0, height: 0 })))
      .toThrow(/not visible \(0x0 at 0,0\).*cannot see/s);
  });

  it('rejects a background tab even when it has a real box', () => {
    expect(() => assertActivePane(pane({ active: false, matches: 3 })))
      .toThrow(/matched 3 panes and the chosen one is not the active pane/);
  });

  it('accepts the visible active pane', () => {
    const geometry = pane();
    expect(assertActivePane(geometry)).toBe(geometry);
  });

  it('names the selector when it matches nothing', async () => {
    await expect(readPaneGeometry(browserReturning([null]), '.gone'))
      .rejects.toThrow(/matched no element: \.gone/);
  });

  it('waits for the pane box to change and then hold still', async () => {
    const browser = browserReturning([
      pane({ width: 800, height: 600 }),
      pane({ width: 1000, height: 600 }),
      pane({ width: 1024, height: 600 }),
      pane({ width: 1024, height: 600 }),
      pane({ width: 1024, height: 600 }),
    ]);
    const settled = await waitForPaneResize(browser, '.editor-pane', { width: 800, height: 600 }, { pollIntervalMs: 1 });
    expect(settled.width).toBe(1024);
  });

  it('times out with both boxes named when the resize never lands', async () => {
    const browser = browserReturning([pane({ width: 800, height: 600 })]);
    await expect(waitForPaneResize(browser, '.editor-pane', { width: 800, height: 600 }, { timeoutMs: 60, pollIntervalMs: 5 }))
      .rejects.toThrow(/did not settle at a new size within 60ms: before 800x600, last 800x600/);
  });

  it('does not accept a resize that landed on an inactive pane', async () => {
    const browser = browserReturning([pane({ width: 1024, active: false, matches: 2 })]);
    await expect(waitForPaneResize(browser, '.editor-pane', { width: 800, height: 600 }, { timeoutMs: 60, pollIntervalMs: 5 }))
      .rejects.toThrow(/not the active pane/);
  });
});
