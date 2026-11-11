// The crates publish route (publish.yml scans, plan, pack, verify; publish-direct.yml) under the two rules it must follow:
//  - RightKit is one Cargo workspace with one root Cargo.lock (rightkit#1505): a crate unit is read at crates/<x>, its cargo scans run in
//    the workspace that owns it, and its scan receipt is bound to the lock that governs it (the root lock for a member);
//  - a crate that ran zero tests with no failure, skip or ignore passes (GATE-CONTRACT GC-A08; Adrian, 2026-11-03, end-to-end tests only).
import test from "node:test";
import assert from "node:assert/strict";
import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { canonicalPublicationEvidence, crateUnitsFromMetadata, readCrateMetadata } from "./publish.mjs";
import { publicationContent } from "./run-deterministic-scans.mjs";
import { ROOT_CARGO_LOCK, buildScanReceipt, candidateCargoLocks, candidateCrateWorkspaces, cargoLocksMatch, requiredReceiptScanIds, resultProblems, verifyScanReceipt } from "./scan-receipt.mjs";
import { validatePublicationEvidence } from "../../packages/release/publication-gate.mjs";

const sha256 = (bytes) => crypto.createHash("sha256").update(bytes).digest("hex");
const ROOT_LOCK = "# root lock\n";
const OWN_LOCK = "# rightkit-mesh-ffi lock\n";

/** A checkout shaped as the repository is: one root workspace of crates/*, and one crate that declares its own workspace and lock. */
function checkout(t, { rootLock = ROOT_LOCK } = {}) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "rk-publish-route-"));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  const write = (file, text) => { fs.mkdirSync(path.dirname(path.join(dir, file)), { recursive: true }); fs.writeFileSync(path.join(dir, file), text); };
  write("Cargo.toml", '[workspace]\nresolver = "3"\nmembers = ["crates/*"]\nexclude = ["crates/rightkit-mesh-ffi"]\n');
  if (rootLock !== null) write("Cargo.lock", rootLock);
  write("crates/rightkit-fs/Cargo.toml", '[package]\nname = "rightkit-fs"\nversion = "0.5.1"\n');
  write("crates/rightkit-time/Cargo.toml", '[package]\nname = "rightkit-time"\nversion = "0.3.2"\n');
  write("crates/rightkit-mesh-ffi/Cargo.toml", '[package]\nname = "rightkit-mesh-ffi"\nversion = "0.1.0"\n\n[workspace]\n');
  write("crates/rightkit-mesh-ffi/Cargo.lock", OWN_LOCK);
  return dir;
}

/** `cargo metadata --no-deps` as cargo answers it: every member of the workspace that owns the directory it ran in. */
function metadataOf(dir) {
  const pkg = (name, version, rel) => ({ id: `path+file://${dir}/${rel}#${version}`, name, version, manifest_path: path.join(dir, rel, "Cargo.toml"), dependencies: [], publish: null });
  const root = [pkg("rightkit-fs", "0.5.1", "crates/rightkit-fs"), pkg("rightkit-time", "0.3.2", "crates/rightkit-time")];
  const own = [pkg("rightkit-mesh-ffi", "0.1.0", "crates/rightkit-mesh-ffi")];
  return (cwd) => (path.resolve(cwd) === path.resolve(dir, "crates/rightkit-mesh-ffi")
    ? { workspace_root: path.join(dir, "crates/rightkit-mesh-ffi"), target_directory: path.join(dir, "crates/rightkit-mesh-ffi/target"), workspace_members: own.map((p) => p.id), packages: own }
    : { workspace_root: dir, target_directory: path.join(dir, "target"), workspace_members: root.map((p) => p.id), packages: root });
}

