import {
  useCallback,
  useEffect,
  type Dispatch,
  type MutableRefObject,
  type SetStateAction,
  type WheelEvent as ReactWheelEvent,
} from "react";
import type { PDFDocumentProxy } from "pdfjs-dist";
import { createShortcutRegistry } from "@rightkit/react-shell";
import { clampScale, ZOOM_MAX, ZOOM_MIN } from "./pdfShared.js";

interface UsePdfViewControlsArgs {
  active: boolean;
  numPages: number;
  pageDraft: string | null;
  scale: number;
  scaleDraft: string;
  scrollRef: MutableRefObject<HTMLDivElement | null>;
  pdfRef: MutableRefObject<PDFDocumentProxy | null>;
  goToPage: (page: number) => void;
  setPageDraft: Dispatch<SetStateAction<string | null>>;
  setPageShake: Dispatch<SetStateAction<boolean>>;
  setScale: Dispatch<SetStateAction<number>>;
  setScaleDraft: Dispatch<SetStateAction<string>>;
  setFitMode: Dispatch<SetStateAction<"width" | "page" | null>>;
  setRotation: Dispatch<SetStateAction<number>>;
}

export function isPdfTypingTarget(target: EventTarget | null): boolean {
  if (!(target instanceof Element)) return false;
  return (
    target.tagName === "INPUT" ||
    target.tagName === "TEXTAREA" ||
    (target instanceof HTMLElement && target.isContentEditable) ||
    target.closest("[contenteditable='true']") !== null
  );
}

type PdfViewShortcutHandlers = {
  /** True while Home/End should page-jump (the scroll pane itself has focus). */
  homeEnd: () => boolean;
  goHome: () => void;
  goEnd: () => void;
  zoomBy: (delta: number) => void;
  fitWidth: () => void;
};

const LEGACY_KEY_MATCH = {
  ignoreExtraModifiers: true,
  allowDefaultPrevented: true,
  allowComposing: true,
} as const;

/**
 * The PDF pane's view shortcuts as a registry: Home/End to the first/last page (scroll pane
 * focused), Ctrl/Cmd `=`/`+` zoom in, `-` zoom out, `0` fit to width. A key that lands in a text
 * field or editable region is left alone (`run` answers `false`).
 *
 * The raw handler these replace accepted every modifier combination around its named key (Shift,
 * Alt, both Ctrl and Meta), ignored whether the event was already prevented, and did not look at
 * IME composition. The registry options keep that: `ignoreExtraModifiers` (a named modifier must be
 * held, any other may be), `ctrlormeta` (Ctrl or Meta, either or both), `allowDefaultPrevented`
 * and `allowComposing`.
 */
export function createPdfViewShortcuts(handlers: PdfViewShortcutHandlers) {
  const registry = createShortcutRegistry();
  const guarded =
    (run: () => void) =>
    (event: KeyboardEvent): void | boolean => {
      if (isPdfTypingTarget(event.target)) return false;
      run();
    };
  registry.register({
    id: "pdf.first-page",
    label: "First page",
    chord: "home",
    hidden: true,
    ...LEGACY_KEY_MATCH,
    when: handlers.homeEnd,
    run: guarded(handlers.goHome),
  });
  registry.register({
    id: "pdf.last-page",
    label: "Last page",
    chord: "end",
    hidden: true,
    ...LEGACY_KEY_MATCH,
    when: handlers.homeEnd,
    run: guarded(handlers.goEnd),
  });
  registry.register({
    id: "pdf.zoom-in",
    label: "Zoom in",
    chord: ["ctrlormeta+=", "ctrlormeta+plus"],
    hidden: true,
    ...LEGACY_KEY_MATCH,
    run: guarded(() => handlers.zoomBy(0.15)),
  });
  registry.register({
    id: "pdf.zoom-out",
    label: "Zoom out",
    chord: "ctrlormeta+-",
    hidden: true,
    ...LEGACY_KEY_MATCH,
    run: guarded(() => handlers.zoomBy(-0.15)),
  });
  registry.register({
    id: "pdf.fit-width",
    label: "Fit to width",
    chord: "ctrlormeta+0",
    hidden: true,
    ...LEGACY_KEY_MATCH,
    run: guarded(handlers.fitWidth),
  });
  return registry;
}

