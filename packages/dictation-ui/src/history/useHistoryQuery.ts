import { useCallback, useEffect, useRef, useState } from "react";
import { dictationClient } from "../host.js";
import type { HistoryItem } from "../lib/homeMetrics.js";
import {
  HISTORY_PAGE_SIZE,
  type HistoryPageResult,
  type HistoryQueryArgs,
} from "./historyQueryTypes.js";

/**
 * Owns the History page's query/pagination lifecycle: the paged host history reads (`queryHistory` on the
 * dictation client), `items`/`total`/`hasMore`/`loaded`/`loadingMore` state, the stale-response generation guard,
 * and every refresh trigger (debounced filter changes, `deliveryRecorded`, `historyChanged`).
 *
 * What the caller asks for arrives as `queryArgs`; what the page displays from the loaded set (source-checkbox
 * scoping, day grouping) is derived downstream by the page/list, and mutation commands stay with the page.
 */
export function useHistoryQuery({
  queryArgs,
  exhaustive,
  setError,
}: {
  queryArgs: HistoryQueryArgs;
  /**
   * True when the caller needs the COMPLETE matching set on every refresh: device provenance lives in each record's
   * source payload while the native query filters only date/app/outcome, so a source-scoped view must walk every
   * native page for its client-side scoping to be exact. When true, `hasMore` is forced false and `loadMore` is
   * inert: nothing is left unloaded to page in.
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
  const requestPending = useRef(false);
  const nextPage = useRef<{
    fetch: (offset: number) => Promise<HistoryPageResult>;
    offset: number;
  } | null>(null);

  const fetchPage = useCallback(
    (offset: number): Promise<HistoryPageResult> => {
      const client = dictationClient();
      if (!client.queryHistory) {
        return Promise.reject(new Error("@rightkit/dictation-ui: the host client has no `queryHistory`"));
      }
      return client.queryHistory({ ...queryArgs, offset, limit: HISTORY_PAGE_SIZE });
    },
    [queryArgs],
  );

  const refresh = useCallback(async () => {
    const generation = ++queryGeneration.current;
    requestPending.current = true;
    setLoadingMore(false);
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

      nextPage.current = !exhaustive && page.has_more
        ? { fetch: fetchPage, offset: page.items.length }
        : null;
      setItems(page.items);
      setTotal(page.total);
      setHasMore(!exhaustive && page.has_more);
    } catch (e) {
      if (generation !== queryGeneration.current) return;
      setError(String(e));
    } finally {
      if (generation === queryGeneration.current) {
        requestPending.current = false;
        setLoaded(true);
      }
    }
  }, [exhaustive, fetchPage, setError]);

  const loadMore = useCallback(async () => {
    const cursor = nextPage.current;
    if (requestPending.current || exhaustive || cursor?.fetch !== fetchPage) return;
    const generation = queryGeneration.current;
    requestPending.current = true;
    setLoadingMore(true);
    try {
      const page = await fetchPage(cursor.offset);
      if (generation !== queryGeneration.current) return;
      nextPage.current = page.has_more
        ? { fetch: fetchPage, offset: cursor.offset + page.items.length }
        : null;
      setItems((current) => [...current, ...page.items]);
      setTotal(page.total);
      setHasMore(page.has_more);
    } catch (e) {
      if (generation === queryGeneration.current) setError(String(e));
    } finally {
      if (generation === queryGeneration.current) {
        requestPending.current = false;
        setLoadingMore(false);
      }
    }
  }, [exhaustive, fetchPage, setError]);

  // Debounced initial/filter-change fetch: the 180ms window coalesces search keystrokes into one query.
  useEffect(() => {
    const timeout = window.setTimeout(() => void refresh(), 180);
    return () => window.clearTimeout(timeout);
  }, [refresh]);

  useEffect(() => {
    const unsubscribe = dictationClient().on("deliveryRecorded", () => void refresh());
    const cleanup = unsubscribe.catch(() => undefined);
    return () => {
      void cleanup.then((fn) => fn?.());
    };
  }, [refresh]);

  // Transcripts pulled from iCloud land in the database without a local delivery, so `deliveryRecorded` never
  // fires for them. The host reports them as `historyChanged`.
  useEffect(() => {
    const unsubscribe = dictationClient().on("historyChanged", () => void refresh());
    const cleanup = unsubscribe.catch(() => undefined);
    return () => {
      void cleanup.then((fn) => fn?.());
    };
  }, [refresh]);

  return { items, total, hasMore, loaded, loadingMore, refresh, loadMore };
}
