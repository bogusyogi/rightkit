import assert from "node:assert/strict";
import { existsSync, lstatSync, mkdirSync, readFileSync, realpathSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { spawn } from "node:child_process";
import { once } from "node:events";

import {
  CACHE_SCHEMA,
  DEFAULT_DESIRED_FREE_BYTES,
  DEFAULT_HARD_FREE_BYTES,
  DEFAULT_SCCACHE_MAX_BYTES,
  DEFAULT_TARGET_MAX_BYTES,
  acquireCacheLease,
  acquireSuiteBuildSlot,
  applyCachePrune,
  assertWriteVolumeFloors,
  ensureCacheEntry,
  inspectWriteVolumes,
  markCacheEntrySuccessful,
  migrateLegacyCache,
  planCachePrune,
  readCachePolicy,
  resolveCacheLayout,
  resolveSharedCacheIdentity,
  resolveSharedCacheRoot,
} from "./cache-policy.mjs";

function root() { return path.join(os.tmpdir(), `rightkit-cache-${process.pid}-${Date.now()}-${Math.random().toString(16).slice(2)}`); }
function layout(dir, fingerprint = "a1b2c3d4e5f60708") {
  return resolveCacheLayout({ cacheRoot: dir, platform: "mac", architecture: "aarch64", app: "fixture", fingerprint, kind: "release" });
}

function migrationFixture() {
  const base = root();
  const fingerprint = "abcdef0123456789";
  const legacyRoot = path.join(base, "repo", "app", "src-tauri", "target");
  const legacyTargetRoot = path.join(base, "repo", ".right-release", "cache", "cargo-target", "mac", fingerprint);
  const legacyCargoHome = path.join(base, "repo", ".right-release", "cache", "cargo-home");
  mkdirSync(legacyTargetRoot, { recursive: true });
  writeFileSync(path.join(legacyTargetRoot, "artifact"), "legacy-target");
  mkdirSync(legacyCargoHome, { recursive: true });
  writeFileSync(path.join(legacyCargoHome, "registry"), "legacy-cargo-home");
  mkdirSync(path.dirname(legacyRoot), { recursive: true });
  symlinkSync(legacyTargetRoot, legacyRoot);
  const sharedCacheRoot = path.join(base, "shared-cache");
  return {
    fingerprint,
    legacyRoot,
    legacyTargetRoot,
    legacyCargoHome,
    layout: layout(sharedCacheRoot, fingerprint),
    app: "fixture",
    legacyReleaseLockPath: path.join(base, "repo", ".right-release", "locks", "mac.lock.json"),
  };
}

test("cache roots are platform-native and overrides must be absolute", () => {
  assert.throws(() => resolveSharedCacheRoot({ platform: "mac", home: "/Users/test", env: {} }), /RIGHT_RELEASE_CACHE_ROOT is required/);
  assert.equal(resolveSharedCacheRoot({ platform: "win", env: { LOCALAPPDATA: "C:/Users/test/AppData/Local" } }), "C:\\Users\\test\\AppData\\Local\\RightSuite\\Cache\\release");
  assert.equal(resolveSharedCacheRoot({ platform: "linux", home: "/home/test", xdgCacheHome: "/tmp/xdg", env: {} }), "/tmp/xdg/rightsuite/release");
  assert.equal(resolveSharedCacheRoot({ platform: "mac", env: { RIGHT_RELEASE_CACHE_ROOT: "/tmp/cache" } }), "/tmp/cache");
  assert.throws(() => resolveSharedCacheRoot({ platform: "mac", env: { RIGHT_RELEASE_CACHE_ROOT: "relative" } }), /absolute/i);
});

test("layout isolates app, platform, kind, and fingerprint", () => {
  const base = root();
  const release = layout(base);
  const testLayout = resolveCacheLayout({ cacheRoot: base, platform: "mac", architecture: "aarch64", app: "other", fingerprint: "a1b2c3d4e5f60708", kind: "test" });
  assert.match(release.targetDir, /targets[\\/]mac[\\/]fixture[\\/]a1b2c3d4e5f60708$/);
  assert.match(testLayout.targetDir, /test-targets[\\/]mac[\\/]other[\\/]a1b2c3d4e5f60708$/);
  assert.notEqual(release.targetDir, testLayout.targetDir);
});

test("policy defaults are the approved target, compiler, desired and hard limits", () => {
  const policy = readCachePolicy({});
  assert.equal(CACHE_SCHEMA, 1);
  assert.equal(policy.targetMaxBytes, DEFAULT_TARGET_MAX_BYTES);
  assert.equal(policy.sccacheMaxBytes, DEFAULT_SCCACHE_MAX_BYTES);
  assert.equal(policy.desiredFreeBytes, DEFAULT_DESIRED_FREE_BYTES);
  assert.equal(policy.hardFreeBytes, DEFAULT_HARD_FREE_BYTES);
});

test("entry metadata is atomic, complete, and updates success only after requested", () => {
  const entry = layout(root());
  ensureCacheEntry({ layout: entry, metadata: { toolchain: { cargo: "cargo 1", rustc: "rustc 1", host: "host" } }, now: new Date("2026-07-21T00:00:00Z") });
  const created = JSON.parse(readFileSync(entry.markerPath, "utf8"));
  assert.equal(created.schema, CACHE_SCHEMA);
  assert.equal(created.id, "target:mac:fixture:a1b2c3d4e5f60708");
  assert.equal(created.lastSuccessfulBuildAt, null);
  markCacheEntrySuccessful({ layout: entry, now: new Date("2026-07-21T01:00:00Z") });
  assert.equal(JSON.parse(readFileSync(entry.markerPath, "utf8")).lastSuccessfulBuildAt, "2026-07-21T01:00:00.000Z");
});

test("entry leases protect live entries, reclaim stale owners, and suite slots time out without a fallback", () => {
  const entry = layout(root());
  ensureCacheEntry({ layout: entry });
  const lease = acquireCacheLease({ layout: entry, pid: process.pid, argv: ["test"] });
  assert.throws(() => acquireCacheLease({ layout: entry, pid: process.pid + 100000, argv: ["second"] }), /lease.*active/i);
  lease.release();
  writeFileSync(entry.leasePath, JSON.stringify({ pid: 99999999, createdAt: "2000-01-01T00:00:00.000Z" }));
  const reclaimed = acquireCacheLease({ layout: entry, pid: process.pid, argv: ["reclaimed"] });
  reclaimed.release();
  const slot = acquireSuiteBuildSlot({ layout: entry, pid: process.pid, argv: ["holder"], waitMs: 0 });
  assert.throws(() => acquireSuiteBuildSlot({ layout: entry, pid: process.pid + 100000, argv: ["waiter"], waitMs: 0, sleep: () => {} }), /suite build slot.*active|timed out/i);
  slot.release();
});

test("build and migration derive identical native-feature cache fingerprints", () => {
  const base = root(); const cargoLock = path.join(base, "Cargo.lock"); const cargoToml = path.join(base, "Cargo.toml");
  mkdirSync(base, { recursive: true });
  writeFileSync(cargoLock, "[[package]]\nname = 'fixture'\n");
  writeFileSync(cargoToml, "[dependencies]\nrusqlite = { version = '1', features = [\"bundled-sqlcipher\"] }\n");
  const inputs = { cargoLockPath: cargoLock, cargoTomlPath: cargoToml, rustcVerbose: "rustc 1.91.0\nhost: aarch64-apple-darwin", targetTriple: "aarch64-apple-darwin", profile: "release" };
  const build = resolveSharedCacheIdentity(inputs);
  const migration = resolveSharedCacheIdentity(inputs);
  assert.equal(build.fingerprint, migration.fingerprint);
  assert.notEqual(build.fingerprint, resolveSharedCacheIdentity({ ...inputs, features: [] }).fingerprint);
});

test("cache identity ignores local workspace version bumps but keeps registry dependency identity", () => {
  const base = root();
  const cargoLock = path.join(base, "Cargo.lock");
  mkdirSync(base, { recursive: true });
  const lock = (appVersion, depVersion, checksum) => `version = 4\n\n[[package]]\nname = "fixture"\nversion = "${appVersion}"\ndependencies = [\n "serde",\n]\n\n[[package]]\nname = "serde"\nversion = "${depVersion}"\nsource = "registry+https://github.com/rust-lang/crates.io-index"\nchecksum = "${checksum}"\n`;
  const inputs = { cargoLockPath: cargoLock, rustcVerbose: "rustc 1.91.0\nhost: aarch64-apple-darwin", targetTriple: "aarch64-apple-darwin" };

  writeFileSync(cargoLock, lock("1.0.0", "1.0.0", "aaa"));
  const original = resolveSharedCacheIdentity(inputs);
  writeFileSync(cargoLock, lock("1.0.1", "1.0.0", "aaa"));
  assert.equal(resolveSharedCacheIdentity(inputs).fingerprint, original.fingerprint);
  writeFileSync(cargoLock, lock("1.0.1", "1.0.1", "bbb"));
  assert.notEqual(resolveSharedCacheIdentity(inputs).fingerprint, original.fingerprint);
});

test("migration moves an actual src-tauri target symlink and keeps dry-run physically pure", () => {
  const fixture = migrationFixture();
  const preview = migrateLegacyCache({ ...fixture, dryRun: true });
  assert.equal(preview.moved, false);
  assert.equal(existsSync(fixture.layout.cacheRoot), false);
  assert.equal(lstatSync(fixture.legacyRoot).isSymbolicLink(), true);
  const result = migrateLegacyCache({ ...fixture, dryRun: false });
  assert.equal(result.moved, true);
  assert.equal(existsSync(fixture.legacyRoot), false);
  assert.equal(readFileSync(path.join(fixture.layout.targetDir, "artifact"), "utf8"), "legacy-target");
  assert.equal(readFileSync(path.join(fixture.layout.cargoHome, "registry"), "utf8"), "legacy-cargo-home");
});

test("migration rejects target symlinks outside the exact derived fingerprint directory", () => {
  const fixture = migrationFixture();
  const other = path.join(path.dirname(fixture.legacyTargetRoot), "other-fingerprint");
  mkdirSync(other, { recursive: true });
  symlinkSync(other, `${fixture.legacyRoot}-wrong`);
  assert.throws(() => migrateLegacyCache({ ...fixture, legacyRoot: `${fixture.legacyRoot}-wrong`, dryRun: true }), /unexpected target link/i);
  symlinkSync(path.join(other, "missing"), `${fixture.legacyRoot}-dangling`);
  assert.throws(() => migrateLegacyCache({ ...fixture, legacyRoot: `${fixture.legacyRoot}-dangling`, dryRun: true }), /unexpected target link/i);
});

test("migration rolls back target, Cargo home, and src-tauri link after every injected post-move failure", () => {
  for (const failureAt of ["after-target-move", "after-entry-marker", "after-cargo-home-move", "before-receipt", "receipt-write"]) {
    const fixture = migrationFixture();
    assert.throws(() => migrateLegacyCache({ ...fixture, dryRun: false, onPostMove: (step) => { if (step === failureAt) throw new Error(`injected ${step}`); }, writeMigrationReceipt: failureAt === "receipt-write" ? () => { throw new Error("injected receipt write"); } : undefined }), /injected/);
    assert.equal(readFileSync(path.join(fixture.legacyTargetRoot, "artifact"), "utf8"), "legacy-target", failureAt);
    assert.equal(realpathSync(fixture.legacyRoot), realpathSync(fixture.legacyTargetRoot), failureAt);
    assert.equal(lstatSync(fixture.legacyRoot).isSymbolicLink(), true, failureAt);
    assert.equal(readFileSync(path.join(fixture.legacyCargoHome, "registry"), "utf8"), "legacy-cargo-home", failureAt);
    assert.equal(existsSync(fixture.layout.targetDir), false, failureAt);
    assert.equal(existsSync(fixture.layout.cargoHome), false, failureAt);
  }
});

test("a competing suite build slot blocks migration before it moves a legacy cache", () => {
  const fixture = migrationFixture();
  const slot = acquireSuiteBuildSlot({ layout: fixture.layout, pid: process.pid, argv: ["build"], waitMs: 0 });
  try {
    assert.throws(() => migrateLegacyCache({ ...fixture, dryRun: false, waitMs: 0 }), /suite build slot.*active|timed out/i);
    assert.equal(realpathSync(fixture.legacyRoot), realpathSync(fixture.legacyTargetRoot));
    assert.equal(existsSync(fixture.layout.targetDir), false);
  } finally { slot.release(); }
});

test("a live shared build lease blocks migration during in-lock revalidation", () => {
  const fixture = migrationFixture();
  mkdirSync(fixture.layout.leasesDir, { recursive: true });
  writeFileSync(path.join(fixture.layout.leasesDir, "another-build.json"), JSON.stringify({ pid: process.pid, createdAt: new Date().toISOString() }));
  assert.throws(() => migrateLegacyCache({ ...fixture, dryRun: false }), /shared entry lease is live/i);
  assert.equal(realpathSync(fixture.legacyRoot), realpathSync(fixture.legacyTargetRoot));
  assert.equal(existsSync(fixture.layout.targetDir), false);
  assert.equal(existsSync(fixture.layout.gcLockPath), false);
  assert.equal(existsSync(fixture.layout.suiteSlotPath), false);
});

test("a live legacy release lock blocks migration during in-lock revalidation", () => {
  const fixture = migrationFixture();
  mkdirSync(path.dirname(fixture.legacyReleaseLockPath), { recursive: true });
  writeFileSync(fixture.legacyReleaseLockPath, JSON.stringify({ pid: process.pid }));
  assert.throws(() => migrateLegacyCache({ ...fixture, dryRun: false }), /legacy release lock is live/i);
  assert.equal(realpathSync(fixture.legacyRoot), realpathSync(fixture.legacyTargetRoot));
  assert.equal(existsSync(fixture.layout.targetDir), false);
  assert.equal(existsSync(fixture.layout.gcLockPath), false);
  assert.equal(existsSync(fixture.layout.suiteSlotPath), false);
});

test("pruning is oldest successful use first and dry-run equals apply candidates", () => {
  const base = root();
  const older = layout(base, "1111111111111111");
  const newer = layout(base, "2222222222222222");
  for (const [entry, stamp] of [[older, "2026-01-01T00:00:00.000Z"], [newer, "2026-02-01T00:00:00.000Z"]]) {
    ensureCacheEntry({ layout: entry, now: new Date(stamp) });
    writeFileSync(path.join(entry.targetDir, "payload"), "x".repeat(32));
    markCacheEntrySuccessful({ layout: entry, now: new Date(stamp) });
  }
  const snapshot = { cacheRoot: base, targetBytes: 64, entries: [
    { id: newer.id, dir: newer.targetDir, bytes: 32, marker: JSON.parse(readFileSync(newer.markerPath)), leased: false },
    { id: older.id, dir: older.targetDir, bytes: 32, marker: JSON.parse(readFileSync(older.markerPath)), leased: false },
  ] };
  const plan = planCachePrune({ snapshot, policy: { targetMaxBytes: 31, desiredFreeBytes: 0 }, protectedEntryIds: new Set() });
  assert.deepEqual(plan.candidates.map((x) => x.id), [older.id, newer.id]);
  assert.deepEqual(applyCachePrune({ plan, cacheRoot: base, dryRun: true }).candidateIds, applyCachePrune({ plan, cacheRoot: base, dryRun: false }).candidateIds);
  assert.equal(existsSync(older.targetDir), false);
  assert.equal(existsSync(newer.targetDir), false);
});

test("pruning fails closed for malformed markers, symlink escapes, and cache root", () => {
  const base = root();
  const safe = layout(base);
  mkdirSync(safe.targetDir, { recursive: true });
  writeFileSync(safe.markerPath, "not-json");
  const outside = root(); mkdirSync(outside, { recursive: true });
  const link = layout(base, "2222222222222222"); mkdirSync(path.dirname(link.targetDir), { recursive: true }); symlinkSync(outside, link.targetDir);
  const plan = { candidates: [
    { id: "bad", dir: safe.targetDir, bytes: 1 },
    { id: "link", dir: link.targetDir, bytes: 1 },
    { id: "root", dir: base, bytes: 1 },
  ] };
  const result = applyCachePrune({ plan, cacheRoot: base, dryRun: false });
  assert.deepEqual(result.removedIds, []);
  assert.equal(existsSync(safe.targetDir), true);
  assert.equal(lstatSync(link.targetDir).isSymbolicLink(), true);
});

test("entry creation rejects a symlinked cache ancestor before writing outside the cache root", () => {
  const parent = root(); const outside = root(); mkdirSync(parent, { recursive: true }); mkdirSync(outside, { recursive: true });
  symlinkSync(outside, path.join(parent, "cache"));
  const unsafe = resolveCacheLayout({ cacheRoot: path.join(parent, "cache"), platform: "mac", architecture: "aarch64", app: "fixture", fingerprint: "3333333333333333", kind: "release" });
  assert.throws(() => ensureCacheEntry({ layout: unsafe }), /symlink|unsafe/i);
  assert.equal(existsSync(path.join(outside, "targets")), false);
});

test("prune takes the global GC lock and blocks a racing lease before deletion", () => {
  const base = root(); const entry = layout(base, "4444444444444444"); ensureCacheEntry({ layout: entry }); writeFileSync(path.join(entry.targetDir, "payload"), "x");
  const plan = { cacheRoot: base, candidates: [{ id: entry.id, dir: entry.targetDir, bytes: 1 }] };
  assert.throws(() => applyCachePrune({ plan, cacheRoot: base, dryRun: false, beforeDelete: () => acquireCacheLease({ layout: entry }) }), /global GC lock active/);
  assert.equal(existsSync(entry.targetDir), true);
  assert.equal(existsSync(path.join(base, "gc", "gc.lock.json")), false);
});

test("a real child-process lease acquired after planning excludes its entry from prune", async () => {
  const base = root(); const entry = layout(base, "5555555555555555"); ensureCacheEntry({ layout: entry }); writeFileSync(path.join(entry.targetDir, "payload"), "x");
  const plan = { cacheRoot: base, candidates: [{ id: entry.id, dir: entry.targetDir, bytes: 1 }] };
  const moduleUrl = new URL("./cache-policy.mjs", import.meta.url).href;
  const child = spawn(process.execPath, ["--input-type=module", "--eval", `import { resolveCacheLayout, acquireCacheLease } from ${JSON.stringify(moduleUrl)}; const l=resolveCacheLayout({cacheRoot:process.argv[1],platform:'mac',architecture:'aarch64',app:'fixture',fingerprint:'5555555555555555',kind:'release'}); const lease=acquireCacheLease({layout:l}); console.log('ready'); setTimeout(()=>lease.release(),500);`, base], { stdio: ["ignore", "pipe", "pipe"] });
  await once(child.stdout, "data");
  const result = applyCachePrune({ plan, cacheRoot: base, dryRun: false });
  assert.deepEqual(result.removedIds, []);
  assert.equal(existsSync(entry.targetDir), true);
  await once(child, "exit");
});

test("write-volume inspection deduplicates same devices and enforces both hard floors", () => {
  const statfs = (dir) => ({ bsize: 1024, bavail: dir.includes("cache") ? 20 : 30, dev: dir.includes("same") ? 1 : dir.includes("cache") ? 2 : 3 });
  const same = inspectWriteVolumes({ repoRoot: "/same/repo", vaultRoot: "/same/vault", cacheRoot: "/same/cache", statfs });
  assert.equal(same.length, 1);
  const distinct = inspectWriteVolumes({ repoRoot: "/repo", vaultRoot: "/repo", cacheRoot: "/cache", statfs });
  assert.equal(distinct.length, 2);
  assert.throws(() => assertWriteVolumeFloors({ volumes: distinct, policy: { hardFreeBytes: 25 * 1024 }, configuredRepoMinimumBytes: 25 * 1024 }), /cache|repo/i);
});
