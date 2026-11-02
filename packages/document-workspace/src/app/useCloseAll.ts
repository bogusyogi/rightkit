import { useCallback, useRef, useState, type MutableRefObject } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { isUntitled, type LoadedDocument } from "@rightkit/document-core/documents/types";
import type { DocumentTabs } from "./useDocumentTabs.js";
import type { BlockDraftRegistry } from "@rightkit/document-core/app/blockDrafts";
import type { useHtmlSession } from "./useHtmlSession.js";
import { toast } from "@rightkit/platform-ui/headless";
import { resolveDocumentDrafts } from "./draftResolution.js";

interface CloseAllDeps {
  openDocsRef: MutableRefObject<LoadedDocument[]>;
  isDirty: (doc: LoadedDocument) => boolean;
  setOpenDocs: DocumentTabs["setOpenDocs"];
  setActivePath: DocumentTabs["setActivePath"];
  teardownDoc: (path: string, closingDoc: LoadedDocument | undefined) => void;
  blockDrafts: BlockDraftRegistry;
  /** Whole session object (same as the original deps-array entries): only saveAllHtmlPages is
   *  used, but passing the session keeps both call sites' dep expressions identical to inline. */
  htmlSession: ReturnType<typeof useHtmlSession>;
  onSaveFlushed: (doc: LoadedDocument, options?: { silent?: boolean }) => Promise<string | null>;
  docxSaver: MutableRefObject<(() => Promise<ArrayBuffer | null>) | null>;
  docxSaversRef: MutableRefObject<Map<string, () => Promise<ArrayBuffer | null>>>;
  onDocxSave: (path: string, buffer: ArrayBuffer) => Promise<void>;
  onDocxSaveAs: (doc: LoadedDocument, buffer?: ArrayBuffer) => Promise<string | null>;
}

/**
 * Bulk close + quit flows. Close All snapshots ordered affected paths (sorted dirty tabs) plus the
 * full ordered snapshot so Save All/Don't Save can close clean tabs only after the dirty queue
 * succeeds; the quit flow runs the same Save-All/Discard-All choice before destroying the window.
 * Owns the confirmQuit/confirmCloseAll dialog state (not needed until after requestClose is
 * constructed, so unlike the close/conflict/html dialog state it can live in this hook).
 * Extracted verbatim from useDocuments; bodies are unchanged.
 */
