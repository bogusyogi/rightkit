import { useCallback, useRef, type MutableRefObject } from "react";
import type { EditorView } from "@codemirror/view";
import { findLiteralMatches, type TextMatch } from "@rightkit/document-core/documents/searchUtils";
import { setSearchHighlights } from "./sourceExtensions.js";
import { scrollPosIntoView } from "./cmScroll.js";

type SearchOptions = { matchCase: boolean; wholeWord: boolean; regex?: boolean };

interface SourceSearchSnapshot {
  query: string;
  options: SearchOptions;
  matches: TextMatch[];
  activeIndex: number;
}

export function useSourceSearch({
  viewRef,
  readOnlyRef,
}: {
  viewRef: MutableRefObject<EditorView | null>;
  readOnlyRef: MutableRefObject<boolean>;
}) {
  const searchRef = useRef<SourceSearchSnapshot>({
    query: "",
    options: { matchCase: false, wholeWord: false },
    matches: [],
    activeIndex: -1,
  });
  const searchedDocumentRef = useRef<EditorView["state"]["doc"] | null>(null);

  const dispatchSearchHighlights = useCallback(
    (matches: TextMatch[], activeIndex: number) => {
      const view = viewRef.current;
      if (!view) return;
      const active = activeIndex >= 0 ? matches[activeIndex] : null;
      const selection = active
        ? { anchor: active.start, head: active.end }
        : view.state.selection.main.empty
          ? undefined
          : { anchor: view.state.selection.main.head };
      view.dispatch({
        effects: setSearchHighlights.of({ matches, activeIndex }),
        selection,
      });
      if (active) scrollPosIntoView(view, active.start);
    },
    [viewRef],
  );

  const clearSearch = useCallback(() => {
    searchedDocumentRef.current = null;
    searchRef.current = {
      query: "",
      options: searchRef.current.options,
      matches: [],
      activeIndex: -1,
    };
    dispatchSearchHighlights([], -1);
  }, [dispatchSearchHighlights]);

  const runFind = useCallback(
    (query: string, options: SearchOptions) => {
      const view = viewRef.current;
      if (!view || !query) {
        searchedDocumentRef.current = null;
        searchRef.current = { query, options, matches: [], activeIndex: -1 };
        dispatchSearchHighlights([], -1);
        return { count: 0, activeIndex: -1 };
      }
      const source = view.state.doc.toString();
      const matches = findLiteralMatches(source, query, options);
      const activeIndex = matches.length ? 0 : -1;
      searchedDocumentRef.current = view.state.doc;
      searchRef.current = { query, options, matches, activeIndex };
      dispatchSearchHighlights(matches, activeIndex);
      return { count: matches.length, activeIndex };
    },
    [dispatchSearchHighlights, viewRef],
  );

  const refreshSearch = useCallback(() => {
    const view = viewRef.current;
    const cur = searchRef.current;
    if (!view || !cur.query) return { count: 0, activeIndex: -1 };
    const matches = findLiteralMatches(view.state.doc.toString(), cur.query, cur.options);
    const selection = view.state.selection.main;
    const selectedIndex = matches.findIndex(
      (match) => match.start === selection.from && match.end === selection.to,
    );
    const activeIndex = matches.length
      ? selectedIndex >= 0
        ? selectedIndex
        : Math.min(Math.max(cur.activeIndex, 0), matches.length - 1)
      : -1;
    searchedDocumentRef.current = view.state.doc;
    searchRef.current = { ...cur, matches, activeIndex };
    dispatchSearchHighlights(matches, activeIndex);
    return { count: matches.length, activeIndex };
  }, [dispatchSearchHighlights, viewRef]);

  const stepSearch = useCallback(
    (delta: number) => {
      if (viewRef.current?.state.doc !== searchedDocumentRef.current) refreshSearch();
      const cur = searchRef.current;
      const count = cur.matches.length;
      if (!count) return { count: 0, activeIndex: -1 };
      const activeIndex = (cur.activeIndex + delta + count) % count;
      searchRef.current = { ...cur, activeIndex };
      dispatchSearchHighlights(cur.matches, activeIndex);
      return { count, activeIndex };
    },
    [dispatchSearchHighlights, refreshSearch, viewRef],
  );

  const replaceAndFind = useCallback(
    (replacement: string, all: boolean) => {
      const view = viewRef.current;
      const cur = searchRef.current;
      if (!view || readOnlyRef.current)
        return { count: cur.matches.length, activeIndex: cur.activeIndex };
      // CodeMirror maps its selection/decorations on edits; cached match offsets do not move.
      // Recompute from the live document before selecting or replacing a cached range.
      if (view.state.doc !== searchedDocumentRef.current) refreshSearch();
      const current = searchRef.current;
      const matches = all
        ? current.matches
        : current.matches[current.activeIndex]
          ? [current.matches[current.activeIndex]]
          : [];
      if (matches.length) {
        view.dispatch({
          changes: matches.map((match) => ({
            from: match.start,
            to: match.end,
            insert: replacement,
          })),
          userEvent: "input.replace",
        });
      }
      return runFind(current.query, current.options);
    },
    [readOnlyRef, refreshSearch, runFind, viewRef],
  );

  return {
    clearSearch,
    dispatchSearchHighlights,
    replaceAndFind,
    refreshSearch,
    runFind,
    searchRef,
    stepSearch,
  };
}
