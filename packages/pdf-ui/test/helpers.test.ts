import { describe, expect, it } from "vitest";
import { createPdfLinkService, isSafeExternalUrl } from "../src/linkService.js";
import { boundedCanvasDpr, clampScale, findLiteralMatches, viewportRotation } from "../src/index.js";
import { thumbnailWindow } from "../src/thumbnailWindow.js";

describe("pdf-ui safety and view helpers", () => {
  it("allows only safe external link schemes", () => {
    expect(isSafeExternalUrl("https://example.com")).toBe(true);
    expect(isSafeExternalUrl("mailto:test@example.com")).toBe(true);
    expect(isSafeExternalUrl("javascript:alert(1)")).toBe(false);
    const opened: string[] = [];
    const service = createPdfLinkService({ goToDest() {}, goToPage() {}, currentPage: () => 1, pagesCount: () => 1, openExternal: (url) => opened.push(url) });
    const anchor = { href: "", title: "", rel: "", addEventListener: (type: string, handler: (event: Event) => void) => { if (type === "click") handler({ preventDefault() {} } as Event); } } as unknown as HTMLAnchorElement;
    service.addLinkAttributes(anchor, "javascript:alert(1)");
    expect(opened).toEqual([]);
    service.addLinkAttributes(anchor, "https://example.com");
    expect(opened).toEqual(["https://example.com"]);
  });

  it("keeps zoom, rotation, DPR and thumbnail windows bounded", () => {
    expect(clampScale(0)).toBe(.25);
    expect(clampScale(20)).toBe(5);
    expect(viewportRotation(270, 180)).toBe(90);
    expect(boundedCanvasDpr({ width: 6000, height: 4000 }, 4)).toBeLessThan(4);
    expect(thumbnailWindow({ numPages: 100, scrollTop: 1000, viewportHeight: 500, itemHeight: 120, overscan: 2 })).toMatchObject({ start: 7, end: 15, before: 720 });
    expect(findLiteralMatches("One one stone", "one", { wholeWord: true })).toHaveLength(2);
  });
});
