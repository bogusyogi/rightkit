// The /headless entry is the search hooks for hosts that own their markup: it must import no stylesheet and reach no styled part.
import { existsSync, readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { describe, expect, it } from "vitest";

const root = process.cwd();
const IMPORT = /(?:import|export)\s+(?:type\s+)?(?:[^'"`;]*?\s+from\s+)?["']([^"']+)["']/g;

/** Walk relative imports from `entry`; collect every bare specifier (runtime and type-only) and every file reached. */
function bareImports(entry: string, ext: string[]): { bare: Set<string>; files: string[] } {
  const bare = new Set<string>();
  const seen = new Set<string>();
  const queue = [entry];
  while (queue.length) {
    const file = queue.pop()!;
    if (seen.has(file)) continue;
    seen.add(file);
    const text = readFileSync(file, "utf8");
    for (const m of text.matchAll(IMPORT)) {
      const spec = m[1]!;
      if (spec.startsWith(".")) {
        const base = resolve(dirname(file), spec).replace(/\.js$/, "");
        const hit = ext.map((e) => base + e).find((f) => existsSync(f));
        if (!hit) throw new Error(`cannot resolve ${spec} from ${file}`);
        queue.push(hit);
      } else bare.add(spec);
    }
  }
  return { bare, files: [...seen].map((f) => f.slice(root.length + 1)).sort() };
}

describe("@rightkit/settings-ui/headless import graph", () => {
  const src = bareImports(join(root, "src/headless.ts"), [".ts", ".tsx"]);

  it("source: imports only react, and no stylesheet", () => {
    expect([...src.bare].sort()).toEqual(["react"]);
    expect([...src.bare].some((s) => s.endsWith(".css"))).toBe(false);
  });

  it("source: reaches no styled part (primitives, panel, icons, sections, styled search components)", () => {
    for (const banned of ["src/primitives.tsx", "src/SettingsPanel.tsx", "src/icons.tsx", "src/sections.tsx", "src/search.tsx", "src/standard.ts", "src/standard-entry.ts", "src/index.ts"]) {
      expect(src.files).not.toContain(banned);
    }
  });

  it.runIf(existsSync(join(root, "dist/headless.js")))("built output (dist/headless.js): imports only react, no CSS reference", () => {
    const built = bareImports(join(root, "dist/headless.js"), [".js"]);
    expect([...built.bare].sort()).toEqual(["react", "react/jsx-runtime"]);
    expect(readFileSync(join(root, "dist/headless.js"), "utf8")).not.toMatch(/\.css["']/);
  });

  it("the headless entry exports the hooks and the no-DOM scopes, and no styled component", async () => {
    const mod = await import("./headless.js");
    for (const name of ["useSettingsRow", "useSettingsItem", "useSettingsGroup", "useSettingsSection", "useSettingsSearchField", "useSettingsSearchStatus", "useSettingsSearch", "SettingsSearchProvider", "SettingsGroupScope", "SettingsSectionScope", "useSearchKeys", "highlightText", "parseSearchTerms", "defaultAnnouncement"]) {
      expect(mod).toHaveProperty(name);
    }
    for (const styled of ["SettingsSearchSection", "SettingsResultsAnnouncer", "SettingsItem", "SettingsGroup", "SettingRow", "HighlightedText", "SettingsPanel"]) {
      expect(mod).not.toHaveProperty(styled);
    }
  });
});

describe("package.json headless export", () => {
  const pkg = JSON.parse(readFileSync(join(root, "package.json"), "utf8")) as { exports: Record<string, { types: string; import: string }> };
  it("exposes ./headless with types and the built file", () => {
    expect(pkg.exports["./headless"]).toEqual({ types: "./dist/headless.d.ts", import: "./dist/headless.js" });
  });
});
