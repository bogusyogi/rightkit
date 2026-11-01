import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import { saveViewState } from "../state/library.js";
import { isUntitled, type LoadedDocument, type ViewMeta, type ViewMode } from "@rightkit/document-core/documents/types";
import { resolveDocumentDrafts } from "./draftResolution.js";
import type { BlockDraftRegistry } from "@rightkit/document-core/app/blockDrafts";

/**
 * Per-format view state: preview/source mode per doc, plus the persisted visible location
 * (text/html scroll via the document pane, Markdown via its editor scroller; PDF page/zoom/fit
 * reported by the
 * viewer). Persisted per file (debounced) so reopening restores where you were. Returns the state
 * setter + refs so the document-IO flows can seed view state when a file opens/saves.
 */
export function useViewState(
  activePath: string | null,
  openDocs: LoadedDocument[],
  blockDrafts?: BlockDraftRegistry,
) {
  const [views, setViews] = useState<Record<string, ViewMode>>({});
  // Points at the active non-PDF viewer's actual scroller. Markdown owns an inner scroller so its
  // fixed editing hero never overlaps content; other formats use the document pane itself.
  const contentScrollRef = useRef<HTMLDivElement>(null);
  const viewMetaRef = useRef<Record<string, ViewMeta>>({});
  const viewsRef = useRef<Record<string, ViewMode>>({});
  const viewSaveTimer = useRef<number | null>(null);

  const persistViewMeta = useCallback((path: string) => {
    if (isUntitled(path)) return;
    if (viewSaveTimer.current) window.clearTimeout(viewSaveTimer.current);
    viewSaveTimer.current = window.setTimeout(() => {
      void saveViewState(path, viewsRef.current[path] ?? "preview", viewMetaRef.current[path]);
    }, 400);
  }, []);

  // Restore the saved scroll position for the active non-PDF doc once its scroller is in the DOM.
  // Best-effort for tall async-rendered content (e.g. Markdown); PDF restores page/zoom internally.
  useEffect(() => {
    const el = contentScrollRef.current;
    if (!el || !activePath) return;
    const doc = openDocs.find((d) => d.path === activePath);
    if (!doc || doc.format === "pdf" || doc.format === "web") return;
    const target = viewMetaRef.current[activePath]?.scroll ?? 0;
    const raf = requestAnimationFrame(() => {
      el.scrollTop = target;
    });
    return () => cancelAnimationFrame(raf);
  }, [activePath, openDocs]);

  const [pendingViewChange, setPendingViewChange] = useState<{
    next: ViewMode;
    drafts: ReturnType<BlockDraftRegistry["snapshotModified"]>;
  } | null>(null);

  const applyViewFor = useCallback(async (path: string, next: ViewMode) => {
    setViews((v) => ({ ...v, [path]: next }));
    viewsRef.current[path] = next;
    if (!isUntitled(path)) await saveViewState(path, next, viewMetaRef.current[path]);
  }, []);

  // requestViewChange is handed to every live DocumentRouter, whose memo comparator checks it by
  // identity. Read the active path + draft registry through refs so the callback stays stable across
  // tab switches; otherwise every warm pane (up to the live cap) re-renders on each switch.
  const activePathRef = useRef(activePath);
  const blockDraftsRef = useRef(blockDrafts);
  useLayoutEffect(() => {
    activePathRef.current = activePath;
    blockDraftsRef.current = blockDrafts;
  }, [activePath, blockDrafts]);

  const requestViewChange = useCallback(
    async (next: ViewMode): Promise<void> => {
      const path = activePathRef.current;
      if (!path) return;
      const drafts = blockDraftsRef.current;
      if (drafts) {
        const modified = drafts.snapshotModified(path);
        if (modified.length > 0) {
          // Modified draft: offer Done / Discard / Stay instead of silently discarding.
          setPendingViewChange({ next, drafts: modified });
          return;
        }
        // Clean draft (not in snapshotModified but exists): close silently per contract.
        // snapshotAll returns all drafts for this document (clean only at this point, since
        // modified/stale/committing are already handled above).
        const allForDoc = drafts.snapshotAll(path);
        if (allForDoc.length > 0) {
          for (const d of allForDoc) drafts.discard(d.identity);
        }
      }
      await applyViewFor(path, next);
    },
    [applyViewFor],
  );

  const confirmViewChange = useCallback(
    async (decision: "done" | "discard" | "stay") => {
      if (!pendingViewChange || !activePath || !blockDrafts) {
        setPendingViewChange(null);
        return;
      }
      if (decision === "stay") {
        setPendingViewChange(null);
        return;
      }
      const outcome = await resolveDocumentDrafts(blockDrafts, activePath, "view-change", decision);
      if (outcome !== "resolved") {
        // failed/cancelled: keep dialog open with fresh draft snapshot so error/reason stays visible
        if (blockDrafts) {
          setPendingViewChange({
            next: pendingViewChange.next,
            drafts: blockDrafts.snapshotModified(activePath),
          });
        }
        return;
      }
      const next = pendingViewChange.next;
      setPendingViewChange(null);
      await applyViewFor(activePath, next);
    },
    [activePath, applyViewFor, blockDrafts, pendingViewChange],
  );

  const cancelViewChange = useCallback(() => setPendingViewChange(null), []);

  // Capture the non-PDF scroll position for the active doc (PDF reports its own page/zoom/fit).
  const onContentScroll = useCallback(() => {
    const el = contentScrollRef.current;
    if (!el || !activePath || isUntitled(activePath)) return;
    const doc = openDocs.find((d) => d.path === activePath);
    if (!doc || doc.format === "pdf" || doc.format === "web") return;
    viewMetaRef.current[activePath] = {
      ...(viewMetaRef.current[activePath] ?? {}),
      scroll: el.scrollTop,
    };
    persistViewMeta(activePath);
  }, [activePath, openDocs, persistViewMeta]);

  // Merge PDF view state (page/zoom/fit) reported by the viewer, and persist it.
  const onPdfViewState = useCallback(
    (m: ViewMeta) => {
      if (!activePath) return;
      viewMetaRef.current[activePath] = { ...(viewMetaRef.current[activePath] ?? {}), ...m };
      persistViewMeta(activePath);
    },
    [activePath, persistViewMeta],
  );

  // Drop a closed tab's view state (mode + scroll/page/zoom meta) from all three stores. Without
  // this, `views`/`viewsRef`/`viewMetaRef` only ever grow — every path that was ever opened stays
  // keyed in these Records for the life of the app. Exposed for useDocumentWatcher's teardownDoc to
  // call alongside its other per-close pruning (fingerprints, editsRef, etc.); this hook doesn't call
  // it itself since it has no visibility into tab close.
  const dropViewState = useCallback((path: string) => {
    setViews((v) => {
      if (!(path in v)) return v;
      const n = { ...v };
      delete n[path];
      return n;
    });
    delete viewsRef.current[path];
    delete viewMetaRef.current[path];
  }, []);

  return {
    views,
    setViews,
    viewsRef,
    viewMetaRef,
    persistViewMeta,
    contentScrollRef,
    requestViewChange,
    confirmViewChange,
    cancelViewChange,
    pendingViewChange,
    onContentScroll,
    onPdfViewState,
    dropViewState,
  };
}
