import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

const packageRoot = join(dirname(fileURLToPath(import.meta.url)), "..");
const packageJson = JSON.parse(readFileSync(join(packageRoot, "package.json"), "utf8")) as {
  license?: string;
  files?: string[];
  repository?: { type?: string; url?: string; directory?: string };
};

describe("published package contract", () => {
  it("declares the approved dual license and ships both license texts", () => {
    expect(packageJson.license).toBe("MIT OR Apache-2.0");
    expect(packageJson.files).toEqual(expect.arrayContaining(["LICENSE-MIT", "LICENSE-APACHE"]));
    expect(readFileSync(join(packageRoot, "LICENSE-MIT"), "utf8")).toContain("MIT License");
    expect(readFileSync(join(packageRoot, "LICENSE-APACHE"), "utf8")).toContain(
      "Apache License",
    );
  });

  it("points consumers at the canonical tracked source directory", () => {
    expect(packageJson.repository).toEqual({
      type: "git",
      url: "git+https://github.com/bogusyogi/rightkit.git",
      directory: "packages/platform-ui",
    });
  });
});


describe("ordinary compiled frontend output", () => {
  it("contains no QA selectors or QA entry exports", async () => {
    const { scanWebBundle } = await import("../../release/qa-control-scan.mjs");
    const output = join(packageRoot, "dist");
    const pending = [join(output, "index.js"), join(output, "headless.js")];
    const reachable = new Set<string>();
    while (pending.length) {
      const file = pending.pop()!;
      if (reachable.has(file)) continue;
      reachable.add(file);
      const text = readFileSync(file, "utf8");
      for (const match of text.matchAll(/(?:from\s*|import\s*\()\s*["'](\.[^"']+)["']/g)) {
        pending.push(resolve(dirname(file), match[1]));
      }
    }
    expect(scanWebBundle([...reachable], { requireFiles: true }).findings).toEqual([]);
  });
});
