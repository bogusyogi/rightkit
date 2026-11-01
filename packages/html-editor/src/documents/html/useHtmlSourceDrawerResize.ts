import {
  useCallback,
  useEffect,
  useRef,
  useState,
  type PointerEvent as ReactPointerEvent,
  type RefObject,
} from "react";
import { rightDrawerWidth } from "@rightkit/document-core/documents/splitResizeMath";

const DEFAULT_DRAWER_WIDTH = 460;

/**
 * Source-drawer splitter. The width is measured from the split row's right edge.
 *
 * Timing rules (the previous inline version violated all three):
 * - Bounds are read inside the animation frame, after layout, never captured at pointer-down —
 *   the row can resize mid-drag (window resize, viewport change, drawer reflow).
 * - Pointer-up flushes the final pointer position synchronously instead of cancelling the pending
 *   frame, which used to drop the last move and leave the drawer one step short of the cursor.
 * - An unmount mid-drag detaches the window listeners and cancels the frame.
 */
export function useHtmlSourceDrawerResize(splitRef: RefObject<HTMLDivElement | null>) {
  const [drawerWidth, setDrawerWidth] = useState(DEFAULT_DRAWER_WIDTH);
  const teardownRef = useRef<(() => void) | null>(null);

  useEffect(() => () => teardownRef.current?.(), []);

  const startResize = useCallback(
    (e: ReactPointerEvent<HTMLDivElement>) => {
      e.preventDefault();
      const container = splitRef.current;
      if (!container) return;
      teardownRef.current?.();
      let frame = 0;
      let pointerX = e.clientX;
      const apply = () => {
        const el = splitRef.current;
        if (el) setDrawerWidth(rightDrawerWidth(el.getBoundingClientRect(), pointerX));
      };
      const onMove = (ev: PointerEvent) => {
        pointerX = ev.clientX;
        if (frame) return;
        frame = requestAnimationFrame(() => {
          frame = 0;
          apply();
        });
      };
      const detach = () => {
        window.removeEventListener("pointermove", onMove);
        window.removeEventListener("pointerup", onUp);
        window.removeEventListener("pointercancel", onUp);
        if (frame) cancelAnimationFrame(frame);
        frame = 0;
        teardownRef.current = null;
      };
      const onUp = () => {
        const hadPendingFrame = frame !== 0;
        detach();
        if (hadPendingFrame) apply();
      };
      window.addEventListener("pointermove", onMove);
      window.addEventListener("pointerup", onUp);
      window.addEventListener("pointercancel", onUp);
      teardownRef.current = detach;
    },
    [splitRef],
  );

  return { drawerWidth, startResize };
}
