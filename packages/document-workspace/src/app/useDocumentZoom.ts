import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type WheelEvent as ReactWheelEvent,
} from "react";
import { isMarkdownFormat } from "../documents/capabilities";
import type { LoadedDocument, ViewMode } from "@rightkit/document-core/documents/types";
import type { Settings } from "@rightkit/document-core/settings/settings";

interface DocumentZoomDeps {
  active: LoadedDocument | null;
  view: ViewMode;
  settings: Settings;
  update: (patch: Partial<Settings>) => void;
}

/**
 * Ctrl/Cmd+wheel document zoom + the zoom HUD/reset pill. Refs mirror the latest active/view/
 * settings so `onDocumentWheel` stays IDENTITY-STABLE (needed so DocumentRouterTable's memo isn't
 * defeated by a fresh handler every render); wheel ticks accumulate a step count and a single rAF
 * flush applies at most one settings update per frame. Zoom math/keys/HUD behavior unchanged —
 * extracted verbatim from App.tsx.
 */
export function useDocumentZoom({ active, view, settings, update }: DocumentZoomDeps) {
  const [zoomHud, setZoomHud] = useState<string | null>(null);
  const zoomHudTimer = useRef<number | null>(null);
  // T2-2: Ctrl+wheel zoom used to call settings' update() on every wheel tick — a fast trackpad/mouse
  // burst fires many ticks per animation frame, each one a full settings-context re-render. Refs mirror
  // the latest active/view/settings so this handler can stay IDENTITY-STABLE (useCallback with no
  // per-render deps besides `update`/`flushZoom`) — needed so DocumentRouterTable's memo (T2-3) isn't
  // defeated by a fresh onDocumentWheel every render. Wheel events accumulate a step count; a single
  // rAF flush applies at most one settings update per frame. Zoom math/keys/HUD behavior unchanged.
  const activeRef = useRef(active);
  activeRef.current = active;
  const viewRef = useRef(view);
  viewRef.current = view;
  const settingsRef = useRef(settings);
  settingsRef.current = settings;
  const zoomStepsRef = useRef(0);
  const zoomCtxRef = useRef<{
    key: "fontSizePx" | "sourceFontSizePx";
    base: number;
    min: number;
    max: number;
  } | null>(null);
  const zoomRafRef = useRef<number | null>(null);

  useEffect(
    () => () => {
      if (zoomHudTimer.current != null) window.clearTimeout(zoomHudTimer.current);
      if (zoomRafRef.current != null) window.cancelAnimationFrame(zoomRafRef.current);
    },
    [],
  );

  const flushZoom = useCallback(() => {
    zoomRafRef.current = null;
    const ctx = zoomCtxRef.current;
    const steps = zoomStepsRef.current;
    zoomStepsRef.current = 0;
    zoomCtxRef.current = null;
    if (!ctx || steps === 0) return;
    const { key, base, min, max } = ctx;
    const current = settingsRef.current[key];
    const next = Math.max(min, Math.min(max, current + steps));
    if (next === current) return;
    update({ [key]: next });
    setZoomHud(`${Math.round((next / base) * 100)}%`);
    if (zoomHudTimer.current != null) window.clearTimeout(zoomHudTimer.current);
    zoomHudTimer.current = window.setTimeout(() => setZoomHud(null), 900);
  }, [update]);

  // Which zoom setting the visible surface is actually using, mirroring the source-vs-reading split
  // in onDocumentWheel below. The pill must report and reset the SAME setting the wheel changes, or
  // it would offer to reset a value the user never touched.
  const zoomSurface = useMemo(() => {
    const format = active?.format;
    // Only the formats whose ON-SCREEN size this setting actually governs. PDF owns its own zoom
    // toolbar; DOCX and images are not font-scaled; and HTML previews zoom via their own iframe
    // `style.zoom` (HtmlViewer's safe-preview handler and the interactive bridge's pageZoom), which
    // this setting does not touch. Including any of them would render a pill that reports a number
    // the user is not looking at and a Reset that visibly does nothing.
    const zoomable = format === "txt" || isMarkdownFormat(format);
    const sourceZoomable = isMarkdownFormat(format) && view === "source";
    return {
      zoomable,
      ...(sourceZoomable
        ? { key: "sourceFontSizePx" as const, base: 12 }
        : { key: "fontSizePx" as const, base: 16 }),
    };
  }, [active?.format, view]);
  const zoomPercent = Math.round((settings[zoomSurface.key] / zoomSurface.base) * 100);
  const zoomedAwayFromDefault =
    zoomSurface.zoomable && settings[zoomSurface.key] !== zoomSurface.base;
  const zoomPercentLabel = `${zoomPercent}%`;
  const resetDocumentZoom = useCallback(
    () => update({ [zoomSurface.key]: zoomSurface.base }),
    [update, zoomSurface],
  );

  const onDocumentWheel = useCallback(
    (e: ReactWheelEvent<HTMLDivElement>) => {
      const activeDoc = activeRef.current;
      if (!(e.ctrlKey || e.metaKey) || !activeDoc) return;
      const currentView = viewRef.current;
      const target = e.target as HTMLElement | null;
      const sourceZoomable =
        isMarkdownFormat(activeDoc.format) && currentView === "source"
          ? true
          : activeDoc.format === "html" && !!target?.closest?.(".cm-editor");
      const readingZoomable =
        activeDoc.format === "txt" ||
        (isMarkdownFormat(activeDoc.format) && currentView !== "source");
      if (!sourceZoomable && !readingZoomable) return;
      e.preventDefault();
      e.stopPropagation();
      const key: "fontSizePx" | "sourceFontSizePx" = sourceZoomable
        ? "sourceFontSizePx"
        : "fontSizePx";
      const base = sourceZoomable ? 12 : 16;
      const min = sourceZoomable ? 10 : 12;
      const max = sourceZoomable ? 24 : 28;
      zoomCtxRef.current = { key, base, min, max };
      zoomStepsRef.current += e.deltaY < 0 ? 1 : -1;
      if (zoomRafRef.current == null) {
        zoomRafRef.current = window.requestAnimationFrame(flushZoom);
      }
    },
    [flushZoom],
  );

  return { zoomHud, zoomedAwayFromDefault, zoomPercentLabel, resetDocumentZoom, onDocumentWheel };
}
