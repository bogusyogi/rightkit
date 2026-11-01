import { useEffect, useRef } from "react";
import { searchThreads, semanticSearch, type Thread } from "../lib/ipc";
import { meaningOrKeyword } from "../lib/meaningSearch";
import { FOLDER_LABEL } from "../lib/mail";
import {
  hasMorePages,
  nextSearchCursor,
  startSearchPager,
} from "../lib/searchPaging";
import {
  searchAccountId,
  visibleThreadMatches,
} from "../lib/searchRouting";
import { useAppStore } from "../store";

const DEBOUNCE_MS = 250;

export function useSearchRouting() {
  const activeAccount = useAppStore((s) => s.activeAccount);
  const activeLabel = useAppStore((s) => s.activeLabel);
  const folder = useAppStore((s) => s.folder);
  const query = useAppStore((s) => s.query);
  const threads = useAppStore((s) => s.threads);
  const meaning = useAppStore((s) => s.meaning && s.meaningReady);
  const setMeaningNote = useAppStore((s) => s.setMeaningNote);

  const setErr = useAppStore((s) => s.setErr);
  const setResults = useAppStore((s) => s.setResults);
  const setSearching = useAppStore((s) => s.setSearching);
  const setNoMoreOlder = useAppStore((s) => s.setNoMoreOlder);

  // Bumped on every (query, scope) change. Older responses check this and bail.
  const genRef = useRef(0);

  useEffect(() => {
    const trimmedQuery = query.trim();
    if (!trimmedQuery) {
      setMeaningNote(null);
      startSearchPager(null);
      setNoMoreOlder(false);
      setResults([]);
      setSearching(false);
      return;
    }

    // Seed visible-row matches immediately so the UI never sits empty during debounce.
    setResults(visibleThreadMatches(threads, trimmedQuery));
    setSearching(true);
    // No "load more" until the first page says there is one.
    startSearchPager(null);
    setNoMoreOlder(true);

    const gen = ++genRef.current;
    const accountId = searchAccountId(folder, activeAccount, activeLabel);
    const label = activeLabel ?? FOLDER_LABEL[folder];
    const liveThreads = threads;

    const timer = window.setTimeout(() => {
      const keyword = () => searchThreads(trimmedQuery, accountId, label);
      const run: Promise<{ rows: Thread[]; note: string | null; usedMeaning: boolean }> =
        meaning
          ? meaningOrKeyword(() => semanticSearch(trimmedQuery, accountId, label), keyword)
          : keyword().then((rows) => ({ rows, note: null, usedMeaning: false }));
      run
        .then(({ rows: r, note, usedMeaning }) => {
          if (genRef.current !== gen) return;
          setMeaningNote(note);
          if (usedMeaning) {
            // Meaning results come as one ranked list: no "load more".
            startSearchPager(null);
            setNoMoreOlder(true);
            setResults(r);
            return;
          }
          startSearchPager({
            query: trimmedQuery,
            accountId,
            label,
            cursor: nextSearchCursor(r),
          });
          setNoMoreOlder(!hasMorePages(r));
          // Merge server/local-style fallback in one pass: server wins, dedup by key.
          setResults(mergeDedup(r, visibleThreadMatches(liveThreads, trimmedQuery)));
        })
        .catch((e) => {
          if (genRef.current !== gen) return;
          setErr(String(e));
        })
        .finally(() => {
          if (genRef.current === gen) setSearching(false);
        });
    }, DEBOUNCE_MS);

    return () => window.clearTimeout(timer);
  }, [
    activeAccount,
    activeLabel,
    folder,
    meaning,
    query,
    setErr,
    setMeaningNote,
    setNoMoreOlder,
    setResults,
    setSearching,
    threads,
  ]);
}

function mergeDedup<T extends { account_id?: string; thread_id?: string }>(
  primary: T[],
  fallback: T[],
): T[] {
  const seen = new Set<string>();
  const out: T[] = [];
  for (const t of [...primary, ...fallback]) {
    const k = t.account_id && t.thread_id
      ? `${t.account_id}:${t.thread_id}`
      : JSON.stringify(t);
    if (seen.has(k)) continue;
    seen.add(k);
    out.push(t);
  }
  return out;
}