test("crate units are read once per owning workspace and sit at crates/<x>, never below another crate", (t) => {
  const dir = checkout(t);
  const answer = metadataOf(dir);
  const cwds = [];
  const spawn = (command, args, options) => { cwds.push(path.relative(dir, options.cwd) || "."); return { status: 0, stdout: JSON.stringify(answer(options.cwd)) }; };
  const perCrate = readCrateMetadata(dir, spawn);
  assert.deepEqual(cwds, [".", "crates/rightkit-mesh-ffi"]);
  assert.deepEqual([...perCrate].map(([name, c]) => [name, c.dir, c.workspace, c.lock]), [
    ["rightkit-fs", "crates/rightkit-fs", ".", "Cargo.lock"],
    ["rightkit-mesh-ffi", "crates/rightkit-mesh-ffi", "crates/rightkit-mesh-ffi", "crates/rightkit-mesh-ffi/Cargo.lock"],
    ["rightkit-time", "crates/rightkit-time", ".", "Cargo.lock"],
  ]);
  assert.equal(perCrate.get("rightkit-fs").metadata.target_directory, path.join(dir, "target"));
  const units = crateUnitsFromMetadata(perCrate);
  assert.deepEqual([...units].map(([name, u]) => [name, u.dir, u.version]).sort(), [
    ["rightkit-fs", "crates/rightkit-fs", "0.5.1"], ["rightkit-mesh-ffi", "crates/rightkit-mesh-ffi", "0.1.0"], ["rightkit-time", "crates/rightkit-time", "0.3.2"],
  ]);
});

test("the publication scans plan a root-workspace crate in the repository root", (t) => {
  const dir = checkout(t);
  const files = ["Cargo.toml", "Cargo.lock", "crates/rightkit-fs/Cargo.toml", "crates/rightkit-fs/src/lib.rs", "crates/rightkit-time/Cargo.toml", "crates/rightkit-mesh-ffi/Cargo.toml", "crates/rightkit-mesh-ffi/Cargo.lock"];
  const readBlob = (f) => (f.endsWith(".rs") ? "pub fn f() {}\n" : fs.readFileSync(path.join(dir, f), "utf8"));
  const workspace = new Map([["rightkit-fs", { name: "rightkit-fs", dir: "crates/rightkit-fs" }], ["rightkit-mesh-ffi", { name: "rightkit-mesh-ffi", dir: "crates/rightkit-mesh-ffi" }]]);
  const selection = publicationContent({ registry: "crates", units: "rightkit-fs rightkit-mesh-ffi", files, workspace, readBlob });
  assert.deepEqual(selection.rustUnits, [
    { name: "rightkit-fs", dir: "crates/rightkit-fs", cargoDir: ".", crate: "rightkit-fs" },
    { name: "rightkit-mesh-ffi", dir: "crates/rightkit-mesh-ffi", cargoDir: "crates/rightkit-mesh-ffi", crate: "rightkit-mesh-ffi" },
  ]);
  // The root manifest and lock are part of what the scans cover.
  assert.ok(selection.files.includes("Cargo.toml") && selection.files.includes("Cargo.lock"));
});

test("a member of the root workspace is bound to the root Cargo.lock; a crate that is its own workspace to its own", (t) => {
  const dir = checkout(t);
  assert.deepEqual(candidateCrateWorkspaces(dir).map((w) => [w.name, w.workspace, w.lock]), [
    ["rightkit-fs", ".", ROOT_CARGO_LOCK], ["rightkit-mesh-ffi", "crates/rightkit-mesh-ffi", "crates/rightkit-mesh-ffi/Cargo.lock"], ["rightkit-time", ".", ROOT_CARGO_LOCK],
  ]);
  assert.deepEqual(candidateCargoLocks({ candidateDir: dir, units: ["rightkit-fs", "rightkit-time"], registry: "crates" }), {
    "rightkit-fs": { path: "Cargo.lock", sha256: sha256(ROOT_LOCK) }, "rightkit-time": { path: "Cargo.lock", sha256: sha256(ROOT_LOCK) },
  });
  assert.deepEqual(candidateCargoLocks({ candidateDir: dir, units: ["rightkit-mesh-ffi"], registry: "crates" }), { "rightkit-mesh-ffi": { path: "crates/rightkit-mesh-ffi/Cargo.lock", sha256: sha256(OWN_LOCK) } });
});

test("a crate unit with no governing lock, or that is no crate, is refused", (t) => {
  const dir = checkout(t, { rootLock: null });
  assert.throws(() => candidateCargoLocks({ candidateDir: dir, units: ["rightkit-fs"], registry: "crates" }), /crate unit rightkit-fs has no Cargo\.lock to bind/);
  assert.throws(() => candidateCargoLocks({ candidateDir: dir, units: ["rightkit-absent"], registry: "crates" }), /crate unit rightkit-absent has no crates\/<x> manifest to bind/);
});

