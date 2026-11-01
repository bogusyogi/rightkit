import { useCallback, useEffect, useRef, type MutableRefObject } from "react";
import * as pdfjs from "pdfjs-dist";
import type { PDFDocumentProxy } from "pdfjs-dist";
import { pdfDocumentInit, pdfWasmUrl } from "@rightkit/pdf-ui";
import { recomposePdfPreview } from "./pdfSave.js";
import { composePdfPreview } from "../../state/tauriBridge.js";
import { logError } from "../../state/diagnostics.js";
import { acquireResource } from "../../app/resourceCensus.js";
import type { usePdfEditSession } from "./usePdfEditSession.js";

interface PdfPreviewRecomposeDeps {
  documentPath: string;
  editSession: ReturnType<typeof usePdfEditSession>;
  /** Best-known document_sha256 (§5.1) — populated from every inspected page graph. */
  sourceShaRef: MutableRefObject<string | null>;
  /** Guards against concurrent publishes racing: a superseded call discards its own task. */
  previewSeqRef: MutableRefObject<number>;
  previewLoadingTaskRef: MutableRefObject<ReturnType<typeof pdfjs.getDocument> | null>;
  previewDocRef: MutableRefObject<PDFDocumentProxy | null>;
  previewPagesRef: MutableRefObject<Set<number>>;
  renderedRef: MutableRefObject<Map<number, () => void>>;
  /** Indirection to the per-page renderer so this path never grows renderPage's dep list. */
  renderPageRef: MutableRefObject<(n: number) => Promise<void>>;
}

/**
 * §9.4 in-memory candidate preview: recompose the candidate document from the committed-but-unsaved
 * source commands and re-render each affected page on every source-history mutation (commit, undo,
 * redo — anything that calls `editSession.publish()`). Extracted from PdfViewer verbatim — no
 * behavior change.
 */
