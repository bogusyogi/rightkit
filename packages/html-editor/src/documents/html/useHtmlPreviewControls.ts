import { useCallback, useEffect, useRef, useState, type MutableRefObject, type RefObject } from "react";
import { printIsolatedHtmlMarkup } from "@rightkit/document-core/app/documentPrinting";
import { createShortcutRegistry } from "@rightkit/react-shell";
import { ctrlMetaChords } from "../../app/shortcutChords.js";
import { logError } from "@rightkit/document-core/state/diagnostics";
import { toast } from "@rightkit/platform-ui/headless";
import {
  postInteractivePrint,
  postInteractiveScrollTop,
  postInteractiveSelectAll,
  postInteractiveZoomReset,
} from "./htmlInteractiveProtocol.js";
import { documentEvent } from "@rightkit/document-core/host";

interface Args {
  active: boolean;
  drawer: boolean;
  error: string | null;
  mode: "interactive" | "preview";
  sessionId: string;
  iframeRef: RefObject<HTMLIFrameElement | null>;
  previewZoomTimer: MutableRefObject<number | null>;
  inspectScrollCancelRef: MutableRefObject<(() => void) | null>;
  scrollYRef: MutableRefObject<number>;
}

/**
 * Preview-surface controls: print routing, the Interactive<->Safe swap mask, select-all / Ctrl+A
 * scoping to the rendered frame, and the scroll-to-top / zoom-reset requests.
 */
export function useHtmlPreviewControls({
  active,
  drawer,
  error,
  mode,
  sessionId,
  iframeRef,
  previewZoomTimer,
  inspectScrollCancelRef,
  scrollYRef,
}: Args) {
  // Mask the Interactive<->Safe swap with an opaque ink layer (effect below, once `mode` is in scope).
  const [swapping, setSwapping] = useState(false);
  const swapFirstRun = useRef(true);

  useEffect(() => {
    if (!active || mode !== "interactive" || drawer || error) return;
    const onPrint = (event: Event) => {
      event.preventDefault();
      if (mode === "interactive") {
        // Ask the frame for its rendered markup rather than printing it directly: the iframe is
        // pinned to the on-screen content-pane box, so contentWindow.print() would paginate against
        // that fixed viewport, not the full document (HtmlDocumentBridge.tsx handles the reply).
        postInteractivePrint(iframeRef.current, sessionId);
        return;
      }
      // Safe mode's iframe uses sandbox="allow-same-origin" (no allow-scripts), so the parent can
      // read its DOM directly — same clipping risk applies (fixed h-full/w-full box), same fix.
      const markup = iframeRef.current?.contentDocument?.documentElement?.outerHTML;
      if (!markup) return;
      void printIsolatedHtmlMarkup(markup).then((printed) => {
        if (printed) return;
        logError("print:failed");
        toast("Couldn't open print dialog", "error");
      });
    };
    window.addEventListener(documentEvent("print"), onPrint);
    return () => window.removeEventListener(documentEvent("print"), onPrint);
  }, [active, drawer, error, iframeRef, mode, sessionId]);

  // WebView2 paints a freshly-mounted iframe's compositor surface WHITE before its document commits
  // (ignoring the element's CSS background/color-scheme), so swapping the two preview subtrees flashes
  // white. Cover the handoff with a short timed ink overlay — not iframe onLoad (srcdoc's first load is
  // unreliable here); both modes boot to an ink document, so the overlay just hides the swap gap.
  useEffect(() => {
    if (swapFirstRun.current) {
      swapFirstRun.current = false;
      return; // first paint already loads under the boot doc; only MASK subsequent mode switches
    }
    setSwapping(true);
    const t = window.setTimeout(() => setSwapping(false), 220);
    return () => window.clearTimeout(t);
  }, []);

  const selectInteractivePreview = useCallback((): boolean => {
    if (mode !== "interactive" || drawer || error) return false;
    postInteractiveSelectAll(iframeRef.current, sessionId);
    window.getSelection()?.removeAllRanges();
    return true;
  }, [drawer, error, mode, sessionId, iframeRef]);

  const scrollInteractivePreviewToTop = useCallback(() => {
    postInteractiveScrollTop(iframeRef.current, sessionId);
    scrollYRef.current = 0;
  }, [sessionId, iframeRef, scrollYRef]);

  // The frame owns its zoom (opaque origin — the parent cannot restyle it), so reset is a request.
  // The frame answers with a fresh percent, which is what clears the HUD.
  const resetInteractivePreviewZoom = useCallback(() => {
    postInteractiveZoomReset(iframeRef.current, sessionId);
  }, [sessionId, iframeRef]);

  useEffect(
    () => () => {
      if (previewZoomTimer.current != null) window.clearTimeout(previewZoomTimer.current);
      inspectScrollCancelRef.current?.();
    },
    [previewZoomTimer, inspectScrollCancelRef],
  );

  useEffect(() => {
    if (!active) return;
    const onSelectAll = (event: Event) => {
      if (!selectInteractivePreview()) return;
      event.preventDefault();
    };
    window.addEventListener(documentEvent("html-select-all"), onSelectAll);
    return () => window.removeEventListener(documentEvent("html-select-all"), onSelectAll);
  }, [active, selectInteractivePreview]);

  useEffect(() => {
    // Ctrl, Meta or both with no Alt or Shift; the capture-phase listener delegates to a registry so
    // it still runs before the iframe and editors see the key. An already-prevented event is skipped.
    const shortcuts = createShortcutRegistry();
    shortcuts.register({
      id: "html-preview.select-all",
      label: "Select all in the preview",
      chord: ctrlMetaChords("a"),
      hidden: true,
      allowComposing: true,
      run: (event) => {
        const target = event.target as HTMLElement | null;
        if (target?.closest?.("input, textarea, select, [contenteditable='true']")) return false;
        if (!selectInteractivePreview()) return false;
        event.stopPropagation();
      },
    });
    const onKeyDown = (event: KeyboardEvent) => {
      shortcuts.handle(event);
    };
    window.addEventListener("keydown", onKeyDown, true);
    return () => window.removeEventListener("keydown", onKeyDown, true);
  }, [selectInteractivePreview]);

  return { swapping, scrollInteractivePreviewToTop, resetInteractivePreviewZoom };
}
