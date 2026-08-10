import { describe, expect, it } from 'vitest';
import { findAcrossFrames, resolveUniqueFrame, withContentFrame, withFrame } from './frames.js';
import type { QaFrameBrowser } from './frames.js';

/**
 * A driver over a fake frame tree. `documents` maps a frame path ("" for top) to
 * the number of matches for the selector under test and its child frame count.
 */
function fakeBrowser(documents: Record<string, { matches: number; frames: number }>) {
  let current: number[] = [];
  const visited: string[] = [];
  const browser: QaFrameBrowser & { visited: string[]; current: () => number[] } = {
    visited,
    current: () => current,
    async execute<T>(script: string): Promise<T> {
      const key = current.join('.');
      const document = documents[key] ?? { matches: 0, frames: 0 };
      visited.push(key || 'top');
      return (script.includes('window.frames.length') ? document.frames : document.matches) as unknown as T;
    },
    async switchToFrame(target: number | null | unknown) {
      if (target === null) current = [];
      else current = [...current, Number(target)];
      return undefined;
    },
    async switchToParentFrame() {
      current = current.slice(0, -1);
      return undefined;
    },
  };
  return browser;
}

describe('frame-scoped selectors', () => {
  it('finds matches in nested frames, not only the top document', async () => {
    const browser = fakeBrowser({
      '': { matches: 0, frames: 2 },
      '0': { matches: 0, frames: 0 },
      '1': { matches: 1, frames: 0 },
    });
    await expect(findAcrossFrames(browser, '.page')).resolves.toEqual([{ framePath: [1], count: 1 }]);
  });

  it('rejects a selector that matches both the chrome and the content', async () => {
    const browser = fakeBrowser({
      '': { matches: 1, frames: 1 },
      '0': { matches: 1, frames: 0 },
    });
    await expect(resolveUniqueFrame(browser, '.document-title'))
      .rejects.toThrow(/matches in 2 documents: top document \(1\), frame 0 \(1\).*withFrame/s);
  });

  it('names the selector when nothing matches anywhere', async () => {
    const browser = fakeBrowser({ '': { matches: 0, frames: 0 } });
    await expect(resolveUniqueFrame(browser, '.missing')).rejects.toThrow(/matched no document: \.missing/);
  });

  it('returns to the top document even when the body throws', async () => {
    const browser = fakeBrowser({ '': { matches: 0, frames: 1 }, '0': { matches: 1, frames: 0 } });
    await expect(withFrame(browser, [0], async () => { throw new Error('assertion failed inside frame'); }))
      .rejects.toThrow('assertion failed inside frame');
    expect(browser.current()).toEqual([]);
  });

  it('returns to the top document after a successful body', async () => {
    const browser = fakeBrowser({ '': { matches: 0, frames: 1 }, '0': { matches: 1, frames: 0 } });
    await expect(withFrame(browser, [0], async () => 'ok')).resolves.toBe('ok');
    expect(browser.current()).toEqual([]);
  });

  it('runs the body inside the one document that matches', async () => {
    const browser = fakeBrowser({
      '': { matches: 0, frames: 2 },
      '0': { matches: 0, frames: 0 },
      '1': { matches: 1, frames: 0 },
    });
    const seen = await withContentFrame(browser, '.page', async (framePath) => {
      expect(browser.current()).toEqual([1]);
      return framePath;
    });
    expect(seen).toEqual([1]);
    expect(browser.current()).toEqual([]);
  });

  it('stops descending at the configured depth', async () => {
    const browser = fakeBrowser({
      '': { matches: 0, frames: 1 },
      '0': { matches: 0, frames: 1 },
      '0.0': { matches: 1, frames: 0 },
    });
    await expect(findAcrossFrames(browser, '.deep', { maxDepth: 1 })).resolves.toEqual([]);
    await expect(findAcrossFrames(browser, '.deep', { maxDepth: 2 })).resolves.toEqual([{ framePath: [0, 0], count: 1 }]);
  });
});
