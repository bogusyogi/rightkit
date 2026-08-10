// Selectors must name a document, not just an element.
//
// The app renders untrusted document content inside iframes. A selector run
// against the top-level document therefore searches the CHROME, not the content
// — and because the chrome often contains a same-named element (a toolbar title,
// a placeholder, a template node), the query succeeds and the assertion passes
// while never having looked at the thing under test.
//
// Two rules encoded here:
//   - entering a frame is always paired with leaving it, even when the body throws;
//   - a selector that matches in more than one document is an error, not a
//     coin-flip resolved by document order.

export interface QaFrameBrowser {
  execute<T>(script: string, ...args: unknown[]): Promise<T>;
  switchToFrame(target: number | null | unknown): Promise<unknown>;
  switchToParentFrame(): Promise<unknown>;
}

/** Index path from the top document to a nested frame (`[]` is the top document). */
export type FramePath = readonly number[];

export interface FrameMatch {
  framePath: FramePath;
  count: number;
}

/**
 * Run `body` with the driver focused on `framePath`, then return to the top
 * document. Restoration happens in a finally block: a spec that throws mid-frame
 * used to leave every later selector silently scoped to that iframe.
 */
export async function withFrame<T>(
  browser: QaFrameBrowser,
  framePath: FramePath,
  body: () => Promise<T>,
): Promise<T> {
  await enterFrame(browser, framePath);
  try {
    return await body();
  } finally {
    await browser.switchToFrame(null);
  }
}

async function enterFrame(browser: QaFrameBrowser, framePath: FramePath): Promise<void> {
  await browser.switchToFrame(null);
  for (const index of framePath) {
    if (!Number.isInteger(index) || index < 0) throw new Error(`invalid frame index in path: ${JSON.stringify(framePath)}`);
    await browser.switchToFrame(index);
  }
}

export function countMatchesScript(selector: string): string {
  return `return document.querySelectorAll(${JSON.stringify(selector)}).length;`;
}

export function countFramesScript(): string {
  return 'return window.frames.length;';
}

/**
 * Search every document — top level and nested frames — and report each one that
 * matches. Callers get the ambiguity rather than a single arbitrary winner.
 */
export async function findAcrossFrames(
  browser: QaFrameBrowser,
  selector: string,
  { maxDepth = 4 }: { maxDepth?: number } = {},
): Promise<FrameMatch[]> {
  const matches: FrameMatch[] = [];

  async function visit(framePath: number[], depth: number): Promise<void> {
    await enterFrame(browser, framePath);
    const count = Number(await browser.execute<number>(countMatchesScript(selector)));
    if (count > 0) matches.push({ framePath: [...framePath], count });
    if (depth >= maxDepth) return;
    const frameCount = Number(await browser.execute<number>(countFramesScript()));
    for (let index = 0; index < frameCount; index += 1) {
      await visit([...framePath, index], depth + 1);
    }
  }

  try {
    await visit([], 0);
  } finally {
    await browser.switchToFrame(null);
  }
  return matches;
}

/**
 * Resolve the one document a selector is allowed to match. Ambiguity fails with
 * both locations named, because "it matched somewhere" is what let chrome-level
 * matches stand in for content-level ones.
 */
export async function resolveUniqueFrame(
  browser: QaFrameBrowser,
  selector: string,
  options: { maxDepth?: number } = {},
): Promise<FramePath> {
  const matches = await findAcrossFrames(browser, selector, options);
  if (matches.length === 0) throw new Error(`selector matched no document: ${selector}`);
  if (matches.length > 1) {
    const where = matches.map((match) => `${describeFrame(match.framePath)} (${match.count})`).join(', ');
    throw new Error(
      `selector ${selector} matches in ${matches.length} documents: ${where}. `
      + 'Scope it with withFrame() so the assertion names the document it means.',
    );
  }
  return matches[0].framePath;
}

/** Run `body` inside the single document that matches `selector`. */
export async function withContentFrame<T>(
  browser: QaFrameBrowser,
  selector: string,
  body: (framePath: FramePath) => Promise<T>,
  options: { maxDepth?: number } = {},
): Promise<T> {
  const framePath = await resolveUniqueFrame(browser, selector, options);
  return withFrame(browser, framePath, () => body(framePath));
}

export function describeFrame(framePath: FramePath): string {
  return framePath.length === 0 ? 'top document' : `frame ${framePath.join('.')}`;
}
