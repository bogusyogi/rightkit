interface SourcePosition {
  start: { offset?: number };
  end: { offset?: number };
}

export function sourceSliceForPosition(
  source: string,
  position: SourcePosition | undefined,
): string | null {
  const start = position?.start.offset;
  const end = position?.end.offset;
  if (typeof start !== "number" || typeof end !== "number") return null;
  if (!Number.isInteger(start) || !Number.isInteger(end)) return null;
  if (start < 0 || end < start || end > source.length) return null;
  return source.slice(start, end);
}