export function usePdfViewControls({
  active,
  numPages,
  pageDraft,
  scale,
  scaleDraft,
  scrollRef,
  pdfRef,
  goToPage,
  setPageDraft,
  setPageShake,
  setScale,
  setScaleDraft,
  setFitMode,
  setRotation,
}: UsePdfViewControlsArgs) {
  const goToDest = useCallback(
    async (dest: string | unknown[] | null) => {
      const pdf = pdfRef.current;
      if (!pdf || !dest) return;
      try {
        const explicit = typeof dest === "string" ? await pdf.getDestination(dest) : dest;
        const ref = Array.isArray(explicit) ? explicit[0] : null;
        if (!ref) return;
        const pageIndex = await pdf.getPageIndex(
          ref as Parameters<PDFDocumentProxy["getPageIndex"]>[0],
        );
        goToPage(pageIndex + 1);
      } catch {
        /* malformed destination - ignore */
      }
    },
    [goToPage, pdfRef],
  );

  const commitPageDraft = useCallback(() => {
    if (pageDraft == null) return;
    const n = Number(pageDraft);
    if (Number.isFinite(n) && n >= 1 && (numPages === 0 || n <= numPages)) {
      goToPage(n);
    } else {
      setPageShake(true);
      window.setTimeout(() => setPageShake(false), 360);
    }
    setPageDraft(null);
  }, [goToPage, numPages, pageDraft, setPageDraft, setPageShake]);

  const zoom = useCallback(
    (delta: number) => {
      setFitMode(null);
      setScale((s) => clampScale(+(s + delta).toFixed(2)));
    },
    [setFitMode, setScale],
  );

  useEffect(() => {
    setScaleDraft(String(Math.round(scale * 100)));
  }, [scale, setScaleDraft]);

  const commitScaleDraft = useCallback(() => {
    const parsed = Number.parseInt(scaleDraft, 10);
    if (Number.isNaN(parsed)) {
      setScaleDraft(String(Math.round(scale * 100)));
      return;
    }
    const clamped = Math.max(
      Math.round(ZOOM_MIN * 100),
      Math.min(Math.round(ZOOM_MAX * 100), parsed),
    );
    setFitMode(null);
    setScale(clamped / 100);
    setScaleDraft(String(clamped));
  }, [scale, scaleDraft, setFitMode, setScale, setScaleDraft]);

  const onWheelZoom = useCallback(
    (event: ReactWheelEvent<HTMLDivElement>) => {
      if (!(event.ctrlKey || event.metaKey)) return;
      event.preventDefault();
      event.stopPropagation();
      zoom(event.deltaY < 0 ? 0.1 : -0.1);
    },
    [zoom],
  );

  const rotate = useCallback(
    (delta: number) => setRotation((r) => (((r + delta) % 360) + 360) % 360),
    [setRotation],
  );

  useEffect(() => {
    if (!active) return;
    const el = scrollRef.current;
    const registry = createPdfViewShortcuts({
      homeEnd: () => Boolean(el) && document.activeElement === el,
      goHome: () => goToPage(1),
      goEnd: () => goToPage(numPages),
      zoomBy: (delta) => {
        setFitMode(null);
        setScale((s) => clampScale(+(s + delta).toFixed(2)));
      },
      fitWidth: () => setFitMode("width"),
    });
    return registry.attach(window);
  }, [active, goToPage, numPages, scrollRef, setFitMode, setScale]);

  return { goToDest, commitPageDraft, zoom, commitScaleDraft, onWheelZoom, rotate };
}
