/**
 * Performance event counters for the performance contract.
 *
 * Tracks the six dimensions named in docs/plans/2026-08-13-block-local-editing-repair.md:
 *  source writes, parse/projection rebuilds, history entries,
 *  preview-generation changes, iframe loads, disk writes.
 *
 * The counters are global singletons so integration tests can
 * reset them, drive a user interaction sequence, then assert
 * exact event counts as per the table in docs/plans/2026-08-13-block-local-editing-repair.md.
 *
 * Counters increment only on the commit boundary (Done) or
 * explicit save, never on pointer movement, draft typing, or
 * Cancel. Cache misses increment parse/projection counters;
 * cache hits are free.
 */

export interface PerfEventCounters {
  /** Canonical source mutations (core applyCommand or HTML splice). */
  sourceWrites: number;
  /** Parse invocations that actually parsed (cache miss). */
  parseCount: number;
  /** Projection rebuilds that actually rebuilt (cache miss). */
  projectionRebuilds: number;
  /** History entries / undo groups created. */
  historyEntries: number;
  /** Preview generations advanced (rendered-revision change). */
  previewGenerations: number;
  /** Iframe loads triggered (srcDoc swap that would reload). */
  iframeLoads: number;
  /** Disk writes (persist to Tauri). */
  diskWrites: number;
}

const counters: PerfEventCounters = {
  sourceWrites: 0,
  parseCount: 0,
  projectionRebuilds: 0,
  historyEntries: 0,
  previewGenerations: 0,
  iframeLoads: 0,
  diskWrites: 0,
};

export function resetPerfCounters(): void {
  counters.sourceWrites = 0;
  counters.parseCount = 0;
  counters.projectionRebuilds = 0;
  counters.historyEntries = 0;
  counters.previewGenerations = 0;
  counters.iframeLoads = 0;
  counters.diskWrites = 0;
}

export function getPerfCounters(): Readonly<PerfEventCounters> {
  return { ...counters };
}

export function incSourceWrites(delta = 1): void {
  counters.sourceWrites += delta;
}

export function incParseCount(delta = 1): void {
  counters.parseCount += delta;
}

export function incProjectionRebuilds(delta = 1): void {
  counters.projectionRebuilds += delta;
}

export function incHistoryEntries(delta = 1): void {
  counters.historyEntries += delta;
}

export function incPreviewGenerations(delta = 1): void {
  counters.previewGenerations += delta;
}

export function incIframeLoads(delta = 1): void {
  counters.iframeLoads += delta;
}

export function incDiskWrites(delta = 1): void {
  counters.diskWrites += delta;
}

// -------------------------------------------------------------------------------------------
// window.__viewrightQaPerf — QA-only counter readout for scripts/qa-block-edit-counts-e2e.mjs.
// Same guard as installResourceCensusDevHook (src/app/resourceCensusDev.ts): dev or browser-QA
// builds only; production has no global.
// -------------------------------------------------------------------------------------------

/** A named counter family that joins the QA readout. `reset` zeroes cumulative counters only. */
export interface PerfQaFamily {
  read(): unknown;
  reset(): void;
}

declare global {
  interface Window {
    __viewrightQaPerf?: {
      read(): Record<string, unknown>;
      reset(): void;
    };
  }
}

const qaFamilies = new Map<string, PerfQaFamily>();

function qaReadoutEnabled(): boolean {
  return (
    typeof window !== "undefined" &&
    (import.meta.env?.DEV || import.meta.env?.VITE_RIGHTKIT_BROWSER_QA === "1")
  );
}

function installQaReadout(): void {
  if (!qaReadoutEnabled()) return;
  window.__viewrightQaPerf = {
    read: () => ({
      perf: getPerfCounters(),
      ...Object.fromEntries([...qaFamilies].map(([name, family]) => [name, family.read()])),
    }),
    reset: () => {
      resetPerfCounters();
      for (const family of qaFamilies.values()) family.reset();
    },
  };
}

/** Adds a named counter family (for example the Markdown resource lifecycle) to the QA readout. */
export function exposePerfFamilyForQa(name: string, family: PerfQaFamily): void {
  qaFamilies.set(name, family);
  installQaReadout();
}

installQaReadout();
