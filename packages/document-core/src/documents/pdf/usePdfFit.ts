import {
  useCallback,
  useEffect,
  type Dispatch,
  type MutableRefObject,
  type SetStateAction,
} from "react";
import { clampScale, type Size } from "./pdfShared";

interface PdfFitDeps {
  scrollRef: MutableRefObject<HTMLDivElement | null>;
  /** Base page-1 viewport at scale 1 (null until the document loads). */
  base: Size | null;
  /** 0 | 90 | 180 | 270, applied to every page. */
  rotation: number;
  fitMode: "width" | "page" | null;
  setScale: Dispatch<SetStateAction<number>>;
}

/**
 * Fit-to-width/page scale math: derive `scale` from the container's client box (rotation-aware),
 * apply it when the base page size or fit mode changes, and re-apply on container resize —
 * rAF-coalesced because ResizeObserver can fire many times per frame during a window drag.
 * Extracted from PdfViewer verbatim — no behavior change.
 */
export function usePdfFit({ scrollRef, base, rotation, fitMode, setScale }: PdfFitDeps) {
  // --- Fit-to-width: compute scale from the container width ----------------------
  // biome-ignore lint/correctness/useExhaustiveDependencies: refs/setters arrive as stable hook params
  const applyFit = useCallback(
    (mode: "width" | "page") => {
      const el = scrollRef.current;
      if (!el || !base) return;
      // On 90°/270° rotation the page's effective footprint has width/height swapped.
      const swap = rotation % 180 !== 0;
      const bw = swap ? base.h : base.w;
      const bh = swap ? base.w : base.h;
      const availW = el.clientWidth - 48; // page horizontal margin
      const availH = el.clientHeight - 48;
      const next = mode === "width" ? availW / bw : Math.min(availW / bw, availH / bh);
      setScale(clampScale(next));
    },
    [base, rotation],
  );

  useEffect(() => {
    if (base && fitMode) applyFit(fitMode);
  }, [base, fitMode, applyFit]);

  // Recompute fit on container resize. rAF-coalesced: ResizeObserver can fire many times per frame
  // during an interactive window drag, and each call previously re-rasterized every visible page —
  // collapse to at most one applyFit per animation frame.
  // biome-ignore lint/correctness/useExhaustiveDependencies: refs arrive as stable hook params
  useEffect(() => {
    if (!fitMode) return;
    const el = scrollRef.current;
    if (!el) return;
    let raf: number | null = null;
    const ro = new ResizeObserver(() => {
      if (raf != null) return;
      raf = requestAnimationFrame(() => {
        raf = null;
        applyFit(fitMode);
      });
    });
    ro.observe(el);
    return () => {
      ro.disconnect();
      if (raf != null) cancelAnimationFrame(raf);
    };
  }, [fitMode, applyFit]);
}
