import { useEffect, useMemo, useRef, useState, type Dispatch, type MutableRefObject, type SetStateAction } from "react";
import type { BlockDraftRegistry } from "@rightkit/document-core/app/blockDrafts";
import type { HtmlBlockHistoryEntry } from "./useHtmlPreviewLifecycle";
import type { HtmlSafeBlockTarget } from "./sourceMap";

interface Args {
  readOnly: boolean;
  drawerOpen: boolean;
  setDrawerOpen: Dispatch<SetStateAction<boolean>>;
  entryPath: string;
  activePath: string;
  activeBuffer: string;
  blockDrafts: BlockDraftRegistry;
  safeTarget: HtmlSafeBlockTarget | null;
  setSafeTarget: Dispatch<SetStateAction<HtmlSafeBlockTarget | null>>;
  restoreInteractiveAfterTransientEdit: () => void;
  revisionForSource: (source: string) => number;
  blockHistoryRef: MutableRefObject<HtmlBlockHistoryEntry[]>;
  redoHistoryRef: MutableRefObject<HtmlBlockHistoryEntry[]>;
  pendingEditorCloseRef: MutableRefObject<{ srcDocAtCommit: string; timer: number } | null>;
  pendingSafeTargetRef: MutableRefObject<HtmlSafeBlockTarget | null>;
}

/**
 * Block-draft lifecycle for the active page: resolves drafts when the page or source changes,
 * releases the safe target when a draft closes, and re-renders the surface on draft updates.
 */
export function useHtmlBlockDrafts({
  readOnly,
  drawerOpen,
  setDrawerOpen,
  entryPath,
  activePath,
  activeBuffer,
  blockDrafts,
  safeTarget,
  setSafeTarget,
  restoreInteractiveAfterTransientEdit,
  revisionForSource,
  blockHistoryRef,
  redoHistoryRef,
  pendingEditorCloseRef,
  pendingSafeTargetRef,
}: Args) {
  const draftPageRef = useRef(activePath);
  useEffect(() => {
    // A navigation changes iframe generation. Resolve any draft belonging to the page being left
    // before its target can be mistaken for a target in the new page. External source replacement
    // never discards typed text: a draft with changes stays open and is marked stale, as a Markdown
    // block draft is. Its source range no longer names the bytes the editor opened, so Done is
    // refused (commit reports the revision mismatch) and the user copies the text or discards the
    // draft. An editor with nothing typed has nothing to lose and closes.
    const previousPath = draftPageRef.current;
    draftPageRef.current = activePath;
    if (previousPath !== activePath) {
      const previousIdentity = { documentPath: entryPath, pagePath: previousPath };
      void blockDrafts.resolve(previousIdentity, "view-change", "discard");
      if (safeTarget?.pagePath === previousPath) {
        setSafeTarget(null);
        restoreInteractiveAfterTransientEdit();
      }
      return;
    }
    if (!safeTarget) return;
    const identity = { documentPath: entryPath, pagePath: activePath };
    const draft = blockDrafts.get(identity);
    if (
      draft &&
      draft.status !== "committing" &&
      draft.originalRevision !== revisionForSource(activeBuffer)
    ) {
      const keepTypedText = blockDrafts.hasModifiedDraft(identity);
      if (keepTypedText) {
        if (draft.status !== "stale")
          blockDrafts.markStale(identity, "Source changed while this block was open.");
      } else void blockDrafts.resolve(identity, "external-change", "discard");
      // Source changed while block editor was open — prior inverse patches are no longer valid.
      blockHistoryRef.current = blockHistoryRef.current.filter((e) => e.pagePath !== activePath);
      redoHistoryRef.current = redoHistoryRef.current.filter((e) => e.pagePath !== activePath);
      if (!keepTypedText) {
        setSafeTarget(null);
        restoreInteractiveAfterTransientEdit();
      }
    }
  }, [
    activeBuffer,
    activePath,
    blockDrafts,
    blockHistoryRef,
    redoHistoryRef,
    entryPath,
    restoreInteractiveAfterTransientEdit,
    revisionForSource,
    safeTarget,
    setSafeTarget,
  ]);

  // Entering Read mid-edit: the authoring row and overlay unmount, so discard the transient
  // draft, release the safe target, drop any pending interactive→safe handoff, and close the
  // drawer state so it can't spring back open on return to Edit via a stale pending range.
  useEffect(() => {
    if (!readOnly) return;
    pendingSafeTargetRef.current = null;
    if (drawerOpen) setDrawerOpen(false);
    if (safeTarget) {
      void blockDrafts.resolve(
        { documentPath: entryPath, pagePath: activePath },
        "view-change",
        "discard",
      );
      setSafeTarget(null);
      restoreInteractiveAfterTransientEdit();
    }
  }, [
    readOnly,
    drawerOpen,
    safeTarget,
    activePath,
    entryPath,
    blockDrafts,
    restoreInteractiveAfterTransientEdit,
    pendingSafeTargetRef,
    setDrawerOpen,
    setSafeTarget,
  ]);

  // Rich-edit session state for the top toolbar (mirrors the overlay mount condition below).
  const blockIdentity = useMemo(
    () => ({ documentPath: entryPath, pagePath: activePath }),
    [activePath, entryPath],
  );
  // Draft edits are intentionally kept outside canonical source, so registry updates must still
  // repaint this surface. In particular targetTag changes need to reach both the authoring row and
  // the transient ProseMirror DOM before Done, while the iframe/source buffer remains untouched.
  const [blockDraftTick, setBlockDraftTick] = useState(0);
  useEffect(
    () => blockDrafts.subscribe(() => setBlockDraftTick((tick) => tick + 1)),
    [blockDrafts],
  );
  void blockDraftTick;
  const blockSnapshot = safeTarget ? blockDrafts.get(blockIdentity) : undefined;
  useEffect(() => {
    if (!safeTarget || blockSnapshot) return;
    const pending = pendingEditorCloseRef.current;
    if (pending) {
      window.clearTimeout(pending.timer);
      pendingEditorCloseRef.current = null;
    }
    setSafeTarget(null);
    restoreInteractiveAfterTransientEdit();
  }, [blockSnapshot, restoreInteractiveAfterTransientEdit, safeTarget, pendingEditorCloseRef, setSafeTarget]);

  return { blockIdentity, blockSnapshot };
}