test("the scans a receipt owes follow the workspace of each bound crate", () => {
  const root = { path: "Cargo.lock", sha256: "a".repeat(64) };
  const own = { path: "crates/rightkit-mesh-ffi/Cargo.lock", sha256: "b".repeat(64) };
  assert.deepEqual(requiredReceiptScanIds({ "rightkit-fs": root, "rightkit-time": root }), ["actionlint", "yaml-parse", "cargo-metadata", "cargo-deny", "cargo-audit"]);
  assert.deepEqual(requiredReceiptScanIds({ "rightkit-mesh-ffi": own }), ["actionlint", "yaml-parse", "cargo-metadata:rightkit-mesh-ffi", "cargo-deny:rightkit-mesh-ffi", "cargo-audit:rightkit-mesh-ffi"]);
  assert.deepEqual(requiredReceiptScanIds({ "rightkit-fs": root, "rightkit-mesh-ffi": own }), ["actionlint", "yaml-parse", "cargo-metadata", "cargo-deny", "cargo-audit", "cargo-metadata:rightkit-mesh-ffi", "cargo-deny:rightkit-mesh-ffi", "cargo-audit:rightkit-mesh-ffi"]);
  assert.deepEqual(requiredReceiptScanIds({}), ["actionlint", "yaml-parse"]);
});

const sourceSha = "8e6f0c433d00192f6947c0cb204a487507cbcce3";
const observedTools = { "cargo-deny": "cargo-deny 0.20.2", "cargo-audit": "cargo-audit-audit 0.22.2", actionlint: "1.7.12" };
const scanResult = (ids) => ({ ok: true, complete: true, classes: ["A"], deferred: [], inputDigest: "c".repeat(64), toolVersions: {}, scans: ids.map((id) => ({ id, class: "A", command: `scan ${id}`, status: "pass" })) });
const ROOT_IDS = ["actionlint", "yaml-parse", "cargo-metadata", "cargo-deny", "cargo-audit", "cargo-package-list:rightkit-fs"];

test("a receipt of the root workspace's scans verifies against the root lock and against nothing else", (t) => {
  const dir = checkout(t);
  const cargoLocks = candidateCargoLocks({ candidateDir: dir, units: ["rightkit-fs"], registry: "crates" });
  const result = scanResult(ROOT_IDS);
  assert.deepEqual(resultProblems(result, result.scans, cargoLocks), []);
  const receipt = buildScanReceipt({ result, plannedClassA: result.scans, sourceCommit: sourceSha, headCommit: sourceSha, cargoLocks, observedTools, scope: { registry: "crates", units: ["rightkit-fs"] }, run: { repository: "bogusyogi/rightkit", runId: "41", runAttempt: "1" } });
  assert.equal(receipt.passed, true);
  const expected = { sourceSha, cargoLocks, runId: "41", runAttempt: "1", repository: "bogusyogi/rightkit", registry: "crates", units: ["rightkit-fs"] };
  assert.equal(verifyScanReceipt(receipt, expected), true);
  // The lock binding is exact: another root lock, another lock path, another commit or another run is refused.
  const moved = checkout(t, { rootLock: "# another root lock\n" });
  assert.throws(() => verifyScanReceipt(receipt, { ...expected, cargoLocks: candidateCargoLocks({ candidateDir: moved, units: ["rightkit-fs"], registry: "crates" }) }), /not bound to the candidate crate Cargo\.lock hashes/);
  assert.throws(() => verifyScanReceipt(receipt, { ...expected, cargoLocks: { "rightkit-fs": { path: "crates/rightkit-fs/Cargo.lock", sha256: sha256(ROOT_LOCK) } } }), /not bound to the candidate crate Cargo\.lock hashes/);
  assert.throws(() => verifyScanReceipt(receipt, { ...expected, sourceSha: "0".repeat(40) }), /not bound to the candidate source commit/);
  assert.throws(() => verifyScanReceipt(receipt, { ...expected, runAttempt: "2" }), /not bound to the original run/);
  assert.equal(cargoLocksMatch(receipt.cargoLocks, { "rightkit-fs": { path: "Cargo.lock", sha256: sha256(ROOT_LOCK) }, "rightkit-time": { path: "Cargo.lock", sha256: sha256(ROOT_LOCK) } }), false);
});

