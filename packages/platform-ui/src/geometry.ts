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
