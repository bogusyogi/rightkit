/**
 * Search-provider hook for the Markdown hybrid editor.
 *
 * Owns the find/replace session state ({@link searchStateRef}) and the
 * DOM-highlight driven find callbacks, and registers a {@link SearchProvider}
 * with the owning chrome while mounted. Extracted from MarkdownEditor.tsx —
 * the callbacks operate on the live {@link EditorView} held by the caller's
 * `viewRef`, so they stay valid across re-renders without re-subscribing.
 */

import { useCallback, useEffect, useRef, type MutableRefObject } from "react";
import type { EditorView } from "prosemirror-view";
import type { SearchOptions, SearchProvider, SearchResult } from "@rightkit/document-core/documents/types";
import {
  applyHighlights,
  clearHighlights,
  findLiteralMatches,
  mapOffsetToDOM,
} from "@rightkit/document-core/documents/searchUtils";
import { searchMappingsFor } from "./markdownHybridDom";
import { editablePmRangeForMatch } from "./projection/pmEditorState";

const EMPTY_SEARCH: SearchResult = { count: 0, activeIndex: -1 };
/** Transaction meta stamped on find/replace edits so the commit path can label
 *  them "Replace" and isolate history instead of coalescing with typing. */
export const FIND_REPLACE_META = "viewright:markdown-find-replace";

/**
 * Options for {@link useMarkdownSearchProvider}.
 */
export interface UseMarkdownSearchProviderOptions {
  readonly viewRef: MutableRefObject<EditorView | null>;
  readonly registerSearchProvider?: (provider: SearchProvider | null) => void;
  /** Read mode: find stays live but replace is hidden and guarded (Wave 2). */
  readonly readOnly?: boolean;
}

/**
 * Hook return value. Only {@link refreshFind} is consumed by the editor —
 * it re-runs the active query after doc changes (commit, external text sync).
 */
export interface UseMarkdownSearchProviderResult {
  readonly refreshFind: () => void;
}

export function useMarkdownSearchProvider({
  viewRef,
  registerSearchProvider,
  readOnly = false,
}: UseMarkdownSearchProviderOptions): UseMarkdownSearchProviderResult {
  const readOnlyRef = useRef(readOnly);
  useEffect(() => {
    readOnlyRef.current = readOnly;
  }, [readOnly]);
  const searchStateRef = useRef<{ query: string; options: SearchOptions; activeIndex: number }>({
    query: "",
    options: { matchCase: false, wholeWord: false },
    activeIndex: -1,
  });

  const runFind = useCallback(
    (query: string, options: SearchOptions, requestedIndex = 0): SearchResult => {
      const view = viewRef.current;
      if (!view || !query) {
        clearHighlights();
        searchStateRef.current = { query, options, activeIndex: -1 };
        return EMPTY_SEARCH;
      }

      const { text: flatText, mappings } = searchMappingsFor(view.dom);
      const matches = findLiteralMatches(flatText, query, options);
      if (!matches.length) {
        clearHighlights();
        searchStateRef.current = { query, options, activeIndex: -1 };
        return EMPTY_SEARCH;
      }

      const activeIndex = ((requestedIndex % matches.length) + matches.length) % matches.length;
      const ranges = matches.map((match) => {
        const start = mapOffsetToDOM(mappings, match.start);
        const end = mapOffsetToDOM(mappings, match.end);
        const range = document.createRange();
        range.setStart(start.node, start.offset);
        range.setEnd(end.node, end.offset);
        return range;
      });
      applyHighlights(ranges, activeIndex);
      ranges[activeIndex]?.startContainer.parentElement?.scrollIntoView?.({ block: "center" });
      searchStateRef.current = { query, options, activeIndex };
      return { count: matches.length, activeIndex };
    },
    [viewRef],
  );

  const stepFind = useCallback(
    (direction: 1 | -1): SearchResult => {
      const state = searchStateRef.current;
      if (!state.query || state.activeIndex < 0) return EMPTY_SEARCH;
      return runFind(state.query, state.options, state.activeIndex + direction);
    },
    [runFind],
  );

  const replaceFindMatch = useCallback(
    (replacement: string): SearchResult => {
      const view = viewRef.current;
      const state = searchStateRef.current;
      if (readOnlyRef.current || !view || !state.query || state.activeIndex < 0)
        return EMPTY_SEARCH;
      const { text: flatText, mappings } = searchMappingsFor(view.dom);
      const matches = findLiteralMatches(flatText, state.query, state.options);
      const match = matches[state.activeIndex];
      const range = match ? editablePmRangeForMatch(view, mappings, match) : null;
      if (!range) return runFind(state.query, state.options, state.activeIndex + 1);
      view.dispatch(
        view.state.tr
          .insertText(replacement, range.from, range.to)
          .setMeta(FIND_REPLACE_META, { label: "Replace" }),
      );
      return runFind(state.query, state.options, state.activeIndex);
    },
    [runFind, viewRef],
  );

  const replaceAllFindMatches = useCallback(
    (replacement: string): SearchResult => {
      const view = viewRef.current;
      const state = searchStateRef.current;
      if (readOnlyRef.current || !view || !state.query) return EMPTY_SEARCH;
      const { text: flatText, mappings } = searchMappingsFor(view.dom);
      const ranges = findLiteralMatches(flatText, state.query, state.options)
        .map((match) => editablePmRangeForMatch(view, mappings, match))
        .filter((range): range is { from: number; to: number } => range !== null)
        .sort((left, right) => right.from - left.from);
      if (!ranges.length) return runFind(state.query, state.options, 0);
      let transaction = view.state.tr;
      for (const range of ranges)
        transaction = transaction.insertText(replacement, range.from, range.to);
      view.dispatch(transaction.setMeta(FIND_REPLACE_META, { label: "Replace" }));
      return runFind(state.query, state.options, 0);
    },
    [runFind, viewRef],
  );

  const refreshFind = useCallback(() => {
    const state = searchStateRef.current;
    if (state.query) runFind(state.query, state.options, state.activeIndex);
  }, [runFind]);

  useEffect(() => {
    if (!registerSearchProvider) return;
    const provider: SearchProvider = {
      kind: "markdown-preview",
      canReplace: !readOnly,
      supportsMatchCase: true,
      supportsWholeWord: true,
      supportsRegex: true,
      find: runFind,
      next: () => stepFind(1),
      prev: () => stepFind(-1),
      replace: replaceFindMatch,
      replaceAll: replaceAllFindMatches,
      seek: (matchIndex) => {
        const state = searchStateRef.current;
        if (state.query) runFind(state.query, state.options, matchIndex);
      },
      close: () => {
        searchStateRef.current = {
          query: "",
          options: searchStateRef.current.options,
          activeIndex: -1,
        };
        clearHighlights();
      },
    };
    registerSearchProvider(provider);
    return () => {
      provider.close();
      registerSearchProvider(null);
    };
  }, [
    registerSearchProvider,
    replaceAllFindMatches,
    replaceFindMatch,
    runFind,
    stepFind,
    readOnly,
  ]);

  return { refreshFind };
}
