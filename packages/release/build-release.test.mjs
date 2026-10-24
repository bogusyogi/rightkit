import "./test-support/broker-guard.mjs";
import { tempRoot } from "./test-support/temp-roots.mjs";
import assert from "node:assert/strict";
import { createHash, generateKeyPairSync, sign as cryptoSign } from "node:crypto";
import * as fs from "node:fs";
import { existsSync, mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { execFileSync, spawnSync } from "./background-process.mjs";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { runInNewContext } from "node:vm";
import { parseCpuSeconds, processGroupCpuSeconds } from "./process-liveness.mjs";
import { brokerManagedHost } from "./cargo-contract.mjs";
import { ANDROID_SIGNING_CONTRACT, androidSealEntry, verifyAndroidReceipt } from "./android-release.mjs";
import { IOS_SIGNING_CONTRACT, iosSealEntry, verifyIosReceipt } from "./ios-package-recipe.mjs";
import { assertUpdaterSignature } from "./github-updater-release.mjs";

// LF whatever the checkout: the patterns below span lines.
const source = readFileSync(path.join(path.dirname(fileURLToPath(import.meta.url)), "build-release.mjs"), "utf8").replace(/\r\n/g, "\n");
const stateMachine = readFileSync(path.join(path.dirname(fileURLToPath(import.meta.url)), "release-state.mjs"), "utf8");
const build = path.join(path.dirname(fileURLToPath(import.meta.url)), "build-release.mjs");

function updaterSealFixture(t, algorithm = "Ed") {
  const root = tempRoot("right-updater-seal-");
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const bytes = Buffer.from("exact updater artifact bytes\0\r\n");
  const { publicKey, privateKey } = generateKeyPairSync("ed25519");
  const keyId = Buffer.from("0102030405060708", "hex");
  const publicKeyText = Buffer.concat([Buffer.from("Ed"), keyId, publicKey.export({ type: "spki", format: "der" }).subarray(-32)]).toString("base64");
  const signature = cryptoSign(null, algorithm === "ED" ? createHash("blake2b512").update(bytes).digest() : bytes, privateKey);
  const comment = "fixture trusted comment  ";
  const envelope = Buffer.from(`untrusted comment: fixture\n${Buffer.concat([Buffer.from(algorithm), keyId, signature]).toString("base64")}\ntrusted comment: ${comment}\n${cryptoSign(null, Buffer.concat([signature, Buffer.from(comment)]), privateKey).toString("base64")}\n`).toString("base64");
  fs.writeFileSync(path.join(root, "updater.bin"), bytes);
  fs.writeFileSync(path.join(root, "updater.sig"), envelope);
  const copies = [];
  const start = source.indexOf("function sealRelease(");
  const end = source.indexOf("\nfunction collectToolVersions", start);
  const seal = runInNewContext(`${source.slice(start, end)}\nsealRelease;`, {
    ...fs, path, process, assertUpdaterSignature,
    hashFile: (file) => createHash("sha256").update(fs.readFileSync(file)).digest("hex"),
    copyFileSync: (from, to) => { copies.push([from, to]); fs.copyFileSync(from, to); },
    writeJson: (file, value) => fs.writeFileSync(file, JSON.stringify(value)),
    fail: (message) => { throw new Error(message); },
    resolveArtifactDir: () => root, materializeArtifacts: () => {}, completeArtifactLease: () => {}, retainArtifacts: () => ({ removed: [] }),
    resolveDevtoolsAllowance: () => ({ mode: "denied" }), devtoolsEvidence: (value) => value,
  });
  const options = { configRoot: root, sealedDir: path.join(root, "sealed"), releaseId: "fixture", platform: "win", commit: "a".repeat(40),
    config: { app: "fixture", version: "1.0.0", distribution: { updaterPublicKey: publicKeyText } },
    target: { updater: { artifacts: [{ file: "updater.bin", signature: "updater.sig", key: "fixture/updates/current/updater.bin", platform: "windows-x86_64" }] } },
    inputHashes: {}, toolVersions: {}, cacheKey: "fixture" };
  return { root, bytes, options, seal, copies };
}

test("updater seal refuses absent, blank & non-string keys before copying or creating a seal", (t) => {
  for (const key of [undefined, "", " \t\n", 123]) {
    const fixture = updaterSealFixture(t);
    fixture.options.config.distribution.updaterPublicKey = key;
    assert.throws(() => fixture.seal(fixture.options), /updater public key is required/);
    assert.equal(fixture.copies.length, 0);
    assert.equal(fs.existsSync(fixture.options.sealedDir), false);
    assert.equal(fs.existsSync(`${fixture.options.sealedDir}.tmp-${process.pid}`), false);
  }
});

for (const algorithm of ["Ed", "ED"]) {
  test(`${algorithm} updater seals only cryptographically valid exact bytes`, (t) => {
    const fixture = updaterSealFixture(t, algorithm);
    fixture.seal(fixture.options);
    assert.deepEqual(fs.readFileSync(path.join(fixture.options.sealedDir, "updater.bin")), fixture.bytes);
    assert.ok(fs.existsSync(path.join(fixture.options.sealedDir, "release-manifest.json")));
    assert.equal(fixture.copies.length, 2);
  });
  test(`${algorithm} corrupted updater refuses seal before copying`, (t) => {
    const fixture = updaterSealFixture(t, algorithm);
    fs.writeFileSync(path.join(fixture.root, "updater.bin"), "corrupted artifact");
    assert.throws(() => fixture.seal(fixture.options), /updater signature rejected at seal/);
    assert.equal(fixture.copies.length, 0);
    assert.equal(fs.existsSync(fixture.options.sealedDir), false);
    assert.equal(fs.existsSync(`${fixture.options.sealedDir}.tmp-${process.pid}`), false);
  });
}

test("installer-only & appinstaller seals need no minisign public key", (t) => {
  for (const kind of ["installer-only", "appinstaller"]) {
    const fixture = updaterSealFixture(t);
    delete fixture.options.config.distribution.updaterPublicKey;
    fixture.options.target = kind === "installer-only"
      ? { installer: { artifacts: [{ file: "updater.bin", key: "fixture/installers/current/fixture.exe" }] } }
      : { updater: { kind: "appinstaller", artifacts: [{ file: "updater.bin", key: "fixture/updates/current/fixture.appinstaller" }] } };
    fixture.seal(fixture.options);
    assert.ok(fs.existsSync(path.join(fixture.options.sealedDir, "release-manifest.json")));
  }
});

// Execute the production preflight guard without starting the CLI's build flow.
function checkOwnershipCommit(evidence, commit) {
  const start = source.indexOf('  const ownershipEvidence = preflight.find(');
  const end = source.indexOf('  for (const check of preflight)', start);
  assert.ok(start >= 0 && end > start, "ownership commit guard must exist before preflight diagnostics");
  runInNewContext(source.slice(start, end), {
    preflight: [{ name: "ownership", evidence }],
    commit,
    fail: (message) => { throw new Error(message); },
  });
}

for (const evidenceSource of ["enrollment-pending-advisory", "committed-git-tree"]) {
  test(`build rejects ${evidenceSource} evidence from a different commit`, () => {
    const scanCommit = "a".repeat(40);
    const releaseCommit = "b".repeat(40);
    assert.throws(() => checkOwnershipCommit({ source: evidenceSource, commit: scanCommit }, releaseCommit), (error) => {
      assert.ok(error.message.includes(`ownership scan commit ${scanCommit}`));
      assert.ok(error.message.includes(`release commit ${releaseCommit}`));
      assert.ok(error.message.includes(`right-release ownership at release commit ${releaseCommit}`));
      return true;
    });
  });

  test(`build accepts ${evidenceSource} evidence from the release commit`, () => {
    const commit = "a".repeat(40);
    assert.doesNotThrow(() => checkOwnershipCommit({ source: evidenceSource, commit }, commit));
  });

  test(`build rejects ${evidenceSource} evidence without a commit`, () => {
    assert.throws(() => checkOwnershipCommit({ source: evidenceSource }, "b".repeat(40)), /ownership scan commit \(missing\).*right-release ownership/);
  });
}

test("general build dry-run is filesystem-pure and never starts its package command", () => {
  const root = tempRoot("right-build-dry-run-");
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
  assert.match(source, /\[WORKER, "--config", configPath, "--platform", platform, "--no-upload", "--resume"/);
  assert.match(source, /sealRelease\(\{ configRoot: appRoot,/);
});

test("product preSeal checks run after hardening and before immutable sealing", () => {
  const hardening = source.indexOf('checkpoint(stateRoot, "hardened")');
  const preSeal = source.indexOf('preSeal: async ({ sealedDir })');
  const seal = source.indexOf('seal: async ({ sealedDir })');
  assert.ok(hardening >= 0 && preSeal > hardening, "preSeal must follow hardening");
  assert.ok(seal > preSeal, "preSeal must precede seal");
  assert.match(source, /RIGHT_RELEASE_SEALED_DIR: sealedDir/);
  assert.match(source, /runConfiguredReleaseCheck\(target\.preSeal/);
  assert.match(stateMachine, /await ops\.preSeal\?\.\(\{ root, app, version, commit, platform, releaseId, sealedDir \}\)/);
});

test("hardening checkpoint and seal status derive from the worker scan targets, never asserted unconditionally", () => {
  assert.match(source, /function hardeningScanTargets\(target\) \{\n  return target\?\.hardening \?\? target\?\.artifacts \?\? \[\];\n\}/);
  assert.match(source, /if \(hardeningScanTargets\(target\)\.length > 0\) checkpoint\(stateRoot, "hardened"\)/);
  assert.match(source, /status: hardeningScanned \? "passed" : "skipped", passed: hardeningScanned/);
  assert.match(source, /\.\.\.\(hardeningScanned \? \["hardened"\] : \[\]\)/);
  assert.doesNotMatch(source, /hardening\.json"\), \{ schema: 1, passed: true/);
  // Execute the production helper: no targets means no scan ran in the worker.
  const start = source.indexOf("function hardeningScanTargets(target)");
  const end = source.indexOf("\n}\n", start) + 3;
  assert.ok(start >= 0 && end > start);
  const hardeningScanTargets = runInNewContext(`${source.slice(start, end)}; hardeningScanTargets`, {});
  assert.deepEqual([...hardeningScanTargets({})], []);
  assert.deepEqual([...hardeningScanTargets({ artifacts: ["a"] })], ["a"]);
  assert.deepEqual([...hardeningScanTargets({ hardening: [], artifacts: ["a"] })], [], "an explicit empty hardening list means the worker scans nothing");
});

test("seal verifies updater signatures over exact bytes with a required declared public key", () => {
  assert.match(source, /import \{ assertUpdaterSignature \} from "\.\/github-updater-release\.mjs"/);
  assert.match(source, /config\.distribution\?\.updaterPublicKey \?\? config\.distribution\?\.updaterPubkey \?\? target\?\.updater\?\.pubkey \?\? target\?\.updater\?\.publicKey/);
  assert.match(source, /assertUpdaterSignature\(readFileSync\(source\), signature, updaterPublicKey/);
  assert.match(source, /updater signature rejected at seal/);
  const sealGate = source.indexOf("updater signature rejected at seal");
  const firstCopy = source.indexOf("copyFileSync(source, path.join(temp, name))");
  assert.ok(sealGate >= 0 && firstCopy > sealGate, "signature gate must run before any artifact is copied into the seal");
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

test("build exposes explicit macOS phase resume without rebuilding earlier phases", () => {
  assert.match(source, /--resume-from/);
  assert.match(source, /RIGHT_RELEASE_STATE_ROOT/);
  assert.match(source, /\[WORKER, "--config", configPath, "--platform", platform, "--no-upload", "--resume"/);
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
  // Portable contracts declare bare archives under target.artifacts; the seal must read them.
  assert.match(source, /for \(const artifact of target\.artifacts \?\? \[\]\)/);
  assert.match(source, /addSource\(source, source === aabSource \? "aab" : source === ipaSource \? "ipa" : "portable"\)/);
  assert.match(source, /nothing to seal: declare installer\.artifacts, updater\.artifacts or artifacts/);
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
  assert.match(source, /const signatureSource = target\.updater\?\.kind === \"appinstaller\" \? null : resolveArtifactPath\(artifact\.signature\);/);
});

test("production builds never create or use an internal Git worktree", () => {
  assert.doesNotMatch(source, /git", \["worktree", "add"/);
  assert.doesNotMatch(source, /path\.join\(vaultRoot, "worktrees"/);
  assert.doesNotMatch(source, /worktreeConfigPath/);
});

test("dirty release config is rejected before the config module is imported", () => {
  const guard = source.indexOf("dirtyBuildInputs(repoRoot, { include: [relativeConfig], required: [relativeConfig] }, commit)");
  const imported = source.indexOf("await importReleaseConfig(");
  assert.ok(guard >= 0, "config cleanliness guard must exist");
  assert.ok(imported >= 0, "the release config must load through importReleaseConfig");
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
  assert.match(source, /signingReceiptPath\(\{ appRoot, target, platform, phase, stateRoot \}\)/);
  assert.match(source, /\["signing", "notarization"\]/);
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
  assert.match(source, /livenessOwnerTimeoutMs: target\.package\?\.livenessOwnerTimeoutMs \?\? null/);
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
    /const managedCargoTarget = swiftPlan \? swiftPlan\.managedCargoTarget : brokerManagedHost\(process\.env\) \? resolveTargetRoot\(cargoToml\) : null;/,
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

// ---- android: the AAB is sealed from its verified build receipt ------------------------------------------------------
// Execute the production android pre-seal block (receipt present, bytes re-hashed, signer pinned) without the CLI flow.
function androidPreSeal({ sources, aabSource, signingIdentity }) {
  const start = source.indexOf("  let androidReceipt = null;");
  // The block ends where the media-sidecar block begins (that one needs the target, state root and sidecar validators, none of which android uses).
  const end = source.indexOf("  // rightkit-ffmpeg decode sidecar", start);
  assert.ok(start >= 0 && end > start, "android pre-seal receipt block must exist before the media-sidecar block");
  assert.ok(source.indexOf("  const temp = `${sealedDir}.tmp-", end) > end, "the temp seal directory follows the pre-seal blocks");
  return runInNewContext(`${source.slice(start, end)}\nandroidReceipt;`, {
    platform: "android",
    sources,
    aabSource,
    signingIdentity,
    existsSync,
    verifyAndroidReceipt,
    fail: (message) => { throw new Error(message); },
  });
}

function androidFixture() {
  const dir = tempRoot("right-build-android-");
  const aabFile = path.join(dir, "app-release.aab");
  const bytes = Buffer.from("PK fake aab bytes for sealing");
  writeFileSync(aabFile, bytes);
  const certSha256 = "ab".repeat(32);
  const receiptPath = path.join(dir, "android-aab.json");
  const receipt = {
    schema: 1,
    contract: ANDROID_SIGNING_CONTRACT,
    app: "fixture",
    file: { name: "app-release.aab", sha256: createHash("sha256").update(bytes).digest("hex"), sizeBytes: bytes.length },
    signer: { alias: "upload", block: "META-INF/UPLOAD.RSA", certSha256, subject: "CN=Upload" },
  };
  writeFileSync(receiptPath, JSON.stringify(receipt));
  const signingIdentity = { contract: ANDROID_SIGNING_CONTRACT, uploadCertSha256: certSha256, receiptInputs: [receiptPath] };
  return { dir, aabFile, aabSource: aabFile, receiptPath, receipt, signingIdentity, sources: new Map([[aabFile, new Set(["aab"])]]) };
}

test("build accepts --platform android and keys its identity, state dir and receipt to the Android lane", () => {
  assert.match(source, /platform === "android" \? \{\s*contract: target\.signingContract \?\? "<missing>",\s*pipeline: ANDROID_PIPELINE_FINGERPRINT,/);
  assert.match(source, /uploadCertSha256: androidTarget\.signing\.expectedCertSha256/);
  assert.match(source, /receiptInputs: \[path\.resolve\(stateRoot, ANDROID_RECEIPT_NAME\)\]/);
  // The sealed entry is the receipt's, and the receipt is a sealed signing receipt like win/mac ones.
  assert.match(source, /androidSealEntry\(androidReceipt\)/);
  assert.match(source, /present\.map\(\(file\) => \(\{ file, sha256: hashFile\(file\) \}\)\)/);
});

test("android seal verifies the receipt against the AAB bytes and returns it for the files entry", () => {
  const fx = androidFixture();
  const receipt = androidPreSeal(fx);
  assert.deepEqual(androidSealEntry(receipt), { role: "aab", name: "app-release.aab", sha256: fx.receipt.file.sha256, sizeBytes: fx.receipt.file.sizeBytes });
});

test("android seal fails on a missing receipt, changed AAB bytes, another file name, or an unpinned signer", () => {
  let fx = androidFixture();
  rmSync(fx.receiptPath);
  assert.throws(() => androidPreSeal(fx), /no signing receipts found to seal; expected at least .*android-aab\.json/);

  fx = androidFixture();
  writeFileSync(fx.aabFile, "PK different bytes, same length!!");
  assert.throws(() => androidPreSeal(fx), /android AAB receipt rejected: AAB bytes differ from the build receipt/);

  fx = androidFixture();
  const other = path.join(fx.dir, "other.aab");
  writeFileSync(other, readFileSync(fx.aabFile));
  assert.throws(() => androidPreSeal({ ...fx, aabSource: other, sources: new Map([[other, new Set(["aab"])]]) }), /receipt names a different file/);

  fx = androidFixture();
  assert.throws(() => androidPreSeal({ ...fx, signingIdentity: { ...fx.signingIdentity, uploadCertSha256: "cd".repeat(32) } }), /not the pinned upload certificate/);

  fx = androidFixture();
  assert.throws(() => androidPreSeal({ ...fx, sources: new Map() }), /requires targets\.android\.artifacts to list aab\.file/);
});

// ---- ios: the IPA is sealed from its verified build receipt, for the team and bundle id the release config pins ----------------
function iosPreSeal({ sources, ipaSource, signingIdentity, commit }) {
  const start = source.indexOf("  let iosReceipt = null;");
  const end = source.indexOf("  // rightkit-ffmpeg decode sidecar", start);
  assert.ok(start >= 0 && end > start, "ios pre-seal receipt block must exist before the media-sidecar block");
  return runInNewContext(`${source.slice(start, end)}\niosReceipt;`, {
    platform: "ios",
    sources,
    ipaSource,
    signingIdentity,
    commit: commit ?? "c".repeat(40),
    existsSync,
    verifyIosReceipt,
    fail: (message) => { throw new Error(message); },
  });
}

function iosFixture() {
  const dir = tempRoot("right-build-ios-");
  const ipaFile = path.join(dir, "Pulse.ipa");
  const bytes = Buffer.from("PK fake ipa bytes for sealing");
  writeFileSync(ipaFile, bytes);
  const receiptPath = path.join(dir, "ios-archive.json");
  const receipt = {
    schema: 1,
    contract: IOS_SIGNING_CONTRACT,
    app: "fixture",
    teamId: "6KLGD3LLKF",
    bundleId: "dev.orthic.pulse.ios",
    sourceCommit: "c".repeat(40),
    file: { name: "Pulse.ipa", sha256: createHash("sha256").update(bytes).digest("hex"), sizeBytes: bytes.length },
  };
  writeFileSync(receiptPath, JSON.stringify(receipt));
  const signingIdentity = { contract: IOS_SIGNING_CONTRACT, teamId: "6KLGD3LLKF", bundleId: "dev.orthic.pulse.ios", receiptInputs: [receiptPath] };
  return { dir, ipaFile, ipaSource: ipaFile, receiptPath, receipt, signingIdentity, sources: new Map([[ipaFile, new Set(["ipa"])]]) };
}

test("build accepts --platform ios and keys its identity, state dir and receipt to the iOS lane", () => {
  assert.match(source, /platform === "ios" \? \{\s*contract: "ios-xcodebuild-archive-automatic-v1",\s*pipeline: IOS_PIPELINE_FINGERPRINT,/);
  assert.match(source, /teamId: iosSpec\.teamId,\s*bundleId: iosSpec\.bundleId,/);
  assert.match(source, /receiptInputs: \[path\.resolve\(stateRoot, IOS_RECEIPT_NAME\)\]/);
  assert.match(source, /iosSealEntry\(iosReceipt\)/);
  assert.match(source, /expandIosPackageTarget\(target, \{ appRoot \}\)/);
});

test("ios seal verifies the receipt against the IPA bytes and returns it for the files entry", () => {
  const fx = iosFixture();
  const receipt = iosPreSeal(fx);
  assert.deepEqual(iosSealEntry(receipt), { role: "ipa", name: "Pulse.ipa", sha256: fx.receipt.file.sha256, sizeBytes: fx.receipt.file.sizeBytes });
});

test("ios seal fails on a missing receipt, changed IPA bytes, another file name, another team or bundle id, or an unlisted IPA", () => {
  let fx = iosFixture();
  rmSync(fx.receiptPath);
  assert.throws(() => iosPreSeal(fx), /no signing receipts found to seal; expected at least .*ios-archive\.json/);

  fx = iosFixture();
  writeFileSync(fx.ipaFile, "PK different bytes, same length!!");
  assert.throws(() => iosPreSeal(fx), /ios IPA receipt rejected: IPA bytes differ from the build receipt/);

  fx = iosFixture();
  const other = path.join(fx.dir, "other.ipa");
  writeFileSync(other, readFileSync(fx.ipaFile));
  assert.throws(() => iosPreSeal({ ...fx, ipaSource: other, sources: new Map([[other, new Set(["ipa"])]]) }), /receipt names a different file/);

  fx = iosFixture();
  assert.throws(() => iosPreSeal({ ...fx, signingIdentity: { ...fx.signingIdentity, teamId: "OTHERTEAM1" } }), /team or bundle id is not the one pinned/);
  assert.throws(() => iosPreSeal({ ...fx, signingIdentity: { ...fx.signingIdentity, bundleId: "dev.other.app" } }), /team or bundle id is not the one pinned/);

  fx = iosFixture();
  assert.throws(() => iosPreSeal({ ...fx, commit: "d".repeat(40) }), /was built from c+, not the release commit d+/);

  fx = iosFixture();
  assert.throws(() => iosPreSeal({ ...fx, sources: new Map() }), /requires targets\.ios\.artifacts to list iosPackage\.ipa/);
});

// ---- behaviour: which platforms build accepts, and which need a package command ------------------------------------------
function dryRunIn(platform, targets) {
  const root = tempRoot("right-build-platforms-");
  writeFileSync(path.join(root, "right-release.config.mjs"), `export default { app: 'fixture', version: '1.0.0', targets: ${JSON.stringify(targets)} };\n`);
  for (const args of [["init", "--initial-branch", "main"], ["config", "user.email", "test@example.com"], ["config", "user.name", "Test"], ["add", "."], ["commit", "-m", "init"]]) execFileSync("git", args, { cwd: root });
  return spawnSync(process.execPath, [build, "--config", "right-release.config.mjs", "--platform", platform, "--dry-run"], { cwd: root, encoding: "utf8" });
}

test("build --dry-run accepts android and ios without a package command, and still demands one for mac and win", () => {
  for (const platform of ["android", "ios"]) {
    const result = dryRunIn(platform, { [platform]: { signed: true } });
    assert.equal(result.status, 0, result.stderr);
    assert.match(result.stdout, new RegExp(`build dry-run: fixture 1\\.0\\.0 ${platform}`));
  }
  for (const platform of ["mac", "win"]) {
    const result = dryRunIn(platform, { [platform]: { signed: true } });
    assert.notEqual(result.status, 0);
    assert.match(result.stderr, new RegExp(`has no ${platform} package command`));
  }
});

test("build refuses a platform it does not know, and names the four it does", () => {
  const result = dryRunIn("watchos", { watchos: {} });
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /--platform must be win, mac, android or ios/);
});
