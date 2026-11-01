import {
  useCallback,
  useRef,
  useState,
  type Dispatch,
  type MutableRefObject,
  type SetStateAction,
} from "react";
import type { ViewMode } from "@rightkit/document-core/documents/types";
import type { HtmlSafeBlockTarget } from "./sourceMap";
import type { HtmlEditorToolbarState, HtmlSafeBlockEditorHandle } from "./HtmlSafeBlockEditor";

interface Args {
  onChangeView?: (view: ViewMode) => void;
  setFallbackView: Dispatch<SetStateAction<ViewMode | null>>;
  srcDocRef: MutableRefObject<string>;
  pendingEditorCloseRef: MutableRefObject<{ srcDocAtCommit: string; timer: number } | null>;
}

/**
 * Rich-edit session state: the safe block target, the transient interactive->safe view handoff,
 * the top toolbar's formatting snapshot, and the deferred inline-editor close (held until the
 * rebuilt srcDoc carrying the commit is on screen).
 */
export function useHtmlRichEditState({
  onChangeView,
  setFallbackView,
  srcDocRef,
  pendingEditorCloseRef,
}: Args) {
  const [safeTarget, setSafeTarget] = useState<HtmlSafeBlockTarget | null>(null);
  const pendingSafeTargetRef = useRef<HtmlSafeBlockTarget | null>(null);
  const restoreInteractiveAfterEditRef = useRef(false);
  const [richFormatting, setRichFormatting] = useState<HtmlEditorToolbarState>();
  // Narrow controls for the active rich-edit session; the top toolbar dereferences this at event
  // time (the editor mounts in the same render pass that first shows the toolbar).
  const richEditRef = useRef<HtmlSafeBlockEditorHandle | null>(null);

  const restoreInteractiveAfterTransientEdit = useCallback(() => {
    if (!restoreInteractiveAfterEditRef.current) return;
    restoreInteractiveAfterEditRef.current = false;
    pendingSafeTargetRef.current = null;
    if (onChangeView) onChangeView("interactive");
    else setFallbackView("interactive");
  }, [onChangeView, setFallbackView]);
  const clearPendingEditorClose = useCallback(() => {
    const pending = pendingEditorCloseRef.current;
    if (!pending) return;
    window.clearTimeout(pending.timer);
    pendingEditorCloseRef.current = null;
    setSafeTarget(null);
    restoreInteractiveAfterTransientEdit();
  }, [pendingEditorCloseRef, restoreInteractiveAfterTransientEdit]);
  const closeInlineEditor = useCallback(() => {
    if (pendingEditorCloseRef.current) return;
    pendingEditorCloseRef.current = {
      srcDocAtCommit: srcDocRef.current,
      timer: window.setTimeout(clearPendingEditorClose, 1_200),
    };
  }, [clearPendingEditorClose, pendingEditorCloseRef, srcDocRef]);

  return {
    safeTarget,
    setSafeTarget,
    pendingSafeTargetRef,
    restoreInteractiveAfterEditRef,
    richFormatting,
    setRichFormatting,
    richEditRef,
    restoreInteractiveAfterTransientEdit,
    closeInlineEditor,
    clearPendingEditorClose,
  };
}
