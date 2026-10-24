import test from "node:test";
import assert from "node:assert/strict";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { execFileSync, spawnSync } from "./background-process.mjs";
import { fileURLToPath } from "node:url";
import { resolveSwiftCacheIdentity } from "./cache-policy.mjs";
import { resolveReleaseBuildInputs } from "./release-invocation.mjs";
import { packageCommandOptional } from "./project-type.mjs";
import { collectSwiftToolVersions, planSwiftBuild, swiftArchitecture } from "./swift-build-context.mjs";
import { managedEnv } from "./test-support/fixture.mjs";

// Not run on Windows: Swift packages are built on macOS.
const WINDOWS_SKIP = process.platform === "win32" ? "Swift packages are built on macOS" : false;

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

// A SwiftPM app at HEAD: the committed Package.swift and the release config, in a repository of its own.
function swiftApp(configText) {
  const root = tmp();
  writeFileSync(path.join(root, "Package.swift"), "// swift-tools-version:6.0\n");
  writeFileSync(path.join(root, "right-release.config.mjs"), configText);
  for (const args of [["init", "--initial-branch", "main"], ["config", "user.email", "t@e.com"], ["config", "user.name", "T"], ["add", "."], ["commit", "-m", "i"]]) execFileSync("git", args, { cwd: root });
  return root;
}
const dryRun = (root, env) => spawnSync(process.execPath, [path.join(here, "build-release.mjs"), "--config", "right-release.config.mjs", "--platform", "win", "--dry-run"], { cwd: root, encoding: "utf8", ...(env ? { env } : {}) });
const rowsOf = (stderr) => [...stderr.matchAll(/^right-release build: ([a-z][a-z-]*): (.*)$/gm)].map((match) => [match[1], match[2]]);

// rightkit#946: a config that declares no schema, no buildInputs.include, nothing to seal and no distribution is refused by the build's
// admission, row by row, before anything starts. A SwiftPM app needs no package.json, Cargo or rust toolchain, and none is asked for.
test("build --dry-run refuses a minimal swiftpm config with the rows that name what it lacks, and asks for no package.json, Cargo or rust toolchain", { skip: WINDOWS_SKIP }, () => {
  const root = swiftApp("export default { app: 'swiftfixture', version: '1.0.0', targets: { win: { projectType: 'swiftpm', signed: true, package: { cmd: 'true' } } } };\n");
  const result = dryRun(root);
  assert.equal(result.status, 1, `${result.stdout}\n${result.stderr}`);
  const found = rowsOf(result.stderr);
  const byRow = Object.fromEntries(found);
  assert.equal(byRow["config-identity"], "unsupported config schema: <missing> (expected 1)");
  assert.equal(byRow["build-inputs"], "release config must declare non-empty buildInputs.include paths");
  assert.equal(byRow["seal-routes"], "nothing to seal: declare installer.artifacts, updater.artifacts or artifacts for the win target");
  assert.equal(byRow["distribution-provider"], "release config distribution.provider must be r2-rightapps or github-releases");
  // the only other row that can speak is the host probe for the one tool a SwiftPM build runs, and only where swift is not installed
  assert.deepEqual(found.map(([row]) => row).filter((row) => !["config-identity", "build-inputs", "seal-routes", "distribution-provider"].includes(row)).map((row) => `${row}: ${byRow[row]}`).filter((line) => line !== "host-tools: required executable not found: swift"), []);
  assert.doesNotMatch(result.stderr, /cargo|rustc|rustup|package\.json|pnpm/i);
  assert.match(result.stderr, /right-release build: admission found \d+ blocking problem\(s\); nothing was started/);
  assert.doesNotMatch(result.stderr, /\n\s+at .*\.mjs:\d+/, "a refusal prints its message, never a stack");
  assert.doesNotMatch(result.stdout, /build dry-run/, "a refused dry run prints no plan");
  assert.equal(existsSync(path.join(root, ".right-release")), false);
  rmSync(root, { recursive: true });
});

test("build --dry-run accepts a swiftpm config that declares its schema, buildInputs.include, an artifact and its distribution, with swift on PATH", { skip: WINDOWS_SKIP }, () => {
  const root = swiftApp("export default { schema: 1, app: 'swiftfixture', version: '1.0.0', distribution: { provider: 'r2-rightapps' }, targets: { win: { projectType: 'swiftpm', signed: true, package: { cmd: 'true' }, buildInputs: { include: ['Package.swift'] }, artifacts: ['dist/Fixture.zip'] } } };\n");
  const tools = tmp();
  writeFileSync(path.join(tools, "swift"), '#!/bin/sh\necho "Swift version 6.2"\n', { mode: 0o755 });
  const config = path.join(root, "right-release.config.mjs");
  const result = dryRun(root, managedEnv(config, { tools: false, env: { PATH: `${tools}${path.delimiter}${process.env.PATH}` } }));
  assert.equal(result.status, 0, `${result.stdout}\n${result.stderr}`);
  assert.match(result.stdout, /build dry-run/);
  assert.deepEqual(rowsOf(result.stderr), [], "no row refused it");
  assert.match(result.stdout, /\[ok {4}\] host-tools/);
  assert.match(result.stdout, /\[ok {4}\] seal-routes/);
  assert.equal(existsSync(path.join(root, ".right-release")), false, "a dry run starts nothing");
  rmSync(root, { recursive: true });
  rmSync(tools, { recursive: true });
});

test("swiftpm mac/win targets need no separate package command; tauri targets still do", () => {
  assert.equal(packageCommandOptional({ prePackage: { cmd: "sh" } }, "swiftpm"), true);
  assert.equal(packageCommandOptional({ signingContract: "windows-msix-signed-v1" }, "swiftpm"), true);
  assert.equal(packageCommandOptional({ prePackage: { cmd: "sh" } }, "tauri"), false);
  assert.equal(packageCommandOptional({}, "swiftpm"), false);
});
