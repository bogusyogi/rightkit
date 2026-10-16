// Installed QA stage artifact preparation (Orthic-Labs/rightkit#184).
//
// `right-release qa --stage installed` used to hand the sealed directory to the QA runner and nothing more:
// install-app is Mac-only and nothing mounted or ran the sealed artifacts. This module opens the exact sealed
// bytes the receipt will name, without touching real user data:
//
//   macOS    mounts the sealed DMG read-only and not browsable (`hdiutil attach -readonly -nobrowse -noautoopen`)
//            at a throwaway mount root under the OS temp directory, and exposes the .app inside the mount.
//            Nothing is copied to /Applications and no user data directory is read or written.
//   Windows  runs the sealed NSIS installer silently (`/S /D=<throwaway dir>`; /D is the NSIS install-dir
//            override and must be last) into a directory under the OS temp directory. Running an installer registers
//            an uninstall entry for the current user, so it needs an explicit opt-in on a VM or throwaway host
//            (`RIGHT_RELEASE_QA_ALLOW_INSTALL=1`) or GitHub-hosted CI. It is refused when an install directory
//            outside the throwaway root is requested without that opt-in. Cleanup runs the installed uninstaller
//            silently and removes the directory.
//
// Both paths also give the runner a throwaway QA data root (`RIGHT_RELEASE_QA_ROOT`) so the app under QA never
// has to use the real per-user data location. The caller owns `cleanup()`; it never throws.
import { existsSync, mkdirSync, mkdtempSync, readdirSync, rmSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync as defaultSpawnSync } from "./background-process.mjs";
import { detachWithRetrySync } from "./hdiutil-detach.mjs";
import { mountPointsFromAttach } from "./install-app.mjs";

export const QA_ALLOW_INSTALL_ENV = "RIGHT_RELEASE_QA_ALLOW_INSTALL";

export class InstalledQaError extends Error {
  constructor(code, message) {
    super(message);
    this.name = "InstalledQaError";
    this.code = code;
  }
}

const lower = (name) => String(name).toLowerCase();

/** The one sealed DMG (macOS) or NSIS setup executable (Windows). Anything else is ambiguous and refused. */
export function selectSealedInstaller({ files, platform }) {
  const names = (files ?? []).map((file) => file.name);
  const pick = platform === "win"
    ? names.filter((name) => lower(name).endsWith(".exe") && lower(name).includes("setup"))
    : names.filter((name) => lower(name).endsWith(".dmg"));
  if (pick.length !== 1) {
    throw new InstalledQaError("INSTALLED_QA_ARTIFACT", `installed QA needs exactly one sealed ${platform === "win" ? "NSIS setup .exe" : ".dmg"}; found ${pick.length}${pick.length ? ` (${pick.join(", ")})` : ""}`);
  }
  return pick[0];
}

/** Windows installers write an uninstall entry: only on CI, or when the operator opted in on a VM / throwaway host. */
export function assertWindowsInstallAllowed(env = process.env) {
  const ci = env.GITHUB_ACTIONS === "true" && env.RUNNER_ENVIRONMENT === "github-hosted";
  if (ci || env[QA_ALLOW_INSTALL_ENV] === "1") return;
  throw new InstalledQaError("INSTALLED_QA_INSTALL_NOT_ALLOWED", `running the sealed NSIS installer registers the app for the current user; run it on a VM or throwaway host and set ${QA_ALLOW_INSTALL_ENV}=1 (never on a machine holding real app data)`);
}

const describe = (result) => `${result?.stderr ?? ""}${result?.stdout ?? ""}`.trim() || result?.error?.message || `exit ${result?.status}`;

function prepareMac({ sealedDir, name, tmpdir, spawn, log }) {
  const dmg = path.join(sealedDir, name);
  const mountRoot = mkdtempSync(path.join(tmpdir, "right-qa-dmg-"));
  let mountPoint = null;
  const cleanup = () => {
    if (mountPoint) {
      const out = detachWithRetrySync({ mount: mountPoint, log, detach: (args) => spawn("/usr/bin/hdiutil", args, { encoding: "utf8", windowsHide: true }) });
      if (!out.ok) log({ event: "installed-qa-detach-failed", mount: mountPoint });
      mountPoint = null;
    }
    try { rmSync(mountRoot, { recursive: true, force: true }); } catch { /* best effort */ }
  };
  const attached = spawn("/usr/bin/hdiutil", ["attach", "-nobrowse", "-readonly", "-noautoopen", "-mountrandom", mountRoot, "-plist", dmg], { encoding: "utf8", windowsHide: true, maxBuffer: 16 * 1024 * 1024 });
  if (attached.status !== 0) { cleanup(); throw new InstalledQaError("INSTALLED_QA_MOUNT", `hdiutil attach failed for ${name}: ${describe(attached)}`); }
  let mounts;
  try { mounts = mountPointsFromAttach(attached.stdout); } catch (error) { cleanup(); throw new InstalledQaError("INSTALLED_QA_MOUNT", `cannot read hdiutil attach output for ${name}: ${error.message}`); }
  if (!mounts.length) { cleanup(); throw new InstalledQaError("INSTALLED_QA_MOUNT", `${name} attached but exposed no mount point`); }
  mountPoint = mounts[0];
  const apps = [];
  for (const mount of mounts) for (const entry of readdirSync(mount)) if (entry.endsWith(".app")) apps.push(path.join(mount, entry));
  if (apps.length !== 1) { cleanup(); throw new InstalledQaError("INSTALLED_QA_APP", `expected exactly one .app in ${name}; found ${apps.length}`); }
  log({ event: "installed-qa-mounted", dmg: name, mount: mountPoint, app: apps[0] });
  return { kind: "dmg", installedApp: apps[0], installedDir: mountPoint, cleanup };
}

