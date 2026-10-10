import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { aliasTokens, applyTokens, fontTokens, presetTokens, SUITE_ALIASES, type StyleTarget } from "./tokens.js";
import { applyBrand } from "./runtime.js";

function fakeRoot(initial: Record<string, string> = {}) {
  const values = new Map(Object.entries(initial));
  const root: StyleTarget & { values: Map<string, string> } = {
    values,
    style: {
      setProperty: (n, v) => void values.set(n, v),
      removeProperty: (n) => {
        const old = values.get(n) ?? "";
        values.delete(n);
        return old;
      },
      getPropertyValue: (n) => values.get(n) ?? "",
    },
  };
  return root;
}

describe("applyTokens", () => {
  it("sets arbitrary custom properties and leaves other names alone", () => {
    const root = fakeRoot({ "--other": "keep" });
    applyTokens({ "--ink": "#101216", "--paper": "#F2F3F5", "--skip": undefined }, root);
    expect(Object.fromEntries(root.values)).toEqual({ "--other": "keep", "--ink": "#101216", "--paper": "#F2F3F5" });
  });

  it("undo restores the previous inline value and removes names it added", () => {
    const root = fakeRoot({ "--ink": "#000000" });
    const undo = applyTokens({ "--ink": "#111111", "--copper": "#C46A3D" }, root);
    undo();
    expect(root.values.get("--ink")).toBe("#000000");
    expect(root.values.has("--copper")).toBe(false);
  });

  it("rejects names that are not custom properties", () => {
    expect(() => applyTokens({ "ink": "#fff" }, fakeRoot())).toThrow(/Invalid CSS custom property/);
  });

  it("is a no-op without a root", () => {
    expect(() => applyTokens({ "--ink": "#fff" }, null)()).not.toThrow();
  });
});

describe("aliasTokens and SUITE_ALIASES", () => {
  it("maps unprefixed names to var() references", () => {
    expect(aliasTokens({ "--ink": "--rk-fg" })).toEqual({ "--ink": "var(--rk-fg)" });
  });

  it("matches the names app-shell's suite-aliases.css defines", () => {
    const css = readFileSync(join(dirname(fileURLToPath(import.meta.url)), "../../app-shell/suite-aliases.css"), "utf8");
    const declared = [...css.matchAll(/(--[a-z0-9-]+):\s*var\((--rk-[a-z0-9-]+)/g)].map((m) => [m[1], m[2]]);
    // The regex sees `var(--rk-wordmark-font, inherit)` as well, so the names must match exactly.
    expect(declared.length).toBeGreaterThan(30);
    expect(Object.entries(SUITE_ALIASES)).toEqual(declared);
  });
});

describe("presets and fonts", () => {
  const preset = {
    id: "viewright-paper",
    mode: "light" as const,
    tokens: { "--ink": "#FFFDF8", "--paper": "#211C18", "--copper": "#9D5530" },
    accents: { blue: { "--copper": "#0C6AA5" } },
  };

  it("merges an accent over the preset base", () => {
    expect(presetTokens(preset, "blue")).toEqual({ "--ink": "#FFFDF8", "--paper": "#211C18", "--copper": "#0C6AA5" });
    expect(presetTokens(preset, "missing")["--copper"]).toBe("#9D5530");
    expect(presetTokens(preset)["--copper"]).toBe("#9D5530");
  });

  it("maps font stacks onto the suite font variables", () => {
    expect(fontTokens({ mono: "Menlo, monospace", wordmark: '"Tanker", sans-serif' })).toEqual({
      "--rk-font-mono": "Menlo, monospace",
      "--rk-wordmark-font": '"Tanker", sans-serif',
    });
  });
});

describe("applyBrand suite aliases", () => {
  const brand = { name: "Test", colors: { brand: "#0C6AA5" } };

  it("writes the aliases only when asked, and undo removes them", () => {
    const off = fakeRoot();
    applyBrand(brand, "light", off);
    expect(off.values.has("--ink")).toBe(false);

    const on = fakeRoot();
    const undo = applyBrand(brand, "light", on, { applySuiteAliases: true });
    expect(on.values.get("--ink")).toBe("var(--rk-ink)");
    undo();
    expect(on.values.has("--ink")).toBe(false);
    expect(on.values.has("--rk-brand")).toBe(false);
  });
});
