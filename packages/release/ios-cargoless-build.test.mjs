import "./test-support/broker-guard.mjs";
import { tempRoot } from "./test-support/temp-roots.mjs";
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import nodeFs, { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { execFileSync, spawnSync } from "./background-process.mjs";
import { collectIosNativeToolVersions, collectIosXcodeToolVersions, iosPrepareExecutables, planIosNativeBuild, releaseSwitchInputs, resolveIosBuildContext, stripRustEnvironment } from "./ios-build-context.mjs";
import { IOS_RECEIPT_NAME, IOS_SIGNING_CONTRACT, runIosPackageRecipe, validateIosPackageConfig, verifyIosReceipt, iosSealEntry } from "./ios-package-recipe.mjs";
import { releaseEnvironment, verifySealedRelease } from "./release-state.mjs";
import { verifyPublicationRelease } from "./publication-artifacts.mjs";
import { beginArtifactLease, resolveArtifactDir, materializeArtifacts, completeArtifactLease, retainArtifacts } from "./artifact-storage.mjs";
import { devtoolsEvidence, resolveDevtoolsAllowance } from "./devtools-policy.mjs";
import { MEDIA_SIDECAR_RECEIPT_NAME, validateMediaSidecarConfig, validateMediaSidecarReceipt, windowsSidecarSigningProblems } from "./media-sidecar.mjs";
import { validateNotarizationReceipt } from "./macos-notarization.mjs";
import { androidSealEntry, verifyAndroidReceipt } from "./android-release.mjs";
import { assertUpdaterSignature } from "./github-updater-release.mjs";
import { runInNewContext } from "node:vm";
import { featureClosureFindings, resolveFeatureClosure } from "./release-switch-scan.mjs";
import { assertNoForbiddenBuildFeatures, resolveCargoManifestPath } from "./qa-build-features.mjs";
import { nativeCargoDeclared, resolveNativeCargoLayout } from "./native-cargo-layout.mjs";
import { collectPreflight } from "./preflight.mjs";
import { requiredBuildExecutables } from "./project-type.mjs";
import { dirtyBuildInputs, isNativeIosTarget, resolveReleaseBuildInputs } from "./release-invocation.mjs";

// Native iOS app (CodeRight shape): right-release.config.mjs, project.yml, source dirs; no src-tauri, no package.json.
const here = path.dirname(fileURLToPath(import.meta.url));
import { admittedWorkerEnv, registryEnv } from "./test-support/admission.mjs";
import { registryManifestFields, resolveRightKitPins, rightKitLockInventory, snapshotDigest } from "./rightkit-registry-resolver.mjs";
const baseEnv = { ...process.env, RIGHT_RELEASE_MIN_FREE_GB: "1" };
// The registries are answered offline from the fixture's locks; a worker run carries the state the controller leaves after admission.
const doctorEnv = (app) => ({ ...baseEnv, ...registryEnv(app.root) });
const workerEnv = (app) => ({ ...baseEnv, ...registryEnv(app.root), ...admittedWorkerEnv(app.configPath, "ios") });
const emptyAdmission = () => { const snapshot = { schema: 1, mode: "admission", observedAt: "2026-10-23T00:00:00.000Z", registries: {}, policyRevision: "test", lockFiles: [], entries: [], latest: [] }; snapshot.digest = snapshotDigest(snapshot); return registryManifestFields(snapshot); };
const buildScript = path.join(here, "build-release.mjs");
const releaseScript = path.join(here, "release.mjs");
const profileTemplate = readFileSync(path.join(here, "templates", "cargo-profiles.toml"), "utf8");

const iosPackage = (extra = {}) => ({
  project: "CodeRight.xcodeproj", projectSpec: "project.yml", scheme: "CodeRight", teamId: "6KLGD3LLKF", bundleId: "dev.orthic.coderight",
  archive: "build/CodeRight.xcarchive", ipa: "build/CodeRight.ipa", ...extra,
});

function git(cwd, ...args) {
  return execFileSync("git", args, { cwd, encoding: "utf8", stdio: ["ignore", "pipe", "pipe"] }).trim();
}

function iosApp({ srcTauri = false, packageJson = false, lock = false, projectSpec = true, nativeAssembly, iosExtra, commit = true } = {}) {
  const root = nodeFs.realpathSync.native(tempRoot("right-ios-cargoless-"));
  const pkg = iosPackage({ ...(projectSpec ? {} : { projectSpec: undefined }), ...(iosExtra ?? {}) });
  const config = {
    schema: 1, app: "coderight", version: "1.0.0",
    // targets.ios refuses a buildInputs key (IOS_TARGET_KEYS), so an iOS app declares them at the top level.
    buildInputs: { include: ["project.yml", "Sources/**"] },
    ...(nativeAssembly ? { nativeAssembly } : {}),
    targets: { ios: { signed: true, iosPackage: pkg, artifacts: ["build/CodeRight.ipa"] } },
  };
  writeFileSync(path.join(root, "right-release.config.mjs"), `export default ${JSON.stringify(config)};\n`);
  // The mandatory QA lane (qa-contract.mjs) is satisfied by right-qa.config.mjs; the iOS-specific Tauri backdoor contract is skipped without a package.json.
  writeFileSync(path.join(root, "right-qa.config.mjs"), "export default {};\n");
  if (projectSpec) writeFileSync(path.join(root, "project.yml"), "name: CodeRight\n");
  mkdirSync(path.join(root, "Sources"), { recursive: true });
  writeFileSync(path.join(root, "Sources", "main.swift"), "print(1)\n");
  if (packageJson) writeFileSync(path.join(root, "package.json"), JSON.stringify({ name: "coderight" }));
  if (lock) writeFileSync(path.join(root, "pnpm-lock.yaml"), "lockfileVersion: '9.0'\n");
  if (srcTauri) {
    mkdirSync(path.join(root, "src-tauri"), { recursive: true });
    writeFileSync(path.join(root, "src-tauri", "Cargo.toml"), `[package]\nname = "x"\nversion = "0.0.0"\nedition = "2021"\n[lib]\npath = "lib.rs"\n${profileTemplate}`);
    writeFileSync(path.join(root, "src-tauri", "Cargo.lock"), "# lock\n");
    writeFileSync(path.join(root, "src-tauri", "lib.rs"), "");
  }
  if (commit) {
    git(root, "init", "--initial-branch", "main");
    git(root, "config", "user.email", "t@example.com");
    git(root, "config", "user.name", "T");
    git(root, "add", ".");
    git(root, "commit", "-m", "init");
  }
  return { root, configPath: path.join(root, "right-release.config.mjs"), config };
}

const inputsOf = (app, target = app.config.targets.ios) => resolveReleaseBuildInputs({ repoRoot: app.root, appRoot: app.root, configPath: app.configPath, config: app.config, target });
const rel = (app, files) => files.map((file) => path.relative(app.root, file).replaceAll("\\", "/"));

test("a native iOS app with no src-tauri and no package.json resolves exactly its config and project.yml", () => {
  const app = iosApp();
  const { requiredInputs, nativeLayout, buildInputs } = inputsOf(app);
  assert.deepEqual(rel(app, requiredInputs), ["right-release.config.mjs", "project.yml"]);
  assert.equal(nativeLayout, null);
  assert.deepEqual(buildInputs.required, ["right-release.config.mjs", "project.yml"]);
  assert.equal(isNativeIosTarget({ appRoot: app.root, config: app.config, target: app.config.targets.ios }), true);
});

test("the cargoless iOS target is recognised from the validated recipe too (the shape the doctor and worker pass)", () => {
  const app = iosApp();
  const recipeTarget = { signed: true, recipe: { kind: "iosPackage", spec: { ...iosPackage() } } };
  assert.deepEqual(rel(app, inputsOf(app, recipeTarget).requiredInputs), ["right-release.config.mjs", "project.yml"]);
});

test("package.json and pnpm-lock.yaml are inputs the moment they exist, and never required when they do not", () => {
  const withBoth = iosApp({ packageJson: true, lock: true });
  assert.deepEqual(rel(withBoth, inputsOf(withBoth).requiredInputs), ["right-release.config.mjs", "package.json", "pnpm-lock.yaml", "project.yml"]);
  const onlyPackage = iosApp({ packageJson: true });
  assert.deepEqual(rel(onlyPackage, inputsOf(onlyPackage).requiredInputs), ["right-release.config.mjs", "package.json", "project.yml"]);
  const onlyLock = iosApp({ lock: true });
  assert.deepEqual(rel(onlyLock, inputsOf(onlyLock).requiredInputs), ["right-release.config.mjs", "pnpm-lock.yaml", "project.yml"]);
});

test("a declared projectSpec that does not exist is refused, and no projectSpec adds no input", () => {
  const app = iosApp();
  const missing = { ...app.config.targets.ios, iosPackage: iosPackage({ projectSpec: "other.yml", project: "Other.xcodeproj" }) };
  assert.throws(() => inputsOf(app, missing), /missing required release input: .*other\.yml/);
  const none = iosApp({ projectSpec: false });
  assert.deepEqual(rel(none, inputsOf(none).requiredInputs), ["right-release.config.mjs"]);
});

test("a declared nativeAssembly manifest keeps native iOS mode: its Cargo.toml and lock become inputs, package.json stays optional, a missing lock is refused", () => {
  const app = iosApp({ nativeAssembly: { cargoManifest: "engine/Cargo.toml" } });
  mkdirSync(path.join(app.root, "engine"), { recursive: true });
  assert.equal(nativeCargoDeclared({ appRoot: app.root, config: app.config }), true);
  assert.equal(isNativeIosTarget({ appRoot: app.root, target: app.config.targets.ios }), true);
  assert.throws(() => inputsOf(app), /native Cargo manifest missing/);
  writeFileSync(path.join(app.root, "engine", "Cargo.toml"), "[package]\nname = \"engine\"\nversion = \"0.0.0\"\n");
  assert.throws(() => inputsOf(app), /native Cargo\.lock missing/);
  writeFileSync(path.join(app.root, "engine", "Cargo.lock"), "# lock\n");
  const { requiredInputs, nativeLayout } = inputsOf(app);
  assert.equal(nativeLayout, null, "native iOS mode owns no Cargo layout, so nothing links a broker target");
  assert.deepEqual(rel(app, requiredInputs), ["right-release.config.mjs", "engine/Cargo.toml", "engine/Cargo.lock", "project.yml"]);
  writeFileSync(path.join(app.root, "package.json"), "{}");
  assert.deepEqual(rel(app, inputsOf(app).requiredInputs), ["right-release.config.mjs", "package.json", "engine/Cargo.toml", "engine/Cargo.lock", "project.yml"]);
});

test("an iOS app with src-tauri keeps today's inputs exactly: config, package.json, pnpm-lock.yaml, Cargo.toml and Cargo.lock", () => {
  const app = iosApp({ srcTauri: true, packageJson: true, lock: true, projectSpec: false });
  const ios = inputsOf(app);
  assert.deepEqual(rel(app, ios.requiredInputs), ["right-release.config.mjs", "package.json", "pnpm-lock.yaml", "src-tauri/Cargo.toml", "src-tauri/Cargo.lock"]);
  assert.ok(ios.nativeLayout);
  // The same app resolved as a desktop target yields the identical list (the unchanged baseline).
  const desktop = inputsOf(app, { signed: true });
  assert.deepEqual(ios.requiredInputs, desktop.requiredInputs);
  assert.equal(isNativeIosTarget({ appRoot: app.root, config: app.config, target: app.config.targets.ios }), false);
});

test("an iOS app with src-tauri keeps today's inputs exactly even when it names a projectSpec (the pre-change set, compared with the same target as a desktop one)", () => {
  const app = iosApp({ srcTauri: true, packageJson: true, lock: true });
  const ios = inputsOf(app);
  const preChange = inputsOf(app, { signed: true });
  assert.deepEqual(ios.requiredInputs, preChange.requiredInputs);
  assert.deepEqual(rel(app, ios.requiredInputs), ["right-release.config.mjs", "package.json", "pnpm-lock.yaml", "src-tauri/Cargo.toml", "src-tauri/Cargo.lock"]);
  assert.deepEqual(ios.cargoLockPaths, []);
  assert.ok(ios.nativeLayout);
});

test("a src-tauri iOS app still refuses a missing package.json, never silently dropping an input", () => {
  const app = iosApp({ srcTauri: true, lock: true });
  assert.throws(() => inputsOf(app), /missing required release input: .*package\.json/);
});

test("a desktop target with no src-tauri still fails on the missing Cargo manifest", () => {
  const app = iosApp();
  assert.throws(() => inputsOf(app, { signed: true }), /native Cargo manifest missing/);
});

test("build --dry-run for the cargoless iOS app succeeds without touching the tree", () => {
  const app = iosApp();
  const result = spawnSync(process.execPath, [buildScript, "--config", "right-release.config.mjs", "--platform", "ios", "--dry-run"], { cwd: app.root, encoding: "utf8" });
  assert.equal(result.status, 0, `${result.stdout}\n${result.stderr}`);
  assert.match(result.stdout, /build dry-run: coderight 1\.0\.0 ios/);
});

test("the worker dry-run plans the iOS recipe for the cargoless app (no package.json, no Cargo, no Tauri)", () => {
  const app = iosApp();
  const result = spawnSync(process.execPath, [releaseScript, "--config", app.configPath, "--platform", "ios", "--dry-run"], {
    cwd: app.root, encoding: "utf8", env: workerEnv(app),
  });
  const output = `${result.stdout}\n${result.stderr}`;
  assert.doesNotMatch(output, /ENOENT|native Cargo manifest missing|package\.json/, output);
  assert.equal(result.status, 0, output);
  assert.match(output, /xcodegen/);
});

test("xcodebuild and xcodegen versions are recorded as one-line strings; xcodegen only when a project.yml is named", () => {
  const exec = (cmd) => ({ xcodebuild: "Xcode 16.2\nBuild version 16C5032a\n", xcodegen: "Version: 2.44.1\n", pnpm: "10.1.0\n" })[cmd];
  assert.deepEqual(collectIosXcodeToolVersions({ exec, projectSpec: "project.yml" }), { xcodebuild: "Xcode 16.2 Build version 16C5032a", xcodegen: "Version: 2.44.1" });
  assert.deepEqual(collectIosXcodeToolVersions({ exec, projectSpec: null }), { xcodebuild: "Xcode 16.2 Build version 16C5032a", xcodegen: null });
  const tools = collectIosNativeToolVersions({ packageManager: null, exec, projectSpec: "project.yml", nodeVersion: "v22.0.0" });
  assert.deepEqual(tools, { node: "v22.0.0", packageManager: null, xcodebuild: "Xcode 16.2 Build version 16C5032a", xcodegen: "Version: 2.44.1", cargo: null, rustc: null, rustHost: null, sccache: null });
  assert.equal(collectIosNativeToolVersions({ packageManager: "pnpm", exec, projectSpec: null }).packageManager, "pnpm 10.1.0");
  assert.throws(() => collectIosXcodeToolVersions({ exec: () => "", projectSpec: null }), /xcodebuild -version printed nothing/);
});

test("the native iOS plan keys cache identity to the Xcode toolchain, runs legacy mode, and strips the release lane's Rust variables", () => {
  const tools = { xcodebuild: "Xcode 16.2", xcodegen: "Version: 2.44.1" };
  const plan = planIosNativeBuild({ appRoot: path.resolve("/app"), vaultRoot: path.resolve("/vault"), toolVersions: tools, hostArch: "arm64", hostEnv: { HOME: "keep" } });
  assert.equal(plan.cacheMode, "legacy");
  assert.equal(plan.managedCargoTarget, null);
  assert.equal(plan.architecture, "arm64");
  assert.match(plan.identity.fingerprint, /^[0-9a-f]{16}$/);
  assert.notEqual(plan.identity.fingerprint, planIosNativeBuild({ appRoot: path.resolve("/app"), vaultRoot: path.resolve("/vault"), toolVersions: { ...tools, xcodebuild: "Xcode 16.3" }, hostArch: "arm64" }).identity.fingerprint);
  assert.equal(plan.targetBridge.run(() => 7), 7);
  const lane = { CARGO_HOME: "lane", CARGO_TARGET_DIR: "x", RUSTC_WRAPPER: "x", RUSTFLAGS: "x", SCCACHE_DIR: "x", RIGHT_RELEASE_CACHE_OWNER: "x", HOME: "keep" };
  assert.deepEqual(plan.sanitizeEnv(lane), { HOME: "keep" });
  assert.throws(() => planIosNativeBuild({ appRoot: "/a", vaultRoot: "/v", toolVersions: {} }), /xcodebuild version/);
});

test("required executables for a cargoless iOS app are git and (only with a package.json) the package manager; others keep cargo and rustc", () => {
  assert.deepEqual(requiredBuildExecutables({ projectType: "tauri", packageManager: "pnpm", usesPackageManager: false, usesCargo: false }), ["git"]);
  assert.deepEqual(requiredBuildExecutables({ projectType: "tauri", packageManager: "pnpm", usesPackageManager: true, usesCargo: false }), ["git", "pnpm"]);
  assert.deepEqual(requiredBuildExecutables({ projectType: "tauri", packageManager: "pnpm" }), ["git", "pnpm", "cargo", "rustc"]);
});

test("preflight skips the target bridge, compiler cache and Rust budget adoption when there is no Rust host", () => {
  const base = { platform: "ios", appRoot: tempRoot("right-ios-preflight-"), env: { PATH: "" }, ownership: false, versionProbe: () => null, isBrokerManagedHost: () => false, freeDisk: () => 500, runtimeBudgetAdoption: () => { throw new Error("must not run"); } };
  const names = (checks) => checks.map((check) => check.name);
  const without = collectPreflight({ ...base, rust: false });
  assert.deepEqual(names(without), ["disk"]);
  const withRust = collectPreflight({ ...base, runtimeBudgetAdoption: () => null });
  assert.deepEqual(names(withRust), ["target-bridge", "sccache", "disk"]);
});

test("native layout presence: a declared manifest or src-tauri/Cargo.toml, nothing else", () => {
  const exists = (set) => (file) => set.has(file.replaceAll("\\", "/"));
  const root = path.resolve("/app");
  assert.equal(nativeCargoDeclared({ appRoot: root, config: {}, exists: exists(new Set()) }), false);
  assert.equal(nativeCargoDeclared({ appRoot: root, config: { nativeAssembly: { cargoManifest: "engine/Cargo.toml" } }, exists: exists(new Set()) }), true);
  assert.equal(nativeCargoDeclared({ appRoot: root, config: { nativeAssembly: { manifest: "engine/Cargo.toml" } }, exists: exists(new Set()) }), true);
  assert.equal(nativeCargoDeclared({ appRoot: root, config: {}, exists: exists(new Set([path.resolve(root, "src-tauri", "Cargo.toml").replaceAll("\\", "/")])) }), true);
});

test("the doctor resolves the cargoless iOS inputs and runs only the preflight checks that apply", () => {
  const app = iosApp();
  const result = spawnSync(process.execPath, [releaseScript, "--doctor", "--config", app.configPath, "--platform", "ios"], {
    cwd: app.root, encoding: "utf8", env: doctorEnv(app),
  });
  const output = `${result.stdout}\n${result.stderr}`;
  assert.doesNotMatch(output, /native Cargo|missing required release input|ENOENT/, output);
  assert.match(output, /preflight:/, output);
  assert.doesNotMatch(output, /target-bridge|sccache/, output);
  const otherFailures = output.split(/\r?\n/).filter((line) => /^\s*\[(?:FAIL|ERROR)\] /.test(line) && !/^\s*\[FAIL\] ownership: /.test(line));
  assert.deepEqual(otherFailures, [], output);
});

// ---- P1-1: prepare commands need a pinned Cargo manifest ---------------------------------------------------------------
const prepare = [{ cmd: "cargo", args: ["build", "--release", "--manifest-path", "engine/Cargo.toml", "--locked"] }];

test("a cargoless iOS target with prepare commands and no nativeAssembly.cargoManifest is refused with the fix named", () => {
  const app = iosApp({ iosExtra: { prepare } });
  assert.throws(() => inputsOf(app), /iosPackage\.prepare builds a Rust library .*declare nativeAssembly\.cargoManifest .*required release inputs/);
  const recipeTarget = { signed: true, recipe: { kind: "iosPackage", spec: validateIosPackageConfig(iosPackage({ prepare })) } };
  assert.throws(() => inputsOf(app, recipeTarget), /declare nativeAssembly\.cargoManifest/);
});

test("the worker refuses the same app directly, before any step runs", () => {
  const app = iosApp({ iosExtra: { prepare } });
  const result = spawnSync(process.execPath, [releaseScript, "--config", app.configPath, "--platform", "ios", "--dry-run"], { cwd: app.root, encoding: "utf8", env: workerEnv(app) });
  assert.notEqual(result.status, 0);
  assert.match(`${result.stdout}\n${result.stderr}`, /declare nativeAssembly\.cargoManifest/);
});

test("prepare commands with a declared manifest pin its Cargo.toml and Cargo.lock, in the app or in a shared tree", () => {
  for (const layout of [{ manifest: "engine/Cargo.toml", lock: "engine/Cargo.lock" }, { manifest: "engine/Cargo.toml", lock: "Cargo.lock" }]) {
    const app = iosApp({ packageJson: true, lock: true, nativeAssembly: { cargoManifest: layout.manifest }, iosExtra: { prepare } });
    mkdirSync(path.join(app.root, "engine"), { recursive: true });
    writeFileSync(path.join(app.root, layout.manifest), "[package]\nname = \"engine\"\nversion = \"0.0.0\"\n");
    writeFileSync(path.join(app.root, layout.lock), "# lock\n");
    const { requiredInputs, buildInputs, nativeLayout } = inputsOf(app);
    assert.equal(nativeLayout, null);
    assert.deepEqual(rel(app, requiredInputs), ["right-release.config.mjs", "package.json", "pnpm-lock.yaml", layout.manifest, layout.lock, "project.yml"]);
    assert.ok(buildInputs.required.includes(layout.manifest) && buildInputs.required.includes(layout.lock));
  }
  // The lock may also be named explicitly; a manifest with no lock anywhere is refused.
  const named = iosApp({ packageJson: true, lock: true, nativeAssembly: { cargoManifest: "engine/Cargo.toml", cargoLockPath: "locks/engine.lock" }, iosExtra: { prepare } });
  mkdirSync(path.join(named.root, "engine"), { recursive: true });
  mkdirSync(path.join(named.root, "locks"), { recursive: true });
  writeFileSync(path.join(named.root, "engine", "Cargo.toml"), "[package]\nname = \"engine\"\nversion = \"0.0.0\"\n");
  assert.throws(() => inputsOf(named), /native Cargo\.lock missing/);
  writeFileSync(path.join(named.root, "locks", "engine.lock"), "# lock\n");
  assert.ok(rel(named, inputsOf(named).requiredInputs).includes("locks/engine.lock"));
});

// ---- P1-2: the real build branch, with every host effect injected -------------------------------------------------------
const noopExecutables = () => {};

test("the iOS build context asserts the Xcode tools before probing them, so a missing tool fails with the normal message", () => {
  const app = iosApp();
  const order = [];
  const assertExecutables = (names) => { order.push(["assert", names]); if (names.includes("xcodebuild")) throw new Error("required executable not found: xcodebuild"); };
  const exec = (cmd) => { order.push(["exec", cmd]); return "x"; };
  const iosSpec = validateIosPackageConfig(iosPackage());
  assert.throws(() => resolveIosBuildContext({ appRoot: app.root, config: app.config, target: app.config.targets.ios, iosSpec, exec, assertExecutables, resolveLayout: () => assert.fail("no layout"), collectRustToolVersions: () => assert.fail("no rust probe") }), /required executable not found: xcodebuild/);
  assert.deepEqual(order, [["assert", ["xcodebuild", "xcodegen"]]]);
});

test("the native iOS context has a null layout and records the Xcode tools (and rustc/cargo only when a manifest is declared); an iOS app with src-tauri gets no context at all", () => {
  const exec = (cmd) => ({ xcodebuild: "Xcode 16.2\nBuild version 16C5032a", xcodegen: "Version: 2.44.1", pnpm: "10.1.0", cargo: "cargo 1.90.0 (abc 2026-01-01)", rustc: "rustc 1.90.0 (abc 2026-01-01)" })[cmd];
  const asserted = [];
  const assertExecutables = (names) => asserted.push(...names);
  const app = iosApp({ packageJson: true, lock: true });
  const iosSpec = validateIosPackageConfig(iosPackage());
  const plain = resolveIosBuildContext({ appRoot: app.root, config: app.config, target: app.config.targets.ios, iosSpec, exec, assertExecutables });
  assert.equal(plain.nativeIos, true);
  assert.equal(plain.nativeLayout, null);
  assert.equal(plain.toolVersions.xcodebuild, "Xcode 16.2 Build version 16C5032a");
  assert.equal(plain.toolVersions.packageManager, "pnpm 10.1.0");
  assert.equal(plain.toolVersions.cargo, null);
  assert.deepEqual(asserted, ["xcodebuild", "xcodegen", "pnpm"]);

  const declared = coderightApp();
  asserted.length = 0;
  const withRust = resolveIosBuildContext({ appRoot: declared.root, config: declared.config, target: declared.config.targets.ios, iosSpec: validateIosPackageConfig(declared.config.targets.ios.iosPackage), exec, assertExecutables });
  assert.deepEqual(asserted, ["xcodebuild", "xcodegen", "cargo", "rustc"]);
  assert.equal(withRust.toolVersions.cargo, "cargo 1.90.0 (abc 2026-01-01)");
  assert.equal(withRust.toolVersions.rustc, "rustc 1.90.0 (abc 2026-01-01)");
  assert.deepEqual(withRust.prepareExecutables, ["cargo"]);

  // Pulse shape (src-tauri): strictly the pre-change path. Nothing asserted, nothing probed, no extra executables.
  const backed = iosApp({ srcTauri: true, packageJson: true, lock: true });
  const touched = [];
  const context = resolveIosBuildContext({ appRoot: backed.root, config: backed.config, target: backed.config.targets.ios, iosSpec, exec: (cmd) => { touched.push(cmd); return "x"; }, assertExecutables: (names) => touched.push(...names) });
  assert.equal(context, null);
  assert.deepEqual(touched, []);
});

test("a null native layout flows through input hashing and sealRelease, and the sealed manifest verifies for publication", () => {
  const app = iosApp({ packageJson: true, lock: true });
  const { requiredInputs, nativeLayout } = inputsOf(app);
  assert.equal(nativeLayout, null);
  const production = productionFunctions();
  const inputHashes = production.hashInputs(requiredInputs);
  const expectedKeys = requiredInputs.map((file) => path.relative(path.dirname(path.dirname(file)), file));
  assert.deepEqual(Object.keys(inputHashes), expectedKeys);
  assert.ok(Object.keys(inputHashes).some((key) => key.endsWith("project.yml")) && Object.keys(inputHashes).some((key) => key.endsWith("package.json")));
  assert.equal(Object.keys(inputHashes).some((key) => /Cargo/.test(key)), false);

  // The IPA and the receipt the recipe would have written.
  const ipaRel = "build/CodeRight.ipa";
  mkdirSync(path.join(app.root, "build"), { recursive: true });
  const ipaBytes = Buffer.from("PK fake ipa bytes for sealing");
  writeFileSync(path.join(app.root, ipaRel), ipaBytes);
  const commit = git(app.root, "rev-parse", "HEAD");
  const stateRoot = path.join(app.root, "state");
  mkdirSync(stateRoot, { recursive: true });
  const receiptPath = path.join(stateRoot, IOS_RECEIPT_NAME);
  writeFileSync(receiptPath, JSON.stringify({
    schema: 1, contract: IOS_SIGNING_CONTRACT, app: "coderight", teamId: "6KLGD3LLKF", bundleId: "dev.orthic.coderight", marketingVersion: "1.0.0", buildNumber: "2026101901", sourceCommit: commit,
    file: { name: "CodeRight.ipa", sha256: createHash("sha256").update(ipaBytes).digest("hex"), sizeBytes: ipaBytes.length },
  }));
  const configSha256 = createHash("sha256").update(readFileSync(app.configPath)).digest("hex");
  const signingIdentity = { contract: IOS_SIGNING_CONTRACT, pipeline: "p", configSha256, teamId: "6KLGD3LLKF", bundleId: "dev.orthic.coderight", receiptInputs: [receiptPath] };
  const toolVersions = collectIosNativeToolVersions({ packageManager: null, exec: (cmd) => (cmd === "xcodebuild" ? "Xcode 16.2\nBuild version 16C5032a" : "Version: 2.44.1"), projectSpec: "project.yml", nodeVersion: process.version });
  const managedRoot = path.join(app.root, "managed");
  const sealedDir = path.join(app.root, ".right-release", "sealed", "coderight-1.0.0-x", "ios");
  beginArtifactLease({ app: "coderight", version: "1.0.0", managedRoot });
  production.sealRelease({
    configRoot: app.root, managedCargoTarget: null, androidTarget: null, iosSpec: { ipa: ipaRel }, nativeLayout: null, sealedDir, releaseId: "coderight-1.0.0-x",
    config: app.config, target: { artifacts: [ipaRel] }, platform: "ios", commit, inputHashes, toolVersions, cacheKey: "k", signingIdentity,
    ownershipEvidence: { commit, source: "test" }, qaAdmission: { status: "recorded" }, managedRoot, stateRoot, rightkitRegistryFields: emptyAdmission(),
  });
  const sealed = verifySealedRelease(sealedDir);
  assert.deepEqual(sealed.manifest.inputs, JSON.parse(JSON.stringify(inputHashes)));
  assert.deepEqual(sealed.manifest.files.map((file) => [file.role, file.name]), [["ipa", "CodeRight.ipa"]]);
  // The extra tools fields (xcodebuild, xcodegen, null packageManager/cargo/rustc) are accepted by the seal readers.
  assert.equal(sealed.manifest.tools.xcodebuild, "Xcode 16.2 Build version 16C5032a");
  assert.equal(sealed.manifest.tools.xcodegen, "Version: 2.44.1");
  assert.equal(sealed.manifest.tools.cargo, null);
  const publication = verifyPublicationRelease({ sealedDir, repoRoot: app.root, appRoot: app.root, commit, app: "coderight", version: "1.0.0", platform: "ios", configPath: app.configPath, releaseId: "coderight-1.0.0-x" }, { verifyOwnership: () => ({ ok: true }) });
  assert.equal(publication.manifest.tools.xcodegen, "Version: 2.44.1");
  assert.ok(publication.artifacts.has("CodeRight.ipa"));
});

test("the environment reaching the recipe's commands drops the release lane's Rust variables and keeps the host's CARGO_HOME, proxy settings and rustup", async () => {
  const hostEnv = { PATH: "/usr/bin", HOME: "/Users/x", RUSTFLAGS: "-C target-cpu=native", CARGO_HOME: "/h/.cargo", CARGO_NET_GIT_FETCH_WITH_CLI: "true", CARGO_HTTP_PROXY: "http://p:1", RUSTUP_HOME: "/h/.rustup", CARGO_BUILD_JOBS: "4" };
  for (const mode of ["legacy", "shared"]) {
    const exported = releaseEnvironment({ root: path.resolve("/vault"), cacheRoot: path.resolve("/cache"), platform: "ios", architecture: "arm64", app: "coderight", cacheKey: "abc123", appRoot: path.resolve("/app"), mode, env: {} });
    const baseEnv = { ...hostEnv, ...exported };
    assert.notEqual(baseEnv.CARGO_HOME, "/h/.cargo", "the lane overrides CARGO_HOME");
    const app = iosApp();
    const plan = planIosNativeBuild({ appRoot: app.root, vaultRoot: path.resolve("/vault"), toolVersions: { xcodebuild: "Xcode 16.2", xcodegen: "2.44" }, hostArch: "arm64", hostEnv });
    const env = plan.sanitizeEnv(baseEnv);
    // Reaches the real recipe; each command's env is recorded and the archive step is made to stop it.
    const seen = [];
    const spec = validateIosPackageConfig(iosPackage());
    const run = (cmd, args, options = {}) => {
      seen.push({ cmd, env: options.env });
      if (cmd === "xcodebuild" && args[0] === "-version") return { status: 0, stdout: "Xcode 16.2\n", stderr: "" };
      if (cmd === "xcodegen" && args[0] === "--version") return { status: 0, stdout: "2.44\n", stderr: "" };
      if (cmd === "xcodegen") { mkdirSync(path.join(app.root, "CodeRight.xcodeproj"), { recursive: true }); return { status: 0, stdout: "", stderr: "" }; }
      return { status: 1, stdout: "", stderr: "stop at archive" };
    };
    await assert.rejects(runIosPackageRecipe({
      root: app.root, spec, env, platform: "darwin", run, log: () => {}, now: () => new Date("2026-10-19T09:00:00Z"), tmpRoot: app.root,
      counterDir: path.join(app.root, "counter"), detectSession: () => ({ guiRunner: false, reason: "Aqua security session" }),
      storage: () => ({ derivedData: path.join(app.root, "dd"), resultBundle: path.join(app.root, "r.xcresult") }), gitHead: () => "a".repeat(40), scanFiles: () => {},
    }), /archive/);
    assert.ok(seen.some((call) => call.cmd === "xcodegen") && seen.some((call) => call.cmd === "xcodebuild"));
    for (const call of seen) {
      for (const name of ["RUSTFLAGS", "CARGO_TARGET_DIR", "RUSTC_WRAPPER", "SCCACHE_DIR", "SCCACHE_CACHE_SIZE", "SCCACHE_BASEDIRS", "SCCACHE_IGNORE_SERVER_IO_ERROR", "RIGHT_RELEASE_CACHE_OWNER"]) {
        assert.equal(Object.hasOwn(call.env, name), false, `${mode}: ${call.cmd} must not see ${name}`);
      }
      assert.equal(call.env.CARGO_HOME, "/h/.cargo", `${mode}: the host's CARGO_HOME, not the lane's`);
      assert.equal(call.env.CARGO_HTTP_PROXY, "http://p:1");
      assert.equal(call.env.CARGO_NET_GIT_FETCH_WITH_CLI, "true");
      assert.equal(call.env.RUSTUP_HOME, "/h/.rustup");
      assert.equal(call.env.HOME, "/Users/x");
    }
    // A host with no CARGO_HOME gets none: the lane's cache path never leaks through.
    const bare = planIosNativeBuild({ appRoot: app.root, vaultRoot: path.resolve("/vault"), toolVersions: { xcodebuild: "Xcode 16.2", xcodegen: "2.44" }, hostArch: "arm64", hostEnv: { PATH: "/usr/bin" } });
    assert.equal(Object.hasOwn(bare.sanitizeEnv(baseEnv), "CARGO_HOME"), false);
    assert.ok(baseEnv.CARGO_TARGET_DIR && baseEnv.RUSTFLAGS, mode);
  }
});

// ---- P2-3: tools named by prepare commands ------------------------------------------------------------------------------
test("the executables named by prepare commands are required; paths and duplicates are not", () => {
  const spec = validateIosPackageConfig(iosPackage({ prepare: [{ cmd: "cargo", args: ["build"] }, { cmd: "cargo" }, { cmd: "node", args: ["x.mjs"] }, { cmd: "./scripts/lib.sh" }] }));
  assert.deepEqual(iosPrepareExecutables(spec), ["cargo", "node"]);
  assert.deepEqual(iosPrepareExecutables(validateIosPackageConfig(iosPackage())), []);
  assert.deepEqual(iosPrepareExecutables(null), []);
});

// ---- P2-4: the Rust environment strip ---------------------------------------------------------------------------------------
test("stripRustEnvironment removes every CARGO_*, RUST*, SCCACHE_* variable legacy and shared modes export, and keeps only the host's allowed ones", () => {
  for (const mode of ["legacy", "shared"]) {
    const exported = releaseEnvironment({ root: path.resolve("/vault"), cacheRoot: path.resolve("/cache"), platform: "mac", architecture: "arm64", app: "a", cacheKey: "k", appRoot: path.resolve("/app"), mode, env: {} });
    const stripped = stripRustEnvironment({ ...exported, HOME: "/h", PATH: "/bin" }, { HOME: "/h", PATH: "/bin" });
    assert.deepEqual(Object.keys(stripped).sort(), ["HOME", "PATH"], mode);
  }
  const host = {
    RUSTFLAGS: "x", RUSTDOCFLAGS: "x", RUSTC: "x", RUSTC_WRAPPER: "x", RUSTC_WORKSPACE_WRAPPER: "x", RUST_BACKTRACE: "1", RUST_LOG: "x",
    CARGO_TARGET_DIR: "x", CARGO_REGISTRIES_FOO_TOKEN: "x", CARGO_INCREMENTAL: "0", SCCACHE_DIR: "x", SCCACHE_ERROR_LOG: "x", RIGHT_RELEASE_CACHE_OWNER: "rightkit-v2",
    CARGO_BUILD_JOBS: "4", CARGO_JOBS: "4", RUST_TEST_THREADS: "2", RUSTUP_HOME: "/r", RUSTUP_TOOLCHAIN: "stable", CARGO_HOME: "/c",
    CARGO_NET_RETRY: "5", CARGO_HTTP_PROXY: "p", GOPATH: "/g", TRUST_ME: "1",
  };
  assert.deepEqual(Object.keys(stripRustEnvironment(host, host)).sort(), ["CARGO_BUILD_JOBS", "CARGO_HOME", "CARGO_HTTP_PROXY", "CARGO_JOBS", "CARGO_NET_RETRY", "GOPATH", "RUSTUP_HOME", "RUSTUP_TOOLCHAIN", "RUST_TEST_THREADS", "TRUST_ME"]);
});

// ---- P2-2: mediaSidecar on a cargoless iOS app --------------------------------------------------------------------------
test("a cargoless iOS app that declares mediaSidecar fails with a clear message, never a TypeError", () => {
  const app = iosApp({ commit: false });
  const config = JSON.parse(JSON.stringify(app.config));
  config.targets.ios.mediaSidecar = { tools: ["ffmpeg"], stageDir: "sidecar", notices: { dir: "notices" } };
  writeFileSync(app.configPath, `export default ${JSON.stringify(config)};\n`);
  git(app.root, "init", "--initial-branch", "main");
  git(app.root, "config", "user.email", "t@example.com");
  git(app.root, "config", "user.name", "T");
  git(app.root, "add", ".");
  git(app.root, "commit", "-m", "init");
  const result = spawnSync(process.execPath, [releaseScript, "--config", app.configPath, "--platform", "ios", "--dry-run"], { cwd: app.root, encoding: "utf8", env: workerEnv(app) });
  const output = `${result.stdout}\n${result.stderr}`;
  assert.notEqual(result.status, 0, output);
  assert.doesNotMatch(output, /TypeError|Cannot read properties/, output);
  assert.match(output, /mediaSidecar|unknown key/, output);
});

// ---- CodeRight shape: no src-tauri, no package.json, a shared crates/ workspace, a prepare that runs cargo -----------------
function coderightApp() {
  const repo = tempRoot("right-coderight-");
  const root = path.join(repo, "apps", "coderight-ios");
  mkdirSync(path.join(root, "Sources"), { recursive: true });
  mkdirSync(path.join(repo, "crates", "x"), { recursive: true });
  const config = {
    schema: 1, app: "coderight", version: "1.0.0",
    buildInputs: { include: ["project.yml", "Sources/**"] },
    nativeAssembly: { cargoManifest: "../../crates/x/Cargo.toml" },
    targets: { ios: { signed: true, iosPackage: iosPackage({ prepare: [{ cmd: "cargo", args: ["build", "--release", "--manifest-path", "../../crates/x/Cargo.toml", "--target", "aarch64-apple-ios", "--locked"] }] }), artifacts: ["build/CodeRight.ipa"] } },
  };
  writeFileSync(path.join(root, "right-release.config.mjs"), `export default ${JSON.stringify(config)};\n`);
  writeFileSync(path.join(root, "right-qa.config.mjs"), "export default {};\n");
  writeFileSync(path.join(root, "project.yml"), "name: CodeRight\n");
  writeFileSync(path.join(root, "Sources", "main.swift"), "print(1)\n");
  writeFileSync(path.join(repo, "Cargo.toml"), "[workspace]\nmembers = [\"crates/x\"]\n");
  writeFileSync(path.join(repo, "Cargo.lock"), "# workspace lock\n");
  writeFileSync(path.join(repo, "crates", "x", "Cargo.toml"), "[package]\nname = \"x\"\nversion = \"0.0.0\"\nedition = \"2021\"\n");
  git(repo, "init", "--initial-branch", "main");
  git(repo, "config", "user.email", "t@example.com");
  git(repo, "config", "user.name", "T");
  git(repo, "add", ".");
  git(repo, "commit", "-m", "init");
  return { repo, root, config, configPath: path.join(root, "right-release.config.mjs") };
}

test("CodeRight shape: the manifest outside the app and its workspace lock are required inputs, and the layout is null", () => {
  const app = coderightApp();
  const { requiredInputs, buildInputs, nativeLayout } = resolveReleaseBuildInputs({ repoRoot: app.repo, appRoot: app.root, configPath: app.configPath, config: app.config, target: app.config.targets.ios });
  assert.equal(nativeLayout, null);
  const fromRepo = requiredInputs.map((file) => path.relative(app.repo, file).replaceAll("\\", "/"));
  assert.deepEqual(fromRepo, ["apps/coderight-ios/right-release.config.mjs", "crates/x/Cargo.toml", "Cargo.lock", "apps/coderight-ios/project.yml"]);
  assert.deepEqual(buildInputs.required, fromRepo);
  // No lock anywhere is refused.
  execFileSync("git", ["rm", "-q", "--cached", "Cargo.lock"], { cwd: app.repo });
  nodeFs.rmSync(path.join(app.repo, "Cargo.lock"));
  assert.throws(() => resolveReleaseBuildInputs({ repoRoot: app.repo, appRoot: app.root, configPath: app.configPath, config: app.config, target: app.config.targets.ios }), /native Cargo\.lock missing/);
});

test("CodeRight shape: the build resolves to the iOS context, with no Tauri layout, broker target or bridge", () => {
  const app = coderightApp();
  const iosSpec = validateIosPackageConfig(app.config.targets.ios.iosPackage);
  const asserted = [];
  const exec = (cmd) => ({ xcodebuild: "Xcode 16.2", xcodegen: "Version: 2.44.1", cargo: "cargo 1.90.0", rustc: "rustc 1.90.0" })[cmd];
  const context = resolveIosBuildContext({ appRoot: app.root, config: app.config, target: app.config.targets.ios, iosSpec, exec, assertExecutables: (names) => asserted.push(...names) });
  assert.equal(context.nativeIos, true);
  assert.equal(context.nativeLayout, null);
  assert.equal(context.hasPackageJson, false);
  assert.deepEqual(asserted, ["xcodebuild", "xcodegen", "cargo", "rustc"]);
  assert.equal(context.toolVersions.xcodebuild, "Xcode 16.2");
  assert.equal(context.toolVersions.rustc, "rustc 1.90.0");
  const plan = planIosNativeBuild({ appRoot: app.root, vaultRoot: path.join(app.repo, "vault"), toolVersions: context.toolVersions, hostArch: "arm64" });
  assert.equal(plan.managedCargoTarget, null);
  assert.equal(plan.cacheMode, "legacy");
  assert.equal(plan.targetBridge.ensure(), undefined);
  assert.equal(plan.targetBridge.run(() => "ran"), "ran");
});

test("CodeRight shape: cargo is a required executable, with git and no package manager", () => {
  const app = coderightApp();
  const iosSpec = validateIosPackageConfig(app.config.targets.ios.iosPackage);
  assert.deepEqual(iosPrepareExecutables(iosSpec), ["cargo"]);
  const required = [...requiredBuildExecutables({ projectType: "tauri", packageManager: "pnpm", usesPackageManager: false, usesCargo: false }), ...iosPrepareExecutables(iosSpec)];
  assert.deepEqual(required, ["git", "cargo"]);
});

test("CodeRight shape: the worker dry-run plans the iOS recipe, and the prepare command is the app's own cargo build", () => {
  const app = coderightApp();
  const result = spawnSync(process.execPath, [releaseScript, "--config", app.configPath, "--platform", "ios", "--dry-run"], { cwd: app.root, encoding: "utf8", env: workerEnv(app) });
  const output = `${result.stdout}\n${result.stderr}`;
  assert.equal(result.status, 0, output);
  assert.match(output, /prepare: cargo build --release --manifest-path \.\.\/\.\.\/crates\/x\/Cargo\.toml --target aarch64-apple-ios/);
});

test("CodeRight shape: the doctor pins the manifest and lock and reports no target-bridge or compiler-cache check", () => {
  const app = coderightApp();
  const result = spawnSync(process.execPath, [releaseScript, "--doctor", "--config", app.configPath, "--platform", "ios"], { cwd: app.root, encoding: "utf8", env: doctorEnv(app) });
  const output = `${result.stdout}\n${result.stderr}`;
  assert.doesNotMatch(output, /native Cargo|missing required release input|ENOENT|target-bridge|sccache/, output);
  const otherFailures = output.split(/\r?\n/).filter((line) => /^\s*\[(?:FAIL|ERROR)\] /.test(line) && !/^\s*\[FAIL\] ownership: /.test(line));
  assert.deepEqual(otherFailures, [], output);
});

test("two release inputs that would share a hash key are refused instead of one silently replacing the other", () => {
  const production = productionFunctions();
  const dir = tempRoot("right-hash-keys-");
  mkdirSync(path.join(dir, "a", "engine"), { recursive: true });
  mkdirSync(path.join(dir, "b", "engine"), { recursive: true });
  writeFileSync(path.join(dir, "a", "engine", "Cargo.lock"), "one");
  writeFileSync(path.join(dir, "b", "engine", "Cargo.lock"), "two");
  assert.throws(() => production.hashInputs([path.join(dir, "a", "engine", "Cargo.lock"), path.join(dir, "b", "engine", "Cargo.lock")]), /share the hash key engine.Cargo\.lock/);
});

// ---- the release-switch feature closure checks the Rust the prepare commands build ---------------------------------------
const cargoPrepare = (...extra) => [{ cmd: "cargo", args: ["build", "--release", "--manifest-path", "../../crates/x/Cargo.toml", "--target", "aarch64-apple-ios", "--locked", ...extra] }];
const treeWith = (feature) => `x v0.0.0 (crates/x)\n├── x feature "default"\n${feature ? `├── x feature "${feature}"\n` : ""}└── serde v1.0.0\n`;

test("release-switch inputs for CodeRight use the declared manifest and scan the prepare cargo commands", () => {
  const app = coderightApp();
  const inputs = releaseSwitchInputs({ appRoot: app.root, nativeIos: true, config: app.config, target: app.config.targets.ios, prepare: app.config.targets.ios.iosPackage.prepare });
  assert.equal(inputs.cargoManifest, "../../crates/x/Cargo.toml");
  assert.deepEqual(inputs.cargoCommands.map((command) => command.cmd), ["cargo"]);
  assert.deepEqual(inputs.problems, []);
  assert.equal(inputs.commands[0].label, "iosPackage.prepare[0]");
  // targets.<os>.cargoManifest still wins; with no declaration there is no manifest to invent.
  assert.equal(releaseSwitchInputs({ appRoot: app.root, nativeIos: true, config: app.config, target: { cargoManifest: "own/Cargo.toml" }, prepare: [] }).cargoManifest, "own/Cargo.toml");
  assert.equal(releaseSwitchInputs({ nativeIos: true, config: {}, target: {}, prepare: [] }).cargoManifest, undefined);
});

test("the feature closure resolves from the declared manifest outside the app, with the prepare command's own features", () => {
  const app = coderightApp();
  const prepareCommands = cargoPrepare("--no-default-features", "--features", "ffi,metrics");
  const inputs = releaseSwitchInputs({ appRoot: app.root, nativeIos: true, config: app.config, target: app.config.targets.ios, prepare: prepareCommands });
  const calls = [];
  const closure = resolveFeatureClosure({
    root: app.root, cargoManifest: inputs.cargoManifest, rust: undefined, features: [], triple: "aarch64-apple-ios", commandTexts: inputs.cargoCommands,
    run: (cmd, args) => { calls.push({ cmd, args }); return { status: 0, stdout: treeWith(), stderr: "" }; },
  });
  assert.equal(closure.skipped, undefined, "a build that compiles Rust is never skipped");
  assert.equal(closure.manifest, "../../crates/x/Cargo.toml");
  assert.equal(calls.length, 1);
  const args = calls[0].args;
  assert.equal(args[args.indexOf("--manifest-path") + 1], path.join(app.root, "../../crates/x/Cargo.toml"));
  assert.ok(args.includes("--no-default-features"));
  assert.equal(args[args.indexOf("--features") + 1], "ffi,metrics");
  assert.equal(args[args.indexOf("--target") + 1], "aarch64-apple-ios");
  // Without a manifest the same call refuses rather than skipping.
  assert.throws(() => resolveFeatureClosure({ root: app.root, commandTexts: inputs.cargoCommands, run: () => assert.fail("no cargo") }), /release-switch-manifest-missing/);
  // --all-features is carried too.
  const all = [];
  resolveFeatureClosure({ root: app.root, cargoManifest: inputs.cargoManifest, commandTexts: cargoPrepare("--all-features"), run: (cmd, runArgs) => { all.push(runArgs); return { status: 0, stdout: treeWith(), stderr: "" }; } });
  assert.ok(all[0].includes("--all-features"));
});

test("a forbidden release-time feature enabled in prepare is refused by the command scan and by the resolved closure; a clean one passes", () => {
  const app = coderightApp();
  for (const feature of ["qa-native", "test-mode"]) {
    const prepareCommands = cargoPrepare("--features", `ffi,${feature}`);
    const inputs = releaseSwitchInputs({ appRoot: app.root, nativeIos: true, config: app.config, target: app.config.targets.ios, prepare: prepareCommands });
    assert.throws(() => assertNoForbiddenBuildFeatures({ root: app.root, tier: "release/update", cargoManifest: inputs.cargoManifest, commands: inputs.commands }), new RegExp(`${feature}`));
    // The closure layer: the graph cargo resolves for exactly that command carries the feature.
    const closure = resolveFeatureClosure({ root: app.root, cargoManifest: inputs.cargoManifest, commandTexts: inputs.cargoCommands, run: () => ({ status: 0, stdout: treeWith(feature), stderr: "" }) });
    assert.deepEqual(featureClosureFindings(closure.treeText).map((finding) => finding.rule), [`forbidden-feature ${feature}`]);
  }
  const clean = releaseSwitchInputs({ appRoot: app.root, nativeIos: true, config: app.config, target: app.config.targets.ios, prepare: cargoPrepare("--features", "ffi,metrics") });
  assert.doesNotThrow(() => assertNoForbiddenBuildFeatures({ root: app.root, tier: "release/update", cargoManifest: clean.cargoManifest, commands: clean.commands }));
  const closure = resolveFeatureClosure({ root: app.root, cargoManifest: clean.cargoManifest, commandTexts: clean.cargoCommands, run: () => ({ status: 0, stdout: treeWith(), stderr: "" }) });
  assert.deepEqual(featureClosureFindings(closure.treeText), []);
  // --all-features is itself a finding.
  const all = releaseSwitchInputs({ appRoot: app.root, nativeIos: true, config: app.config, target: app.config.targets.ios, prepare: cargoPrepare("--all-features") });
  assert.throws(() => assertNoForbiddenBuildFeatures({ root: app.root, tier: "release/update", cargoManifest: all.cargoManifest, commands: all.commands }), /all-features/);
});

test("with a Rust manifest declared, every prepare command must be a bare cargo command and anything else is refused", () => {
  const app = coderightApp();
  const refused = (prepareCommands, pattern) => assert.match(releaseSwitchInputs({ appRoot: app.root, nativeIos: true, config: app.config, target: app.config.targets.ios, prepare: prepareCommands }).problems.join("\n"), pattern);
  refused([{ cmd: "node", args: ["scripts/prep.mjs"] }], /prepare\[0\] \(node\) is not a bare cargo command/);
  refused([{ cmd: "sh", args: ["-c", "cargo build --features $FEATS"] }], /is not a bare cargo command/);
  refused([{ cmd: "pnpm", args: ["run", "rust"] }], /is not a bare cargo command/);
  refused([{ cmd: "python3", args: ["build.py"] }], /is not a bare cargo command/);
  refused([{ cmd: "rustup", args: ["run", "stable", "cargo", "build"] }], /is not a bare cargo command/);
  refused([{ cmd: "/usr/bin/env", args: ["cargo", "build"] }], /is not a bare cargo command/);
  refused([{ cmd: "./scripts/build-rust.sh" }], /is not a bare cargo command/);
  refused(cargoPrepare().map((command) => ({ ...command, args: ["+nightly", ...command.args] })), /selects a toolchain with \+nightly/);
  refused(cargoPrepare("--manifest-path", "../../crates/other/Cargo.toml"), /not the declared manifest/);
  refused(cargoPrepare("--manifest-path=other/Cargo.toml"), /not the declared manifest/);
  refused(cargoPrepare("--features", "$FEATS"), /selects features from a variable/);
  refused(cargoPrepare("--features=%FEATS%"), /selects features from a variable/);
  // The declared manifest itself, and plain flags, are fine.
  assert.deepEqual(releaseSwitchInputs({ appRoot: app.root, nativeIos: true, config: app.config, target: app.config.targets.ios, prepare: [...cargoPrepare("-F", "ffi"), ...cargoPrepare("--manifest-path", "../../crates/x/Cargo.toml", "--no-default-features")] }).problems, []);
  // The same refusal stops the build inputs, with the instruction to write cargo commands.
  const hidden = { ...app.config.targets.ios, iosPackage: iosPackage({ prepare: [{ cmd: "node", args: ["scripts/prep.mjs"] }] }) };
  assert.throws(() => resolveReleaseBuildInputs({ repoRoot: app.repo, appRoot: app.root, configPath: app.configPath, config: app.config, target: hidden }), /express the Rust build as cargo commands in prepare/);
  // No declared manifest, no Rust to hide.
  assert.deepEqual(releaseSwitchInputs({ appRoot: path.resolve("/app"), nativeIos: true, config: {}, target: {}, prepare: [{ cmd: "sh", args: ["-c", "x"] }] }).problems, []);
});

test("the worker refuses a forbidden feature in prepare and a prepare it cannot read, and never reports the closure as missing for a clean one", () => {
  const worker = (prepareCommands) => {
    const app = coderightApp();
    const config = JSON.parse(JSON.stringify(app.config));
    config.targets.ios.iosPackage.prepare = prepareCommands;
    writeFileSync(app.configPath, `export default ${JSON.stringify(config)};\n`);
    git(app.repo, "add", ".");
    git(app.repo, "commit", "-q", "-m", "prepare");
    const result = spawnSync(process.execPath, [releaseScript, "--config", app.configPath, "--platform", "ios", "--dry-run"], { cwd: app.root, encoding: "utf8", env: workerEnv(app) });
    return { result, output: `${result.stdout}\n${result.stderr}` };
  };
  const forbidden = worker(cargoPrepare("--features", "ffi,qa-native"));
  assert.notEqual(forbidden.result.status, 0, forbidden.output);
  assert.match(forbidden.output, /qa-native/);
  const hidden = worker([{ cmd: "sh", args: ["-c", "cargo build"] }]);
  assert.notEqual(hidden.result.status, 0, hidden.output);
  assert.match(hidden.output, /express the Rust build as cargo commands in prepare/);
  const clean = worker(cargoPrepare("--features", "ffi"));
  assert.doesNotMatch(clean.output, /release-switch-manifest-missing|feature closure skipped/, clean.output);
  assert.equal(clean.result.status, 0, clean.output);
});

// ---- Oracle round 2 --------------------------------------------------------------------------------------------------------
test("only a native iOS target falls back to nativeAssembly.cargoManifest; every other platform keeps the old resolver order", () => {
  const dir = tempRoot("right-switch-order-");
  mkdirSync(path.join(dir, "switches"), { recursive: true });
  writeFileSync(path.join(dir, "switches", "Cargo.toml"), "[package]\nname = \"s\"\nversion = \"0.0.0\"\n");
  writeFileSync(path.join(dir, "Cargo.toml"), "[package]\nname = \"root\"\nversion = \"0.0.0\"\n");
  const config = { nativeAssembly: { cargoManifest: "engine/Cargo.toml" }, releaseSwitches: { manifest: "switches/Cargo.toml" } };
  for (const target of [{}, { signed: true }]) {
    const inputs = releaseSwitchInputs({ appRoot: dir, config, target, prepare: [], nativeIos: false });
    assert.equal(inputs.cargoManifest, undefined, "a desktop app does not adopt nativeAssembly.cargoManifest");
    assert.equal(resolveCargoManifestPath({ root: dir, cargoManifest: inputs.cargoManifest, manifest: config.releaseSwitches.manifest }).rel, "switches/Cargo.toml");
  }
  assert.equal(releaseSwitchInputs({ appRoot: dir, config, target: { cargoManifest: "own/Cargo.toml" }, nativeIos: false }).cargoManifest, "own/Cargo.toml");
  // iOS with src-tauri (Pulse) is not native iOS: no prepare scan, no fallback, no problems, exactly the old inputs.
  const pulse = releaseSwitchInputs({ appRoot: dir, config, target: { cargoManifest: undefined }, prepare: [{ cmd: "node", args: ["x.mjs"] }], nativeIos: false });
  assert.deepEqual(pulse, { cargoManifest: undefined, commands: [], cargoCommands: [], problems: [] });
  // Native iOS does fall back, and an explicit releaseSwitches.manifest is still passed through beside it.
  assert.equal(releaseSwitchInputs({ appRoot: dir, config, target: {}, prepare: [], nativeIos: true }).cargoManifest, "engine/Cargo.toml");
});

test("a declared manifest or Cargo.lock outside the repository is refused", () => {
  const outsideRoot = tempRoot("right-outside-");
  mkdirSync(path.join(outsideRoot, "crates", "x"), { recursive: true });
  writeFileSync(path.join(outsideRoot, "crates", "x", "Cargo.toml"), "[package]\nname = \"x\"\nversion = \"0.0.0\"\n");
  writeFileSync(path.join(outsideRoot, "crates", "x", "Cargo.lock"), "# lock\n");
  const app = iosApp({ nativeAssembly: { cargoManifest: path.join(outsideRoot, "crates", "x", "Cargo.toml") } });
  assert.throws(() => inputsOf(app), /outside the repository/);
  // A manifest inside the repository naming a lock outside it.
  const inside = iosApp({ nativeAssembly: { cargoManifest: "engine/Cargo.toml", cargoLockPath: path.join(outsideRoot, "crates", "x", "Cargo.lock") } });
  mkdirSync(path.join(inside.root, "engine"), { recursive: true });
  writeFileSync(path.join(inside.root, "engine", "Cargo.toml"), "[package]\nname = \"engine\"\nversion = \"0.0.0\"\n");
  assert.throws(() => inputsOf(inside), /Cargo\.lock .* is outside the repository/);
});

test("the workspace search climbs above the app only for a caller that names the repository, and stops at it", () => {
  const parent = tempRoot("right-clamp-");
  const repo = path.join(parent, "repo");
  const appRoot = path.join(repo, "apps", "a");
  mkdirSync(appRoot, { recursive: true });
  mkdirSync(path.join(repo, "crates", "x"), { recursive: true });
  mkdirSync(path.join(parent, "crates", "x"), { recursive: true });
  // A workspace and lock in the repository, and another above it.
  writeFileSync(path.join(repo, "Cargo.toml"), "[workspace]\nmembers = [\"crates/x\"]\n");
  writeFileSync(path.join(repo, "Cargo.lock"), "# repo lock\n");
  writeFileSync(path.join(repo, "crates", "x", "Cargo.toml"), "[package]\nname = \"x\"\nversion = \"0.0.0\"\n");
  writeFileSync(path.join(parent, "Cargo.toml"), "[workspace]\nmembers = [\"crates/x\"]\n");
  writeFileSync(path.join(parent, "Cargo.lock"), "# parent lock\n");
  writeFileSync(path.join(parent, "crates", "x", "Cargo.toml"), "[package]\nname = \"x\"\nversion = \"0.0.0\"\n");
  const inside = { nativeAssembly: { cargoManifest: "../../crates/x/Cargo.toml" } };
  // No repoRoot: the exact historical behaviour, no climb above the app.
  const old = resolveNativeCargoLayout({ appRoot, config: inside });
  assert.equal(old.workspaceRoot, path.join(repo, "crates", "x"));
  assert.equal(old.lockPath, undefined);
  // With the repository named, the workspace and lock in it are found.
  const native = resolveNativeCargoLayout({ appRoot, config: inside, repoRoot: repo });
  assert.equal(native.workspaceRoot, repo);
  assert.equal(native.lockPath, path.join(repo, "Cargo.lock"));
  // A manifest above the repository never reaches the workspace that is also above it.
  const above = { nativeAssembly: { cargoManifest: "../../../crates/x/Cargo.toml" } };
  assert.equal(resolveNativeCargoLayout({ appRoot, config: above, repoRoot: repo }).lockPath, undefined);
});

test("a desktop app with a declared manifest outside the app keeps the old workspaceRoot and lockPath", () => {
  const dir = tempRoot("right-desktop-layout-");
  const appRoot = path.join(dir, "apps", "d");
  mkdirSync(appRoot, { recursive: true });
  mkdirSync(path.join(dir, "crates", "x"), { recursive: true });
  writeFileSync(path.join(dir, "Cargo.toml"), "[workspace]\nmembers = [\"crates/x\"]\n");
  writeFileSync(path.join(dir, "Cargo.lock"), "# ancestor workspace lock\n");
  writeFileSync(path.join(dir, "crates", "x", "Cargo.toml"), "[package]\nname = \"x\"\nversion = \"0.0.0\"\n");
  const config = { nativeAssembly: { cargoManifest: "../../crates/x/Cargo.toml" } };
  const desktop = resolveNativeCargoLayout({ appRoot, config });
  assert.equal(desktop.workspaceRoot, path.join(dir, "crates", "x"), "no ancestor workspace is adopted");
  assert.equal(desktop.lockPath, undefined, "the ancestor lock is not adopted");
  assert.deepEqual(desktop.lockCandidates, [path.join(dir, "crates", "x", "Cargo.lock"), path.join(appRoot, "Cargo.lock")]);
  writeFileSync(path.join(dir, "crates", "x", "Cargo.lock"), "# own lock\n");
  assert.equal(resolveNativeCargoLayout({ appRoot, config }).lockPath, path.join(dir, "crates", "x", "Cargo.lock"));
});

test("a pinned Cargo.lock of a native iOS app is handed to preflight, which runs the Rust budget check but not the bridge or sccache", () => {
  const app = coderightApp();
  const { cargoLockPaths, nativeLayout } = resolveReleaseBuildInputs({ repoRoot: app.repo, appRoot: app.root, configPath: app.configPath, config: app.config, target: app.config.targets.ios });
  assert.equal(nativeLayout, null);
  assert.deepEqual(cargoLockPaths, [path.join(app.repo, "Cargo.lock")]);
  const seen = [];
  const base = { platform: "ios", appRoot: app.root, env: { PATH: "" }, ownership: false, versionProbe: () => null, isBrokerManagedHost: () => false, freeDisk: () => 500 };
  const checks = collectPreflight({ ...base, cargoLockPaths, rust: true, bridge: false, runtimeBudgetAdoption: (input) => { seen.push(input.cargoLockPaths); return { name: "runtime-budget", status: "ok", detail: "stub" }; } });
  assert.deepEqual(checks.map((check) => check.name), ["disk", "runtime-budget"]);
  assert.deepEqual(seen, [cargoLockPaths]);
  // Without a lock at all (the cargoless case) nothing Rust runs.
  assert.deepEqual(collectPreflight({ ...base, rust: false, bridge: false, runtimeBudgetAdoption: () => assert.fail("no adoption") }).map((check) => check.name), ["disk"]);
});

test("a Cargo-backed iOS app (src-tauri) is strictly the pre-change build: no Xcode assertion, no extra tool fields, no prepare executables, no switch inputs", () => {
  const backed = iosApp({ srcTauri: true, packageJson: true, lock: true });
  const iosSpec = validateIosPackageConfig(iosPackage({ prepare: [{ cmd: "node", args: ["scripts/prep.mjs"] }] }));
  const touched = [];
  assert.equal(resolveIosBuildContext({ appRoot: backed.root, config: backed.config, target: backed.config.targets.ios, iosSpec, exec: (cmd) => touched.push(cmd), assertExecutables: (names) => touched.push(...names) }), null);
  assert.deepEqual(touched, []);
  assert.deepEqual(releaseSwitchInputs({ appRoot: backed.root, config: backed.config, target: backed.config.targets.ios, prepare: iosSpec.prepare, nativeIos: false }), { cargoManifest: undefined, commands: [], cargoCommands: [], problems: [] });
  // A node prepare with src-tauri is not refused by the Rust-manifest rules.
  assert.doesNotThrow(() => inputsOf(backed, { ...backed.config.targets.ios, iosPackage: { ...iosPackage(), prepare: iosSpec.prepare } }));
  assert.deepEqual(inputsOf(backed).cargoLockPaths, []);
});

// Load the production hashing and sealing functions out of build-release.mjs and run them with real helpers.
function productionFunctions() {
  const source = readFileSync(buildScript, "utf8").replace(/\r\n/g, "\n");
  const take = (name) => {
    const start = source.indexOf(`function ${name}(`);
    assert.ok(start >= 0, `${name} must exist in build-release.mjs`);
    return source.slice(start, source.indexOf("\n}\n", start) + 3);
  };
  const names = ["hashInputs", "hashFile", "hashFileText", "writeJson", "hardeningScanTargets", "sealRelease"];
  const context = {
    createHash, path, existsSync, readFileSync, writeFileSync, mkdirSync, console, process,
    rmSync: (...a) => nodeFs.rmSync(...a), copyFileSync: (...a) => nodeFs.copyFileSync(...a), statSync: (...a) => nodeFs.statSync(...a),
    renameSync: (...a) => nodeFs.renameSync(...a), chmodSync: (...a) => nodeFs.chmodSync(...a),
    resolveArtifactDir, materializeArtifacts, completeArtifactLease, retainArtifacts, devtoolsEvidence, resolveDevtoolsAllowance,
    MEDIA_SIDECAR_RECEIPT_NAME, validateMediaSidecarConfig, validateMediaSidecarReceipt, windowsSidecarSigningProblems, validateNotarizationReceipt,
    verifyAndroidReceipt, androidSealEntry, verifyIosReceipt, iosSealEntry, assertUpdaterSignature,
    fail: (message) => { throw new Error(message); },
  };
  return runInNewContext(`${names.map(take).join("\n")}\n({ ${names.join(", ")} })`, context);
}

test("prepare is restricted to cargo build, rustc and check on exactly the declared manifest, with no config, -Z or target-dir overrides", () => {
  const app = coderightApp();
  const problemsOf = (args, extra = {}) => releaseSwitchInputs({ appRoot: app.root, nativeIos: true, config: app.config, target: app.config.targets.ios, prepare: [{ cmd: "cargo", args, ...extra }] }).problems.join("\n");
  const manifest = ["--manifest-path", "../../crates/x/Cargo.toml", "--locked"];
  for (const subcommand of ["build", "rustc", "check"]) assert.equal(problemsOf([subcommand, ...manifest, "--release"]), "", subcommand);
  for (const subcommand of ["xtask", "run", "install", "test", "fmt", "br", "cargo-custom", "publish"]) assert.match(problemsOf([subcommand, ...manifest]), new RegExp(`runs cargo ${subcommand}; only cargo build, rustc and check`), subcommand);
  assert.match(problemsOf([]), /runs cargo \(no subcommand\)/);
  // --manifest-path is required and must equal the declared manifest; the working directory is no fallback.
  assert.match(problemsOf(["build", "--release"]), /has no --manifest-path; pass --manifest-path \.\.\/\.\.\/crates\/x\/Cargo\.toml/);
  assert.match(problemsOf(["build", "--manifest-path", "other/Cargo.toml"]), /not the declared manifest/);
  assert.equal(problemsOf(["build", "--locked", "--manifest-path=../../crates/x/Cargo.toml"]), "");
  assert.equal(problemsOf(["build", "--locked", "--manifest-path", "Cargo.toml"], { cwd: "../../crates/x" }), "", "a value is resolved from the command's own cwd");
  assert.match(problemsOf(["build", "--manifest-path", "Cargo.toml"]), /not the declared manifest/);
  // Overrides that change what is built.
  assert.match(problemsOf(["build", ...manifest, "--config", "build.rustflags=['-Cx']"]), /passes --config/);
  assert.match(problemsOf(["build", ...manifest, "--config=build.jobs=1"]), /passes --config/);
  assert.match(problemsOf(["build", ...manifest, "-Z", "build-std"]), /unstable flag -Z/);
  assert.match(problemsOf(["build", ...manifest, "-Zbuild-std=core"]), /unstable flag -Zbuild-std/);
  assert.match(problemsOf(["build", ...manifest, "--target-dir", "out"]), /passes --target-dir/);
  assert.match(problemsOf(["build", ...manifest, "--target-dir=out"]), /passes --target-dir/);
  // The refusals stop the build inputs too.
  const target = { ...app.config.targets.ios, iosPackage: iosPackage({ prepare: [{ cmd: "cargo", args: ["xtask", "build"] }] }) };
  assert.throws(() => resolveReleaseBuildInputs({ repoRoot: app.repo, appRoot: app.root, configPath: app.configPath, config: app.config, target }), /only cargo build, rustc and check/);
});

test("a prepare cargo command without --locked or --frozen is refused, because the lock is hashed before prepare runs", () => {
  const app = coderightApp();
  const problemsOf = (args) => releaseSwitchInputs({ appRoot: app.root, nativeIos: true, config: app.config, target: app.config.targets.ios, prepare: [{ cmd: "cargo", args }] }).problems.join("\n");
  const manifest = ["--manifest-path", "../../crates/x/Cargo.toml"];
  assert.match(problemsOf(["build", ...manifest]), /has no --locked; pass --locked so cargo builds exactly the pinned Cargo\.lock/);
  assert.equal(problemsOf(["build", ...manifest, "--locked"]), "");
  assert.equal(problemsOf(["check", ...manifest, "--frozen"]), "");
  const target = { ...app.config.targets.ios, iosPackage: iosPackage({ prepare: [{ cmd: "cargo", args: ["build", ...manifest] }] }) };
  assert.throws(() => resolveReleaseBuildInputs({ repoRoot: app.repo, appRoot: app.root, configPath: app.configPath, config: app.config, target }), /has no --locked/);
});

// ---- reviewed Swift macro/plugin trust: Package.resolved is a required, hashed, tracked release input ----------------
const TRUSTED = [{ package: "swift-composable-architecture", revision: "ead11e04e5011c437722c1990d22f80d87056978" }];
const PINS = JSON.stringify({ version: 3, pins: [{ identity: "swift-composable-architecture", kind: "remoteSourceControl", location: "https://github.com/pointfreeco/swift-composable-architecture", state: { revision: TRUSTED[0].revision, version: "1.0.0" } }] });
const withPins = (options, pinsPath = "Package.resolved") => {
  const app = iosApp({ ...options, commit: false });
  mkdirSync(path.dirname(path.join(app.root, pinsPath)), { recursive: true });
  writeFileSync(path.join(app.root, pinsPath), PINS);
  return app;
};

test("native iOS: a trust list makes the committed Package.resolved a required input; a missing file is refused; no list adds nothing", () => {
  const iosExtra = { packageResolved: "Package.resolved", trustedSwiftMacros: TRUSTED };
  const app = withPins({ iosExtra });
  const { requiredInputs, buildInputs } = inputsOf(app);
  assert.deepEqual(rel(app, requiredInputs), ["right-release.config.mjs", "project.yml", "Package.resolved"]);
  assert.ok(buildInputs.required.includes("Package.resolved"));
  const missing = iosApp({ iosExtra, commit: false });
  assert.throws(() => inputsOf(missing), /missing required release input: .*Package\.resolved/);
  const plain = iosApp({ commit: false });
  writeFileSync(path.join(plain.root, "Package.resolved"), PINS);
  assert.deepEqual(rel(plain, inputsOf(plain).requiredInputs), ["right-release.config.mjs", "project.yml"], "an app with no list keeps its input set exactly");
});

test("native iOS without a projectSpec: the in-xcodeproj Package.resolved is the required input", () => {
  const pinsPath = "CodeRight.xcodeproj/project.xcworkspace/xcshareddata/swiftpm/Package.resolved";
  const app = withPins({ projectSpec: false, iosExtra: { trustedSwiftPlugins: TRUSTED } }, pinsPath);
  assert.deepEqual(rel(app, inputsOf(app).requiredInputs), ["right-release.config.mjs", pinsPath]);
  const recipeTarget = { signed: true, recipe: { kind: "iosPackage", spec: validateIosPackageConfig(iosPackage({ projectSpec: undefined, trustedSwiftPlugins: TRUSTED })) } };
  assert.deepEqual(rel(app, inputsOf(app, recipeTarget).requiredInputs), ["right-release.config.mjs", pinsPath]);
});

test("src-tauri iOS (Pulse shape): a trust list adds Package.resolved, no list keeps the input set", () => {
  const iosExtra = { projectSpec: undefined, packageResolved: "Package.resolved", trustedSwiftMacros: TRUSTED };
  const tauri = withPins({ srcTauri: true, packageJson: true, lock: true, iosExtra });
  assert.equal(isNativeIosTarget({ appRoot: tauri.root, target: tauri.config.targets.ios }), false);
  assert.ok(rel(tauri, inputsOf(tauri).requiredInputs).includes("Package.resolved"));
  const pulse = iosApp({ srcTauri: true, packageJson: true, lock: true, commit: false });
  writeFileSync(path.join(pulse.root, "Package.resolved"), PINS);
  assert.ok(!rel(pulse, inputsOf(pulse).requiredInputs).includes("Package.resolved"));
});

test("Package.resolved is a git-tracked input: untracked or modified counts as dirty, committed is clean", () => {
  const app = withPins({ iosExtra: { packageResolved: "Package.resolved", trustedSwiftMacros: TRUSTED } });
  const inputs = inputsOf(app).buildInputs;
  git(app.root, "init", "--initial-branch", "main");
  git(app.root, "config", "user.email", "t@example.com");
  git(app.root, "config", "user.name", "T");
  git(app.root, "add", "right-release.config.mjs", "project.yml", "Sources");
  git(app.root, "commit", "-m", "init");
  assert.ok(dirtyBuildInputs(app.root, inputs).includes("Package.resolved"), "untracked");
  git(app.root, "add", "Package.resolved");
  git(app.root, "commit", "-m", "pins");
  assert.ok(!dirtyBuildInputs(app.root, inputs).includes("Package.resolved"), "committed");
  writeFileSync(path.join(app.root, "Package.resolved"), PINS.replace("1.0.0", "1.0.1"));
  assert.ok(dirtyBuildInputs(app.root, inputs).includes("Package.resolved"), "modified");
});

test("native iOS: a nativeAssembly Rust lock in a sibling directory is admitted through the canonical build-input inventory", async () => {
  const repo = nodeFs.realpathSync.native(tempRoot("right-ios-sibling-"));
  const appRoot = path.join(repo, "apps", "coderight");
  const engine = path.join(repo, "engine");
  mkdirSync(appRoot, { recursive: true });
  mkdirSync(engine, { recursive: true });
  const config = { schema: 1, app: "coderight", version: "1.0.0", buildInputs: { include: ["project.yml"] }, nativeAssembly: { cargoManifest: "../../engine/Cargo.toml" }, targets: { ios: { signed: true, iosPackage: iosPackage(), artifacts: ["build/CodeRight.ipa"] } } };
  const configPath = path.join(appRoot, "right-release.config.mjs");
  writeFileSync(configPath, `export default ${JSON.stringify(config)};\n`);
  writeFileSync(path.join(appRoot, "project.yml"), "name: CodeRight\n");
  writeFileSync(path.join(engine, "Cargo.toml"), '[package]\nname = "engine"\nversion = "0.0.0"\n[dependencies]\nrightkit-foo = "=0.2.0"\n');
  const sum = "2".repeat(64);
  writeFileSync(path.join(engine, "Cargo.lock"), `version = 4\n\n[[package]]\nname = "rightkit-foo"\nversion = "0.7.0"\nsource = "registry+https://github.com/rust-lang/crates.io-index"\nchecksum = "${sum}"\n`);
  const inputs = resolveReleaseBuildInputs({ repoRoot: repo, appRoot, configPath, config, target: config.targets.ios });
  assert.equal(inputs.nativeLayout, null);
  assert.deepEqual(inputs.cargoLockPaths, [path.join(engine, "Cargo.lock")], "the preflight inventory carries the sibling lock");
  assert.ok(path.relative(appRoot, inputs.cargoLockPaths[0]).startsWith(".."), "the lock lies outside the app root scan");
  const inventory = rightKitLockInventory({ requiredInputs: inputs.requiredInputs, cargoLockPaths: inputs.cargoLockPaths, nativeLayout: inputs.nativeLayout, config, root: appRoot });
  assert.deepEqual(inventory, [path.join(engine, "Cargo.lock")]);
  const fetchImpl = async (url) => {
    const u = new URL(url);
    if (u.host === "index.crates.io") return new Response([{ name: "rightkit-foo", vers: "0.2.0", cksum: sum, yanked: false }].map((r) => JSON.stringify(r)).join("\n"));
    return new Response(JSON.stringify({ crate: { max_stable_version: "0.2.0" } }));
  };
  const versions = { schema: 2, cargo: { "rightkit-foo": "0.1.0" }, registry: { cargo: { "rightkit-foo": "0.1.0" } } };
  const base = { root: appRoot, versions, ownership: { capabilities: [{ package: "rightkit-foo" }] }, fetchImpl, sleep: async () => {}, cacheDir: null, attempts: 1, warn() {} };
  // Without the inventory (the reconstructed, app-root-only list) nothing in the sibling directory was ever admitted.
  assert.equal((await resolveRightKitPins(base)).entries.length, 0);
  // With it, the sibling lock is resolved: its unpublished 0.7.0 is refused.
  await assert.rejects(resolveRightKitPins({ ...base, extraLocks: inventory }), /rightkit-foo 0\.7\.0 is not published on crates\.io/);
  // And the sibling manifest's declaration is covered by the same lock.
  writeFileSync(path.join(engine, "Cargo.lock"), `version = 4\n\n[[package]]\nname = "rightkit-foo"\nversion = "0.2.0"\nsource = "registry+https://github.com/rust-lang/crates.io-index"\nchecksum = "${sum}"\n`);
  assert.deepEqual((await resolveRightKitPins({ ...base, extraLocks: inventory })).entries.map((e) => e.version), ["0.2.0"]);
  writeFileSync(path.join(engine, "Cargo.lock"), "version = 4\n");
  await assert.rejects(resolveRightKitPins({ ...base, extraLocks: inventory }), /declares rightkit-foo but no Cargo\.lock entry proves it/);
});
