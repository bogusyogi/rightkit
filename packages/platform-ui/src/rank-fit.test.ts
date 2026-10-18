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
  it("opt-in sub-label and alias tiers; default ignores sub/aliases and keeps input-order ties", () => {
    const extended = [
      { label: "Zeta Save" },
      { label: "Alpha", sub: "save file", aliases: ["persist"] },
      { label: "Beta", aliases: ["save"] },
    ];
    // Default: only label matching; sub/alias-only items do not match via those fields.
    expect(rankItems(extended, "persist").map((i) => i.label)).toEqual([]);
    expect(rankItems(extended, "save").map((i) => i.label)).toEqual(["Zeta Save"]);
    // Opt-in: alias exact (Beta) above word-start label (Zeta Save), sub prefix (Alpha) below them.
    expect(rankItems(extended, "save", 50, { subLabel: true, aliasTiers: true }).map((i) => i.label)).toEqual(["Beta", "Zeta Save", "Alpha"]);
    expect(rankItems(extended, "persist", 50, { aliasTiers: true }).map((i) => i.label)).toEqual(["Alpha"]);
    expect(rankItems(extended, "per", 50, { aliasTiers: true }).map((i) => i.label)).toEqual(["Alpha"]);
  });
  it("opt-in label tie-break reorders equal scores; default remains input order", () => {
    const ties = [{ label: "Zulu" }, { label: "Alpha" }];
    // Both contain "l" as substring at different positions, so use equal keyword tier instead.
    const equal = [
      { label: "Zulu", keywords: ["go"] },
      { label: "Alpha", keywords: ["go"] },
    ];
    expect(rankItems(equal, "go").map((i) => i.label)).toEqual(["Zulu", "Alpha"]);
    expect(rankItems(equal, "go", 50, { tieBreak: "label" }).map((i) => i.label)).toEqual(["Alpha", "Zulu"]);
    expect(rankItems(ties, "").map((i) => i.label)).toEqual(["Zulu", "Alpha"]);
  });
});
