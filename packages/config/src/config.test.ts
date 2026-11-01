import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { rightkitEslintConfig, rightkitViteConfig, DEFAULT_WATCH_IGNORED, RIGHTKIT_ESLINT_RULES } from "./index.js";

describe("vite preset", () => {
  it("pins host, port and watcher ignores", () => {
    const c = rightkitViteConfig({ port: 1421, watchIgnored: ["**/extra/**"] });
    expect(c.clearScreen).toBe(false);
    expect(c.server?.host).toBe("127.0.0.1");
    expect(c.server?.port).toBe(1421);
    expect(c.server?.strictPort).toBe(true);
    expect(c.server?.watch?.ignored).toEqual([...DEFAULT_WATCH_IGNORED, "**/extra/**"]);
    expect(c.optimizeDeps).toBeUndefined();
  });
  it("switches dependency discovery off in QA mode", () => {
    const c = rightkitViteConfig({ port: 1, strictPort: false, qa: { include: ["react-dom/client"] } });
    expect(c.optimizeDeps?.noDiscovery).toBe(true);
    expect(c.optimizeDeps?.include).toEqual(["react-dom/client"]);
    expect(c.server?.strictPort).toBe(false);
  });
});

describe("eslint preset", () => {
  it("composes ignores, the TS block and overrides through tseslint.config", () => {
    const out = rightkitEslintConfig(
      {
        js: { configs: { recommended: { js: true } } },
        globals: { browser: { window: false }, es2022: { Promise: false } },
        tseslint: { configs: { recommended: [{ ts: true }] }, config: (...c: unknown[]) => c },
        reactHooks: { configs: { recommended: { rules: { "react-hooks/x": "error" } } } },
        reactRefresh: {},
      },
      { ignores: ["gen/**"], overrides: [{ files: ["a.ts"] }] },
    ) as Array<Record<string, any>>;
    expect(out).toHaveLength(3);
    expect(out[0].ignores).toContain("gen/**");
    expect(out[1].rules["react-hooks/rules-of-hooks"]).toBe(RIGHTKIT_ESLINT_RULES["react-hooks/rules-of-hooks"]);
    expect(out[1].rules["react-hooks/x"]).toBe("error");
    expect(out[2]).toEqual({ files: ["a.ts"] });
  });
});

describe("shipped presets", () => {
  it("are valid JSON / non-empty TOML", () => {
    for (const f of ["tsconfig/base.json", "tsconfig/react-app.json", "tsconfig/library.json"]) {
      expect(JSON.parse(readFileSync(new URL(`../${f}`, import.meta.url), "utf8")).compilerOptions).toBeTruthy();
    }
    for (const f of ["deny.toml", "audit.toml", "rust-toolchain.toml", "rustfmt.toml", "about.toml", "cargo-config.toml"]) {
      expect(readFileSync(new URL(`../rust/${f}`, import.meta.url), "utf8").length).toBeGreaterThan(10);
    }
  });
});
