// Orthic-Labs/rightkit#184: installed QA stage mounts the sealed DMG / runs the sealed NSIS installer in a throwaway place.
import assert from "node:assert/strict";
import { existsSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { assertWindowsInstallAllowed, InstalledQaError, prepareInstalledArtifacts, QA_ALLOW_INSTALL_ENV, selectSealedInstaller } from "./release-qa-installed.mjs";

const SHA = (c) => c.repeat(64);
const plistFor = (mount) => `<?xml version="1.0" encoding="UTF-8"?>\n<plist version="1.0"><dict><key>system-entities</key><array><dict><key>mount-point</key><string>${mount}</string></dict></array></dict></plist>`;
const scratch = () => mkdtempSync(path.join(os.tmpdir(), "rk-qa-installed-test-"));

test("selectSealedInstaller needs exactly one DMG (mac) or setup exe (win)", () => {
  const files = [{ name: "Demo_1.0.0.dmg" }, { name: "Demo_1.0.0_x64-setup.exe" }, { name: "Demo.app.tar.gz" }];
  assert.equal(selectSealedInstaller({ files, platform: "mac" }), "Demo_1.0.0.dmg");
  assert.equal(selectSealedInstaller({ files, platform: "win" }), "Demo_1.0.0_x64-setup.exe");
  assert.throws(() => selectSealedInstaller({ files: [{ name: "a.dmg" }, { name: "b.dmg" }], platform: "mac" }), /exactly one sealed \.dmg; found 2/);
  assert.throws(() => selectSealedInstaller({ files: [{ name: "a.dmg" }], platform: "win" }), /found 0/);
});

test("mac: mounts the sealed DMG read-only and not browsable at a throwaway root, exposes the app, detaches and removes on cleanup", () => {
  const sealedDir = scratch();
  const mount = scratch();
  mkdirSync(path.join(mount, "Demo.app"));
  const calls = [];
  const spawn = (cmd, args) => {
    calls.push([cmd, ...args]);
    if (args[0] === "attach") return { status: 0, stdout: plistFor(mount), stderr: "" };
    return { status: 0, stdout: "", stderr: "" };
  };
  try {
    const opened = prepareInstalledArtifacts({ sealedDir, manifest: { files: [{ name: "Demo.dmg", sha256: SHA("a") }] }, platform: "mac", deps: { spawn, hostPlatform: "darwin", env: {} } });
    const attach = calls[0];
    assert.equal(attach[0], "/usr/bin/hdiutil");
    for (const flag of ["-readonly", "-nobrowse", "-noautoopen"]) assert.ok(attach.includes(flag), flag);
    assert.equal(attach.at(-1), path.join(sealedDir, "Demo.dmg"));
    assert.ok(attach[attach.indexOf("-mountrandom") + 1].startsWith(os.tmpdir()), "mount root is under the OS temp directory");
    assert.equal(opened.installedApp, path.join(mount, "Demo.app"));
    assert.equal(opened.env.RIGHT_RELEASE_QA_INSTALLED_APP, path.join(mount, "Demo.app"));
    assert.equal(opened.env.RIGHT_RELEASE_QA_INSTALLED_KIND, "dmg");
    assert.ok(opened.qaRoot.startsWith(os.tmpdir()) && existsSync(opened.qaRoot));
    assert.equal(calls.some((call) => call.join(" ").includes("/Applications")), false, "nothing is installed into /Applications");
    opened.cleanup();
    assert.deepEqual(calls.at(-1), ["/usr/bin/hdiutil", "detach", mount]);
    assert.equal(existsSync(opened.qaRoot), false);
  } finally { rmSync(sealedDir, { recursive: true, force: true }); rmSync(mount, { recursive: true, force: true }); }
});

test("mac: a failed attach or an ambiguous DMG throws and leaves nothing mounted", () => {
  const sealedDir = scratch();
  const mount = scratch();
  mkdirSync(path.join(mount, "A.app")); mkdirSync(path.join(mount, "B.app"));
  const manifest = { files: [{ name: "Demo.dmg" }] };
  try {
    assert.throws(
      () => prepareInstalledArtifacts({ sealedDir, manifest, platform: "mac", deps: { spawn: () => ({ status: 1, stdout: "", stderr: "no" }), hostPlatform: "darwin" } }),
      (error) => error instanceof InstalledQaError && error.code === "INSTALLED_QA_MOUNT",
    );
    const calls = [];
    const spawn = (cmd, args) => { calls.push(args[0]); return args[0] === "attach" ? { status: 0, stdout: plistFor(mount), stderr: "" } : { status: 0, stdout: "", stderr: "" }; };
    assert.throws(() => prepareInstalledArtifacts({ sealedDir, manifest, platform: "mac", deps: { spawn, hostPlatform: "darwin" } }), /exactly one \.app/);
    assert.deepEqual(calls, ["attach", "detach"]);
  } finally { rmSync(sealedDir, { recursive: true, force: true }); rmSync(mount, { recursive: true, force: true }); }
});

test("hosts: mounting a DMG needs macOS and the NSIS installer needs Windows", () => {
  const manifest = { files: [{ name: "Demo.dmg" }, { name: "Demo-setup.exe" }] };
  assert.throws(() => prepareInstalledArtifacts({ sealedDir: "/x", manifest, platform: "mac", deps: { hostPlatform: "win32" } }), /needs macOS/);
  assert.throws(() => prepareInstalledArtifacts({ sealedDir: "/x", manifest, platform: "win", deps: { hostPlatform: "darwin" } }), /needs Windows/);
});

test("windows: the installer is refused unless on hosted CI or explicitly allowed", () => {
  assert.throws(() => assertWindowsInstallAllowed({}), (error) => error.code === "INSTALLED_QA_INSTALL_NOT_ALLOWED");
  assert.throws(() => assertWindowsInstallAllowed({ GITHUB_ACTIONS: "true" }), /VM or throwaway host/);
  assertWindowsInstallAllowed({ [QA_ALLOW_INSTALL_ENV]: "1" });
  assertWindowsInstallAllowed({ GITHUB_ACTIONS: "true", RUNNER_ENVIRONMENT: "github-hosted" });
  const spawn = () => assert.fail("installer must not run when refused");
  assert.throws(() => prepareInstalledArtifacts({ sealedDir: "/x", manifest: { files: [{ name: "Demo-setup.exe" }] }, platform: "win", deps: { spawn, hostPlatform: "win32", env: {} } }), /VM or throwaway host/);
});

test("windows: runs the sealed installer silently into a throwaway dir with /D last, uninstalls on cleanup", () => {
  const sealedDir = scratch();
  const calls = [];
  const spawn = (cmd, args) => {
    calls.push([cmd, ...args]);
    const dirArg = args.find((arg) => arg.startsWith("/D="));
    if (dirArg) { writeFileSync(path.join(dirArg.slice(3), "Demo.exe"), "x"); writeFileSync(path.join(dirArg.slice(3), "uninstall.exe"), "x"); }
    return { status: 0, stdout: "", stderr: "" };
  };
  try {
    const opened = prepareInstalledArtifacts({ sealedDir, manifest: { files: [{ name: "Demo_1.0.0_x64-setup.exe" }] }, platform: "win", deps: { spawn, hostPlatform: "win32", env: { [QA_ALLOW_INSTALL_ENV]: "1" } } });
    const [installer, ...args] = calls[0];
    assert.equal(installer, path.join(sealedDir, "Demo_1.0.0_x64-setup.exe"));
    assert.equal(args[0], "/S");
    assert.match(args.at(-1), /^\/D=/);
    assert.ok(opened.installedDir.startsWith(os.tmpdir()));
    assert.equal(opened.installedApp, path.join(opened.installedDir, "Demo.exe"));
    assert.equal(opened.env.RIGHT_RELEASE_QA_INSTALLED_KIND, "nsis");
    opened.cleanup();
    const [uninstaller, ...uninstallArgs] = calls.at(-1);
    assert.equal(uninstaller, path.join(opened.installedDir, "uninstall.exe"));
    assert.deepEqual(uninstallArgs, ["/S", `_?=${opened.installedDir}`]);
    assert.equal(existsSync(opened.installedDir), false);
    assert.equal(existsSync(opened.qaRoot), false);
  } finally { rmSync(sealedDir, { recursive: true, force: true }); }
});

test("windows: an install directory outside the throwaway root needs the explicit opt-in, and a failing installer is cleaned up", () => {
  const sealedDir = scratch();
  const manifest = { files: [{ name: "Demo-setup.exe" }] };
  const outside = path.join(path.parse(os.tmpdir()).root, "rk-qa-never-created");
  try {
    assert.throws(
      () => prepareInstalledArtifacts({ sealedDir, manifest, platform: "win", installDir: outside, deps: { spawn: () => assert.fail("must not run"), hostPlatform: "win32", env: { GITHUB_ACTIONS: "true", RUNNER_ENVIRONMENT: "github-hosted" } } }),
      (error) => error.code === "INSTALLED_QA_INSTALL_DIR",
    );
    assert.equal(existsSync(outside), false);
    assert.throws(
      () => prepareInstalledArtifacts({ sealedDir, manifest, platform: "win", deps: { spawn: () => ({ status: 5, stdout: "", stderr: "denied" }), hostPlatform: "win32", env: { [QA_ALLOW_INSTALL_ENV]: "1" } } }),
      (error) => error.code === "INSTALLED_QA_INSTALL" && /denied/.test(error.message),
    );
  } finally { rmSync(sealedDir, { recursive: true, force: true }); }
});
