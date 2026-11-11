/** Pure page-picking helper, kept free of pdf.js so it is testable without a DOM-bound viewer. */
export function nearestVisiblePdfPage(
  root: HTMLElement,
  slots: ReadonlyMap<number, HTMLElement>,
  visiblePages: ReadonlySet<number>,
  fallbackPage: number,
): number {
  const rootRect = root.getBoundingClientRect();
  const centerY = rootRect.top + rootRect.height / 2;
  let bestPage = fallbackPage;
  let bestDistance = Number.POSITIVE_INFINITY;
  const pages = visiblePages.size > 0 ? visiblePages : slots.keys();

  for (const page of pages) {
    const slot = slots.get(page);
    if (!slot) continue;
    const rect = slot.getBoundingClientRect();
    if (rect.bottom < rootRect.top || rect.top > rootRect.bottom) continue;
    const distance = Math.abs(rect.top + rect.height / 2 - centerY);
    if (distance < bestDistance) {
      bestDistance = distance;
      bestPage = page;
    }
  }
  return bestPage;
}
