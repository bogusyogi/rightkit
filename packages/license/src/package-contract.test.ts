import { existsSync, readFileSync, readdirSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

const packageRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const rightkitRoot = path.resolve(packageRoot, "../..");

function nativeRustFiles(root: string): string[] {
  const found: string[] = [];
  for (const entry of readdirSync(root, { withFileTypes: true })) {
    if (["dist", "node_modules"].includes(entry.name)) continue;
    const absolute = path.join(root, entry.name);
    if (entry.isDirectory()) found.push(...nativeRustFiles(absolute));
    else if (entry.name === "Cargo.toml" || entry.name.endsWith(".rs")) {
      found.push(path.relative(root, absolute));
    }
  }
  return found;
}

describe("npm package boundary", () => {
  it("keeps testing outside main entry & removes renderer transport dependencies", () => {
    const manifest = JSON.parse(readFileSync(path.join(packageRoot, "package.json"), "utf8"));
    expect(manifest.version).toBe("0.3.0");
    expect(manifest.exports["./testing"]).toEqual({
      development: { types: "./dist/testing.d.ts", import: "./dist/testing.js" },
      default: null,
    });
    expect(manifest.peerDependencies?.["@tauri-apps/plugin-http"]).toBeUndefined();
    expect(manifest.devDependencies["@tauri-apps/plugin-http"]).toBeUndefined();
    const main = readFileSync(path.join(packageRoot, "src/index.ts"), "utf8");
    expect(main).not.toMatch(/from ['"].*testing|RIGHTKIT_LICENSE_TESTING_DO_NOT_SHIP|classifyLifecycle|\bfetch\(/);
    // Real resolver fixtures cover export conditions; final-artifact sentinel gate remains separate.
  });
  it("contains no native Rust policy payload", () => {
    const manifest = JSON.parse(readFileSync(path.join(packageRoot, "package.json"), "utf8")) as {
      files?: string[];
    };

    expect(manifest.files?.some((entry) => entry === "rust" || entry.startsWith("rust/"))).toBe(false);
    expect(existsSync(path.join(packageRoot, "rust"))).toBe(false);
    expect(nativeRustFiles(packageRoot)).toEqual([]);
  });

  it("does not instruct apps to vendor rightkit-license", () => {
    const text = [
      readFileSync(path.join(rightkitRoot, "README.md"), "utf8"),
      readFileSync(path.join(packageRoot, "src/index.ts"), "utf8"),
    ].join("\n");

    expect(text).not.toMatch(/vendor\/rightkit-license|rightkit-license\s*=\s*\{\s*path|vendored Rust crate/i);
  });
});
