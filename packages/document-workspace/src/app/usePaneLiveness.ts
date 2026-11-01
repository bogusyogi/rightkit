import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import type { LoadedDocument } from "@rightkit/document-core/documents/types";
import type { BlockDraftRegistry } from "@rightkit/document-core/app/blockDrafts";
import { documentHasPinnedState, type DocumentFormat } from "@rightkit/document-core/app/resourceCensus";
import { dirtyPaneMayShell, liveDocumentPaths } from "./tabMountPolicy";
import { loadTabWarmOrder, restoredTabWarmOrder, saveTabWarmOrder } from "./tabWarmOrder";
import { nativeAutomationOwnsSession } from "./useDocumentRuntimeEffects";
import { scheduleAfterPaintOrDelay } from "@rightkit/document-core/app/deferredMount";

interface PaneLivenessDeps {
  openDocs: LoadedDocument[];
  activePath: string | null;
  isDirty: (doc: LoadedDocument) => boolean;
  /** In-flight saves (useDocuments' pendingSavePaths) — a saving tab's router stays live. */
  pendingSavePaths: ReadonlySet<string>;
  /** True while the launch restore is in flight; gates the post-restore warm-up walk. */
  restoringSession: boolean;
  /** Block draft registry: a tab with any open block draft is never shelled (editor-owned state). */
  blockDrafts?: BlockDraftRegistry;
}

/**
 * Warm-MRU pane liveness: which open tabs keep a live router vs a lightweight shell. Every open
 * tab keeps a shell; only active + most-recent ordinary panes stay live, and writes or dirty state
 * without an app-owned source/draft snapshot protect a path until safe to evict. Owns the MRU
 * history (`recentPaths`), the previous-active tracker, the warm-order persistence + post-restore
 * warm-up walk, `protectedPaths`/`formatByPath`/`desiredLivePaths`, and the deferred rendered/live
 * reconciliation. `pruneClosedPaths` is called by App's shared closed-tab eviction effect so
 * rendered/live/recent markers die with their tab. Pure policy lives in tabMountPolicy.ts /
 * tabWarmOrder.ts. Extracted verbatim from App.tsx; bodies are unchanged.
 */
