/** Keep a fixed floating surface (context menu, popover) inside the viewport, shrinking before it overflows. */
export interface FloatingPanelInput {
  x: number;
  y: number;
  width: number;
  estimatedHeight: number;
  viewportWidth: number;
  viewportHeight: number;
  gutter?: number;
}

export interface FloatingPanelStyle {
  left: number;
  top: number;
  width: number;
  maxWidth: number;
  maxHeight: number;
}

export function floatingPanelStyle({ x, y, width, estimatedHeight, viewportWidth, viewportHeight, gutter = 8 }: FloatingPanelInput): FloatingPanelStyle {
  const safe = Math.max(0, Math.min(gutter, Math.floor(viewportWidth / 2), Math.floor(viewportHeight / 2)));
  const maxWidth = Math.max(0, viewportWidth - safe * 2);
  const maxHeight = Math.max(0, viewportHeight - safe * 2);
  const w = Math.min(width, maxWidth);
  const h = Math.min(estimatedHeight, maxHeight);
  const maxLeft = Math.max(safe, viewportWidth - safe - w);
  const maxTop = Math.max(safe, viewportHeight - safe - h);
  return {
    left: Math.max(safe, Math.min(x, maxLeft)),
    top: Math.max(safe, Math.min(y, maxTop)),
    width: w,
    maxWidth,
    maxHeight,
  };
}

export interface FitToViewportInput {
  /** Requested top-left corner (a pointer position). */
  x: number;
  y: number;
  /** Measured size of the surface (e.g. from `getBoundingClientRect`). Wins over `estimatedSize`. */
  size?: { width: number; height: number };
  /** Fixed estimate used before a measurement exists, or by an app that never measures. */
  estimatedSize?: { width: number; height: number };
  viewport: { width: number; height: number };
  /** Space kept free at every viewport edge. Default 8. */
  margin?: number;
}

export interface FitToViewportResult {
  left: number;
  top: number;
  /** The size used, shrunk to fit the viewport minus margins. */
  width: number;
  height: number;
  maxWidth: number;
  maxHeight: number;
}

/**
 * Pure placement: the position that keeps a `size` (measured) or `estimatedSize` (fixed) surface inside the
 * viewport, shrinking it before it overflows. Same clamp as {@link floatingPanelStyle}, which it matches when the
 * size is `{ width, height: estimatedHeight }`.
 */
export function fitToViewport({ x, y, size, estimatedSize, viewport, margin = 8 }: FitToViewportInput): FitToViewportResult {
  const base = size ?? estimatedSize;
  if (!base) throw new TypeError("fitToViewport needs size or estimatedSize");
  const safe = Math.max(0, Math.min(margin, Math.floor(viewport.width / 2), Math.floor(viewport.height / 2)));
  const maxWidth = Math.max(0, viewport.width - safe * 2);
  const maxHeight = Math.max(0, viewport.height - safe * 2);
  const width = Math.min(base.width, maxWidth);
  const height = Math.min(base.height, maxHeight);
  const maxLeft = Math.max(safe, viewport.width - safe - width);
  const maxTop = Math.max(safe, viewport.height - safe - height);
  return {
    left: Math.max(safe, Math.min(x, maxLeft)),
    top: Math.max(safe, Math.min(y, maxTop)),
    width,
    height,
    maxWidth,
    maxHeight,
  };
}
