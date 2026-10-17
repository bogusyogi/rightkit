// The stylesheet split: shell.css (0.5.x behaviour, unlayered) is structure + visual, nothing lost,
// nothing duplicated, and the layered entry wraps the same two files.
import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
// @ts-expect-error plain ESM script without types
import { combined } from "../scripts/build-shell-css.mjs";

const read = (name: string) => readFileSync(new URL(`../${name}`, import.meta.url), "utf8");
const pkg = JSON.parse(read("package.json")) as { version: string; exports: Record<string, unknown>; files: string[] };

/** Every declaration as "context | selector | property: value", comments and whitespace ignored. */
function declarations(css: string, context = ""): string[] {
  const out: string[] = [];
  const text = css.replace(/\/\*[\s\S]*?\*\//g, "");
  let i = 0;
  while (i < text.length) {
    const open = text.indexOf("{", i);
    if (open === -1) break;
    const head = text.slice(i, open).trim();
    let depth = 1;
    let k = open + 1;
    while (k < text.length && depth) {
      if (text[k] === "{") depth++;
      else if (text[k] === "}") depth--;
      k++;
    }
    const body = text.slice(open + 1, k - 1);
    i = k;
    if (head.startsWith("@media")) out.push(...declarations(body, `${context}${head} `));
    else if (head.startsWith("@keyframes")) out.push(`${context}${head} | ${body.replace(/\s+/g, " ").trim()}`);
    else {
      let cur = "";
      let paren = 0;
      const flush = () => { if (cur.trim()) out.push(`${context}| ${head.replace(/\s+/g, " ")} | ${cur.trim().replace(/\s+/g, " ")}`); cur = ""; };
      for (const ch of body) {
        if (ch === "(") paren++;
        if (ch === ")") paren--;
        if (ch === ";" && paren === 0) flush();
        else cur += ch;
      }
      flush();
    }
  }
  return out;
}

const sorted = (list: string[]) => [...list].sort();

/** 0.6.5: the selector of the explicit-ink rule (shell.visual.css), whitespace as `declarations()` normalises it. */
const INK_ELEMENTS =
  ":where(.rk-top, .rk-top__title, .rk-side, .rk-plane, .rk-nav, .rk-item__label, .rk-brand-foot, .rk-page__band, .rk-page__scroll, .rk-page__head h1, .rk-field label, .rk-modal__title, .rk-modal__label, .rk-palette__label, .rk-toast__msg)";

describe("shell.css split", () => {
  it("shell.css is generated from shell.structure.css + shell.visual.css and is not stale", () => {
    expect(read("shell.css")).toBe(combined());
  });

  it("is unlayered and self-contained, so existing adopters resolve nothing new", () => {
    const css = read("shell.css");
    expect(css).not.toMatch(/@layer/);
    expect(css).not.toMatch(/@import/);
  });

  it("keeps every 0.5.1 declaration; the only changes are the toast width variable, the toast centring, the menu exit rules, the reduced-motion opt-out selectors and (0.6.4) the toggle and segmented size variables and (0.6.5) one zero-specificity explicit `color: var(--rk-ink)` rule on shell text elements, each defaulting to the 0.5.1 value", () => {
    const motionSelectors = ["modal", "palette", "menu", "toast", "overlay"].map((c) => `.rk-${c}:where(:not([data-rk-motion="full"], [data-rk-motion="full"] *))`).join(", ");
    const before = declarations(read("test-fixtures/shell-0.5.1.css"));
    const after = declarations(read("shell.css"));
    const removed = before.filter((d) => !after.includes(d));
    const added = after.filter((d) => !before.includes(d));
    expect(sorted(removed)).toEqual(
      sorted([
        "| .rk-toasts | width: min(26rem, calc(100vw - 2rem))",
        "| .rk-toasts | transform: translateX(-50%)",
        "| .rk-toggle | width: 36px",
        "| .rk-toggle | height: 22px",
        "| .rk-toggle__knob | width: 18px",
        "| .rk-toggle__knob | height: 18px",
        "| .rk-toggle[aria-checked=\"true\"] .rk-toggle__knob | transform: translateX(14px)",
        "| .rk-seg | gap: 2px",
        "| .rk-caption__btn:focus-visible | outline: 2px solid var(--rk-focus)",
        "| .rk-caption__btn:focus-visible | outline-offset: -2px",
        "@media (prefers-reduced-motion: reduce) | .rk-modal, .rk-palette, .rk-menu, .rk-toast, .rk-overlay | animation: none !important",
        "@media (prefers-reduced-motion: reduce) | .rk-modal, .rk-palette, .rk-menu, .rk-toast, .rk-overlay | transition: none !important",
      ]),
    );
    // Additions since 0.5.1 are enumerated in test-fixtures/shell-added-since-0.5.1.txt (one declaration per line);
    // regenerate it deliberately when a release adds rules, never by hand-editing shell.css.
    const allowedAdditions = read("test-fixtures/shell-added-since-0.5.1.txt").split("\n").filter(Boolean);
    expect(sorted([...new Set(added)])).toEqual(sorted(allowedAdditions));
    // Same count of declarations apart from those: nothing duplicated or dropped. The net +8 is the two menu exit
    // declarations, the one (0.6.5) explicit-ink declaration the (0.6.7) `.rk-caption__btn` cursor declaration and the four (0.6.8) top-center toast placement lines; every other added line replaces a removed one.
    // Net growth equals the allowed additions minus the allowed removals: nothing duplicated or dropped.
    expect(after.length - before.length).toBe(allowedAdditions.length - removed.length);
  });

  it("0.6.5: shell text elements carry an explicit zero-specificity `color: var(--rk-ink)`, so a data-theme switch restyles them directly instead of through inheritance (WKWebView stale-colour bug)", () => {
    const rule = `| ${INK_ELEMENTS} | color: var(--rk-ink)`;
    expect(declarations(read("shell.visual.css"))).toContain(rule);
    expect(declarations(read("shell.structure.css"))).not.toContain(rule);
    // :where() so the rule never outranks a host or shell rule, and none of the listed elements has its own colour rule at higher specificity to conflict.
    expect(INK_ELEMENTS.startsWith(":where(")).toBe(true);
  });

  it("no (selector, property) pair appears in both halves, so cascade order inside a selector is unchanged", () => {
    const key = (d: string) => {
      const at = d.lastIndexOf(" | ");
      return `${d.slice(0, at)} | ${d.slice(at + 3).split(":")[0]}`;
    };
    const structure = new Set(declarations(read("shell.structure.css")).map(key));
    const visual = declarations(read("shell.visual.css")).map(key);
    expect(visual.filter((k) => structure.has(k))).toEqual([]);
  });

  it("structure carries no colour, type, radius, shadow or motion", () => {
    const visualProps = /\|\s(color|background(-[a-z]+)?|border(-[a-z-]+)?|font(-[a-z]+)?|line-height|letter-spacing|box-shadow|opacity|animation|transition|fill|outline(-[a-z]+)?|text-[a-z-]+|cursor|user-select|padding(-[a-z]+)?|margin(-[a-z]+)?|gap):/;
    const offenders = declarations(read("shell.structure.css")).filter((d) => visualProps.test(d.replace(/^.*?\| .*? \| /, "| ")));
    expect(offenders).toEqual([]);
    // Colour variables live in the visual half; only layout variables are in structure.
    const rootVars = declarations(read("shell.structure.css")).filter((d) => d.startsWith("| :root |"));
    expect(rootVars.every((d) => /--rk-(sidebar-width|titlebar-[a-z-]+|lights-width|caption-width|toast-offset|palette-width):/.test(d))).toBe(true);
  });

  it("0.6.1: rules that restyle a host's surface without being needed to function are visual, and moving them reorders nothing that matters", () => {
    const moved = [
      "| .rk-btn | display: inline-flex",
      "| .rk-btn | align-items: center",
      "| .rk-btn | justify-content: center",
      "| .rk-btn:disabled | pointer-events: none",
      "| .rk-modal | overflow: auto",
      "| .rk-palette | overflow: hidden",
    ];
    const structure = declarations(read("shell.structure.css"));
    const visual = declarations(read("shell.visual.css"));
    for (const d of moved) {
      expect(structure, d).not.toContain(d);
      expect(visual, d).toContain(d);
    }
    // The move changes source order only relative to visual rules. None of them sets the same property for a selector
    // that can match the same element (same class token), so the computed style is the same wherever the rule sits.
    const parts = (d: string) => {
      const [, selector = "", decl = ""] = d.split(" | ");
      return { classes: new Set(selector.match(/\.rk-[a-z0-9_-]+/g) ?? []), prop: decl.split(":")[0] };
    };
    for (const d of moved) {
      const m = parts(d);
      const clashes = visual.filter((o) => {
        if (o === d) return false;
        const p = parts(o);
        return p.prop === m.prop && [...p.classes].some((c) => m.classes.has(c));
      });
      expect(clashes, d).toEqual([]);
    }
  });

  it("0.6.1: the toast stack centres with the individual `translate` property, never `transform`, so a host translate utility does not stack with it", () => {
    for (const file of ["shell.structure.css", "shell.css"]) {
      const toasts = declarations(read(file)).filter((d) => d.includes("| .rk-toasts |"));
      expect(toasts, file).toContain("| .rk-toasts | translate: var(--rk-toast-translate, -50% 0)");
      expect(toasts.some((d) => /\| transform:/.test(d)), file).toBe(false);
    }
  });

  it("G2: the overlay button font/colour reset is a visual rule, not structural", () => {
    const rule = "| :where(.rk-overlay, .rk-menu, .rk-toasts) button | font: inherit";
    expect(declarations(read("shell.visual.css"))).toContain(rule);
    expect(declarations(read("shell.structure.css"))).not.toContain(rule);
  });

  it("shell.layered.css imports both halves into the rk layer and nothing else", () => {
    const css = read("shell.layered.css").replace(/\/\*[\s\S]*?\*\//g, "");
    expect(css.trim().split("\n").map((l) => l.trim())).toEqual(['@import "./shell.structure.css" layer(rk);', '@import "./shell.visual.css" layer(rk);']);
  });

  it("package version, exports and files list the new stylesheets", () => {
    expect(pkg.version).toBe("0.7.0");
    for (const name of ["shell.css", "shell.layered.css", "shell.structure.css", "shell.visual.css", "suite-aliases.css"]) {
      expect(pkg.exports[`./${name}`]).toBe(`./${name}`);
      expect(pkg.files).toContain(name);
    }
  });
});

describe("reduced motion", () => {
  it("is honoured by default and opted out only through data-rk-motion=\"full\", with the original specificity", () => {
    const css = read("shell.css");
    const block = css.slice(css.indexOf("@media (prefers-reduced-motion: reduce)"));
    for (const c of ["modal", "palette", "menu", "toast", "overlay"]) expect(block).toContain(`.rk-${c}:where(:not([data-rk-motion="full"], [data-rk-motion="full"] *))`);
    expect(block).toContain("animation: none !important; transition: none !important;");
    // :where() contributes no specificity, so a host rule that beat the old selector still beats this one and vice versa.
    for (const c of ["skeleton", "progress__fill"]) expect(block).toContain(`.rk-${c}:where(:not([data-rk-motion="full"], [data-rk-motion="full"] *))`);
    expect(block.match(/:where\(/g)?.length).toBe(7);
  });
});
