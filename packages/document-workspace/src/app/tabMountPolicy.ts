import {
  documentHasPinnedState,
  formatIsIntrinsicallyHeavy,
  heavyResourcesForDocument,
  type DocumentFormat,
} from "@rightkit/document-core/app/resourceCensus";

/** Maximum number of ordinary document routers kept paint-warm (DocumentRouterTable's own limit). */
export const ORDINARY_LIVE_PANE_LIMIT = 2;

// Original policy (protected ∪ active ∪ previousActive) cold-remounted every tab on a A→B→C→A cycle,
// which reads as a bug in ordinary use even though it was the deliberate stress-test-tuned plateau from
// 553d6b55. An MRU window keeps recently-visited tabs mounted (not paint-warm — see DocumentRouterTable's
// separate, still-tight isPaintWarm) so returning to a tab you left seconds ago doesn't cold-mount it.
// 8 is generous headroom above the 2-pane paint-warm limit while still bounding a 50-tab stress session.
//
// The window is a resource BUDGET, not a slot count: a document currently holding a heavy runtime
// (docx-editor, pdf-document, html-iframe, image-viewer, …) consumes more of it than a light or
// medium one, so heavyweight background panes shell sooner than lightweight ones at the same MRU
// rank. This makes the limit resource-aware without lowering it — VR-P0-4.
//
// Measured 2026-10-11 by `runTabLivenessWindow` (scripts/qa-resources-e2e.mjs, receipt
// `memoryPerTab`; hidden Chrome, JS heap after a forced collection, plain-text tabs only): a live
// text tab costs about 0.24 MB of heap and a shelled one about 0.03 MB, so 50 tabs with 8 live sit
// at 11.1 MB against 8.0 MB for one tab. Heavy formats (PDF, DOCX, HTML frames) and native WebView
// memory are not in that figure; they are why the budget is weighted rather than raised.
export const LIVE_TAB_LIMIT = 8;

/** Heavy documents consume more of the MRU budget than light/medium ones, so they age out sooner. */
const HEAVY_CANDIDATE_WEIGHT = 2;
const ORDINARY_CANDIDATE_WEIGHT = 1;

/**
 * A candidate's cost is the max of two independent signals:
 *  - intrinsic: a static fact about the document's FORMAT (html/docx/pdf/image always allocate a
 *    heavy resource while mounted), true whether or not the pane is currently mounted at all;
 *  - resident: what the census currently has allocated for this path, which is a live-only signal
 *    that goes to zero the moment a shelled pane releases its resources.
 * Taking the max means resident information can only ever RAISE the cost above the format's floor
 * (e.g. a doc holding more than one heavy resource kind at once), never lower a known-heavy format
 * to ordinary weight just because it is not currently resident — see VR-P0-4 follow-up.
 */
function candidateWeight(path: string, format: DocumentFormat | undefined): number {
  const intrinsic = formatIsIntrinsicallyHeavy(format)
    ? HEAVY_CANDIDATE_WEIGHT
    : ORDINARY_CANDIDATE_WEIGHT;
  const resident =
    heavyResourcesForDocument(path) > 0 ? HEAVY_CANDIDATE_WEIGHT : ORDINARY_CANDIDATE_WEIGHT;
  return Math.max(intrinsic, resident);
}

export type TabMountPolicyInput = Readonly<{
  activePath: string | null;
  previousActivePath: string | null;
  protectedPaths: ReadonlySet<string>;
  /** Recently-active paths, most-recent-first. Only the first LIVE_TAB_LIMIT entries stay live. */
  recentPaths?: readonly string[];
  /**
   * Document format per path, for the intrinsic-cost floor in `candidateWeight`. Optional and
   * missing entries fall back to the census-only signal (matches prior behavior) so existing
   * callers/tests that don't care about the eviction-cost-decay case keep working unchanged.
   */
  formatByPath?: ReadonlyMap<string, DocumentFormat>;
}>;

export type TabMountCounters = Readonly<{
  shellCount: number;
  liveCount: number;
  protectedLiveCount: number;
  ordinaryLiveLimit: number;
}>;

/**
 * Dirty text sessions may leave the live set only after their authoritative source (or a
 * structured draft snapshot) is owned above the router. Binary editors deliberately never get
 * this escape hatch: their mounted runtime remains their only authoritative dirty state.
 */
export function dirtyPaneMayShell(format: string, hasExternalSourceOrDraftOwner: boolean): boolean {
  return (
    hasExternalSourceOrDraftOwner && (format === "txt" || format === "md" || format === "html")
  );
}

/**
 * Active + previous-active + a resource-weighted MRU window of recently-active panes stay live.
 * `protectedPaths` protection is deliberately additive and unbounded: a draft or pending save
 * cannot be evicted merely because another tab became active or aged out of the MRU window.
 *
 * Beyond that, every candidate is checked against the resource census before it can age out:
 * a document reporting pinned state (dirty with no external owner — `documentHasPinnedState`)
 * is kept live no matter its MRU rank or weight, full stop — there is no limit or heuristic that
 * overrides this. A document whose state is dirty-but-externalized, or simply clean, competes for
 * the MRU budget like any other, weighted by `candidateWeight` so heavyweight panes shell out
 * before lightweight ones at the same recency — and stay counted as heavyweight even once shelled,
 * via `formatByPath`'s intrinsic-cost floor (see `candidateWeight`'s own doc comment).
 */
export function liveDocumentPaths({
  activePath,
  previousActivePath,
  protectedPaths,
  recentPaths = [],
  formatByPath,
}: TabMountPolicyInput): Set<string> {
  const paths = new Set(protectedPaths);
  let budget = LIVE_TAB_LIMIT;
  const weightOf = (path: string) => candidateWeight(path, formatByPath?.get(path));

  // Active and previous-active are unconditionally live, but they still spend from the same
  // shared budget as everything else — otherwise the window would exceed LIVE_TAB_LIMIT whenever
  // they also show up in recentPaths (which they normally do).
  for (const path of [activePath, previousActivePath]) {
    if (!path || paths.has(path)) continue;
    paths.add(path);
    budget = Math.max(0, budget - weightOf(path));
  }

  for (const path of recentPaths) {
    if (paths.has(path)) continue;
    if (documentHasPinnedState(path)) {
      // Never evict a pane with pinned state. No heuristic, no limit, no exception.
      paths.add(path);
      continue;
    }
    const weight = weightOf(path);
    if (weight > budget) continue;
    paths.add(path);
    budget -= weight;
  }
  return paths;
}

/** Deterministic observability for stress tests & diagnostics; never reads wall-clock time. */
export function tabMountCounters(
  documentPaths: readonly string[],
  livePaths: ReadonlySet<string>,
  protectedPaths: ReadonlySet<string>,
): TabMountCounters {
  let protectedLiveCount = 0;
  for (const path of documentPaths)
    if (livePaths.has(path) && protectedPaths.has(path)) protectedLiveCount++;
  const liveCount = documentPaths.filter((path) => livePaths.has(path)).length;
  return {
    shellCount: documentPaths.length - liveCount,
    liveCount,
    protectedLiveCount,
    ordinaryLiveLimit: ORDINARY_LIVE_PANE_LIMIT,
  };
}
