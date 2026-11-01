const PRINTABLE_PAGE_CONTENT = ".layout-page-content, .layout-page-header, .layout-page-footer";
const PRINTABLE_MARKUP = "img, svg, canvas, table, hr";

function hasPrintableContent(page: Element): boolean {
  return Array.from(page.querySelectorAll(PRINTABLE_PAGE_CONTENT)).some(
    (area) => area.textContent?.trim() || area.querySelector(PRINTABLE_MARKUP),
  );
}

/** Serialize Eigenpal's visual pages for native printing.
 *
 * Eigenpal can retain an empty final `.layout-page` after its layout pass. It is invisible in the
 * editor but becomes a second blank sheet when its DOM is printed. Keep all real pages, including
 * a deliberate blank page in the middle, while removing only empty trailing renderer artifacts. */
export function docxPrintMarkup(source: Element): string {
  const clone = source.cloneNode(true) as HTMLElement;
  clone.style.cssText = "display:block;margin:0;padding:0;";
  const pages = Array.from(clone.querySelectorAll<HTMLElement>(".layout-page"));

  while (pages.length > 1 && !hasPrintableContent(pages[pages.length - 1])) pages.pop()?.remove();

  for (const [index, page] of pages.entries()) {
    page.style.boxShadow = "none";
    page.style.margin = "0";
    page.style.breakInside = "avoid";
    if (index < pages.length - 1) {
      page.style.breakAfter = "page";
      page.style.pageBreakAfter = "always";
    }
  }
  return clone.outerHTML;
}
