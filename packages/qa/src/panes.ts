// Assert against the pane the user is actually looking at.
//
// Tab and split-view UIs keep inactive panes in the DOM — hidden, zero-sized, or
// positioned offscreen. A selector that matches "the editor pane" therefore
// resolves just as happily to a background tab, and the resulting assertion
// describes a pane nobody can see. Worse, it is stable: it passes every run, for
// the wrong pane, forever.
//
// So geometry is read together with visibility and activeness, and a resize is
// only considered complete once the ACTIVE pane's box has actually changed and
// settled. Waiting on the window size alone reports success while the pane
// inside it is still laid out at its old dimensions.

export interface QaGeometryBrowser {
  execute<T>(script: string, ...args: unknown[]): Promise<T>;
}

export interface PaneGeometry {
  selector: string;
  x: number;
  y: number;
  width: number;
  height: number;
  visible: boolean;
  active: boolean;
  matches: number;
  viewport: { width: number; height: number; devicePixelRatio: number };
}

export function readPaneGeometryScript(selector: string): string {
  return `
    (function () {
      var nodes = document.querySelectorAll(${JSON.stringify(selector)});
      if (nodes.length === 0) return null;
      // Prefer the node the UI marks as current; fall back to the sole match.
      var chosen = null;
      for (var i = 0; i < nodes.length; i += 1) {
        var node = nodes[i];
        if (node.getAttribute('aria-selected') === 'true'
            || node.getAttribute('aria-current') === 'true'
            || node.dataset.active === 'true'
            || node.classList.contains('active')) { chosen = node; break; }
      }
      if (!chosen) chosen = nodes[0];
      var rect = chosen.getBoundingClientRect();
      var style = getComputedStyle(chosen);
      var hiddenByStyle = style.display === 'none' || style.visibility === 'hidden' || Number(style.opacity) === 0;
      var hiddenByAttr = chosen.hasAttribute('hidden') || chosen.getAttribute('aria-hidden') === 'true';
      return {
        x: rect.x, y: rect.y, width: rect.width, height: rect.height,
        visible: !hiddenByStyle && !hiddenByAttr && rect.width > 0 && rect.height > 0,
        active: chosen.getAttribute('aria-selected') === 'true'
          || chosen.getAttribute('aria-current') === 'true'
          || chosen.dataset.active === 'true'
          || chosen.classList.contains('active')
          || (nodes.length === 1 && !hiddenByStyle && !hiddenByAttr),
        matches: nodes.length,
        viewport: { width: window.innerWidth, height: window.innerHeight, devicePixelRatio: window.devicePixelRatio },
      };
    })();
  `;
}

export async function readPaneGeometry(browser: QaGeometryBrowser, selector: string): Promise<PaneGeometry> {
  const raw = await browser.execute<Omit<PaneGeometry, 'selector'> | null>(readPaneGeometryScript(selector));
  if (!raw) throw new Error(`pane selector matched no element: ${selector}`);
  return { selector, ...raw };
}

/**
 * The pane under test must be the visible, active one. Both halves matter: a
 * hidden pane fails "visible", and a background tab that happens to be laid out
 * fails "active".
 */
export function assertActivePane(geometry: PaneGeometry): PaneGeometry {
  if (!geometry.visible) {
    throw new Error(
      `${geometry.selector} is not visible (${geometry.width}x${geometry.height} at ${geometry.x},${geometry.y}); `
      + 'the assertion would describe a pane the user cannot see',
    );
  }
  if (!geometry.active) {
    throw new Error(
      `${geometry.selector} matched ${geometry.matches} panes and the chosen one is not the active pane; `
      + 'scope the selector to the active tab before asserting',
    );
  }
  return geometry;
}

export interface ResizeSettleOptions {
  timeoutMs?: number;
  pollIntervalMs?: number;
  /** Consecutive identical readings required before the box counts as settled. */
  stableReads?: number;
}

export const DEFAULT_RESIZE_TIMEOUT_MS = 5_000;

/**
 * Wait until the ACTIVE pane's box has changed from `before` and then held still.
 * Bounded: a resize the app never applies fails as a timeout naming both boxes,
 * instead of hanging on an RPC that will not answer.
 */
export async function waitForPaneResize(
  browser: QaGeometryBrowser,
  selector: string,
  before: Pick<PaneGeometry, 'width' | 'height'>,
  options: ResizeSettleOptions = {},
): Promise<PaneGeometry> {
  const timeoutMs = options.timeoutMs ?? DEFAULT_RESIZE_TIMEOUT_MS;
  const pollIntervalMs = options.pollIntervalMs ?? 50;
  const stableReads = options.stableReads ?? 2;
  const deadline = Date.now() + timeoutMs;
  let stable = 0;
  let previous: PaneGeometry | null = null;
  let latest: PaneGeometry | null = null;

  while (Date.now() < deadline) {
    latest = assertActivePane(await readPaneGeometry(browser, selector));
    const changed = latest.width !== before.width || latest.height !== before.height;
    const held = previous !== null && previous.width === latest.width && previous.height === latest.height;
    if (changed && held) {
      stable += 1;
      if (stable >= stableReads) return latest;
    } else if (!held) {
      stable = 0;
    }
    previous = latest;
    await delay(pollIntervalMs);
  }

  throw new Error(
    `${selector} did not settle at a new size within ${timeoutMs}ms: `
    + `before ${before.width}x${before.height}, last ${latest ? `${latest.width}x${latest.height}` : 'unreadable'}`,
  );
}

async function delay(ms: number): Promise<void> {
  await new Promise((resolve) => setTimeout(resolve, ms));
}
