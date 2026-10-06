// Shared viewer primitives: zoom bounds, find-highlight colors, DPR bounding, rotation math, the
// text-layer highlighter, and the thumbnail width.
import { findLiteralMatches } from "./searchUtils.js";

export const ZOOM_MIN = 0.25;
export const ZOOM_MAX = 5;

// Translucent so the canvas-rendered glyph shows through the (invisible) text layer.
export const MARK_BG = "rgba(255,200,0,.40)";
export const MARK_ACTIVE_BG = "rgba(255,140,0,.65)";

export const THUMB_W = 130;
export const MAX_PDF_CANVAS_PIXELS = 16_000_000;

export function boundedCanvasDpr(
  viewport: { width: number; height: number },
  desiredDpr: number,
): number {
  const dpr = Number.isFinite(desiredDpr) && desiredDpr > 0 ? desiredDpr : 1;
  const cssPixels = Math.max(1, viewport.width * viewport.height);
  const maxDpr = Math.sqrt(MAX_PDF_CANVAS_PIXELS / cssPixels);
  return Math.min(Math.max(0.1, dpr), maxDpr);
}

export function viewportRotation(pageRotation: number | undefined, userRotation: number): number {
  const page = Number.isFinite(pageRotation) ? (pageRotation ?? 0) : 0;
  const user = Number.isFinite(userRotation) ? userRotation : 0;
  return (((page + user) % 360) + 360) % 360;
}

export function clampScale(scale: number): number {
  return Math.max(ZOOM_MIN, Math.min(ZOOM_MAX, scale));
}

/** Race `promise` against a timeout so a stuck pdf.js call rejects instead of hanging. */
export async function withTimeout<T>(promise: Promise<T>, ms: number, message: string): Promise<T> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  const timeout = new Promise<never>((_, reject) => {
    timer = setTimeout(() => reject(new Error(message)), ms);
  });
  try {
    return await Promise.race([promise, timeout]);
  } finally {
    if (timer !== undefined) clearTimeout(timer);
  }
}

/** Wrap every occurrence of `q` inside a rendered text-layer span with <mark>. Reads the span's
 *  clean textContent each call, so re-running with a new query/options is idempotent; a span with
 *  no match is flattened back to plain text. */
export function highlightSpan(
  span: HTMLElement,
  q: string,
  options?: { matchCase?: boolean; wholeWord?: boolean },
): void {
  const text = span.textContent ?? "";
  const matches = findLiteralMatches(text, q, options);
  if (matches.length === 0) {
    if (span.childElementCount > 0) span.replaceChildren(document.createTextNode(text));
    return;
  }
  const frag = document.createDocumentFragment();
  let pos = 0;
  for (const m of matches) {
    if (m.start > pos) frag.appendChild(document.createTextNode(text.slice(pos, m.start)));
    const mark = document.createElement("mark");
    mark.style.cssText = `background:${MARK_BG};color:inherit;border-radius:2px`;
    mark.textContent = text.slice(m.start, m.end);
    frag.appendChild(mark);
    pos = m.end;
  }
  if (pos < text.length) frag.appendChild(document.createTextNode(text.slice(pos)));
  span.replaceChildren(frag);
}
