import test from "node:test";
import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { execFileSync, spawnSync } from "./background-process.mjs";
import { fileURLToPath } from "node:url";
import { resolveSwiftCacheIdentity } from "./cache-policy.mjs";
import { resolveReleaseBuildInputs } from "./release-invocation.mjs";
import { packageCommandOptional } from "./project-type.mjs";
import { collectSwiftToolVersions, planSwiftBuild, swiftArchitecture } from "./swift-build-context.mjs";

const tmp = () => mkdtempSync(path.join(os.tmpdir(), "rk-swiftbuild-"));
const here = path.dirname(fileURLToPath(import.meta.url));

test("swift cache identity binds toolchain, Package.resolved, arch and profile; no Rust inputs", () => {
  const dir = tmp();
  const resolved = path.join(dir, "Package.resolved");
  const base = { swiftVersion: "Swift version 6.2", packageResolvedPath: resolved, architecture: "arm64" };
  const none = resolveSwiftCacheIdentity(base);
  assert.equal(none.packageResolvedSha256, "none");
  writeFileSync(resolved, '{"pins":[]}\n');
  const pinned = resolveSwiftCacheIdentity(base);
  assert.notEqual(pinned.fingerprint, none.fingerprint);
  assert.equal(resolveSwiftCacheIdentity({ ...base }).fingerprint, pinned.fingerprint);
  writeFileSync(resolved, '{"pins":[],"x":1}\n');
  assert.notEqual(resolveSwiftCacheIdentity(base).fingerprint, pinned.fingerprint);
  assert.notEqual(resolveSwiftCacheIdentity({ ...base, swiftVersion: "Swift version 6.3" }).fingerprint, pinned.fingerprint);
  assert.notEqual(resolveSwiftCacheIdentity({ ...base, profile: "debug" }).fingerprint, resolveSwiftCacheIdentity(base).fingerprint);
  assert.equal(pinned.rustc, undefined);
  assert.throws(() => resolveSwiftCacheIdentity({ ...base, swiftVersion: "" }), /toolchain/);
  assert.throws(() => resolveSwiftCacheIdentity({ ...base, architecture: "a b" }), /architecture/);
  rmSync(dir, { recursive: true });
});

test("swift tool versions never probe rustc/cargo/sccache", () => {
  const calls = [];
  const v = collectSwiftToolVersions({ packageManager: undefined, exec: (c, a) => { calls.push(c); return "Swift version 6.2\n"; }, nodeVersion: "v26" });
  assert.deepEqual(calls, ["swift"]);
  assert.equal(v.rustc, null);
  assert.equal(v.swift, "Swift version 6.2");
  assert.throws(() => collectSwiftToolVersions({ exec: () => "" }), /printed nothing/);
});

test("swift build plan: legacy cache mode, no cargo target, rust env stripped, swift build dir set", () => {
  const plan = planSwiftBuild({ appRoot: "/app", vaultRoot: "/vault", platform: "win", target: { msix: { identity: { architecture: "arm64" } } }, toolVersions: { swift: "Swift 6.2" } });
  assert.equal(plan.cacheMode, "legacy");
  assert.equal(plan.managedCargoTarget, null);
  assert.equal(plan.architecture, "arm64");
  assert.equal(plan.identity.architecture, "arm64");
  const env = plan.sanitizeEnv({ PATH: "p", CARGO_TARGET_DIR: "x", RUSTC_WRAPPER: "sccache", SCCACHE_DIR: "y", RIGHT_RELEASE_CACHE_OWNER: "rightkit-v2" });
  assert.deepEqual(Object.keys(env).sort(), ["PATH", "RIGHT_RELEASE_SWIFT_BUILD_DIR"]);
  assert.ok(env.RIGHT_RELEASE_SWIFT_BUILD_DIR.includes(path.join("swift-build", "win", plan.identity.fingerprint)));
  let ran = false;
  assert.equal(plan.targetBridge.run(() => { ran = true; return 7; }), 7);
  assert.ok(ran);
  assert.equal(swiftArchitecture({ target: {}, platform: "mac", hostArch: "x64" }), "x86_64");
});

