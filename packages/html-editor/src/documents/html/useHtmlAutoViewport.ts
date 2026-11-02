import { useEffect, useState, type RefObject } from "react";
import { viewportForPreviewWidth, type Viewport } from "@rightkit/document-core/documents/previewViewport";
import { trackHtmlObserver } from "./htmlResourceLifecycle.js";

/** Tracks the preview split's width and reports the matching auto viewport (desktop/tablet/mobile). */
export function useHtmlAutoViewport(splitRef: RefObject<HTMLDivElement | null>): Viewport {
  const [autoViewport, setAutoViewport] = useState<Viewport>("desktop");
  useEffect(() => {
    const container = splitRef.current;
    if (!container) return;
    // IMPORTANT (keep-alive): if an inactive/transitioning pane ever reports width 0, do NOT reset the
    // auto-viewport from that zero width. It would pick a narrow/mobile frame, and on tab-switch-back
    // (real width → desktop → no frame) the device-frame wrapper toggles, RE-PARENTING the iframe, which
    // makes Chromium RELOAD the srcdoc (scripts re-run, scroll lost). Ignoring width 0 keeps the viewport
    // stable across hide/show, so the iframe never moves and never reloads.
    const update = () => {
      const w = container.getBoundingClientRect().width;
      if (w <= 0) return;
      setAutoViewport(viewportForPreviewWidth(w));
    };
    update();
    if (typeof ResizeObserver === "undefined") {
      window.addEventListener("resize", update);
      return () => window.removeEventListener("resize", update);
    }
    const observer = new ResizeObserver(update);
    trackHtmlObserver(1);
    observer.observe(container);
    return () => {
      observer.disconnect();
      trackHtmlObserver(-1);
    };
  }, [splitRef]);
  return autoViewport;
}
