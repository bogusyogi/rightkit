// Shared PDF viewer primitives: @rightkit/pdf-ui's zoom/DPR/rotation/timeout helpers plus
// the host app brand find-highlight colors, text-layer highlighter, and thumbnail width.
import { highlightSpan as highlightSpanWithMarks } from "@rightkit/pdf-ui";

export {
  boundedCanvasDpr,
  clampScale,
  viewportRotation,
  withTimeout,
  ZOOM_MAX,
  ZOOM_MIN,
  type OutlineNode,
  type Size,
} from "@rightkit/pdf-ui";

// Both highlight backgrounds are translucent so the canvas-rendered glyph shows through — the
// text layer is only an invisible selection overlay, so we never repaint its (transparent) text.
export const MARK_BG = "rgba(196,106,61,.40)"; // copper @ 40% — non-active matches
export const MARK_ACTIVE_BG = "rgba(240,160,96,.65)"; // brighter copper @ 65% — active match

export const THUMB_W = 130; // CSS px width of a thumbnail canvas

export interface Match {
  page: number;
  occ: number; // 0-based occurrence index within that page (text order)
}

/** Wrap every occurrence of `q` (honoring match-case / whole-word) inside a rendered text-layer
 *  span with <mark>, using the package's span highlighter with the host app non-active copper
 *  background. The active match is not drawn here: usePdfSearch emphasises it by page-wide
 *  occurrence index. */
export function highlightSpan(
  span: HTMLElement,
  q: string,
  options?: { matchCase?: boolean; wholeWord?: boolean },
) {
  highlightSpanWithMarks(span, q, {
    matchCase: options?.matchCase,
    wholeWord: options?.wholeWord,
    markBg: MARK_BG,
  });
}
