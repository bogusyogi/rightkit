// Measure when the UI was PAINTED, not when the command returned.
//
// A driver command resolves as soon as the app acknowledges it. For anything that
// renders — opening a document, switching tabs, re-laying out after a resize —
// that acknowledgement happens well before pixels change, so timings taken from
// it measure the RPC and report it as UI latency. Every such number is wrong in
// the same direction, which is why they looked plausible.
//
// What this module measures instead: from a caller-marked start, wait for DOM
// mutations in the target subtree to go quiet, then wait two animation frames.
// The second frame callback runs after the frame containing the layout+paint work
// has been committed for rendering.
//
// Honest limit: rAF x2 fires after commit but before the compositor has
// necessarily put pixels on the physical display. It is the right signal for
// catching latency regressions and for ordering ("did it render before we
// asserted?"). It is NOT proof of pixels; when that is what you need, follow it
// with a screenshot comparison.

export interface QaBrowser {
  execute<T>(script: string, ...args: unknown[]): Promise<T>;
  executeAsync<T>(script: string, ...args: unknown[]): Promise<T>;
}

export interface PaintMetrics {
  requestId: string;
  /** Milliseconds from the caller's mark to the second animation frame. */
  durationMs: number;
  /** Milliseconds spent waiting for DOM mutations to stop. */
  settleMs: number;
  mutations: number;
  /** True when rAF x2 completed; false when the page fell back to a timer. */
  animationFrames: boolean;
}

export const DEFAULT_SETTLE_MS = 120;
export const DEFAULT_PAINT_TIMEOUT_MS = 10_000;

const LEDGER = '__rightkitQaPaint';

/**
 * Install the page-side ledger. Idempotent, so specs may call it after every
 * navigation without accumulating observers.
 */
export function installPaintLedgerScript(): string {
  return `
    (function () {
      if (window.${LEDGER}) return true;
      window.${LEDGER} = { marks: new Map() };
      return true;
    })();
  `;
}

/** Record the start of a measured interaction, keyed by request id. */
export function beginPaintScript(requestId: string, selector?: string): string {
  return `
    (function () {
      var ledger = window.${LEDGER};
      if (!ledger) throw new Error('paint ledger is not installed');
      var root = ${selector ? `document.querySelector(${JSON.stringify(selector)})` : 'document.documentElement'};
      if (!root) throw new Error('paint target not found: ' + ${JSON.stringify(selector ?? 'document')});
      var mark = { startedAt: performance.now(), mutations: 0, lastMutationAt: performance.now() };
      mark.observer = new MutationObserver(function (records) {
        mark.mutations += records.length;
        mark.lastMutationAt = performance.now();
      });
      mark.observer.observe(root, { subtree: true, childList: true, characterData: true, attributes: true });
      ledger.marks.set(${JSON.stringify(requestId)}, mark);
      return true;
    })();
  `;
}

/**
 * Resolve once the target subtree has been quiet for `settleMs` and two
 * animation frames have elapsed. Falls back to a timer only if the document is
 * hidden, where the browser throttles rAF indefinitely — reported honestly via
 * `animationFrames: false` rather than silently producing a number.
 */
export function awaitPaintScript(requestId: string, settleMs: number, timeoutMs: number): string {
  return `
    (function () {
      var done = arguments[arguments.length - 1];
      var ledger = window.${LEDGER};
      var mark = ledger && ledger.marks.get(${JSON.stringify(requestId)});
      if (!mark) { done({ error: 'no paint mark for request ${requestId}' }); return; }
      var deadline = performance.now() + ${timeoutMs};

      function finish(animationFrames) {
        mark.observer.disconnect();
        ledger.marks.delete(${JSON.stringify(requestId)});
        done({
          durationMs: performance.now() - mark.startedAt,
          settleMs: mark.lastMutationAt - mark.startedAt,
          mutations: mark.mutations,
          animationFrames: animationFrames,
        });
      }

      function afterSettle() {
        if (document.hidden) { finish(false); return; }
        requestAnimationFrame(function () { requestAnimationFrame(function () { finish(true); }); });
      }

      (function poll() {
        var quietFor = performance.now() - mark.lastMutationAt;
        if (quietFor >= ${settleMs}) { afterSettle(); return; }
        if (performance.now() >= deadline) {
          mark.observer.disconnect();
          ledger.marks.delete(${JSON.stringify(requestId)});
          done({ error: 'paint did not settle within ${timeoutMs}ms; ' + mark.mutations + ' mutations, last ' + Math.round(quietFor) + 'ms ago' });
          return;
        }
        setTimeout(poll, 16);
      })();
    })();
  `;
}

export interface MeasurePaintOptions {
  requestId: string;
  /** CSS selector for the subtree that should render. Defaults to the document. */
  selector?: string;
  settleMs?: number;
  timeoutMs?: number;
}

/**
 * Run `trigger` and return when the resulting render has actually been painted.
 * The mark is installed BEFORE the trigger so no mutation can slip between them.
 */
export async function measurePaint(
  browser: QaBrowser,
  trigger: () => Promise<unknown>,
  options: MeasurePaintOptions,
): Promise<PaintMetrics> {
  const settleMs = options.settleMs ?? DEFAULT_SETTLE_MS;
  const timeoutMs = options.timeoutMs ?? DEFAULT_PAINT_TIMEOUT_MS;
  await browser.execute(installPaintLedgerScript());
  await browser.execute(beginPaintScript(options.requestId, options.selector));
  await trigger();
  const result = await browser.executeAsync<{
    error?: string;
    durationMs?: number;
    settleMs?: number;
    mutations?: number;
    animationFrames?: boolean;
  }>(awaitPaintScript(options.requestId, settleMs, timeoutMs));

  if (!result || result.error) {
    throw new Error(`paint measurement failed for ${options.requestId}: ${result?.error ?? 'no result'}`);
  }
  return {
    requestId: options.requestId,
    durationMs: Number(result.durationMs),
    settleMs: Number(result.settleMs),
    mutations: Number(result.mutations ?? 0),
    animationFrames: result.animationFrames === true,
  };
}

/**
 * Assert a paint completed within budget. Separate from measurement so a spec can
 * record a timing without failing on it, and so the failure text carries the
 * evidence rather than just a boolean.
 */
export function assertPaintWithin(metrics: PaintMetrics, budgetMs: number, label: string): PaintMetrics {
  if (!Number.isFinite(budgetMs) || budgetMs <= 0) throw new Error('paint budget must be a positive number of milliseconds');
  if (metrics.durationMs > budgetMs) {
    throw new Error(
      `${label} painted in ${Math.round(metrics.durationMs)}ms, over its ${budgetMs}ms budget `
      + `(${metrics.mutations} DOM mutations, settled after ${Math.round(metrics.settleMs)}ms, requestId ${metrics.requestId})`,
    );
  }
  return metrics;
}
