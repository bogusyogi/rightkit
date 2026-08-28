import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { resolveNativeArtifactPath, resolveNativeCargoLayout } from "./native-cargo-layout.mjs";

function fixture() {
  return mkdtempSync(path.join(os.tmpdir(), "right-release-native-layout-"));
}

function write(root, relative, text) {
  const file = path.join(root, relative);
  mkdirSync(path.dirname(file), { recursive: true });
  writeFileSync(file, text);
  return file;
}

test("resolves workspace manifest, workspace lock, and workspace target prefix", () => {
  const root = fixture();
  const manifest = write(root, "engine/Cargo.toml", "[package]\nname = \"engine\"\nversion = \"0.1.0\"\n");
  write(root, "Cargo.toml", "[workspace]\nmembers = [\"engine\"]\n");
  const lock = write(root, "Cargo.lock", "# workspace lock\n");
  const layout = resolveNativeCargoLayout({ appRoot: root, config: { nativeAssembly: { cargoManifest: "engine/Cargo.toml" } } });
  assert.equal(layout.manifestPath, manifest);
  assert.equal(layout.lockPath, lock);
  assert.equal(layout.lockSource, "workspace");
  assert.equal(layout.workspaceRoot, root);
  assert.equal(layout.targetLink, path.join(root, "target"));
  assert.equal(layout.targetPrefix, "target/");
});

test("honors an explicitly declared manifest lock source", () => {
  const root = fixture();
  write(root, "engine/Cargo.toml", "[package]\nname = \"engine\"\nversion = \"0.1.0\"\n");
  write(root, "Cargo.toml", "[workspace]\nmembers = [\"engine\"]\n");
  const lock = write(root, "engine/Cargo.lock", "# package lock\n");
  write(root, "Cargo.lock", "# workspace lock\n");
  const layout = resolveNativeCargoLayout({
    appRoot: root,
    config: { nativeAssembly: { cargoManifest: "engine/Cargo.toml", cargoLockSource: "manifest" } },
  });
  assert.equal(layout.lockPath, lock);
  assert.equal(layout.lockSource, "manifest");
});

test("retains src-tauri fallback for existing applications", () => {
  const root = fixture();
  write(root, "src-tauri/Cargo.toml", "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\n");
  write(root, "src-tauri/Cargo.lock", "# fixture lock\n");
  const layout = resolveNativeCargoLayout({ appRoot: root, config: {} });
  assert.equal(layout.fallback, true);
  assert.equal(layout.manifestPath, path.join(root, "src-tauri", "Cargo.toml"));
  assert.equal(layout.lockPath, path.join(root, "src-tauri", "Cargo.lock"));
  assert.equal(layout.targetLink, path.join(root, "src-tauri", "target"));
  assert.equal(resolveNativeArtifactPath(layout, "src-tauri/target/release/app.exe"), path.join(root, "src-tauri", "target", "release", "app.exe"));
});

test("supports an explicit target root for cache and sealing consumers", () => {
  const root = fixture();
  write(root, "engine/Cargo.toml", "[package]\nname = \"engine\"\nversion = \"0.1.0\"\n");
  write(root, "engine/Cargo.lock", "# package lock\n");
  const layout = resolveNativeCargoLayout({
    appRoot: root,
    config: { nativeAssembly: { cargoManifest: "engine/Cargo.toml", targetDir: ".cache/native-target" } },
  });
  assert.equal(layout.targetPrefix, ".cache/native-target/");
  assert.equal(layout.resolveArtifactPath(".cache/native-target/release/app"), path.join(root, ".cache", "native-target", "release", "app"));
});
