import { useCallback, useEffect, useRef, useState } from "react";
import type { HistoryItem } from "@rightkit/dictation-ui/lib/homeMetrics";
import { invoke, listen } from "@rightkit/dictation-ui/lib/tauri";

export const HISTORY_PAGE_SIZE = 50;

export interface NativeHistoryFilter {
  search: string;
  from_ms: number | null;
  until_ms: number | null;
  app: string;
}

export interface HistoryQueryArgs extends NativeHistoryFilter {
  cancelled_only: boolean;
}

export interface HistoryPageResult {
  items: HistoryItem[];
  total: number;
  has_more: boolean;
}

/**
 * Owns the History page's query/pagination lifecycle: the paged
 * `query_history` reads, `items`/`total`/`hasMore`/`loaded`/`loadingMore`
 * state, the stale-response generation guard, and every refresh trigger
 * (debounced filter changes, `delivery-recorded`, `cloud-sync-updated`).
 *
 * What the caller asks for arrives as `queryArgs`; what the page displays
 * from the loaded set (source-checkbox scoping, day grouping) is derived
 * downstream by the page/list, and mutation commands stay with the page.
 */
export function useHistoryQuery({
  queryArgs,
  exhaustive,
  setError,
}: {
  queryArgs: HistoryQueryArgs;
  /**
   * True when the caller needs the COMPLETE matching set on every refresh —
   * device provenance lives in each record's source payload while the native
   * query filters only date/app/outcome, so a source-scoped view (any source
   * checkbox unchecked) must walk every native page for its client-side
   * scoping to be exact even when SQL history exceeds one page. When true,
   * `hasMore` is forced false and `loadMore` is inert: nothing is left
   * unloaded to page in.
   */
  exhaustive: boolean;
  setError: (message: string) => void;
}) {
  const [items, setItems] = useState<HistoryItem[]>([]);
  const [total, setTotal] = useState(0);
  const [hasMore, setHasMore] = useState(false);
  const [loaded, setLoaded] = useState(false);
  const [loadingMore, setLoadingMore] = useState(false);
  const queryGeneration = useRef(0);

  const fetchPage = useCallback(
    (offset: number) =>
      invoke<HistoryPageResult>("query_history", {
        query: { ...queryArgs, offset, limit: HISTORY_PAGE_SIZE },
      }),
    [queryArgs],
  );

  const refresh = useCallback(async () => {
    const generation = ++queryGeneration.current;
    try {
      let page = await fetchPage(0);
      if (generation !== queryGeneration.current) return;

      if (exhaustive) {
        const allItems = [...page.items];
        let offset = page.items.length;
        while (page.has_more && page.items.length > 0) {
          page = await fetchPage(offset);
          if (generation !== queryGeneration.current) return;
          allItems.push(...page.items);
          offset += page.items.length;
        }
        page = { ...page, items: allItems };
      }

      setItems(page.items);
      setTotal(page.total);
      setHasMore(!exhaustive && page.has_more);
    } catch (e) {
      if (generation !== queryGeneration.current) return;
      setError(String(e));
    } finally {
      if (generation === queryGeneration.current) setLoaded(true);
    }
  }, [exhaustive, fetchPage, setError]);

  const loadMore = useCallback(async () => {
    if (loadingMore || !hasMore || exhaustive) return;
    const generation = queryGeneration.current;
    setLoadingMore(true);
    try {
      const page = await fetchPage(items.length);
      if (generation !== queryGeneration.current) return;
      setItems((current) => [...current, ...page.items]);
      setTotal(page.total);
      setHasMore(page.has_more);
    } catch (e) {
      if (generation === queryGeneration.current) setError(String(e));
    } finally {
      if (generation === queryGeneration.current) setLoadingMore(false);
    }
  }, [exhaustive, fetchPage, hasMore, items.length, loadingMore, setError]);

  // Debounced initial/filter-change fetch — the 180ms window coalesces
  // search keystrokes into one query.
  useEffect(() => {
    const timeout = window.setTimeout(() => void refresh(), 180);
    return () => window.clearTimeout(timeout);
  }, [refresh]);

  useEffect(() => {
    const unlisten = listen("delivery-recorded", () => void refresh());
    return () => {
      void unlisten.then((fn) => fn());
    };
  }, [refresh]);

  // Transcripts pulled from iCloud land in the database without any local
  // delivery, so `delivery-recorded` never fires for them and History showed
  // stale rows until something else forced a refresh. cloud_sync.rs has always
  // emitted this after a non-empty pull; nothing listened for it.
  useEffect(() => {
    const unlisten = listen("cloud-sync-updated", () => void refresh());
    return () => {
      void unlisten.then((fn) => fn());
    };
  }, [refresh]);

  return { items, total, hasMore, loaded, loadingMore, refresh, loadMore };
}
