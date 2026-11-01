export interface SplitBounds {
  left: number;
  right: number;
  width: number;
}

export function sourceWidthPercent(bounds: SplitBounds, pointerX: number): number {
  const raw = bounds.right - pointerX;
  const clamped = Math.max(300, Math.min(bounds.width - 320, raw));
  return Math.round((clamped / bounds.width) * 100);
}

export function rightDrawerWidth(bounds: SplitBounds, pointerX: number): number {
  return Math.max(280, Math.min(bounds.width - 360, bounds.right - pointerX));
}
