import assert from "node:assert/strict";
import { mkdtempSync, mkdirSync, realpathSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "./background-process.mjs";
import { test } from "node:test";
import { cargoProfileDirectory, DEVELOPMENT_PROFILE, assertInstalledExecutableMatches, assertNativeDevelopmentHost, developmentEnvironment, resolveCommand, resolvePnpmCli, resolveTauriArtifact, runDevelopment as runDevelopmentActual, swapInstalledMacosBundle, validateDevelopmentConfig } from "./development.mjs";
import { assertReleaseExecutionAllowed } from "./execution-policy.mjs";

// Not run on Windows: installs a macOS app bundle through POSIX paths.
const WINDOWS_SKIP = process.platform === "win32" ? "installs a macOS app bundle through POSIX paths" : false;

const nativePlatform = process.platform === "win32" ? "win" : "mac";
// These fixtures test development mechanics without installing host shims.
const runDevelopment = (options) => runDevelopmentActual({ codesignEnvironment: (env) => env, ...options });

test("dev configuration requires explicit build commands and valid installers", () => {
  assert.throws(() => validateDevelopmentConfig({}, "win"), /non-empty command array/);
  assert.throws(() => validateDevelopmentConfig({ development: { targets: { win: { build: [{ cmd: "pnpm" }], install: { kind: "tauri", productName: "App" } } } } }, "win"), /targetTriple/);
  assert.equal(validateDevelopmentConfig({ development: { targets: { mac: { build: [{ cmd: "pnpm", args: ["build"] }] } } } }, "mac").build.length, 1);
  assert.equal(validateDevelopmentConfig({ development: { targets: { win: { build: [{ cmd: "pnpm" }], install: { kind: "portable", installRoot: "C:\\Apps\\Example", activation: { kind: "current" }, sourceDirectory: "target/debug", entrypoint: "example.exe" } } } } }, "win").install.kind, "portable");
});

test("native target selection fails closed on another host", () => {
  assert.throws(() => assertNativeDevelopmentHost("win", "darwin"), /requires its native host/);
  assert.throws(() => assertNativeDevelopmentHost("mac", "win32"), /requires its native host/);
  assert.doesNotThrow(() => assertNativeDevelopmentHost("win", "win32"));
});

test("development environment preserves Team identity and disables release-only signing material", () => {
  const env = developmentEnvironment({ APPLE_ID: "secret", APPLE_API_KEY: "release-only", APPLE_API_ISSUER: "release-only", APPLE_API_KEY_PATH: "release-only", APPLE_CERTIFICATE: "release-only", APPLE_CERTIFICATE_PASSWORD: "release-only", TAURI_SIGNING_PRIVATE_KEY: "secret", APPLE_SIGNING_IDENTITY: "Developer ID Application: Fixture (6KLGD3LLKF)", APPLE_DEVELOPER_ID: "configured", APPLE_TEAM_ID: "6KLGD3LLKF", KEEP: "yes" });
  assert.equal(env.RIGHTKIT_BUILD_MODE, "dev");
  assert.equal(env.RIGHTKIT_CARGO_PROFILE, "dev");
  assert.equal(env.TAURI_SIGNING_PRIVATE_KEY, "");
  for (const key of ["APPLE_ID", "APPLE_PASSWORD", "APPLE_API_KEY", "APPLE_API_ISSUER", "APPLE_API_KEY_PATH", "APPLE_CERTIFICATE", "APPLE_CERTIFICATE_PASSWORD"]) assert.equal(env[key], undefined);
  assert.equal(env.KEEP, "yes");
  assert.equal(env.APPLE_SIGNING_IDENTITY, "Developer ID Application: Fixture (6KLGD3LLKF)");
  assert.equal(env.APPLE_DEVELOPER_ID, "configured");
  assert.equal(env.APPLE_TEAM_ID, "6KLGD3LLKF");
  assert.equal(developmentEnvironment({}).APPLE_SIGNING_IDENTITY, undefined);
  assert.equal(env.CARGO_INCREMENTAL, undefined);
});

test("Windows pnpm command resolves to its JavaScript CLI and node uses current runtime", () => {
  const pnpm = resolveCommand("pnpm", ["exec", "node", "-e", "console.log('x')"], { root: process.cwd() });
  assert.equal(pnpm.command, process.execPath);
  assert.match(pnpm.args[0], /pnpm\.(?:js|cjs|mjs)$/i);
  assert.equal(resolveCommand("node", ["--version"]).command, process.execPath);
});

test("pnpm resolver follows a Unix PATH symlink to its JavaScript CLI", { skip: process.platform === "win32" }, () => {
  const root = temp();
  try {
    const bin = path.join(root, "bin");
    const entry = path.join(root, "global", "node_modules", "pnpm", "bin", "pnpm.mjs");
    mkdirSync(bin, { recursive: true });
    mkdirSync(path.dirname(entry), { recursive: true });
    writeFileSync(entry, "// fixture\n");
    symlinkSync(entry, path.join(bin, "pnpm"));
    const wrapper = path.join(root, "wrapper-pnpm");
    writeFileSync(wrapper, "#!/bin/sh\nexit 1\n");
    assert.equal(resolvePnpmCli({ root, env: { PATH: bin, npm_execpath: wrapper } }), realpathSync(entry));
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test("public dev dry-run is read-only while actual compilation remains GitHub Actions only", () => {
  const root = temp();
  try {
    git(root, ["init", "-b", "main"]);
    git(root, ["remote", "add", "origin", "https://github.com/Orthic-Labs/example.git"]);
    writeFileSync(path.join(root, ".rightgit.json"), JSON.stringify({ compile: "github-actions-only" }));
    const config = { development: { targets: { [nativePlatform]: { build: [{ cmd: "compile" }] } } } };
    let childCalls = 0;
    const dryRun = runDevelopment({ root, platform: nativePlatform, config, dryRun: true, env: {}, run: () => { childCalls++; return { status: 0 }; } });
    assert.equal(dryRun.built, false);
    assert.equal(dryRun.plan.build[0].cmd, "compile");
    assert.equal(childCalls, 0);
    assert.throws(() => runDevelopment({ root, platform: nativePlatform, config, env: {}, run: () => { childCalls++; return { status: 0 }; } }), /GitHub Actions only/);
    assert.equal(childCalls, 0);
    assert.doesNotThrow(() => assertReleaseExecutionAllowed({ cwd: root, environment: { GITHUB_ACTIONS: "true" }, command: "right-release dev" }));
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test("install command failure is surfaced after successful dev build", () => {
  const root = temp();
  try {
    git(root, ["init", "-b", "main"]);
    const config = { development: { targets: { [nativePlatform]: { build: [{ cmd: "build" }], install: { kind: "command", cmd: "install", args: [] } } } } };
    const calls = [];
    assert.throws(() => runDevelopment({ root, platform: nativePlatform, config, env: {}, run: (cmd) => { calls.push(cmd); return { status: cmd === "install" ? 7 : 0, stdout: "", stderr: "" }; } }), /development install command failed \(7\)/);
    assert.deepEqual(calls, ["build", "install"]);
  } finally { rmSync(root, { recursive: true, force: true }); }
});

// The subprocess resolves pnpm to its JavaScript entry (PATH, a global install, or npm_execpath). A hosted runner
// exposes pnpm as a shim with no reachable JavaScript entry, so the suite passes the pnpm that runs it (set by
// `pnpm run`). With no resolvable entry the test skips with that reason; RIGHTKIT_REQUIRE_ASSETS=1 runs it so it fails.
const suitePnpm = process.env.npm_execpath ? { npm_execpath: process.env.npm_execpath } : {};
const pnpmResolvable = (() => {
  try { resolvePnpmCli({ root: os.tmpdir(), env: { PATH: process.env.PATH ?? "", ...suitePnpm } }); return true; } catch { return false; }
})();
const pnpmSkip = pnpmResolvable || process.env.RIGHTKIT_REQUIRE_ASSETS === "1" ? false : "no JavaScript pnpm entry is reachable from PATH or npm_execpath (run the suite through pnpm)";
test("pnpm subprocess preserves JSON arguments and dev environment without compiling", { skip: pnpmSkip }, () => {
  const root = temp();
  try {
    git(root, ["init", "-b", "main"]);
    writeFileSync(path.join(root, "package.json"), JSON.stringify({ name: "rightkit-dev-fixture", private: true }));
    writeFileSync(path.join(root, "pnpm-workspace.yaml"), "verifyDepsBeforeRun: false\n");
    const code = "process.stdout.write(JSON.stringify({args:process.argv.slice(1),profile:process.env.RIGHTKIT_CARGO_PROFILE,incremental:process.env.CARGO_INCREMENTAL??null}))";
    let output = "";
    runDevelopment({
      root, platform: nativePlatform, buildOnly: true,
      config: { development: { targets: { [nativePlatform]: { build: [{ cmd: "pnpm", args: ["exec", "node", "-e", code, "a \"quoted\" value"] }] } } } },
      env: { PATH: process.env.PATH ?? "", CARGO_INCREMENTAL: "1", ...suitePnpm },
      run: (command, args, options) => {
        const result = spawnSync(command, args, { ...options, stdio: "pipe", encoding: "utf8" });
        output = result.stdout;
        return result;
      },
    });
    const jsonLine = output.trim().split(/\r?\n/).at(-1);
    assert.deepEqual(JSON.parse(jsonLine), { args: ["a \"quoted\" value"], profile: "dev", incremental: null });
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test("Tauri installer resolution uses RightKit metadata target and fails missing or ambiguous output", () => {
  const root = temp();
  const target = temp();
  try {
    mkdirSync(path.join(root, "src-tauri"), { recursive: true });
    writeFileSync(path.join(root, "src-tauri", "Cargo.toml"), "[package]\nname='fixture'\nversion='0.1.0'\n");
    const descriptor = { productName: "Fixture", manifestPath: "src-tauri/Cargo.toml", targetTriple: "x86_64-pc-windows-msvc" };
    const bundleDir = path.join(target, descriptor.targetTriple, "debug", "bundle", "nsis");
    mkdirSync(bundleDir, { recursive: true });
    const calls = [];
    const runCargo = (args) => { calls.push(args); return { status: 0, stdout: JSON.stringify({ target_directory: target }) }; };
    assert.throws(() => resolveTauriArtifact(descriptor, { root, platform: "win", runCargo }), /found 0/);
    writeFileSync(path.join(bundleDir, "Fixture_x64-setup.exe"), "one");
    assert.equal(resolveTauriArtifact(descriptor, { root, platform: "win", runCargo }), path.join(bundleDir, "Fixture_x64-setup.exe"));
    writeFileSync(path.join(bundleDir, "Fixture_arm64-setup.exe"), "two");
    assert.throws(() => resolveTauriArtifact(descriptor, { root, platform: "win", runCargo }), /found 2/);
    assert.ok(calls.every((args) => args.includes("--offline") && args.includes("--no-deps")));
  } finally { rmSync(root, { recursive: true, force: true }); rmSync(target, { recursive: true, force: true }); }
});

test("Windows installed executable must match built dev bytes", () => {
  const root = temp();
  try {
    const built = path.join(root, "coderight.exe");
    const installed = path.join(root, "installed-coderight.exe");
    writeFileSync(built, "current dev build");
    writeFileSync(installed, "older installed build");
    assert.throws(() => assertInstalledExecutableMatches(built, installed), /does not match this dev build/);
    writeFileSync(installed, "current dev build");
    assert.doesNotThrow(() => assertInstalledExecutableMatches(built, installed));
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test("dev module owns no seal or upload path", async () => {
  const source = await import("node:fs/promises").then(({ readFile }) => readFile(new URL("./development.mjs", import.meta.url), "utf8"));
  assert.doesNotMatch(source, /seal|upload-release|publish-update/i);
});

function temp() { const root = mkdtempSync(path.join(os.tmpdir(), "rightkit-dev-test-")); mkdirSync(root, { recursive: true }); return root; }
function git(cwd, args) { const r = spawnSync("git", args, { cwd, encoding: "utf8", windowsHide: true }); assert.equal(r.status, 0, r.stderr); }

test("installed executable check ignores only Tauri's NSIS bundle marker rewrite", async () => {
  const { bundleMarkerBuffer } = await import("./tauri-bundle-marker.mjs");
  const root = temp();
  try {
    const built = path.join(root, "app.exe"); const installed = path.join(root, "installed-app.exe");
    const body = (code, tail = "rest") => Buffer.concat([Buffer.from("head"), bundleMarkerBuffer(code), Buffer.from(tail)]);
    writeFileSync(built, body("UNK")); writeFileSync(installed, body("NSS"));
    assert.doesNotThrow(() => assertInstalledExecutableMatches(built, installed));
    writeFileSync(installed, body("NSS", "different"));
    assert.throws(() => assertInstalledExecutableMatches(built, installed), /does not match this dev build/);
    writeFileSync(installed, Buffer.concat([body("NSS"), bundleMarkerBuffer("NSS")]));
    assert.throws(() => assertInstalledExecutableMatches(built, installed), /2 NSIS bundle markers/);
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test("Windows installer selection keeps older installers and picks this run's product installer", async () => {
  const { utimesSync } = await import("node:fs");
  const { resolveTauriBundleDirectory } = await import("./development.mjs");
  const root = temp();
  try {
    const descriptor = { productName: "HeardRight", bundleDirectory: "bundle", targetTriple: "x86_64-pc-windows-msvc" };
    const bundleDir = path.join(root, "bundle"); mkdirSync(bundleDir);
    const old = path.join(bundleDir, "HeardRight_0.1.220_x64-setup.exe"); writeFileSync(old, "old");
    const past = new Date(Date.now() - 60_000); utimesSync(old, past, past);
    writeFileSync(path.join(bundleDir, "Other_1.0.0_x64-setup.exe"), "other product");
    const sinceMs = Date.now() - 1_000;
    const current = path.join(bundleDir, "HeardRight_0.1.221_x64-setup.exe"); writeFileSync(current, "new");
    assert.equal(resolveTauriBundleDirectory(descriptor, { root, platform: "win", sinceMs }).installer, current);
    assert.throws(() => resolveTauriBundleDirectory(descriptor, { root, platform: "win" }), /found 2/);
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test("managed run lease is written under the managed root and removed on release", async () => {
  const { existsSync, readFileSync } = await import("node:fs");
  const { acquireManagedRunLease } = await import("./managed-cargo.mjs");
  const managed = temp();
  try {
    const client = path.join(managed, "runtime-current", "rustright");
    mkdirSync(path.dirname(client), { recursive: true }); writeFileSync(client, "");
    const lease = acquireManagedRunLease({ command: "right-release dev --platform win", env: { RIGHTKIT_MANAGED_ROOT: managed }, platform: "linux", pid: 4242 });
    assert.equal(path.dirname(lease.path), path.join(managed, "rightkit-build-control", "global", "runs"));
    assert.equal(JSON.parse(readFileSync(lease.path, "utf8")).pid, 4242);
    lease.release(); assert.equal(existsSync(lease.path), false);
    assert.equal(acquireManagedRunLease({ env: {}, platform: "linux" }).path, null);
  } finally { rmSync(managed, { recursive: true, force: true }); }
});

test("Windows dev install relaunches the app only when it was running", async () => {
  const { isWindowsProcessRunning, windowsInstallerArguments } = await import("./development.mjs");
  assert.deepEqual(windowsInstallerArguments({ running: false }), ["/S"]);
  assert.deepEqual(windowsInstallerArguments({ running: true }), ["/S", "/R"]);
  const listing = (stdout, status = 0) => () => ({ status, stdout });
  assert.equal(isWindowsProcessRunning("HeardRight.exe", { run: listing('"heardright.exe","4242","Console","1","120,000 K"\r\n') }), true);
  assert.equal(isWindowsProcessRunning("HeardRight.exe", { run: listing("INFO: No tasks are running which match the specified criteria.\r\n") }), false);
  assert.equal(isWindowsProcessRunning("HeardRight.exe", { run: listing("", 1) }), false);
});

// In-memory file-system seam: files are path -> string; a directory move relocates every path under it.
function memoryFs(files) {
  const under = (dir) => [...files.keys()].filter((file) => file === dir || file.startsWith(`${dir}/`));
  return {
    files,
    existsSync: (target) => under(target).length > 0,
    readFileSync: (file) => { if (!files.has(file)) throw Object.assign(new Error(`ENOENT ${file}`), { code: "ENOENT" }); return Buffer.from(files.get(file)); },
    renameSync: (from, to) => { for (const file of under(from)) { files.set(to + file.slice(from.length), files.get(file)); files.delete(file); } },
    rmSync: (target) => { for (const file of under(target)) files.delete(file); },
  };
}
const INFO = '<plist><dict><key>CFBundleExecutable</key><string>Fixture</string></dict></plist>';
function swapFixture({ installedExecutable = "signed-binary" } = {}) {
  const files = new Map([
    ["/Apps/Fixture.app.stage/Contents/Info.plist", INFO],
    ["/Apps/Fixture.app.stage/Contents/MacOS/Fixture", "signed-binary"],
    ["/Apps/Fixture.app/Contents/Info.plist", INFO],
    ["/Apps/Fixture.app/Contents/MacOS/Fixture", "previous-binary"],
  ]);
  const fs = memoryFs(files);
  // The install lands as staged, then the test decides what the installed copy looks like.
  const rename = fs.renameSync;
  fs.renameSync = (from, to) => {
    rename(from, to);
    if (from === "/Apps/Fixture.app.stage") {
      if (installedExecutable === null) files.delete("/Apps/Fixture.app/Contents/MacOS/Fixture");
      else files.set("/Apps/Fixture.app/Contents/MacOS/Fixture", installedExecutable);
    }
  };
  return { files, fs, args: { staging: "/Apps/Fixture.app.stage", destination: "/Apps/Fixture.app", executableName: "Fixture" } };
}

test("macOS dev install replaces the app in place with NO backup when the installed executable hash matches", { skip: WINDOWS_SKIP }, () => {
  const { files, fs, args } = swapFixture();
  files.set("/Apps/Fixture.app.previous/Contents/MacOS/Fixture", "older-binary");
  swapInstalledMacosBundle({ ...args, fs });
  assert.equal(files.get("/Apps/Fixture.app/Contents/MacOS/Fixture"), "signed-binary");
  assert.deepEqual([...files.keys()].filter((file) => !file.startsWith("/Apps/Fixture.app/")), [], "no .previous (a stale one is removed), no staging, no old copy anywhere");
  assert.equal([...files.values()].includes("previous-binary"), false, "the old executable survives nowhere");
});

test("macOS dev install removes the failed copy, keeps NO old app, and throws a typed error on a hash mismatch", { skip: WINDOWS_SKIP }, () => {
  const { files, fs, args } = swapFixture({ installedExecutable: "corrupted" });
  assert.throws(() => swapInstalledMacosBundle({ ...args, fs }), (error) => error.code === "DEV_INSTALL_EXECUTABLE_MISMATCH" && /does not match/.test(error.message));
  assert.deepEqual([...files.keys()], [], "the new copy and staging are gone and the old app is not restored");
});

test("macOS dev install removes the failed copy, keeps NO old app, and throws a typed error when the installed executable is missing", { skip: WINDOWS_SKIP }, () => {
  const { files, fs, args } = swapFixture({ installedExecutable: null });
  assert.throws(() => swapInstalledMacosBundle({ ...args, fs }), (error) => error.code === "DEV_INSTALL_EXECUTABLE_MISSING" && /missing/.test(error.message));
  assert.deepEqual([...files.keys()], []);
});

test("right-release dev uses Cargo's built-in dev profile, written to target/<triple>/debug", () => {
  assert.equal(DEVELOPMENT_PROFILE, "dev");
  assert.equal(cargoProfileDirectory("dev"), "debug");
  assert.equal(cargoProfileDirectory("test"), "debug");
  assert.equal(cargoProfileDirectory("release"), "release");
  assert.equal(cargoProfileDirectory("bench"), "release");
  assert.equal(cargoProfileDirectory("dist"), "dist");
});
