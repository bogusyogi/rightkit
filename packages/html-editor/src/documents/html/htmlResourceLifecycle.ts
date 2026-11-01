import { acquireResource, type ResourceKind } from "@rightkit/document-core/app/resourceCensus";
import { exposePerfFamilyForQa } from "@rightkit/document-core/documents/perfEventCounters";

/** Deterministic HTML resource accounting used by lifecycle tests & diagnostics. */
export interface HtmlResourceCounters {
  liveIframes: number;
  liveProseMirrors: number;
  liveObservers: number;
  mounts: number;
  unmounts: number;
  remounts: number;
  cancelledBuilds: number;
}
const counters: HtmlResourceCounters = {
  liveIframes: 0,
  liveProseMirrors: 0,
  liveObservers: 0,
  mounts: 0,
  unmounts: 0,
  remounts: 0,
  cancelledBuilds: 0,
};
const mountedPaths = new Set<string>();
export function getHtmlResourceCounters(): Readonly<HtmlResourceCounters> {
  return { ...counters };
}
export function resetHtmlResourceCounters(): void {
  for (const key of Object.keys(counters) as Array<keyof HtmlResourceCounters>) counters[key] = 0;
  mountedPaths.clear();
  for (const stack of Object.values(censusHandles)) {
    while (stack.length) stack.pop()?.();
  }
}
export function trackHtmlMount(remount = false): void {
  counters.mounts += 1;
  if (remount) counters.remounts += 1;
}
export function trackHtmlUnmount(): void {
  counters.unmounts += 1;
}
// The HTML trackers are delta-based while the census is handle-based, so hold a stack of live
// handles per kind and pop one on each -1. Order does not matter: the entries are identical.
const censusHandles: Partial<Record<ResourceKind, Array<() => void>>> = {};

function censusDelta(kind: ResourceKind, delta: 1 | -1): void {
  let stack = censusHandles[kind];
  if (!stack) {
    stack = [];
    censusHandles[kind] = stack;
  }
  if (delta === 1) stack.push(acquireResource({ kind, format: "html" }));
  else stack.pop()?.();
}

export function trackHtmlIframe(delta: 1 | -1): void {
  counters.liveIframes += delta;
  censusDelta("html-iframe", delta);
}
export function trackHtmlProseMirror(delta: 1 | -1): void {
  counters.liveProseMirrors += delta;
  censusDelta("prosemirror-view", delta);
}
export function trackHtmlObserver(delta: 1 | -1): void {
  counters.liveObservers += delta;
}
export function trackCancelledHtmlBuild(): void {
  counters.cancelledBuilds += 1;
}
export function trackHtmlDocumentMount(path: string): void {
  const remount = mountedPaths.has(path);
  mountedPaths.add(path);
  trackHtmlMount(remount);
}

/** Tiny deterministic LRU; no unbounded document content survives a cold tab unmount. */
export class HtmlLruCache<T> {
  private readonly entries = new Map<string, T>();
  constructor(readonly limit: number) {}
  get(key: string): T | undefined {
    const value = this.entries.get(key);
    if (value === undefined) return undefined;
    this.entries.delete(key);
    this.entries.set(key, value);
    return value;
  }
  set(key: string, value: T): void {
    this.entries.delete(key);
    this.entries.set(key, value);
    while (this.entries.size > this.limit) {
      const oldest = this.entries.keys().next();
      if (oldest.done) break;
      this.entries.delete(oldest.value);
    }
  }
  clear(): void {
    this.entries.clear();
  }
  get size(): number {
    return this.entries.size;
  }
}
export interface HtmlSourceMapCacheEntry {
  source: string;
  tree: unknown;
  /** Parse5 diagnostics captured alongside `tree`, when the parse collected them. */
  errors?: Array<{ code: string; offset: number }>;
}
export const htmlSourceMapCache = new HtmlLruCache<HtmlSourceMapCacheEntry>(8);
export const htmlSrcDocCache = new HtmlLruCache<string>(8);
export interface HtmlColdState {
  applied: string;
  renderedRevision: string;
}
export const htmlColdStateCache = new HtmlLruCache<HtmlColdState>(64);
export function getHtmlResourceCacheCounters() {
  return {
    sourceMaps: htmlSourceMapCache.size,
    srcDocs: htmlSrcDocCache.size,
    coldStates: htmlColdStateCache.size,
  };
}
export function clearHtmlResourceCaches(): void {
  htmlSourceMapCache.clear();
  htmlSrcDocCache.clear();
  htmlColdStateCache.clear();
}

exposePerfFamilyForQa("htmlResources", {
  read: getHtmlResourceCounters,
  reset: resetHtmlResourceCounters,
});