export function useCloseAll({
  openDocsRef,
  isDirty,
  setOpenDocs,
  setActivePath,
  teardownDoc,
  blockDrafts,
  htmlSession,
  onSaveFlushed,
  docxSaver,
  docxSaversRef,
  onDocxSave,
  onDocxSaveAs,
}: CloseAllDeps) {
  const [confirmQuit, setConfirmQuit] = useState(false);
  // Close All bulk chooser: snapshots ordered affected paths (sorted dirty tabs) plus full ordered
  // snapshot so Save All/Don't Save can close clean tabs only after the dirty queue succeeds.
  const [confirmCloseAll, setConfirmCloseAll] = useState<{
    orderedPaths: string[];
    affectedPaths: string[];
  } | null>(null);

  // Close All: one operation per spec. Snapshot ordered affected paths. If any dirty
  // (isDirty includes modified block drafts), open the bulk Save/Don't Save/Cancel
  // chooser instead of auto-saving. The dialog lists ordered dirty docs; Save All
  // resolves each draft with Done then persists sequentially, stopping on first
  // failure/cancel (no teardown for remaining). Don't Save discards drafts then
  // closes all. Cancel stays. Clean tabs close only via the chooser's Save/Don't
  // Save path — they are not torn down before the user decides.
  const closeAllInProgress = useRef(false);

  /** After `closedPaths` leave the tab list, keep the current active tab if it survives; otherwise
   *  activate the first remaining tab (snapshot order). closeAll() gets remaining=[] → null, so the
   *  old null-out behavior is preserved. */
  const fixActiveAfterClose = useCallback(
    (closedPaths: string[]) => {
      const closed = new Set(closedPaths);
      setActivePath((cur) => {
        if (cur && !closed.has(cur)) return cur;
        const remaining = openDocsRef.current.filter((d) => !closed.has(d.path));
        return remaining[0]?.path ?? null;
      });
    },
    [openDocsRef, setActivePath],
  );

  /** Close a chosen subset of tabs — the engine behind Close All, Close Others, and Close to the
   *  Right. Dirty docs route through the same Save All / Don't Save bulk chooser closeAll uses. */
  const closeMany = useCallback(
    (paths: string[]) => {
      if (closeAllInProgress.current) return;
      if (confirmCloseAll) return; // dialog already open
      const wanted = new Set(paths);
      const ordered = [...openDocsRef.current]
        .filter((d) => wanted.has(d.path))
        .sort((a, b) => a.path.localeCompare(b.path));
      if (ordered.length === 0) return;
      const affected = ordered.filter((doc) => isDirty(doc));
      if (affected.length === 0) {
        for (const doc of ordered) teardownDoc(doc.path, doc);
        const orderedSet = new Set(ordered.map((d) => d.path));
        setOpenDocs((prev) => prev.filter((d) => !orderedSet.has(d.path)));
        fixActiveAfterClose(ordered.map((d) => d.path));
        return;
      }
      setConfirmCloseAll({
        orderedPaths: ordered.map((d) => d.path),
        affectedPaths: affected.map((d) => d.path),
      });
    },
    [isDirty, teardownDoc, setOpenDocs, fixActiveAfterClose, confirmCloseAll, openDocsRef],
  );

  const closeAll = useCallback(() => {
    closeMany(openDocsRef.current.map((d) => d.path));
  }, [closeMany, openDocsRef]);

  const confirmCloseAllSaveAll = useCallback(async () => {
    const snapshot = confirmCloseAll;
    if (!snapshot) return;
    if (closeAllInProgress.current) return;
    closeAllInProgress.current = true;
    setConfirmCloseAll(null);
    try {
      const { orderedPaths, affectedPaths } = snapshot;
      // Save each affected doc in snapshot order (already sorted)
      for (const path of affectedPaths) {
        const doc = openDocsRef.current.find((d) => d.path === path);
        if (!doc) continue;
        // Save = Done then persist. Resolve draft first; a stale/failed Done blocks the write.
        if (blockDrafts.snapshotModified(path).length > 0) {
          const outcome = await resolveDocumentDrafts(blockDrafts, path, "close-all", "done");
          if (outcome !== "resolved") {
            toast(`Close All cancelled — could not save ${doc.displayName}`, "error");
            return;
          }
        }
        if (doc.format === "html") {
          const ok = await htmlSession.saveAllHtmlPages(path);
          if (!ok) {
            toast(`Close All cancelled — could not save ${doc.displayName}`, "error");
            return;
          }
        } else if (doc.format === "docx") {
          if (isUntitled(doc.path)) {
            const savedPath = await onDocxSaveAs(doc);
            if (!savedPath) {
              toast(`Close All cancelled — could not save ${doc.displayName}`, "error");
              return;
            }
            const newDoc = openDocsRef.current.find((d) => d.path === savedPath) ?? {
              ...doc,
              path: savedPath,
            };
            teardownDoc(savedPath, newDoc as typeof doc);
            // Remove both old untitled path (if still present) and new saved path in one update
            setOpenDocs((prev) => prev.filter((d) => d.path !== path && d.path !== savedPath));
            continue;
          }
          const ab = await (docxSaversRef.current.get(path) ?? docxSaver.current)?.();
          if (ab) {
            await onDocxSave(path, ab);
          } else {
            toast(`Close All cancelled — could not save ${doc.displayName}`, "error");
            return;
          }
        } else if (doc.format === "image") {
          const saved = await onSaveFlushed(doc);
          if (!saved) {
            toast(`Close All cancelled — could not save ${doc.displayName}`, "error");
            return;
          }
          if (isUntitled(doc.path) && typeof saved === "string" && saved !== path) {
            const newDoc = openDocsRef.current.find((d) => d.path === saved) ?? {
              ...doc,
              path: saved,
            };
            teardownDoc(saved, newDoc as typeof doc);
            setOpenDocs((prev) => prev.filter((d) => d.path !== path && d.path !== saved));
            continue;
          }
        } else {
          const saved = await onSaveFlushed(doc);
          if (!saved) {
            toast(`Close All cancelled — could not save ${doc.displayName}`, "error");
            return;
          }
          if (isUntitled(doc.path) && typeof saved === "string" && saved !== path) {
            const newDoc = openDocsRef.current.find((d) => d.path === saved) ?? {
              ...doc,
              path: saved,
            };
            teardownDoc(saved, newDoc as typeof doc);
            setOpenDocs((prev) => prev.filter((d) => d.path !== path && d.path !== saved));
            continue;
          }
        }
        teardownDoc(path, doc);
        setOpenDocs((prev) => prev.filter((d) => d.path !== path));
      }
      // All affected saved; now close clean tabs in one batch
      const cleanPaths = orderedPaths.filter((p) => !affectedPaths.includes(p));
      for (const p of cleanPaths) {
        const doc = openDocsRef.current.find((d) => d.path === p);
        if (doc) teardownDoc(p, doc);
      }
      if (cleanPaths.length > 0) {
        const cleanSet = new Set(cleanPaths);
        setOpenDocs((prev) => prev.filter((d) => !cleanSet.has(d.path)));
      }
      fixActiveAfterClose(orderedPaths);
    } finally {
      closeAllInProgress.current = false;
    }
  }, [
    confirmCloseAll,
    blockDrafts,
    htmlSession,
    onSaveFlushed,
    docxSaver,
    docxSaversRef,
    onDocxSave,
    onDocxSaveAs,
    teardownDoc,
    setOpenDocs,
    fixActiveAfterClose,
    openDocsRef,
  ]);

  const confirmCloseAllDontSave = useCallback(async () => {
    const snapshot = confirmCloseAll;
    if (!snapshot) return;
    if (closeAllInProgress.current) return;
    closeAllInProgress.current = true;
    setConfirmCloseAll(null);
    try {
      const { orderedPaths, affectedPaths } = snapshot;
      for (const path of affectedPaths) {
        if (blockDrafts.snapshotModified(path).length > 0) {
          await resolveDocumentDrafts(blockDrafts, path, "close-all", "discard");
        }
      }
      for (const p of orderedPaths) {
        const doc = openDocsRef.current.find((d) => d.path === p);
        if (doc) teardownDoc(p, doc);
      }
      const orderedSet = new Set(orderedPaths);
      setOpenDocs((prev) => prev.filter((d) => !orderedSet.has(d.path)));
      fixActiveAfterClose(orderedPaths);
    } finally {
      closeAllInProgress.current = false;
    }
  }, [confirmCloseAll, blockDrafts, teardownDoc, setOpenDocs, fixActiveAfterClose, openDocsRef]);

  const confirmCloseAllCancel = useCallback(() => {
    setConfirmCloseAll(null);
  }, []);

  const quitNow = useCallback(async () => {
    await getCurrentWindow().destroy();
  }, []);

  // Save All before quit: snapshot ordered affected paths, resolve/save every affected
  // document+page in sequence; quit only after every Done + disk write succeeds. Any failure
  // cancels the quit with the tab still open. Single sequential operation per spec.
  const quitSaveAll = useCallback(async () => {
    const ordered = [...openDocsRef.current]
      .filter((doc) => isDirty(doc))
      .sort((a, b) => a.path.localeCompare(b.path));
    // Ensure block-draft ordering is respected: snapshotModified is already sorted by identity key,
    // but ordering the doc list by path matches that deterministic order for the document-level loop.
    for (const doc of ordered) {
      if (doc.format === "html") {
        const ok = await htmlSession.saveAllHtmlPages(doc.path);
        if (!ok) {
          toast(`Quit cancelled — could not save ${doc.displayName}`, "error");
          return;
        }
      } else if (doc.format === "docx") {
        const buffer = await (docxSaversRef.current.get(doc.path) ?? docxSaver.current)?.();
        let saved: string | null = null;
        if (isUntitled(doc.path)) saved = await onDocxSaveAs(doc, buffer ?? undefined);
        else if (buffer) {
          await onDocxSave(doc.path, buffer);
          saved = doc.path;
        }
        if (!saved) {
          toast(`Quit cancelled — could not save ${doc.displayName}`, "error");
          return;
        }
      } else {
        const saved = await onSaveFlushed(doc);
        if (!saved) {
          toast(`Quit cancelled — could not save ${doc.displayName}`, "error");
          return;
        }
      }
    }
    await quitNow();
  }, [
    htmlSession.saveAllHtmlPages,
    isDirty,
    onSaveFlushed,
    quitNow,
    docxSaversRef,
    docxSaver,
    onDocxSaveAs,
    onDocxSave,
    openDocsRef,
  ]);

  // Quit without saving: discard every open block draft, then quit (whole-document edits are
  // dropped by the existing dirty-state semantics).
  const quitDiscardAll = useCallback(async () => {
    for (const draft of blockDrafts.snapshotModified())
      await blockDrafts.resolve(draft.identity, "quit", "discard");
    await quitNow();
  }, [blockDrafts, quitNow]);

  return {
    confirmQuit,
    setConfirmQuit,
    confirmCloseAll,
    setConfirmCloseAll,
    closeAll,
    closeMany,
    confirmCloseAllSaveAll,
    confirmCloseAllDontSave,
    confirmCloseAllCancel,
    quitNow,
    quitSaveAll,
    quitDiscardAll,
  };
}