function prepareWindows({ sealedDir, name, tmpdir, spawn, log, env, installDir }) {
  assertWindowsInstallAllowed(env);
  const installer = path.join(sealedDir, name);
  const root = mkdtempSync(path.join(tmpdir, "right-qa-nsis-"));
  const target = installDir ?? path.join(root, "app");
  if (!path.isAbsolute(target)) { rmSync(root, { recursive: true, force: true }); throw new InstalledQaError("INSTALLED_QA_INSTALL_DIR", `install directory must be absolute: ${target}`); }
  if (installDir && !path.resolve(installDir).toLowerCase().startsWith(path.resolve(tmpdir).toLowerCase() + path.sep) && env[QA_ALLOW_INSTALL_ENV] !== "1") {
    rmSync(root, { recursive: true, force: true });
    throw new InstalledQaError("INSTALLED_QA_INSTALL_DIR", `install directory ${installDir} is outside the throwaway root and ${QA_ALLOW_INSTALL_ENV}=1 is not set`);
  }
  const cleanup = () => {
    try {
      const uninstaller = path.join(target, "uninstall.exe");
      // `_?=<dir>` keeps the uninstaller in place and synchronous so the directory can be removed afterwards.
      if (existsSync(uninstaller)) spawn(uninstaller, ["/S", `_?=${target}`], { encoding: "utf8", windowsHide: true, shell: false });
    } catch { /* best effort */ }
    try { rmSync(root, { recursive: true, force: true }); } catch { /* best effort */ }
    if (installDir) { try { rmSync(target, { recursive: true, force: true }); } catch { /* best effort */ } }
  };
  mkdirSync(target, { recursive: true });
  // NSIS: /S silent, /D=<dir> install directory; /D must be the last argument, unquoted.
  const result = spawn(installer, ["/S", `/D=${target}`], { encoding: "utf8", windowsHide: true, shell: false });
  if (result.status !== 0) { cleanup(); throw new InstalledQaError("INSTALLED_QA_INSTALL", `sealed installer ${name} failed: ${describe(result)}`); }
  const exes = existsSync(target) ? readdirSync(target).filter((entry) => lower(entry).endsWith(".exe") && lower(entry) !== "uninstall.exe") : [];
  log({ event: "installed-qa-installed", installer: name, dir: target, executables: exes });
  return { kind: "nsis", installedApp: exes.length === 1 ? path.join(target, exes[0]) : null, installedDir: target, cleanup };
}

/**
 * Open the sealed installer for the runner. Returns `{ kind, installedApp, installedDir, qaRoot, env, cleanup }`.
 * `env` is merged into the runner environment; `expand` placeholders are `{installedApp}`, `{installedDir}`, `{qaRoot}`.
 */
export function prepareInstalledArtifacts({ sealedDir, manifest, platform, installDir = null, deps = {} }) {
  const { spawn = defaultSpawnSync, tmpdir = os.tmpdir(), env = process.env, log = () => {}, hostPlatform = process.platform } = deps;
  if (platform === "mac" && hostPlatform !== "darwin") throw new InstalledQaError("INSTALLED_QA_HOST", "mounting the sealed DMG needs macOS");
  if (platform === "win" && hostPlatform !== "win32") throw new InstalledQaError("INSTALLED_QA_HOST", "running the sealed NSIS installer needs Windows");
  const name = selectSealedInstaller({ files: manifest.files, platform });
  const opened = platform === "win"
    ? prepareWindows({ sealedDir, name, tmpdir, spawn, log, env, installDir })
    : prepareMac({ sealedDir, name, tmpdir, spawn, log });
  const qaRoot = mkdtempSync(path.join(tmpdir, "right-qa-root-"));
  const cleanup = () => { opened.cleanup(); try { rmSync(qaRoot, { recursive: true, force: true }); } catch { /* best effort */ } };
  return {
    kind: opened.kind, installedApp: opened.installedApp, installedDir: opened.installedDir, qaRoot, cleanup,
    env: {
      RIGHT_RELEASE_QA_INSTALLED_KIND: opened.kind,
      RIGHT_RELEASE_QA_INSTALLED_ARTIFACT: name,
      RIGHT_RELEASE_QA_INSTALLED_DIR: opened.installedDir,
      ...(opened.installedApp ? { RIGHT_RELEASE_QA_INSTALLED_APP: opened.installedApp } : {}),
      RIGHT_RELEASE_QA_ROOT: qaRoot,
    },
  };
}
