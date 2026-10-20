import assert from "node:assert/strict";
import { mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import path from "node:path";
import test from "node:test";
import {
  beginArtifactLease,
  completeArtifactLease,
  materializeArtifacts,
  markerPath,
  readPublishedVersions,
  recordPublishedVersion,
  retainArtifacts,
  resolveArtifactDir,
  artifactProcessIdentity,
} from "./artifact-storage.mjs";
import fs from "node:fs";
import os from "node:os";

// macOS: /private/tmp is the real path behind /tmp. Elsewhere: the system temp directory in its real, long form.
const TEST_TMP = process.platform === "darwin" ? "/private/tmp" : fs.realpathSync.native(os.tmpdir());

function fixture() {
  const root = mkdtempSync(path.join(TEST_TMP, "rightkit-artifact-storage-"));
  return { root, managedRoot: path.join(root, "managed") };
}

test("artifact marker uses numeric lease schema and completes after materialization", () => {
  const fx = fixture();
  try {
    beginArtifactLease({ app: "fixture", version: "1.0.0", managedRoot: fx.managedRoot, now: 100, ownerPid: process.pid, leaseMs: 500 });
    const source = path.join(fx.root, "Fixture.exe");
    writeFileSync(source, "signed");
    materializeArtifacts({ app: "fixture", version: "1.0.0", managedRoot: fx.managedRoot, files: [{ source, name: "Fixture.exe" }], now: 200 });
    const marker = completeArtifactLease({ app: "fixture", version: "1.0.0", managedRoot: fx.managedRoot, completedAt: 300 });
    assert.deepEqual(marker, { schemaVersion: 1, app: "fixture", version: "1.0.0", createdAt: 100, ownerPid: 0, leaseUntil: 0, completedAt: 300 });
    assert.equal(readFileSync(path.join(resolveArtifactDir({ app: "fixture", version: "1.0.0", managedRoot: fx.managedRoot }), "Fixture.exe"), "utf8"), "signed");
    assert.deepEqual(JSON.parse(readFileSync(markerPath({ app: "fixture", version: "1.0.0", managedRoot: fx.managedRoot }), "utf8")), marker);
  } finally { rmSync(fx.root, { recursive: true, force: true }); }
});

test("materialization keeps new sealed bytes when a managed name already exists", () => {
  const fx = fixture();
  try {
    beginArtifactLease({ app: "fixture", version: "1.0.0", managedRoot: fx.managedRoot, now: 100, ownerPid: process.pid });
    const source = path.join(fx.root, "Fixture.dmg");
    writeFileSync(source, "old");
    materializeArtifacts({ app: "fixture", version: "1.0.0", managedRoot: fx.managedRoot, files: [{ source, name: "Fixture.dmg" }], now: 101 });
    writeFileSync(source, "new");
    materializeArtifacts({ app: "fixture", version: "1.0.0", managedRoot: fx.managedRoot, files: [{ source, name: "Fixture.dmg" }], now: 102 });
    assert.equal(readFileSync(path.join(resolveArtifactDir({ app: "fixture", version: "1.0.0", managedRoot: fx.managedRoot }), "Fixture.dmg"), "utf8"), "new");
  } finally { rmSync(fx.root, { recursive: true, force: true }); }
});

test("publication pointer replaces current version per platform", () => {
  const fx = fixture();
  try {
    recordPublishedVersion({ app: "fixture", platform: "mac", version: "1.0.0", managedRoot: fx.managedRoot });
    recordPublishedVersion({ app: "fixture", platform: "win", version: "1.0.0", managedRoot: fx.managedRoot });
    const result = recordPublishedVersion({ app: "fixture", platform: "mac", version: "1.1.0", managedRoot: fx.managedRoot });
    assert.deepEqual(result.versions.sort(), ["1.0.0", "1.1.0"]);
    assert.deepEqual(result.platforms, { mac: "1.1.0", win: "1.0.0" });
  } finally { rmSync(fx.root, { recursive: true, force: true }); }
});

test("retention preserves all versions when publication state is unknown", () => {
  const fx = fixture();
  try {
    for (const [version, completedAt] of [["1.0.0", 1], ["1.1.0", 2], ["1.2.0", 3], ["1.3.0", 4]]) {
      beginArtifactLease({ app: "fixture", version, managedRoot: fx.managedRoot, now: completedAt, ownerPid: process.pid });
      completeArtifactLease({ app: "fixture", version, managedRoot: fx.managedRoot, completedAt });
    }
    const result = retainArtifacts({ app: "fixture", managedRoot: fx.managedRoot });
    assert.equal(result.state, "unknown");
    assert.deepEqual(result.removed, []);
    assert.equal(readPublishedVersions({ app: "fixture", managedRoot: fx.managedRoot }).state, "unknown");
  } finally { rmSync(fx.root, { recursive: true, force: true }); }
});

test("retention keeps newest three plus published version after successful publication", () => {
  const fx = fixture();
  try {
    for (const [version, completedAt] of [["1.0.0", 1], ["1.1.0", 2], ["1.2.0", 3], ["1.3.0", 4], ["1.4.0", 5]]) {
      beginArtifactLease({ app: "fixture", version, managedRoot: fx.managedRoot, now: completedAt, ownerPid: process.pid });
      completeArtifactLease({ app: "fixture", version, managedRoot: fx.managedRoot, completedAt });
    }
    recordPublishedVersion({ app: "fixture", version: "1.0.0", managedRoot: fx.managedRoot });
    const result = retainArtifacts({ app: "fixture", managedRoot: fx.managedRoot });
    assert.deepEqual(result.removed, ["1.1.0"]);
    assert.deepEqual(result.retained, ["1.0.0", "1.2.0", "1.3.0", "1.4.0"]);
  } finally { rmSync(fx.root, { recursive: true, force: true }); }
});

test("retention protects a live lease even when marker has completedAt", () => {
  const fx = fixture();
  try {
    beginArtifactLease({ app: "fixture", version: "1.0.0", managedRoot: fx.managedRoot, now: 1, ownerPid: process.pid });
    const directory = resolveArtifactDir({ app: "fixture", version: "1.0.0", managedRoot: fx.managedRoot });
    writeFileSync(markerPath({ app: "fixture", version: "1.0.0", managedRoot: fx.managedRoot }), JSON.stringify({ schemaVersion: 1, app: "fixture", version: "1.0.0", createdAt: 1, ownerPid: process.pid, leaseUntil: 2, completedAt: 1 }));
    const result = retainArtifacts({ app: "fixture", managedRoot: fx.managedRoot, now: 1, keep: 0 });
    assert.deepEqual(result.removed, []);
    assert.ok(directory);
  } finally { rmSync(fx.root, { recursive: true, force: true }); }
});

test("malformed publication pointer fails safe without deletion or overwrite", () => {
  const fx = fixture();
  try {
    const root = path.join(fx.managedRoot, "artifacts", "fixture");
    mkdirSync(root, { recursive: true });
    writeFileSync(path.join(root, "published.json"), "not-json");
    assert.equal(retainArtifacts({ app: "fixture", managedRoot: fx.managedRoot }).state, "unknown");
    assert.throws(() => recordPublishedVersion({ app: "fixture", version: "1.0.0", managedRoot: fx.managedRoot }), /unknown/);
  } finally { rmSync(fx.root, { recursive: true, force: true }); }
});

test("malformed retention lock fails closed", () => {
  const fx = fixture();
  try {
    const lock = path.join(fx.managedRoot, "artifacts", "fixture", ".retention-lock");
    mkdirSync(lock, { recursive: true });
    writeFileSync(path.join(lock, "owner.json"), "not-json");
    assert.throws(() => beginArtifactLease({ app: "fixture", version: "1.0.0", managedRoot: fx.managedRoot }), /retention lock is held/);
  } finally { rmSync(fx.root, { recursive: true, force: true }); }
});

for (const scenario of ["dead-pid", "pid-reused", "live", "foreign-host", "legacy-dead", "legacy-live", "same-pid-reused"]) {
  test(`lease owner identity: ${scenario}`, () => {
    const fx = fixture(), events = [], options = { app: "fixture", version: "1.0.0", managedRoot: fx.managedRoot,
      now: 100, ownerHost: "local", processIdentity: (pid) => ({ alive: true, startTime: pid === process.pid ? "new" : "original" }), log: (e) => events.push(e) };
    try {
      const previous = beginArtifactLease(options);
      assert.equal(previous.ownerHost, "local"); assert.equal(previous.ownerStartTime, "new");
      previous.ownerPid = scenario === "same-pid-reused" ? process.pid : 31151;
      previous.ownerStartTime = scenario.includes("reused") ? "old" : "original";
      if (scenario.startsWith("legacy")) { delete previous.ownerHost; delete previous.ownerStartTime; }
      if (scenario === "foreign-host") previous.ownerHost = "remote";
      writeFileSync(markerPath(options), JSON.stringify(previous));
      const inspect = (pid) => ({ alive: !scenario.includes("dead") || pid === process.pid, startTime: pid === process.pid ? "new" : "original" });
      const reclaim = () => beginArtifactLease({ ...options, now: scenario === "live" ? previous.leaseUntil + 1 : 101, processIdentity: inspect });
      if (["live", "foreign-host", "legacy-live"].includes(scenario)) {
        assert.throws(reclaim, /lease is held/); assert.deepEqual(events, []);
        assert.equal(JSON.parse(readFileSync(markerPath(options))).ownerPid, 31151);
      } else {
        assert.equal(reclaim().ownerPid, process.pid);
        assert.equal(events.length, 1); assert.equal(events[0].event, "artifact-lease-reclaimed");
        assert.equal(events[0].reason, scenario.includes("dead") ? "dead-pid" : "pid-reused");
      }
    } finally { rmSync(fx.root, { recursive: true, force: true }); }
  });
}

test("process start identity stays conservative when inspection denied", () => {
  assert.deepEqual(artifactProcessIdentity(31151, { alive: () => false, run: () => assert.fail() }), { alive: false, startTime: null });
  assert.deepEqual(artifactProcessIdentity(31151, { alive: () => true, run: () => ({ status: 1 }) }), { alive: true, startTime: null });
  assert.deepEqual(artifactProcessIdentity(31151, { alive: () => true, run: () => ({ status: 0, stdout: "start\n" }) }), { alive: true, startTime: "start" });
});

test("the managed root is canonicalised once, so a root moved to another volume behind a symlink still works", async () => {
  const { canonicalManagedRoot, resolveManagedRoot } = await import("./artifact-storage.mjs");
  // resolveManagedRoot stays lexical (the signing agent's allowed roots use that spelling); only canonicalManagedRoot resolves.
  const fsMod = await import("node:fs");
  const osMod = await import("node:os");
  const pathMod = await import("node:path");
  const base = fsMod.mkdtempSync(pathMod.join(fsMod.realpathSync.native(osMod.tmpdir()), "rk-root-"));
  const real = pathMod.join(base, "E-managed");
  fsMod.mkdirSync(real);
  const link = pathMod.join(base, "D-managed");
  let linked = true;
  try { fsMod.symlinkSync(real, link, "junction"); } catch { linked = false; }
  if (linked) {
    assert.equal(canonicalManagedRoot(link), fsMod.realpathSync.native(real));
    assert.equal(resolveManagedRoot({ env: { RIGHTKIT_MANAGED_ROOT: link } }), pathMod.resolve(link));
  }
  const missing = pathMod.join(base, "not-yet");
  assert.equal(canonicalManagedRoot(missing), pathMod.resolve(missing), "a root that does not exist yet stays as configured");
  assert.throws(() => canonicalManagedRoot(real, { realpath: () => { const error = new Error("denied"); error.code = "EACCES"; throw error; } }), /denied/);
  fsMod.rmSync(base, { recursive: true, force: true });
});

test("only a configured managed root itself may be a symlink; symlinks below it or elsewhere are still refused", async () => {
  const { assertSafeMutation } = await import("./artifact-storage.mjs");
  const fsMod = await import("node:fs");
  const osMod = await import("node:os");
  const pathMod = await import("node:path");
  const base = fsMod.mkdtempSync(pathMod.join(fsMod.realpathSync.native(osMod.tmpdir()), "rk-safe-"));
  const real = pathMod.join(base, "E-managed");
  fsMod.mkdirSync(pathMod.join(real, "artifacts"), { recursive: true });
  const link = pathMod.join(base, "D-managed");
  try { fsMod.symlinkSync(real, link, "junction"); } catch { fsMod.rmSync(base, { recursive: true, force: true }); return; }
  const env = { RIGHTKIT_MANAGED_ROOT: link };
  assert.doesNotThrow(() => assertSafeMutation(pathMod.join(link, "artifacts", "App", "1.0.0"), { env }));
  assert.throws(() => assertSafeMutation(pathMod.join(link, "artifacts", "App"), { env: {} }), /through symlink/);
  const inner = pathMod.join(real, "artifacts", "inner-link");
  let innerLinked = true;
  try { fsMod.symlinkSync(base, inner, "junction"); } catch { innerLinked = false; }
  if (innerLinked) assert.throws(() => assertSafeMutation(pathMod.join(link, "artifacts", "inner-link", "x"), { env }), /through symlink/);
  fsMod.rmSync(base, { recursive: true, force: true });
});
