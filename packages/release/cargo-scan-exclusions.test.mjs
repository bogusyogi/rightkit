import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { findCargoManifests, isExcludedScanDirectory } from "./cargo-contract.mjs";

test("scan exclusions: SwiftPM .build, *-probe dirs and tasks/handoffs; product dirs stay scanned", () => {
  assert.equal(isExcludedScanDirectory(".build"), true);
  assert.equal(isExcludedScanDirectory("pdf-021-probe"), true);
  assert.equal(isExcludedScanDirectory("handoffs", "tasks"), true);
  assert.equal(isExcludedScanDirectory("handoffs", "docs"), false);
  assert.equal(isExcludedScanDirectory("tasks", "viewright"), false);
  assert.equal(isExcludedScanDirectory("src-tauri"), false);
  assert.equal(isExcludedScanDirectory("node_modules"), true);
  assert.equal(isExcludedScanDirectory("pdfprobe2"), true);
  assert.equal(isExcludedScanDirectory("tmp"), true);
  assert.equal(isExcludedScanDirectory("probes-not-excluded"), false);
});

test("manifest discovery skips excluded dirs and still finds product manifests", () => {
  const root = mkdtempSync(path.join(os.tmpdir(), "rk-scan-"));
  const write = (rel) => { mkdirSync(path.dirname(path.join(root, rel)), { recursive: true }); writeFileSync(path.join(root, rel), "[package]\nname = \"x\"\n"); };
  for (const rel of ["Cargo.toml", "src-tauri/Cargo.toml", "crates/audio/Cargo.toml"]) write(rel);
  for (const rel of [
    "tasks/handoffs/2026-10-11/pdf-021-probe/Cargo.toml",
    "macos-native/Pkg/.build/checkouts/rightkit/Cargo.toml",
    "tmp/pdfprobe2/Cargo.toml",
    "tmp/other/Cargo.toml",
    "node_modules/x/Cargo.toml",
  ]) write(rel);
  const found = findCargoManifests(root).map((f) => path.relative(root, f).split(path.sep).join("/")).sort();
  assert.deepEqual(found, ["Cargo.toml", "crates/audio/Cargo.toml", "src-tauri/Cargo.toml"]);
  // a product manifest placed under tasks/ (not handoffs) is still scanned
  write("tasks/real/Cargo.toml");
  assert.ok(findCargoManifests(root).some((f) => f.endsWith(path.join("tasks", "real", "Cargo.toml"))));
});