export function usePaneLiveness({
  openDocs,
  activePath,
  isDirty,
  pendingSavePaths,
  restoringSession,
  blockDrafts,
}: PaneLivenessDeps) {
  const previousActiveRef = useRef(activePath);
  const [previousActivePath, setPreviousActivePath] = useState<string | null>(null);
  useLayoutEffect(() => {
    if (previousActiveRef.current === activePath) return;
    setPreviousActivePath(previousActiveRef.current);
    previousActiveRef.current = activePath;
  }, [activePath]);

  // Presence-only subscription: re-derive protection when a document gains or loses a block draft
  // (open / commit / discard). Typing inside a draft does not fire it.
  const [draftPresenceTick, setDraftPresenceTick] = useState(0);
  useEffect(() => {
    if (!blockDrafts) return;
    setDraftPresenceTick((tick) => tick + 1);
    return blockDrafts.subscribePresence(() => setDraftPresenceTick((tick) => tick + 1));
  }, [blockDrafts]);

  // Every open tab keeps a lightweight shell. Only active + most-recent ordinary panes stay live;
  // writes or dirty state without an app-owned source/draft snapshot add a path until safe to evict.
  // biome-ignore lint/correctness/useExhaustiveDependencies: draftPresenceTick re-derives the set when a block draft opens or closes
  const protectedPaths = useMemo(
    () =>
      new Set(
        openDocs
          .filter((doc) => {
            // Switching tabs is the re-render boundary for ref-backed text edits. Read the just-
            // departed active document here before after-paint reconciliation can evict its router.
            const dirty = isDirty(doc);
            if (doc.path === activePath && dirty) return true;
            // An open block draft (clean or modified) lives in its mounted editor, not in editsRef;
            // shelling would orphan it. Protection ends when the draft is committed or discarded.
            if (blockDrafts?.hasDraft(doc.path)) return true;
            // TXT/Markdown sources are synchronously authoritative in editsRef; HTML entry and
            // sub-page sources are synchronously authoritative in htmlPagesRef. Modified rich
            // blocks use the app-owned registry. Those formats may therefore shell while dirty,
            // but only through this narrow, explicit allow-list. DOCX/PDF fail closed; image
            // remains governed by its recipe census.
            const hasExternalSourceOrDraftOwner =
              doc.format === "txt" || doc.format === "md" || doc.format === "html";
            const dirtyProtection =
              doc.format === "image"
                ? documentHasPinnedState(doc.path)
                : dirty && !dirtyPaneMayShell(doc.format, hasExternalSourceOrDraftOwner);
            return dirtyProtection || pendingSavePaths.has(doc.path);
          })
          .map((doc) => doc.path),
      ),
    [activePath, blockDrafts, draftPresenceTick, isDirty, openDocs, pendingSavePaths],
  );
  // Background tabs keep lightweight shells. Active mounts synchronously in DocumentRouterTable;
  // these sets only record visited paths & reconcile active/MRU/protected ownership after paint.
  const [renderedPaths, setRenderedPaths] = useState<Set<string>>(() => new Set());
  const [livePaths, setLivePaths] = useState<Set<string>>(() => new Set());
  // Most-recently-active paths, newest first. Held as state (not a ref) because `desiredLivePaths`
  // memoizes on it: a ref would mutate without re-running the memo, so a revisited tab would never
  // re-enter the live set. Updated only when the active tab actually changes, so its identity is
  // stable across unrelated renders and the after-paint reconciliation below cannot loop.
  const automationOwnsSession = nativeAutomationOwnsSession();
  const savedWarmOrder = useRef<readonly string[]>(automationOwnsSession ? [] : loadTabWarmOrder());
  const restoreStarted = useRef(false);
  const [warmOrderReady, setWarmOrderReady] = useState(false);
  const [recentPaths, setRecentPaths] = useState<readonly string[]>(() => []);
  useEffect(() => {
    if (!activePath) return;
    setRecentPaths((prev) => {
      if (prev[0] === activePath) return prev;
      return [activePath, ...prev.filter((path) => path !== activePath)];
    });
  }, [activePath]);
  // Session restore loads file bytes & chunks in the background, but a restored router still used
  // to cold-mount on first click because its in-memory MRU vanished at quit. Rebuild that bounded
  // warm set after restore, one pane per quiet interval: prior activation order first, then tabs
  // nearest the active one. User activity cancels remaining speculative work.
  useEffect(() => {
    if (restoringSession) {
      restoreStarted.current = true;
      return;
    }
    if (!restoreStarted.current) return;
    restoreStarted.current = false;
    if (automationOwnsSession) return;
    setWarmOrderReady(true);
    const queue = restoredTabWarmOrder(
      openDocs.map((doc) => doc.path),
      activePath,
      savedWarmOrder.current,
    );
    let index = 0;
    let timer = 0;
    const warmNext = () => {
      const path = queue[index++];
      if (!path) return;
      setRecentPaths((prev) => (prev.includes(path) ? prev : [...prev, path]));
      timer = window.setTimeout(warmNext, 250);
    };
    timer = window.setTimeout(warmNext, 250);
    return () => window.clearTimeout(timer);
  }, [activePath, automationOwnsSession, openDocs, restoringSession]);
  useEffect(() => {
    if (!warmOrderReady || automationOwnsSession) return;
    saveTabWarmOrder(recentPaths);
  }, [automationOwnsSession, recentPaths, warmOrderReady]);
  useEffect(() => {
    const openPaths = new Set(openDocs.map((doc) => doc.path));
    setRecentPaths((prev) => {
      const next = prev.filter((path) => openPaths.has(path));
      return next.length === prev.length ? prev : next;
    });
  }, [openDocs]);
  // Format per open path, so a SHELLED heavyweight document's mount-policy cost doesn't decay to
  // ordinary weight merely because its census resources were released on eviction — see
  // `candidateWeight` in tabMountPolicy.ts.
  const formatByPath = useMemo(() => {
    const map = new Map<string, DocumentFormat>();
    for (const doc of openDocs) {
      if (
        doc.format === "txt" ||
        doc.format === "md" ||
        doc.format === "mdx" ||
        doc.format === "html" ||
        doc.format === "web" ||
        doc.format === "docx" ||
        doc.format === "pdf" ||
        doc.format === "image"
      )
        map.set(doc.path, doc.format);
    }
    return map;
  }, [openDocs]);
  const desiredLivePaths = useMemo(
    () =>
      liveDocumentPaths({
        activePath,
        previousActivePath,
        protectedPaths,
        recentPaths: recentPaths.filter((path) => formatByPath.has(path)),
        formatByPath,
      }),
    [activePath, previousActivePath, protectedPaths, recentPaths, formatByPath],
  );
  // Active routers bypass this deferred state below, so a tab click mounts its viewer in the same
  // commit and never paints an empty shell. Reconciliation remains deferred so eviction/cleanup
  // cannot compete with activation work on the click frame.
  useEffect(() => {
    const target = desiredLivePaths;
    return scheduleAfterPaintOrDelay(() => {
      setRenderedPaths((prev) => {
        const next = new Set(prev);
        for (const path of target) next.add(path);
        if (next.size === prev.size && [...next].every((path) => prev.has(path))) return prev;
        return next;
      });
      setLivePaths((prev) => {
        if (prev.size === target.size && [...prev].every((path) => target.has(path))) return prev;
        return new Set(target);
      });
    });
  }, [desiredLivePaths]);

  // T5-2: drop closed tabs' keep-alive markers — otherwise renderedPaths grows for the life of the
  // app instead of tracking only the currently-open tabs, and without the recentPaths prune the MRU
  // window fills with closed paths (which both starves live slots from the tabs actually open and
  // lets a reopened path inherit stale recency it did not earn). Called by App's shared closed-tab
  // eviction effect once it has diffed openDocs, so all per-path cache/liveness cleanup still runs
  // in the same pass.
  const pruneClosedPaths = useCallback((closedPaths: string[]) => {
    setRenderedPaths((prevRendered) => {
      if (!closedPaths.some((p) => prevRendered.has(p))) return prevRendered;
      const next = new Set(prevRendered);
      for (const p of closedPaths) next.delete(p);
      return next;
    });
    setLivePaths((prevLive) => {
      if (!closedPaths.some((p) => prevLive.has(p))) return prevLive;
      const next = new Set(prevLive);
      for (const p of closedPaths) next.delete(p);
      return next;
    });
    setRecentPaths((prevRecent) => {
      if (!closedPaths.some((p) => prevRecent.includes(p))) return prevRecent;
      return prevRecent.filter((p) => !closedPaths.includes(p));
    });
  }, []);

  return { livePaths, renderedPaths, pruneClosedPaths };
}
