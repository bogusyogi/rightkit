import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
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
      url: "git+https://github.com/Orthic-Labs/rightkit.git",
      directory: "packages/platform-ui",
    });
  });
});