test("a receipt without the root workspace's cargo scans is no evidence for a root-workspace crate", (t) => {
  const dir = checkout(t);
  const cargoLocks = candidateCargoLocks({ candidateDir: dir, units: ["rightkit-fs"], registry: "crates" });
  const result = scanResult(ROOT_IDS.filter((id) => id !== "cargo-audit"));
  assert.match(resultProblems(result, result.scans, cargoLocks).join("; "), /required scan cargo-audit is missing from the plan/);
  const receipt = buildScanReceipt({ result, plannedClassA: result.scans, sourceCommit: sourceSha, headCommit: sourceSha, cargoLocks, observedTools, scope: { registry: "crates", units: ["rightkit-fs"] }, run: { repository: "bogusyogi/rightkit", runId: "41", runAttempt: "1" } });
  assert.equal(receipt.passed, false);
  // The ids of a crate's own workspace do not stand in for the root's.
  const suffixed = scanResult(["actionlint", "yaml-parse", "cargo-metadata:rightkit-fs", "cargo-deny:rightkit-fs", "cargo-audit:rightkit-fs"]);
  assert.match(resultProblems(suffixed, suffixed.scans, cargoLocks).join("; "), /required scan cargo-metadata is missing from the plan/);
});

// ---- verify: the native run record of a unit that has no test ---------------------------------

const unit = { name: "rightkit-time", version: "0.3.2", dir: "crates/rightkit-time", sha256: "d".repeat(64) };
const tree = "ae706fe0a86330d6f683b49234ebc53be7168bfd";
const env = { GITHUB_ACTOR: "bogusyogi", GITHUB_JOB: "verify" };
const testRow = (over = {}) => ({ unit: "rust:rightkit-time", command: "cargo test -p rightkit-time --no-fail-fast -- --show-output", isTest: true, ok: true, code: 0, timedOut: false, seconds: 3, executed: 0, failed: 0, ignored: 0, skipped: 0, binaries: 1, ...over });
const record = (results, row = { status: "pass" }) => canonicalPublicationEvidence({ unit, row, summary: { results }, registry: "crates", commit: sourceSha, tree, env });

test("verify records a crate whose test commands ran no test, bound to the commit, tree and sealed archive", () => {
  const { publicationEvidence } = record([testRow()]);
  assert.equal(publicationEvidence.runs[0].passed, 0);
  assert.equal(publicationEvidence.runs[0].failed, 0);
  assert.deepEqual([publicationEvidence.commit, publicationEvidence.tree, publicationEvidence.artifactSha256, publicationEvidence.runs[0].commit, publicationEvidence.runs[0].artifactSha256], [sourceSha, tree, unit.sha256, sourceSha, unit.sha256]);
  // The record is one the publication gate's own validator accepts for this host, and refuses for another commit or archive.
  const expected = { kind: "cargo", name: unit.name, version: unit.version, commit: sourceSha, requiredHosts: [process.platform === "win32" ? "win32" : "darwin"] };
  const onHost = { ...publicationEvidence, runs: [{ ...publicationEvidence.runs[0], host: { os: expected.requiredHosts[0], arch: "arm64" } }] };
  assert.deepEqual(validatePublicationEvidence({ expected, evidence: onHost }).missing, []);
  assert.notDeepEqual(validatePublicationEvidence({ expected: { ...expected, commit: "0".repeat(40) }, evidence: onHost }).missing, []);
  assert.equal(record([testRow({ executed: 4 })]).publicationEvidence.runs[0].passed, 4);
});

test("verify refuses zero tests beside a failure, a skip or an ignore, and a unit with no test command result", () => {
  assert.throws(() => record([testRow({ failed: 1 })]), /does not prove a passing native test run/);
  assert.throws(() => record([testRow({ skipped: 1 })]), /does not prove a passing native test run/);
  assert.throws(() => record([testRow({ ignored: 1 })]), /does not prove a passing native test run/);
  assert.throws(() => record([testRow(), testRow({ command: "cargo test -p rightkit-time --no-fail-fast -- --test-threads=1 --show-output", ignored: 1 })]), /does not prove a passing native test run/);
  assert.throws(() => record([testRow({ ok: false })]), /does not prove a passing native test run/);
  assert.throws(() => record([testRow({ executed: 2, failed: 1 })]), /does not prove a passing native test run/);
  assert.throws(() => record([testRow({ code: 1 })]), /lacks complete passing test command results/);
  assert.throws(() => record([testRow({ timedOut: true })]), /lacks complete passing test command results/);
  assert.throws(() => record([]), /lacks complete passing test command results/);
  assert.throws(() => record([testRow()], null), /no gate unit evidence exists/);
});
