// The /core entry is the panel shell and primitives: it must not pull @rightkit/license, logs, theme, updates or app-shell.
import { existsSync, readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { describe, expect, it } from "vitest";

const root = process.cwd();
const IMPORT = /(?:import|export)\s+(?:type\s+)?(?:[^'"`;]*?\s+from\s+)?["']([^"']+)["']/g;

/** Walk relative imports from `entry`; collect every bare specifier (runtime and type-only) on the way. */
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

describe("@rightkit/settings-ui/core import graph", () => {
  const src = bareImports(join(root, "src/core.ts"), [".ts", ".tsx"]);
  it("source: imports only react", () => {
    expect([...src.bare].sort()).toEqual(["react"]);
  });
  it("source: reaches none of the standard-section modules", () => {
    for (const banned of ["src/sections.tsx", "src/hooks.ts", "src/standard.ts", "src/standard-entry.ts"]) expect(src.files).not.toContain(banned);
  });
  it("the standard entry is where the optional peers live", () => {
    const std = bareImports(join(root, "src/standard-entry.ts"), [".ts", ".tsx"]);
    for (const peer of ["@rightkit/license", "@rightkit/theme", "@rightkit/logs", "@rightkit/app-update"]) expect(std.bare.has(peer)).toBe(true);
  });
  it.runIf(existsSync(join(root, "dist/core.js")))("built output (dist/core.js): imports only react", () => {
    const built = bareImports(join(root, "dist/core.js"), [".js"]);
    expect([...built.bare].sort()).toEqual(["react", "react/jsx-runtime"]);
    const types = bareImports(join(root, "dist/core.d.ts"), [".d.ts"]);
    for (const peer of ["@rightkit/license", "@rightkit/theme", "@rightkit/logs", "@rightkit/app-update", "@rightkit/react-shell"]) expect(types.bare.has(peer)).toBe(false);
  });
  it("the root entry still re-exports the whole 0.3.x surface", async () => {
    const [root_, core, std] = await Promise.all([import("./index.js"), import("./core.js"), import("./standard-entry.js")]);
    for (const name of Object.keys(core)) expect(root_).toHaveProperty(name);
    for (const name of Object.keys(std)) expect(root_).toHaveProperty(name);
    for (const name of ["SettingsPanel", "SettingRow", "SettingsGroup", "standardSections", "useLicenseSection", "useDoctor", "sectionIcons", "createMemorySettings", "createShellBackends", "useSettings", "mergeSections"]) expect(root_).toHaveProperty(name);
  });
});

describe("package.json", () => {
  const pkg = JSON.parse(readFileSync(join(root, "package.json"), "utf8")) as { exports: Record<string, unknown>; peerDependencies: Record<string, string>; peerDependenciesMeta: Record<string, { optional?: boolean }> };
  it("exposes ., ./core, ./standard and ./headless (added in 0.4.2) and keeps the existing subpaths", () => {
    expect(Object.keys(pkg.exports).sort()).toEqual([".", "./core", "./headless", "./settings.css", "./shell-adapter", "./standard"]);
  });
  it("marks logs and updates optional peers; license and theme are required (sections.tsx and hooks.ts import them at runtime)", () => {
    for (const peer of ["@rightkit/license", "@rightkit/logs", "@rightkit/theme", "@rightkit/app-update"]) expect(pkg.peerDependencies[peer]).toBeDefined();
    for (const peer of ["@rightkit/logs", "@rightkit/app-update"]) expect(pkg.peerDependenciesMeta[peer]).toEqual({ optional: true });
    for (const peer of ["@rightkit/license", "@rightkit/theme"]) expect(pkg.peerDependenciesMeta?.[peer]?.optional).not.toBe(true);
  });
});
