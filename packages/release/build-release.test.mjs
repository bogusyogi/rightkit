import assert from "node:assert/strict";
import { mkdtempSync, readFileSync, readdirSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { execFileSync, spawnSync } from "node:child_process";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { parseCpuSeconds, processGroupCpuSeconds } from "./process-liveness.mjs";
import { brokerManagedHost } from "./cargo-contract.mjs";

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

test("native local builds retain Cache V2 while CI & explicit legacy use safe fallback", () => {
  assert.match(source, /assertCleanSource/);
  assert.match(source, /right-release build/);
  assert.match(source, /RIGHT_RELEASE_CACHE_MODE/);
  assert.match(source, /resolveReleaseCacheMode/);
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
  assert.match(source, /\[WORKER, "--config", configPath, "--platform", platform, "--no-upload", \.\.\.\(skipChecks \? \["--skip-checks"\] : \[\]\)\]/);
  assert.match(source, /sealRelease\(\{ configRoot: appRoot,/);
});

test("build forwards --skip-checks only to configured package checks", () => {
  const worker = readFileSync(new URL("./release.mjs", import.meta.url), "utf8");
  assert.match(source, /let skipChecks = false;/);
  assert.match(source, /else if \(arg === "--skip-checks"\) skipChecks = true;/);
  assert.match(source, /\.\.\.\(skipChecks \? \["--skip-checks"\] : \[\]\)/);
  assert.match(worker, /await runConfiguredChecks\(\{[\s\S]*skipChecks: opts\.skipChecks,[\s\S]*run: runPackageScript,/);
  assert.match(source, /const preflight = buildPreflight\(/);
  assert.match(worker, /await signWindows\(|await signMacos\(/);
});

test("CI cache fallback avoids shared-cache prerequisites before signing", () => {
  assert.match(source, /resolveReleaseCacheMode\(\{ requested: process\.env\.RIGHT_RELEASE_CACHE_MODE, platform, env: process\.env \}\)/);
  assert.match(source, /const sharedCacheRoot = cacheMode === "shared" \? resolveSharedCacheRoot/);
  assert.match(source, /if \(cacheMode === "shared"\) assertSccacheVersion/);
  assert.match(source, /if \(cacheMode !== "shared" && !commandExists\("sccache"\)\) delete env\.RUSTC_WRAPPER;/);
});

test("Windows native executables preserve arguments containing spaces", () => {
  assert.match(source, /const useShell = process\.platform === "win32" && !\/\\\.\(\?:exe\|com\)\$\/i\.test\(cmd\);/);
  assert.match(source, /shell: useShell/);
  assert.doesNotMatch(source, /shell: process\.platform === "win32"/);
});

test("sealing resolves installer and updater artifacts against the broker target on a managed host", () => {
  assert.match(source, /sealRelease\(\{ configRoot: appRoot, managedCargoTarget,/);
  assert.match(source, /function sealRelease\(\{ configRoot, managedCargoTarget,/);
  assert.match(source, /const targetPrefix = "src-tauri\/target\/";/);
  assert.match(source, /if \(managedCargoTarget && normalized\.startsWith\(targetPrefix\)\) \{/);
  // Every configRoot-relative artifact lookup inside sealRelease must route
  // through the resolver, or a managed build fails at seal after a full
  // compile, sign, and notarize because src-tauri/target is never populated
  // locally on a broker-managed host (target-bridge.mjs leaves it alone).
  assert.doesNotMatch(source, /addSource\(path\.resolve\(configRoot, artifact\.file\)/);
  assert.doesNotMatch(source, /addSource\(path\.resolve\(configRoot, artifact\.signature\)/);
  assert.match(source, /addSource\(resolveArtifactPath\(artifact\.file\), "installer"\)/);
  assert.match(source, /addSource\(resolveArtifactPath\(artifact\.file\), "updater"\)/);
  assert.match(source, /addSource\(resolveArtifactPath\(artifact\.signature\), "updater-signature"\)/);
  assert.match(source, /const source = resolveArtifactPath\(artifact\.file\);/);
  assert.match(source, /const signatureSource = resolveArtifactPath\(artifact\.signature\);/);
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
  // A broker-managed host also routes through the bridge even though cacheMode
  // is forced to "broker" there — its brokerManaged flag is what keeps a real
  // (non-symlink) target directory from being treated as corruption.
  assert.match(source, /if \(cacheMode === "shared" \|\| managedCargoTarget\) targetBridge\.ensure\(\)/);
});

test("a broker-managed host forces broker cacheMode so Cache V2's lease/prune/floor chain never runs against an unrelated volume", () => {
  assert.match(
    source,
    /const cacheMode = managedCargoTarget \? "broker" : cacheModeRequested;/,
  );
  // Every Cache V2 lease/slot/prune/floor/mark call stays gated on cacheMode,
  // never on cacheModeRequested directly, so forcing cacheMode to "broker" on a
  // broker host is sufficient to skip all of them without a second gate.
  for (const guarded of [
    /acquireSuiteBuildSlot\(\{ layout: sharedLayout/,
    /cacheLease = acquireCacheLease\(\{ layout: sharedLayout/,
    /planCachePrune\(\{ snapshot, policy, protectedEntryIds: new Set\(\[sharedLayout\.id\]\) \}\)/,
    /assertWriteVolumeFloors\(\{ volumes, policy,/,
    /markCacheEntrySuccessful\(\{ layout: sharedLayout \}\)/,
  ]) {
    assert.match(source, guarded);
  }
  assert.doesNotMatch(source, /if \(cacheModeRequested === "shared"\) \{\s*\n\s*suiteSlot/);
});

test("Windows & macOS seals and cache identity bind signing contracts, config, and receipts", () => {
  assert.match(source, /signingIdentity = platform === "win"/);
  assert.match(source, /platform === "mac"/);
  assert.match(source, /contract: target\.signingContract/);
  assert.match(source, /configSha256: hashFile\(configPath\)/);
  assert.match(source, /windows-\$\{phase\}\.json/);
  assert.match(source, /macos-signing\.json/);
  assert.match(source, /macos-notarization\.json/);
  assert.match(source, /sign-macos\.mjs/);
  assert.match(source, /notarize-macos\.mjs/);
  assert.match(source, /inputHashes\["\.right-release\/signing-identity\.json"\]/);
  assert.match(source, /cache: cacheIdentity\.fingerprint, signingIdentity/);
  // The seal binds every produced receipt and refuses to seal with none;
  // receipt paths honor the config's sign.receipt override, not only the
  // default windows-<phase>.json name.
  assert.match(source, /receiptInputs\.filter\(\(file\) => existsSync\(file\)\)/);
  assert.match(source, /no signing receipts found to seal/);
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

test("package liveness owner timeout floors outer watchdog through shared release limits", () => {
  assert.match(source, /livenessOwnerTimeoutMs: target\.package\.livenessOwnerTimeoutMs \?\? null/);
  assert.match(source, /releaseProgressLimits\(env, \{ livenessOwnerTimeoutMs \}\)/);
});

// Regression: the managed-host gate must not depend on RIGHTKIT_BUILD_BROKER_SOCKET
// alone. That variable is exported by login shells only, while the managed cargo
// shim is on PATH in every shell — so an agent shell took the Cache V2 branch,
// bridged src-tauri/target into the release cache, and then lost CARGO_TARGET_DIR
// to the broker's unconditional override. The app built into the broker workspace
// and packaging failed on an empty cache before notarize/staple.
const managedHostDetected = (env, platform, exists) => brokerManagedHost(env, platform, exists);

test("managed-host gate fires from the managed shim on PATH when no broker socket is exported", () => {
  // The build gate is exactly the shared brokerManagedHost predicate cargo-contract.mjs exports,
  // so there is exactly one definition of "is this a broker-managed host" in the codebase.
  assert.match(
    source,
    /const managedCargoTarget = brokerManagedHost\(process\.env\) \? resolveTargetRoot\(cargoToml\) : null;/,
  );
  assert.match(source, /import \{ brokerManagedHost \} from "\.\/cargo-contract\.mjs";/);
  assert.match(source, /import \{ resolveTargetRoot \} from "\.\/cargo-target\.mjs";/);

  const shim = "/Volumes/D/.rightkit-managed/agent-bin/cargo";
  const exists = (candidate) => candidate === shim;

  // Agent (non-login) shell: no socket variable, managed shim first on PATH.
  assert.equal(managedHostDetected({ PATH: `${path.dirname(shim)}:/usr/bin` }, "darwin", exists), true);
  // Login shell: unchanged behaviour, with or without the shim resolvable.
  assert.equal(
    managedHostDetected({ PATH: `${path.dirname(shim)}:/usr/bin`, RIGHTKIT_BUILD_BROKER_SOCKET: "/managed/broker.sock" }, "darwin", exists),
    true,
  );
  assert.equal(managedHostDetected({ PATH: "/usr/bin", RIGHTKIT_BUILD_BROKER_SOCKET: "/managed/broker.sock" }, "darwin", () => false), true);
  // Unmanaged host: neither signal, so the Cache V2 branch still owns the build.
  assert.equal(managedHostDetected({ PATH: "/usr/bin:/usr/local/bin" }, "darwin", () => false), false);
  // PATH precedence still decides: an unmanaged toolchain ahead of the shim is unmanaged.
  const both = new Set(["/usr/local/bin/cargo", shim]);
  assert.equal(managedHostDetected({ PATH: `/usr/local/bin:${path.dirname(shim)}` }, "darwin", (c) => both.has(c)), false);
});

test("brokerManagedHost is the single shared predicate: env var alone, shim alone, both, or neither", () => {
  const shim = "/Volumes/D/.rightkit-managed/agent-bin/cargo";
  const exists = (candidate) => candidate === shim;

  // Broker socket alone (login shell), no shim resolvable.
  assert.equal(brokerManagedHost({ RIGHTKIT_BUILD_BROKER_SOCKET: "/managed/broker.sock", PATH: "/usr/bin" }, "darwin", () => false), true);
  // Shim on PATH alone (agent/non-login shell), no socket exported.
  assert.equal(brokerManagedHost({ PATH: `${path.dirname(shim)}:/usr/bin` }, "darwin", exists), true);
  // Both signals present.
  assert.equal(
    brokerManagedHost({ RIGHTKIT_BUILD_BROKER_SOCKET: "/managed/broker.sock", PATH: `${path.dirname(shim)}:/usr/bin` }, "darwin", exists),
    true,
  );
  // Neither signal: unmanaged host.
  assert.equal(brokerManagedHost({ PATH: "/usr/bin:/usr/local/bin" }, "darwin", () => false), false);
});

test("the managed branch hands the Rust target directory to the broker while the unmanaged branch keeps Cache V2", () => {
  // Managed: CARGO_TARGET_DIR (and the rest of the Cache V2 environment) is deleted
  // from the child environment, so the broker's own override is the only one.
  assert.match(
    source,
    /if \(managedCargoTarget\) \{\n\s*for \(const name of \["CARGO_HOME", "CARGO_TARGET_DIR", "RUSTC_WRAPPER", "SCCACHE_DIR", "SCCACHE_BASEDIRS", "SCCACHE_CACHE_SIZE", "RUSTFLAGS"\]\) \{\n\s*delete env\[name\];/,
  );
  // Managed: no vault build root, and the bridge points at the metadata-reported target.
  assert.match(source, /const buildRoot = managedCargoTarget \? null :/);
  assert.match(source, /target: managedCargoTarget \?\? env\.CARGO_TARGET_DIR,/);
  assert.match(source, /ownedRoot: path\.dirname\(managedCargoTarget \?\? env\.CARGO_TARGET_DIR\),/);
  // The managed target is read from crate metadata through the shim, which is what
  // makes the bridge target equal to the directory the broker actually builds into.
  // resolveTargetRoot (cargo-target.mjs) is the single shared implementation —
  // build-release.mjs no longer defines its own copy.
  assert.doesNotMatch(source, /function resolveManagedCargoTarget/);
  // Unmanaged (managedCargoTarget === null): Cache V2 still supplies CARGO_TARGET_DIR.
  assert.match(source, /\.\.\.releaseEnvironment\(\{ root: vaultRoot, cacheRoot: sharedCacheRoot,/);
});
