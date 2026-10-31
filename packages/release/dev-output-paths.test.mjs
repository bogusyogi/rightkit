// GB-60 (GATE output pinning; E16/E19): a receipt-managed dev build's configured output paths resolve to the receipt's output directory. A
// source-relative bundleDirectory or stage source never resolves in the snapshot (a stale same-name file there cannot win); a target-relative one is
// migrated explicitly; an incompatible one is refused with a typed error; a direct host keeps its source-relative resolution.
import "./test-support/broker-guard.mjs";
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import test from "node:test";
import { installCarrier } from "./carried-cargo.mjs";
import { receiptRelativeOutputPath, resolveTauriBundleDirectory } from "./development.mjs";
import { resolveStageSource, stageDevelopmentBinaries } from "./stage-binary.mjs";
import { brokerSection } from "./snapshot-broker.mjs";
import { makeFakeBroker } from "./test-support/fake-broker.mjs";

const POSIX = process.platform === "win32" ? "the fixture wrapper is POSIX only" : false;
const TRIPLE = "aarch64-apple-darwin";

function managedHost(t) {
  const fx = makeFakeBroker(t);
  const checkoutDirs = { ".": fx.checkout, native: path.join(fx.checkout, "native") };
  const installed = installCarrier({ base: fs.mkdtempSync(path.join(fx.root, "carrier-")), snapshotRoot: fx.snapshot, checkoutDirs, section: brokerSection(fx.sb), known: { [path.join(fx.checkout, "native")]: fx.outputDir }, platform: process.platform, env: {} });
  t.after(() => installed.cleanup());
  const appRoot = path.join(fx.snapshot, "native");
  return { fx, env: { ...installed.env }, appRoot };
}

test("[GB-60] a receipt-managed build's bundleDirectory is the receipt's output directory: a target-relative setting is migrated, a stale same-name installer in the snapshot cannot win, a source-relative one is refused", { skip: POSIX }, (t) => {
  const h = managedHost(t);
  const bundle = path.join(h.fx.outputDir, TRIPLE, "debug", "bundle", "macos");
  fs.mkdirSync(path.join(bundle, "Fixture.app"), { recursive: true });
  // A stale artifact of the same name lives where the source-relative reading would look.
  const stale = path.join(h.appRoot, "src-tauri", "target", TRIPLE, "debug", "bundle", "macos");
  fs.mkdirSync(path.join(stale, "Fixture.app"), { recursive: true });
  fs.writeFileSync(path.join(stale, "Fixture.app", "STALE"), "old");
  const base = { productName: "Fixture", manifestPath: "src-tauri/Cargo.toml", targetTriple: TRIPLE };
  const found = resolveTauriBundleDirectory({ ...base, bundleDirectory: `src-tauri/target/${TRIPLE}/debug/bundle/macos` }, { root: h.appRoot, platform: "mac", env: h.env });
  assert.equal(found.bundleRoot, bundle, "the receipt's directory, not the snapshot's");
  assert.equal(found.installer, path.join(bundle, "Fixture.app"));
  assert.ok(!fs.existsSync(path.join(found.installer, "STALE")));
  const bare = resolveTauriBundleDirectory({ ...base, bundleDirectory: `target/${TRIPLE}/debug/bundle/macos` }, { root: h.appRoot, platform: "mac", env: h.env });
  assert.equal(bare.bundleRoot, bundle, "a workspace-level target/ spelling migrates the same way");
  assert.throws(() => resolveTauriBundleDirectory({ ...base, bundleDirectory: "dist/bundle/macos" }, { root: h.appRoot, platform: "mac", env: h.env }), (e) => e.code === "DEV_OUTPUT_SOURCE_RELATIVE" && /receipt-managed/.test(e.message), "a source-relative output path is never silently ignored");
  // No receipt output for the manifest (a direct host): the configured source-relative path is used as written.
  const direct = path.join(h.fx.root, "direct-app");
  fs.mkdirSync(path.join(direct, "out", "Fixture.app"), { recursive: true });
  const plain = resolveTauriBundleDirectory({ ...base, bundleDirectory: "out" }, { root: direct, platform: "mac", env: {} });
  assert.equal(plain.bundleRoot, path.join(direct, "out"));
});

test("[GB-60] a stage entry's source under a target directory resolves to the receipt's output directory; several output directories are ambiguous and refused; outside a target directory it is used as written; direct hosts are unchanged", () => {
  const root = path.resolve("/app");
  const out = path.resolve("/managed/ws/abc/g/001/target");
  const entry = { source: `src-tauri/target/${TRIPLE}/debug/rightapp` };
  assert.equal(resolveStageSource(entry, { root, managedOutputDirs: [out] }), path.join(out, TRIPLE, "debug", "rightapp"));
  assert.equal(resolveStageSource(entry, { root, managedOutputDirs: [out, out] }), path.join(out, TRIPLE, "debug", "rightapp"), "the same directory twice is one directory");
  assert.throws(() => resolveStageSource(entry, { root, managedOutputDirs: [out, path.resolve("/managed/ws/def/g/001/target")] }), (e) => e.code === "DEV_OUTPUT_AMBIGUOUS");
  assert.equal(resolveStageSource({ source: "scripts/helper.sh" }, { root, managedOutputDirs: [out] }), path.join(root, "scripts", "helper.sh"), "a source-tree file is not an output");
  assert.equal(resolveStageSource(entry, { root }), path.join(root, "src-tauri", "target", TRIPLE, "debug", "rightapp"), "a direct host resolves where it always did");
  assert.equal(resolveStageSource({ manifestPath: "src-tauri/Cargo.toml", bin: "rightapp", targetTriple: TRIPLE }, { root, resolveTarget: () => out }), path.join(out, TRIPLE, "debug", "rightapp"), "the manifest form resolves through the receipt-aware target");
});

test("[GB-60] staging copies the receipt's bytes, never a stale snapshot file of the same name", () => {
  const root = fs.mkdtempSync(path.join(fs.realpathSync(process.env.TMPDIR ?? "/tmp"), "rk-gb60-"));
  try {
    const out = path.join(root, "managed-out"); fs.mkdirSync(path.join(out, "debug"), { recursive: true });
    fs.writeFileSync(path.join(out, "debug", "rightapp"), "FRESH");
    const staleDir = path.join(root, "app", "target", "debug"); fs.mkdirSync(staleDir, { recursive: true });
    fs.writeFileSync(path.join(staleDir, "rightapp"), "STALE");
    const [staged] = stageDevelopmentBinaries([{ source: "target/debug/rightapp" }], { root: path.join(root, "app"), managedOutputDirs: [out] });
    assert.equal(fs.readFileSync(staged, "utf8"), "FRESH");
    assert.equal(receiptRelativeOutputPath({ declared: "src-tauri/target/x", root: "/r", manifestDir: "src-tauri", managedOutputDir: "/o", label: "x" }), path.join("/o", "x"));
  } finally { fs.rmSync(root, { recursive: true, force: true }); }
});
