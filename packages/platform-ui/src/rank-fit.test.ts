import { describe, expect, it } from "vitest";
import { fitToViewport, floatingPanelStyle } from "./geometry.js";
import { matchScore, rankCommands, rankItems } from "./rank.js";

describe("fitToViewport", () => {
  const viewport = { width: 1000, height: 768 };
  it("matches floatingPanelStyle for the same width and height", () => {
    for (const [x, y] of [[0, 0], [990, 700], [500, 400], [-20, 2000], [999, 1]] as const) {
      const a = fitToViewport({ x, y, estimatedSize: { width: 200, height: 260 }, viewport });
      const b = floatingPanelStyle({ x, y, width: 200, estimatedHeight: 260, viewportWidth: 1000, viewportHeight: 768 });
      expect({ left: a.left, top: a.top, width: a.width, maxWidth: a.maxWidth, maxHeight: a.maxHeight }).toEqual(b);
      expect(a.height).toBe(Math.min(260, b.maxHeight));
    }
  });
  it("measured size wins over the estimate", () => {
    const r = fitToViewport({ x: 900, y: 700, size: { width: 100, height: 50 }, estimatedSize: { width: 300, height: 400 }, viewport });
    expect(r).toMatchObject({ left: 892, top: 700, width: 100, height: 50 });
  });
  it("keeps the margin, shrinks an oversize surface, and defaults the margin to 8", () => {
    const r = fitToViewport({ x: 0, y: 0, size: { width: 5000, height: 5000 }, viewport });
    expect(r).toMatchObject({ left: 8, top: 8, width: 984, height: 752, maxWidth: 984, maxHeight: 752 });
    expect(fitToViewport({ x: 0, y: 0, size: { width: 10, height: 10 }, viewport, margin: 20 })).toMatchObject({ left: 20, top: 20 });
  });
  it("throws without any size", () => {
    expect(() => fitToViewport({ x: 0, y: 0, viewport })).toThrow(TypeError);
  });
});

describe("ranking (moved from app-shell)", () => {
  const items = [
    { label: "Open file", keywords: ["load"] },
    { label: "Save", hint: "disk" },
    { label: "Save As…" },
    { label: "Print" },
  ];
  it("empty query keeps input order", () => {
    expect(rankCommands(items, "").map((i) => i.label)).toEqual(items.map((i) => i.label));
  });
  it("exact, prefix, keyword, hint and subsequence tiers", () => {
    expect(matchScore("save", "Save")).toBe(200);
    expect(matchScore("sa", "Save As…")).toBe(160 - 6);
    expect(matchScore("load", "Open file", ["load"])).toBe(70);
    expect(matchScore("disk", "Save", [], "disk")).toBe(60);
    expect(matchScore("opf", "Open file")).not.toBeNull();
    expect(matchScore("zzz", "Open file")).toBeNull();
  });
  it("sorts by score, ties by input order, and applies the limit", () => {
    expect(rankItems(items, "save").map((i) => i.label)).toEqual(["Save", "Save As…"]);
    expect(rankItems(items, "", 2)).toHaveLength(2);
  });
});
