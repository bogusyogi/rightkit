import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { renderToStaticMarkup } from "react-dom/server";
import type { WindowControlAdapter } from "@rightkit/platform-ui";
import { Titlebar, blurThen } from "./layout.js";

const adapter: WindowControlAdapter = { minimize: async () => {}, toggleMaximize: async () => {}, close: async () => {} };
const shellCss = readFileSync(new URL("../../shell.css", import.meta.url), "utf8");

/** The declaration blocks of every rule whose selector is exactly `selector` (structure and visual halves), joined. */
const ruleOf = (selector: string) => {
  const blocks: string[] = [];
  for (let start = shellCss.indexOf(`\n${selector} {`); start !== -1; start = shellCss.indexOf(`\n${selector} {`, start + 1)) {
    blocks.push(shellCss.slice(start, shellCss.indexOf("}", start)));
  }
  if (!blocks.length) throw new Error(`no rule for ${selector}`);
  return blocks.join("\n");
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
    const root = ruleOf(":root");
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

describe("Titlebar caption and root options (defaults unchanged)", () => {
  const bar = (extra: object = {}) => renderToStaticMarkup(<Titlebar platform="windows" windowControls={adapter} title="Doc" {...extra} />);

  it("default markup: header root, title equal to the label, bare button classes", () => {
    const html = bar();
    expect(html.startsWith('<header class="rk-top" data-tauri-drag-region="true">')).toBe(true);
    expect(html).toContain('<button type="button" class="rk-caption__btn" title="Minimize window" aria-label="Minimize window">');
    expect(html).toContain('class="rk-caption__btn rk-caption__close" title="Close window" aria-label="Close window"');
  });
  it("captionButtonClasses adds a class per button after the built-in ones", () => {
    const html = bar({ captionButtonClasses: { minimize: "m", maximize: "x", close: "c" } });
    expect(html).toContain('class="rk-caption__btn m"');
    expect(html).toContain('class="rk-caption__btn x"');
    expect(html).toContain('class="rk-caption__btn rk-caption__close c"');
  });
  it("captionTitles replaces, omits per button, or omits all; aria-label keeps the label", () => {
    const html = bar({ captionTitles: { minimize: "Min", close: false } });
    expect(html).toContain('title="Min" aria-label="Minimize window"');
    expect(html).toContain('title="Maximize or restore window" aria-label="Maximize or restore window"');
    expect(html).toMatch(/class="rk-caption__btn rk-caption__close" aria-label="Close window"/);
    const none = bar({ captionTitles: false });
    expect(none).not.toContain("title=");
    expect(none).toContain('aria-label="Minimize window"');
  });
  it("as changes the root element and keeps its classes and drag attribute", () => {
    const html = bar({ as: "div", className: "host" });
    expect(html.startsWith('<div class="rk-top host" data-tauri-drag-region="true">')).toBe(true);
    expect(html.endsWith("</div>")).toBe(true);
    expect(html).not.toContain("<header");
  });
});

describe("blurThen", () => {
  const click = (blur?: boolean) => {
    const calls: string[] = [];
    const event = { currentTarget: { blur: () => calls.push("blur") } } as unknown as Parameters<ReturnType<typeof blurThen>>[0];
    const handler = blur === undefined ? blurThen(async () => { calls.push("run"); }) : blurThen(async () => { calls.push("run"); }, blur);
    handler(event);
    return calls;
  };
  it("blurs then runs by default", () => expect(click()).toEqual(["blur", "run"]));
  it("keeps focus when blur is false", () => expect(click(false)).toEqual(["run"]));
});

describe("caption cursor and outline tokens", () => {
  it("fall back to today's values", () => {
    expect(ruleOf(".rk-caption__btn")).toContain("cursor: var(--rk-caption-cursor, default)");
    expect(ruleOf(".rk-caption__btn:focus-visible")).toContain("outline: var(--rk-caption-outline, 2px solid var(--rk-focus))");
    expect(ruleOf(".rk-caption__btn:focus-visible")).toContain("outline-offset: var(--rk-caption-outline-offset, -2px)");
  });
});
