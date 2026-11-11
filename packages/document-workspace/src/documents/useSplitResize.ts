import { useCallback, type PointerEvent as ReactPointerEvent, type RefObject } from "react";
import { sourceWidthPercent } from "@rightkit/document-core/documents/splitResizeMath";

/**
 * Pointer-drag handler for the source/preview split divider: clamps the source pane between
 * minRight..(width - minLeft) and reports it as a width percentage. Shared by the markdown + mdx
 * split panes (the drag math is identical; only the container ref + width setter differ).
 */
export function useSplitResize(
  containerRef: RefObject<HTMLDivElement | null>,
  setSourceWidth: (pct: number) => void,
) {
  return useCallback(
    (event: ReactPointerEvent<HTMLDivElement>) => {
      event.preventDefault();
      const container = containerRef.current;
      if (!container) return;
      const bounds = container.getBoundingClientRect();
      let frame = 0;
      let pointerX = event.clientX;
      const onMove = (ev: PointerEvent) => {
        pointerX = ev.clientX;
        if (frame) return;
        frame = requestAnimationFrame(() => {
          frame = 0;
          setSourceWidth(sourceWidthPercent(bounds, pointerX));
        });
      };
      const onUp = () => {
        window.removeEventListener("pointermove", onMove);
        window.removeEventListener("pointerup", onUp);
        window.removeEventListener("pointercancel", onUp);
        if (frame) cancelAnimationFrame(frame);
      };
      window.addEventListener("pointermove", onMove);
      window.addEventListener("pointerup", onUp);
      window.addEventListener("pointercancel", onUp);
    },
    [containerRef, setSourceWidth],
  );
}
