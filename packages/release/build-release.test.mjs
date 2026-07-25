import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

const source = readFileSync(path.join(path.dirname(fileURLToPath(import.meta.url)), "build-release.mjs"), "utf8");

test("macOS and Windows builds default to Cache V2 while explicit legacy mode remains available", () => {
  assert.match(source, /RIGHT_RELEASE_CACHE_MODE/);
  assert.match(source, /platform === "mac" \|\| platform === "win" \? "shared" : "legacy"/);
  assert.match(source, /cacheMode !== "legacy" && cacheMode !== "shared"/);
  assert.match(source, /resolveSharedCacheRoot/);
  assert.match(source, /acquireSuiteBuildSlot/);
  assert.match(source, /acquireCacheLease/);
  assert.match(source, /markCacheEntrySuccessful/);
  assert.match(source, /RIGHT_RELEASE_CACHE_OWNER/);
  assert.match(source, /suiteSlot\?\.release\(\)/);
  assert.match(source, /cacheLease\?\.release\(\)/);
  assert.doesNotMatch(source, /process\.exit\(signal/);
  assert.match(source, /throwIfInterrupted/);
  assert.match(source, /delete env\.RIGHT_RELEASE_CACHE_OWNER/);
  assert.match(source, /assertPrimaryReleaseCheckout\(repoRoot\)/);
  assert.match(source, /dirtyBuildInputs\(repoRoot/);
});

test("production builds package from the real primary app checkout", () => {
  assert.match(source, /const configPath = path\.resolve\(configName\)/);
  assert.match(source, /const appRoot = path\.dirname\(configPath\)/);
  assert.match(source, /\[WORKER, "--config", configPath, "--platform", platform, "--no-upload"\]/);
  assert.match(source, /sealRelease\(\{ configRoot: appRoot,/);
});

test("production builds never create or use an internal Git worktree", () => {
  assert.doesNotMatch(source, /git", \["worktree", "add"/);
  assert.doesNotMatch(source, /path\.join\(vaultRoot, "worktrees"/);
  assert.doesNotMatch(source, /worktreeConfigPath/);
});

test("dirty release config is rejected before the config module is imported", () => {
  const guard = source.indexOf("dirtyBuildInputs(repoRoot, { include: [relativeConfig], required: [relativeConfig] }, commit)");
  const imported = source.indexOf("await import(");
  assert.ok(guard >= 0, "config cleanliness guard must exist");
  assert.ok(guard < imported, "config cleanliness guard must run before module import");
});

test("shared Cache V2 target bridge is owned by a finally-cleaned lifecycle", () => {
  assert.match(source, /createTargetBridge\(\{\s*link: targetLink,\s*target: env\.CARGO_TARGET_DIR,/);
  // ownedRoot must stay wired: without it the bridge refuses every link left by
  // an earlier fingerprint, so any Cargo.lock or version bump hard-stops the
  // next build until someone deletes the link by hand.
  assert.match(source, /ownedRoot: path\.dirname\(env\.CARGO_TARGET_DIR\)/);
  assert.match(source, /targetBridge\.run\(.*runBuildStateMachine/s);
  assert.match(source, /if \(cacheMode === "shared"\) targetBridge\.ensure\(\)/);
});
