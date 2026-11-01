import type { Viewport } from "@rightkit/document-core/documents/previewViewport";

// Compare two HTML strings ignoring inline <style> contents: a CSS-only edit yields equal results,
// so the live preview can hot-apply that CSS instead of doing a (script-restarting) full rebuild.
export function structuralPart(html: string): string {
  return html.replace(/<style\b[^>]*>[\s\S]*?<\/style>/gi, "<style></style>");
}

// How long typing must pause before the preview rebuilds. A full structural rebuild reparses the
// document and re-inlines scripts, so this cannot fire per keystroke — but 250ms sat far enough past
// the ~100ms threshold where a change still feels caused by your typing that it read as lag. CSS-only
// edits never reach here; the bridge hot-applies those with no rebuild at all.
export const LIVE_PREVIEW_DEBOUNCE_MS = 120;

export function initialSourceDrawerOpen(): boolean {
  if (typeof window === "undefined") return false;
  // A dev/QA entry point only; the guard folds away in production builds.
  if (!import.meta.env?.DEV && import.meta.env?.VITE_RIGHTKIT_BROWSER_QA !== "1") return false;
  return new URLSearchParams(window.location.search).get("source") === "1";
}

export type ViewportMode = "auto" | Viewport;
