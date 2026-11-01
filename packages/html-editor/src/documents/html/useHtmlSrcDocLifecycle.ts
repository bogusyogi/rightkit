import { useEffect, type Dispatch, type MutableRefObject, type SetStateAction } from "react";
import { LIVE_PREVIEW_DEBOUNCE_MS, structuralPart } from "./htmlDocumentShared.js";

interface Args {
  drawer: boolean;
  mode: "interactive" | "preview";
  activeBuffer: string;
  applied: string;
  setApplied: Dispatch<SetStateAction<string>>;
  srcDoc: string;
  srcDocRef: MutableRefObject<string>;
  pendingEditorCloseRef: MutableRefObject<{ srcDocAtCommit: string; timer: number } | null>;
  clearPendingEditorClose: () => void;
}

/**
 * srcDoc lifecycle effects: the debounced live rebuild from the edit buffer, the srcDoc ref sync
 * that releases a parked inline-editor close once the rebuilt document is on screen, and the
 * timer cleanup on unmount.
 */
export function useHtmlSrcDocLifecycle({
  drawer,
  mode,
  activeBuffer,
  applied,
  setApplied,
  srcDoc,
  srcDocRef,
  pendingEditorCloseRef,
  clearPendingEditorClose,
}: Args) {
  // Live preview: rebuild from the live buffer shortly after typing stops, so HTML/structure edits
  // appear WITHOUT pressing Save (Save still persists to disk). In Interactive a CSS-only edit is
  // skipped here — the bridge hot-applies it with no reload. Scroll is preserved via scrollYRef.
  useEffect(() => {
    if (!drawer || activeBuffer === applied) return;
    if (mode === "interactive" && structuralPart(activeBuffer) === structuralPart(applied)) return;
    const id = window.setTimeout(() => setApplied(activeBuffer), LIVE_PREVIEW_DEBOUNCE_MS);
    return () => window.clearTimeout(id);
  }, [activeBuffer, applied, drawer, mode, setApplied]);

  useEffect(() => {
    srcDocRef.current = srcDoc;
    const pending = pendingEditorCloseRef.current;
    // Only a DIFFERENT srcDoc proves the rebuild carrying the commit is on screen.
    if (pending && srcDoc !== pending.srcDocAtCommit) clearPendingEditorClose();
  }, [srcDoc, clearPendingEditorClose, pendingEditorCloseRef, srcDocRef]);
  useEffect(
    () => () => {
      const pending = pendingEditorCloseRef.current;
      if (pending) window.clearTimeout(pending.timer);
    },
    [pendingEditorCloseRef],
  );
}
