import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { renderToStaticMarkup } from "react-dom/server";
import type { WindowControlAdapter } from "@rightkit/platform-ui";
import { Titlebar } from "./layout.js";

const adapter: WindowControlAdapter = { minimize: async () => {}, toggleMaximize: async () => {}, close: async () => {} };
const shellCss = readFileSync(new URL("../../shell.css", import.meta.url), "utf8");

/** The declaration block of the first rule whose selector is exactly `selector`. */
const ruleOf = (selector: string) => {
  const start = shellCss.indexOf(`\n${selector} {`);
  if (start === -1) throw new Error(`no rule for ${selector}`);
  return shellCss.slice(start, shellCss.indexOf("}", start));
};

describe("Titlebar token defaults", () => {
  it("keeps today's values as fallbacks in the rules", () => {
    expect(ruleOf(".rk-top")).toContain("background: var(--rk-top-bg, var(--rk-chrome))");
    expect(ruleOf(".rk-top")).toContain("border-bottom: var(--rk-top-border, none)");
    expect(ruleOf(".rk-top__end")).toContain("gap: var(--rk-top-end-gap, var(--rk-titlebar-gap))");
    expect(ruleOf(".rk-caption")).toContain("margin-right: var(--rk-caption-margin-end, calc(-1 * var(--rk-titlebar-pad-end)))");
    expect(ruleOf(".rk-caption")).toContain("margin-left: var(--rk-caption-margin-start, 6px)");
    expect(ruleOf(".rk-caption__btn:hover")).toContain("background: var(--rk-caption-hover-bg, var(--rk-fill-2))");
  });
  it("does not define the derived tokens on :root, which would freeze them to the light values", () => {
    const root = shellCss.slice(shellCss.indexOf(":root {"), shellCss.indexOf("}", shellCss.indexOf(":root {")));
    for (const token of ["--rk-top-bg", "--rk-caption-hover-bg", "--rk-caption-margin-start", "--rk-caption-margin-end", "--rk-top-end-gap"]) {
      expect(root).not.toContain(token);
    }
  });
});

describe("Titlebar captionGlyphs", () => {
  const caps = (extra: object) => renderToStaticMarkup(<Titlebar platform="windows" windowControls={adapter} title="Doc" {...extra} />);

  it("replaces every built-in glyph when all four are given", () => {
    const html = caps({ captionGlyphs: { minimize: <i>m</i>, maximize: <i>x</i>, restore: <i>r</i>, close: <i>c</i> } });
    expect(html).toContain("<i>m</i>");
    expect(html).toContain("<i>x</i>");
    expect(html).toContain("<i>c</i>");
    expect(html).not.toContain("<rect");
    expect(html).not.toContain("<path");
  });
  it("replaces only the glyphs given and keeps the rest built-in", () => {
    const html = caps({ captionGlyphs: { close: <i>c</i> } });
    expect(html).toContain("<i>c</i>");
    expect(html).toContain('<rect x="0" y="4.5"');
    expect(html).toContain('<rect x="0.5" y="0.5"');
  });
  it("shows the restore glyph only while maximized, and falls back to the maximize glyph then the square", () => {
    const glyphs = { maximize: <i>max</i>, restore: <i>rest</i> };
    expect(caps({ captionGlyphs: glyphs })).toContain("<i>max</i>");
    expect(caps({ captionGlyphs: glyphs, maximized: true })).toContain("<i>rest</i>");
    expect(caps({ captionGlyphs: { maximize: <i>max</i> }, maximized: true })).toContain("<i>max</i>");
    expect(caps({ maximized: true })).toContain('<rect x="0.5" y="0.5"');
  });
});

describe("Titlebar end cluster", () => {
  it("wraps the end content in .rk-top__end with the slot class and the drag attribute", () => {
    const html = renderToStaticMarkup(<Titlebar platform="linux" end={<span>E</span>} slots={{ end: "tb-end" }} />);
    expect(html).toMatch(/<div class="rk-top__end tb-end" data-tauri-drag-region="true"><span>E<\/span><\/div>/);
  });
  it("adds no wrapper when there is no end content", () => {
    expect(renderToStaticMarkup(<Titlebar platform="linux" title="x" />)).not.toContain("rk-top__end");
  });
});
