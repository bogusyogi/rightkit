import { useCallback, useState, type Dispatch, type MutableRefObject, type RefObject, type SetStateAction } from "react";
import type { EditorView } from "@codemirror/view";
import { postScrollToInspect } from "./htmlInteractiveProtocol.js";
import { scheduleAfterPaint } from "@rightkit/document-core/app/deferredMount";
import { findSourceRange, parseAndMapSource, type HtmlInspectTarget, type SourceRange } from "./sourceMap.js";
import { writeClipboardText } from "@rightkit/document-core/state/tauriBridge";

export interface InspectMenuState {
  target: HtmlInspectTarget;
  x: number;
  y: number;
}

interface Args {
  readOnly: boolean;
  activeBuffer: string;
  mode: "interactive" | "preview";
  sessionId: string;
  iframeRef: RefObject<HTMLIFrameElement | null>;
  sourceEditorRef: MutableRefObject<EditorView | null>;
  pendingInspectRangeRef: MutableRefObject<SourceRange | null>;
  inspectScrollCancelRef: MutableRefObject<(() => void) | null>;
  revealSourceRange: (range: SourceRange) => boolean;
  setDrawerOpen: Dispatch<SetStateAction<boolean>>;
}

/** Inspect-to-source (View source) and the element context menu that offers it. */
export function useHtmlInspect({
  readOnly,
  activeBuffer,
  mode,
  sessionId,
  iframeRef,
  sourceEditorRef,
  pendingInspectRangeRef,
  inspectScrollCancelRef,
  revealSourceRange,
  setDrawerOpen,
}: Args) {
  const [inspectMenu, setInspectMenu] = useState<InspectMenuState | null>(null);

  const handleInspectTarget = useCallback(
    (target: HtmlInspectTarget) => {
      // Read mode: "View source" opens the editing drawer — keep it closed.
      if (readOnly) return;
      const range = findSourceRange(parseAndMapSource(activeBuffer), target);
      // Editor already mounted (drawer open) → reveal now. Drawer CLOSED → the editor is (re)mounting,
      // so we ONLY stash the range and let onEditorReady reveal it AFTER the editor measures. Firing our
      // own reveal here would race the mount: it can run on a not-yet-measured editor (scroll no-ops)
      // AND clear `pending`, preempting the robust post-mount reveal — that race was the intermittent
      // "View source works once, then not until you reopen the drawer" bug.
      const editorReady = !!sourceEditorRef.current;
      setDrawerOpen(true);
      // Opening the drawer narrows the preview, which can reflow the page (a CSS @media breakpoint) and
      // scroll the clicked element out of view. Ask the frame to scroll it back — twice, so we catch the
      // layout AFTER the iframe has finished resizing/reflowing (timing varies). Interactive only (the
      // bridge lives in that iframe; iframeRef is null in Safe mode so this is a harmless no-op there).
      if (mode === "interactive") {
        inspectScrollCancelRef.current?.();
        let cancelSecond: (() => void) | null = null;
        const cancelFirst = scheduleAfterPaint(() => {
          postScrollToInspect(iframeRef.current, sessionId);
          cancelSecond = scheduleAfterPaint(() =>
            postScrollToInspect(iframeRef.current, sessionId),
          );
        });
        inspectScrollCancelRef.current = () => {
          cancelFirst();
          cancelSecond?.();
        };
      }
      if (!range) return;
      pendingInspectRangeRef.current = range;
      if (editorReady) {
        window.requestAnimationFrame(() => {
          const pending = pendingInspectRangeRef.current;
          if (pending && revealSourceRange(pending)) pendingInspectRangeRef.current = null;
        });
      }
    },
    [
      activeBuffer,
      revealSourceRange,
      mode,
      sessionId,
      readOnly,
      iframeRef,
      inspectScrollCancelRef,
      pendingInspectRangeRef,
      setDrawerOpen,
      sourceEditorRef,
    ],
  );

  const openInspectMenu = useCallback(
    (target: HtmlInspectTarget, point: { x: number; y: number }) => {
      setInspectMenu({
        target,
        x: Math.max(8, Math.min(window.innerWidth - 140, point.x)),
        y: Math.max(8, Math.min(window.innerHeight - 64, point.y)),
      });
    },
    [],
  );

  const inspectFromMenu = useCallback(() => {
    const target = inspectMenu?.target;
    setInspectMenu(null);
    if (target) handleInspectTarget(target);
  }, [handleInspectTarget, inspectMenu]);

  const copyMenuValue = useCallback((value: string) => {
    void writeClipboardText(value).catch(() => {
      // Context-menu copying is best-effort; do not dismiss a useful menu on permission failure.
    });
    setInspectMenu(null);
  }, []);

  return {
    inspectMenu,
    setInspectMenu,
    handleInspectTarget,
    openInspectMenu,
    inspectFromMenu,
    copyMenuValue,
  };
}
