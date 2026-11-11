import { useCallback, useEffect, useRef, useState } from "react";
import type { LibraryQuery, LibraryRow, LibrarySource } from "../contract.js";

export const LIBRARY_PAGE_SIZE = 50;

/** A query without its page window. */
export type LibraryFilter = Omit<LibraryQuery, "offset" | "limit">;

/** Every page matching `filter`, in order. */
export async function collectAllRows(source: LibrarySource, filter: LibraryFilter): Promise<LibraryRow[]> {
  const collected: LibraryRow[] = [];
  let offset = 0;
  let page = await source.query({ ...filter, offset, limit: LIBRARY_PAGE_SIZE });
  collected.push(...page.rows);
  while (page.hasMore && page.rows.length > 0) {
    offset += page.rows.length;
    page = await source.query({ ...filter, offset, limit: LIBRARY_PAGE_SIZE });
    collected.push(...page.rows);
  }
  return collected;
}

/**
 * Owns the list's query/pagination lifecycle: the paged `source.query` reads,
 * `rows`/`total`/`hasMore`/`loaded`/`loadingMore` state, the stale-response
 * generation guard, and every refresh trigger (debounced filter changes and
 * the source's change subscription).
 *
 * What the list displays from the loaded set (group scoping, day grouping) is
 * derived downstream, and mutations stay with the screen.
 */
export function useLibraryQuery({
  source,
  filter,
  exhaustive,
  setError,
}: {
  source: LibrarySource;
  filter: LibraryFilter;
  /**
   * True when the caller needs the COMPLETE matching set on every refresh: a
   * group-scoped view (any group switched off) is filtered client-side, so it
   * must walk every page for that scoping to be exact. When true, `hasMore` is
   * forced false and `loadMore` is inert.
   */
  exhaustive: boolean;
  setError: (message: string) => void;
}) {
  const [rows, setRows] = useState<LibraryRow[]>([]);
  const [total, setTotal] = useState(0);
  const [hasMore, setHasMore] = useState(false);
  const [loaded, setLoaded] = useState(false);
  const [loadingMore, setLoadingMore] = useState(false);
  const queryGeneration = useRef(0);

  const fetchPage = useCallback(
    (offset: number) => source.query({ ...filter, offset, limit: LIBRARY_PAGE_SIZE }),
    [source, filter],
  );

  const refresh = useCallback(async () => {
    const generation = ++queryGeneration.current;
    try {
      let page = await fetchPage(0);
      if (generation !== queryGeneration.current) return;

      if (exhaustive) {
        const all = [...page.rows];
        let offset = page.rows.length;
        while (page.hasMore && page.rows.length > 0) {
          page = await fetchPage(offset);
          if (generation !== queryGeneration.current) return;
          all.push(...page.rows);
          offset += page.rows.length;
        }
        page = { ...page, rows: all };
      }

      setRows(page.rows);
      setTotal(page.total);
      setHasMore(!exhaustive && page.hasMore);
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
      const page = await fetchPage(rows.length);
      if (generation !== queryGeneration.current) return;
      setRows((current) => [...current, ...page.rows]);
      setTotal(page.total);
      setHasMore(page.hasMore);
    } catch (e) {
      if (generation === queryGeneration.current) setError(String(e));
    } finally {
      if (generation === queryGeneration.current) setLoadingMore(false);
    }
  }, [exhaustive, fetchPage, hasMore, rows.length, loadingMore, setError]);

  // Debounced initial/filter-change fetch: the 180ms window coalesces
  // search keystrokes into one query.
  useEffect(() => {
    const timeout = window.setTimeout(() => void refresh(), 180);
    return () => window.clearTimeout(timeout);
  }, [refresh]);

  // New rows (a delivery, a sync pull) arrive through the source's subscription.
  useEffect(() => {
    if (!source.subscribe) return;
    const unsubscribe = source.subscribe(() => void refresh());
    return () => {
      void unsubscribe.then((fn) => fn()).catch(() => undefined);
    };
  }, [source, refresh]);

  return { rows, total, hasMore, loaded, loadingMore, refresh, loadMore };
}
