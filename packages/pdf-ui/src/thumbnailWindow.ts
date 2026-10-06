export interface ThumbnailWindowInput {
  numPages: number;
  scrollTop: number;
  viewportHeight: number;
  itemHeight: number;
  overscan: number;
}

export interface ThumbnailWindow {
  start: number;
  end: number;
  before: number;
  after: number;
}

/** Computes the inclusive page range that needs a mounted thumbnail button. */
export function thumbnailWindow({
  numPages,
  scrollTop,
  viewportHeight,
  itemHeight,
  overscan,
}: ThumbnailWindowInput): ThumbnailWindow {
  if (numPages <= 0 || itemHeight <= 0) return { start: 1, end: 0, before: 0, after: 0 };
  const totalHeight = numPages * itemHeight;
  const effectiveTop = Math.max(0, Math.min(scrollTop, Math.max(0, totalHeight - viewportHeight)));
  const firstVisible = Math.floor(effectiveTop / itemHeight) + 1;
  const visibleCount = Math.max(1, Math.ceil(viewportHeight / itemHeight));
  const start = Math.max(1, firstVisible - overscan);
  const end = Math.min(numPages, firstVisible + visibleCount - 1 + overscan);
  return {
    start,
    end,
    before: (start - 1) * itemHeight,
    after: (numPages - end) * itemHeight,
  };
}