test("build inputs for a swiftpm app need Package.swift only; Cargo and Node manifests are not required", () => {
  const dir = tmp();
  writeFileSync(path.join(dir, "right-release.config.mjs"), "");
  const config = { app: "a", version: "1.0.0", buildInputs: { include: ["Package.swift"] } }, target = { projectType: "swiftpm" };
  assert.throws(() => resolveReleaseBuildInputs({ repoRoot: dir, appRoot: dir, configPath: path.join(dir, "right-release.config.mjs"), config, target }), /Package\.swift/);
  writeFileSync(path.join(dir, "Package.swift"), "// swift-tools-version:6.0\n");
  const r = resolveReleaseBuildInputs({ repoRoot: dir, appRoot: dir, configPath: path.join(dir, "right-release.config.mjs"), config, target });
  assert.deepEqual(r.requiredInputs.map((f) => path.basename(f)), ["right-release.config.mjs", "Package.swift"]);
  writeFileSync(path.join(dir, "Package.resolved"), "{}");
  assert.ok(resolveReleaseBuildInputs({ repoRoot: dir, appRoot: dir, configPath: path.join(dir, "right-release.config.mjs"), config, target }).requiredInputs.some((f) => f.endsWith("Package.resolved")));
  // Tauri default still demands its manifests.
  assert.throws(() => resolveReleaseBuildInputs({ repoRoot: dir, appRoot: dir, configPath: path.join(dir, "right-release.config.mjs"), config, target: { projectType: "tauri" } }), /native Cargo manifest missing/);
  rmSync(dir, { recursive: true });
});

test("build-release wires the swift plan through every Rust-only step", () => {
  const src = readFileSync(path.join(here, "build-release.mjs"), "utf8");
  for (const re of [
    /isSwiftBuild \? collectSwiftToolVersions\(/, /swiftPlan \? swiftPlan\.identity : resolveSharedCacheIdentity\(/,
    /swiftPlan \? swiftPlan\.managedCargoTarget : brokerManagedHost/, /swiftPlan \? swiftPlan\.cacheMode : resolveReleaseCacheMode/,
    /swiftPlan \? swiftPlan\.sanitizeEnv\(baseEnv\)/, /swiftPlan \? swiftPlan\.targetBridge : createTargetBridge/,
    /swiftPlan \? swiftPlan\.buildDir/, /swiftPlan \? swiftPlan\.watchDirs/, /target\.package\?\.livenessOwnerTimeoutMs/,
  ]) assert.match(src, re);
});

test("build --dry-run accepts a minimal swiftpm config with no package.json, Cargo, or rust toolchain", () => {
  const root = tmp();
  writeFileSync(path.join(root, "Package.swift"), "// swift-tools-version:6.0\n");
  writeFileSync(path.join(root, "right-release.config.mjs"), "export default { app: 'swiftfixture', version: '1.0.0', targets: { win: { projectType: 'swiftpm', signed: true, package: { cmd: 'true' } } } };\n");
  for (const args of [["init", "--initial-branch", "main"], ["config", "user.email", "t@e.com"], ["config", "user.name", "T"], ["add", "."], ["commit", "-m", "i"]]) execFileSync("git", args, { cwd: root });
  const result = spawnSync(process.execPath, [path.join(here, "build-release.mjs"), "--config", "right-release.config.mjs", "--platform", "win", "--dry-run"], { cwd: root, encoding: "utf8" });
  assert.equal(result.status, 0, result.stderr);
  assert.match(result.stdout, /build dry-run/);
  rmSync(root, { recursive: true });
});

test("swiftpm mac/win targets need no separate package command; tauri targets still do", () => {
  assert.equal(packageCommandOptional({ prePackage: { cmd: "sh" } }, "swiftpm"), true);
  assert.equal(packageCommandOptional({ signingContract: "windows-msix-signed-v1" }, "swiftpm"), true);
  assert.equal(packageCommandOptional({ prePackage: { cmd: "sh" } }, "tauri"), false);
  assert.equal(packageCommandOptional({}, "swiftpm"), false);
});
