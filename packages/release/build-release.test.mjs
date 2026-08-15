import assert from "node:assert/strict";
import { mkdtempSync, readFileSync, readdirSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { execFileSync, spawnSync } from "node:child_process";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { parseCpuSeconds, processGroupCpuSeconds } from "./process-liveness.mjs";

const source = readFileSync(path.join(path.dirname(fileURLToPath(import.meta.url)), "build-release.mjs"), "utf8");
const build = path.join(path.dirname(fileURLToPath(import.meta.url)), "build-release.mjs");

test("general build dry-run is filesystem-pure and never starts its package command", () => {
  const root = mkdtempSync(path.join(os.tmpdir(), "right-build-dry-run-"));
  writeFileSync(path.join(root, "right-release.config.mjs"), "export default { app: 'fixture', version: '1.0.0', targets: { mac: { package: { cmd: 'false' } } } };\n");
  for (const args of [["init", "--initial-branch", "main"], ["config", "user.email", "test@example.com"], ["config", "user.name", "Test"], ["add", "."], ["commit", "-m", "init"]]) execFileSync("git", args, { cwd: root });
  const before = readdirSync(root).sort();
  const result = spawnSync(process.execPath, [build, "--config", "right-release.config.mjs", "--platform", "mac", "--dry-run"], { cwd: root, encoding: "utf8" });
  assert.equal(result.status, 0, result.stderr);
  assert.match(result.stdout, /build dry-run/);
  assert.deepEqual(readdirSync(root).sort(), before);
});

test("macOS and Windows builds default to Cache V2 while explicit legacy mode remains available", () => {
  assert.match(source, /assertCleanSource/);
  assert.match(source, /right-release build/);
  assert.match(source, /RIGHT_RELEASE_CACHE_MODE/);
  assert.match(source, /platform === "mac" \|\| platform === "win" \? "shared" : "legacy"/);
  assert.match(source, /cacheMode !== "legacy" && cacheMode !== "shared"/);
  assert.match(source, /resolveSharedCacheRoot/);
  assert.match(source, /acquireSuiteBuildSlot/);
  assert.match(source, /acquireHeavyWorkSlot/);
  assert.match(source, /heavyCommandEnvironment/);
  assert.match(source, /heavySlot\.trackChild/);
  assert.match(source, /detached: process\.platform !== "win32"/);
  assert.match(source, /acquireCacheLease/);
  assert.match(source, /markCacheEntrySuccessful/);
  assert.match(source, /RIGHT_RELEASE_CACHE_OWNER/);
  assert.match(source, /suiteSlot\?\.release\(\)/);
  assert.match(source, /heavySlot\?\.release\(\)/);
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
  assert.match(source, /createTargetBridge\(\{\s*link: targetLink,\s*target: managedCargoTarget \?\? env\.CARGO_TARGET_DIR,/);
  // ownedRoot must stay wired: without it the bridge refuses every link left by
  // an earlier fingerprint, so any Cargo.lock or version bump hard-stops the
  // next build until someone deletes the link by hand.
  assert.match(source, /ownedRoot: path\.dirname\(managedCargoTarget \?\? env\.CARGO_TARGET_DIR\)/);
  assert.match(source, /targetBridge\.run\(.*runBuildStateMachine/s);
  assert.match(source, /if \(cacheMode === "shared"\) targetBridge\.ensure\(\)/);
});

test("Windows seal and cache identity bind the signing contract, config, and receipts", () => {
  assert.match(source, /signingIdentity = platform === "win"/);
  assert.match(source, /contract: target\.signingContract/);
  assert.match(source, /configSha256: hashFile\(configPath\)/);
  assert.match(source, /windows-\$\{phase\}\.json/);
  assert.match(source, /inputHashes\["\.right-release\/signing-identity\.json"\]/);
  assert.match(source, /cache: cacheIdentity\.fingerprint, signingIdentity/);
  assert.match(source, /receipts: signingIdentity\.receiptInputs\.map/);
  assert.match(readFileSync(new URL("./release.mjs", import.meta.url), "utf8"), /item\.after\?\.sha256 !== currentHashes/);
});

test("CPU sampling separates a silent working step from a hang", () => {
  // Real `ps -o cputime=` shapes, including a multi-process group.
  assert.equal(parseCpuSeconds("        0:03.21\n"), 3.21);
  assert.equal(parseCpuSeconds("  1:02:03\n"), 3723);
  assert.equal(parseCpuSeconds("  2-01:00:00\n"), 176400);
  // A process group sums every member, which is how a linker under cargo shows up.
  assert.equal(parseCpuSeconds("  0:10.00\n  0:05.00\n"), 15);
  // A vanished process yields no rows: null, not zero, so the caller falls back
  // to output and mtime rather than treating it as "no CPU used".
  assert.equal(parseCpuSeconds(""), null);
  assert.equal(parseCpuSeconds("   \n"), null);
  assert.equal(parseCpuSeconds("garbage"), null);
  assert.equal(parseCpuSeconds(undefined), null);
});

test("CPU sampling degrades to the old behaviour rather than inventing a verdict", () => {
  const ok = (stdout) => () => ({ status: 0, stdout });
  // A working process group reports advancing CPU.
  assert.equal(processGroupCpuSeconds(123, { platform: "darwin", run: ok("  0:42.50\n") }), 42.5);
  // Windows has no `ps`; sampling is skipped, not guessed.
  assert.equal(processGroupCpuSeconds(123, { platform: "win32", run: ok("  0:42.50\n") }), null);
  // No pid, a failed probe, or an empty result must all be "cannot measure",
  // never 0 — 0 would look like a hang and kill a healthy build.
  assert.equal(processGroupCpuSeconds(0, { platform: "darwin", run: ok("  0:42.50\n") }), null);
  assert.equal(processGroupCpuSeconds(123, { platform: "darwin", run: () => ({ status: 1, stdout: "" }) }), null);
  assert.equal(processGroupCpuSeconds(123, { platform: "darwin", run: ok("") }), null);
});
