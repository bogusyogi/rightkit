import { useEffect, useRef } from "react";

export interface PillPopupSize { width: number; height: number }
export interface PillPopupGeometryOptions {
  /** Measure host content including any clipping/shadow guard space it requires. */
  measure?: (element: HTMLDivElement) => PillPopupSize;
  /** Ignore smaller CSS-pixel changes; default reports every integer size change. */
  resizeTolerance?: number;
}

/** Size hidden native popups before acknowledging readiness; cancel stale presentations. */
export function usePillPopupGeometry(
  element: HTMLDivElement | null,
  active: boolean,
  presentation: unknown,
  resize: (width: number, height: number) => Promise<void>,
  ready: () => Promise<void>,
  options: PillPopupGeometryOptions = {},
) {
  const callbacks = useRef({ resize, ready, measure: options.measure });
  callbacks.current = { resize, ready, measure: options.measure };
  const tolerance = Math.max(0, options.resizeTolerance ?? 0);
  useEffect(() => {
    if (!active || !element) return;
    let disposed = false;
    let raf = 0;
    let sequence = 0;
    let last = { width: 0, height: 0 };
    let pendingResize: Promise<void> = Promise.resolve();
    const push = () => {
      raf = 0;
      const measured = callbacks.current.measure?.(element) ?? element.getBoundingClientRect();
      const width = Math.ceil(measured.width);
      const height = Math.ceil(measured.height);
      if (!Number.isFinite(width) || !Number.isFinite(height) || width < 1 || height < 1) return;
      const current = ++sequence;
      const resized = Math.abs(width - last.width) > tolerance || Math.abs(height - last.height) > tolerance;
      if (resized) {
        pendingResize = pendingResize.catch(() => undefined).then(() => {
          if (!disposed) return callbacks.current.resize(width, height);
        });
      }
      last = { width, height };
      void pendingResize.catch(() => undefined).finally(() => {
        if (!disposed && current === sequence) void callbacks.current.ready().catch(() => undefined);
      });
    };
    const observer = typeof ResizeObserver === "undefined" ? null : new ResizeObserver(() => {
      if (!raf) raf = requestAnimationFrame(push);
    });
    observer?.observe(element);
    push();
    return () => {
      disposed = true;
      observer?.disconnect();
      if (raf) cancelAnimationFrame(raf);
    };
  }, [element, active, presentation, tolerance, options.measure]);
}