export function usePdfPreviewRecompose({
  documentPath,
  editSession,
  sourceShaRef,
  previewSeqRef,
  previewLoadingTaskRef,
  previewDocRef,
  previewPagesRef,
  renderedRef,
  renderPageRef,
}: PdfPreviewRecomposeDeps) {
  const onPublish = editSession.onPublish;
  // Mirrors editSession.session.sourceCommands on every render. `publish()` (from commitDraft, or
  // from the addCommands `post` above on undo/redo) fires SYNCHRONOUSLY inside the same call that
  // just invoked `setSession`, i.e. before React has re-rendered — so a listener reading
  // `editSession.session` directly would see the state from BEFORE this mutation. Deferring the
  // read to a microtask (below) lets React's synchronous end-of-event-handler flush happen first,
  // by which point this ref (updated every render, during the render itself) is current.
  const sourceCommandsRef = useRef(editSession.session.sourceCommands);
  sourceCommandsRef.current = editSession.session.sourceCommands;

  // A publish can leave work queued behind React's render, while pdf.js can keep a loading task
  // alive long after its caller has gone away. Keep hook lifetime separate from the shared preview
  // sequence so document switches & unmounts invalidate both queued work & in-flight loads.
  const mountedRef = useRef(false);
  const lifecycleRef = useRef(0);
  const queueEpochRef = useRef(0);
  const publishEpochRef = useRef(0);
  const pendingLoadingTaskRef = useRef<ReturnType<typeof pdfjs.getDocument> | null>(null);
  const retirementBarrierRef = useRef<{
    task: ReturnType<typeof pdfjs.getDocument>;
    promise: Promise<void>;
  } | null>(null);
  const deferredResumeRef = useRef<{
    lifecycle: number;
    publishEpoch: number;
    queueEpoch: number;
  } | null>(null);
  const resumeWatcherRef = useRef<Promise<void> | null>(null);
  const handleRecomposePreviewRef = useRef<
    ((lifecycle: number, publishEpoch: number) => Promise<void>) | null
  >(null);
  const destroyedLoadingTasksRef = useRef<WeakSet<object>>(new WeakSet());
  const taskDestroyPromisesRef = useRef<WeakMap<object, Promise<void>>>(new WeakMap());
  const taskOwnershipRef = useRef<
    WeakMap<object, { worker: () => void; document: (() => void) | null }>
  >(new WeakMap());

  const ensureDestroyLoadingTask = useCallback(
    (task: ReturnType<typeof pdfjs.getDocument>): Promise<void> => {
      const existing = taskDestroyPromisesRef.current.get(task);
      if (existing) return existing;
      const ownership = taskOwnershipRef.current.get(task);
      const destroyPromise = (async () => {
        try {
          if (!task.destroyed) await task.destroy();
        } catch {
          // Destruction is best-effort during unmount/path replacement.
        } finally {
          ownership?.document?.();
          ownership?.worker();
          destroyedLoadingTasksRef.current.add(task);
        }
      })();
      taskDestroyPromisesRef.current.set(task, destroyPromise);
      return destroyPromise;
    },
    [],
  );

  const waitForDestroy = useCallback(
    async (task: ReturnType<typeof pdfjs.getDocument> | null): Promise<boolean> => {
      if (!task) return true;
      const destroyPromise = ensureDestroyLoadingTask(task);
      let timeout: number | undefined;
      const completed = await Promise.race([
        destroyPromise.then(() => true),
        new Promise<boolean>((resolve) => {
          timeout = window.setTimeout(() => resolve(false), 500);
        }),
      ]);
      if (timeout !== undefined) window.clearTimeout(timeout);
      return completed;
    },
    [ensureDestroyLoadingTask],
  );

  const beginRetirement = useCallback(
    (task: ReturnType<typeof pdfjs.getDocument>): Promise<void> => {
      const promise = ensureDestroyLoadingTask(task);
      retirementBarrierRef.current = { task, promise };
      void promise.then(() => {
        if (retirementBarrierRef.current?.task === task) retirementBarrierRef.current = null;
      });
      return promise;
    },
    [ensureDestroyLoadingTask],
  );

  const deferUntilRetirement = useCallback(
    (lifecycle: number, publishEpoch: number, queueEpoch: number) => {
      const barrier = retirementBarrierRef.current;
      if (!barrier) return;
      deferredResumeRef.current = { lifecycle, publishEpoch, queueEpoch };
      if (resumeWatcherRef.current === barrier.promise) return;
      const watcher = barrier.promise.then(() => {
        if (resumeWatcherRef.current === watcher) resumeWatcherRef.current = null;
        const deferred = deferredResumeRef.current;
        deferredResumeRef.current = null;
        if (
          deferred &&
          mountedRef.current &&
          lifecycleRef.current === deferred.lifecycle &&
          publishEpochRef.current === deferred.publishEpoch &&
          queueEpochRef.current === deferred.queueEpoch
        )
          void handleRecomposePreviewRef.current?.(deferred.lifecycle, deferred.publishEpoch);
      });
      resumeWatcherRef.current = watcher;
    },
    [],
  );

  // biome-ignore lint/correctness/useExhaustiveDependencies: refs arrive as stable hook params
  const clearPreview = useCallback(() => {
    previewSeqRef.current += 1;
    const pending = pendingLoadingTaskRef.current;
    pendingLoadingTaskRef.current = null;
    const published = previewLoadingTaskRef.current;
    previewLoadingTaskRef.current = null;
    previewDocRef.current = null;
    previewPagesRef.current = new Set();
    if (pending) {
      const pendingRetirement = beginRetirement(pending);
      if (published && published !== pending)
        void pendingRetirement.then(() => beginRetirement(published));
    } else if (published) beginRetirement(published);
  }, [beginRetirement]);

  // biome-ignore lint/correctness/useExhaustiveDependencies: refs arrive as stable hook params
  const handleRecomposePreview = useCallback(
    async (lifecycle: number, publishEpoch = publishEpochRef.current) => {
      const isCurrent = () =>
        mountedRef.current &&
        lifecycleRef.current === lifecycle &&
        publishEpochRef.current === publishEpoch;
      if (!isCurrent()) return;
      const sha = sourceShaRef.current;
      if (!sha) return; // nothing inspected yet — no possible edits either
      const seq = ++previewSeqRef.current;
      const queueEpoch = queueEpochRef.current;
      const barrier = retirementBarrierRef.current;
      if (barrier) {
        const reaped = await waitForDestroy(barrier.task);
        if (!reaped) {
          deferUntilRetirement(lifecycle, publishEpoch, queueEpoch);
          return;
        }
      }
      const previousPending = pendingLoadingTaskRef.current;
      pendingLoadingTaskRef.current = null;
      if (previousPending) {
        beginRetirement(previousPending);
        const reaped = await waitForDestroy(previousPending);
        if (!reaped) {
          deferUntilRetirement(lifecycle, publishEpoch, queueEpoch);
          return;
        }
        retirementBarrierRef.current = null;
        if (!isCurrent() || previewSeqRef.current !== seq) return;
      }
      let task: ReturnType<typeof pdfjs.getDocument> | null = null;
      try {
        const composed = await recomposePdfPreview(
          { composePdfPreview },
          documentPath,
          sha,
          sourceCommandsRef.current,
        );
        if (!isCurrent() || previewSeqRef.current !== seq) return; // superseded while composing
        task = pdfjs.getDocument(
          pdfDocumentInit(composed.candidateBytes.slice(), { wasmUrl: pdfWasmUrl() }),
        );
        taskOwnershipRef.current.set(task, {
          worker: acquireResource({
            kind: "pdf-preview-worker",
            documentPath,
            format: "pdf",
          }),
          document: null,
        });
        pendingLoadingTaskRef.current = task;
        const doc = await task.promise;
        if (!isCurrent() || previewSeqRef.current !== seq) {
          if (pendingLoadingTaskRef.current === task) pendingLoadingTaskRef.current = null;
          beginRetirement(task);
          const reaped = await waitForDestroy(task); // superseded while loading — this call's own task
          if (!reaped) deferUntilRetirement(lifecycle, publishEpoch, queueEpoch);
          return;
        }
        if (pendingLoadingTaskRef.current === task) pendingLoadingTaskRef.current = null;
        const ownership = taskOwnershipRef.current.get(task);
        if (ownership && !ownership.document) {
          ownership.document = acquireResource({
            kind: "pdf-preview-document",
            documentPath,
            format: "pdf",
          });
        }
        const previousPublished = previewLoadingTaskRef.current;
        // An undo can remove the last command, making the new candidate's affected-pages list
        // empty. Re-render pages from the previous candidate too, so those slots switch back to
        // the on-disk document instead of retaining stale edited pixels.
        const pagesToRender = new Set([...previewPagesRef.current, ...composed.affectedPages]);
        if (!isCurrent() || previewSeqRef.current !== seq) {
          beginRetirement(task);
          return;
        }
        previewLoadingTaskRef.current = task;
        previewDocRef.current = doc;
        previewPagesRef.current = new Set(composed.affectedPages);
        if (previousPublished && previousPublished !== task) beginRetirement(previousPublished);
        for (const n of pagesToRender) {
          renderedRef.current.get(n)?.();
          void renderPageRef.current(n);
        }
      } catch {
        if (pendingLoadingTaskRef.current === task) pendingLoadingTaskRef.current = null;
        if (task) beginRetirement(task);
        if (isCurrent() && previewSeqRef.current === seq) {
          logError("pdf-editor:preview");
        }
      }
    },
    [beginRetirement, deferUntilRetirement, documentPath, waitForDestroy],
  );

  handleRecomposePreviewRef.current = handleRecomposePreview;

  useEffect(() => {
    mountedRef.current = true;
    const lifecycle = ++lifecycleRef.current;
    const unsubscribe = onPublish(() => {
      const queueEpoch = queueEpochRef.current;
      const publishEpoch = ++publishEpochRef.current;
      queueMicrotask(() => {
        if (
          mountedRef.current &&
          lifecycleRef.current === lifecycle &&
          queueEpochRef.current === queueEpoch &&
          publishEpochRef.current === publishEpoch
        ) {
          void handleRecomposePreview(lifecycle, publishEpoch);
        }
      });
    });
    return () => {
      unsubscribe();
      mountedRef.current = false;
      lifecycleRef.current += 1;
      clearPreview();
    };
  }, [clearPreview, handleRecomposePreview, onPublish]);

  // Save commits invalidate queued/in-flight candidate work without destroying the currently
  // published preview. If reload fails, that preview remains visible as last-known-good evidence;
  // a successful reload clears it after the fresh document is installed. Later source publishes
  // remain valid because lifecycleRef is deliberately left untouched.
  const invalidatePreview = useCallback(() => {
    queueEpochRef.current += 1;
    previewSeqRef.current += 1;
    const pending = pendingLoadingTaskRef.current;
    pendingLoadingTaskRef.current = null;
    if (pending) beginRetirement(pending);
  }, [beginRetirement, previewSeqRef]);

  return { invalidatePreview };
}
