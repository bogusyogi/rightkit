// `install.kind: "tauri-devid"`: install a freshly built Tauri .app at ONE fixed path, signed with the stable
// Developer ID, so macOS privacy grants (TCC) and Keychain code requirements survive every rebuild.
//
// Pipeline (installTauriDevId): copy the built .app to a private staging copy -> sign nested code, then the app,
// with the stable Developer ID (signMacosDevelopmentBundle) -> assert the designated requirement equals the
// installed copy's (assertStableDesignatedRequirement) -> install at the fixed path with NO backup of the old copy
// (installApp, dev mode; Orthic-Labs/rightkit#348) -> verify the installed copy; any verify failure removes the new
// copy and leaves the path absent (reinstall from an older DMG). Nothing ever creates `<dest>.previous`.
// resetTauriDevId removes the install, only when asked (mode "remove").
//
// Every external call (copy, sign, run, install, file system) is injectable through `deps`; the defaults are the
// real RightKit pieces. macOS paths are POSIX, so this module uses path.posix throughout.
import { createHash } from "node:crypto";
import { cpSync, existsSync, mkdtempSync, readFileSync, renameSync, rmSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "./background-process.mjs";
import { assertSignedIdentities, assertStableDesignatedRequirement, readDevSigningSettings, signMacosDevelopmentBundle } from "./development-signing.mjs";
import { installApp } from "./install-app.mjs";
import { previousPathOf } from "./in-place-install.mjs"; // only to clear a stale `.previous` an older RightKit left; never to create one

export const TAURI_DEVID_KIND = "tauri-devid";

export class TauriDevIdError extends Error {
  constructor(code, message, details = {}) {
    super(message);
    this.name = "TauriDevIdError";
    this.code = code;
    Object.assign(this, details);
  }
}

const P = path.posix;
const absolute = (appRoot, value) => (P.isAbsolute(value) ? P.normalize(value) : P.join(appRoot, value));
const text = (result) => `${result?.stdout ?? ""}\n${result?.stderr ?? ""}`.trim();

/** Real implementations of every external call. Tests replace any of them. */
export function defaultDeps() {
  return {
    run: spawnSync,
    sign: signMacosDevelopmentBundle,
    assertRequirement: assertStableDesignatedRequirement,
    assertIdentities: assertSignedIdentities,
    installApp,
    exists: existsSync,
    makeTemp: () => mkdtempSync(P.join(os.tmpdir(), "rightkit-tauri-devid-")),
    copyApp: (from, to) => cpSync(from, to, { recursive: true, verbatimSymlinks: true }),
    rename: renameSync,
    remove: (target) => rmSync(target, { recursive: true, force: true }),
    readFile: readFileSync,
    log: (line) => console.log(line),
  };
}

const invalid = (message) => new TauriDevIdError("TAURI_DEVID_CONFIG_INVALID", message);
const SAME_IDENTITY_RULE = "dev builds use the SAME install path and bundle id as production (else permission issues), replaced in place with no backup copy; no separate .dev identity";

/** The tauri config file for a descriptor: `install.tauriConfig` (contained, relative to the app root) or `tauri.conf.json` beside the manifest. */
function tauriConfigPath(descriptor, appRoot) {
  if (descriptor.tauriConfig !== undefined) return absolute(appRoot, descriptor.tauriConfig);
  return absolute(appRoot, P.join(P.dirname(descriptor.manifestPath ?? "src-tauri/Cargo.toml"), "tauri.conf.json"));
}

/** `{ productName?, identifier? }` from the tauri config, or null when it cannot be read or parsed. */
function readTauriConfig(descriptor, appRoot, readFile) {
  try {
    const parsed = JSON.parse(String(readFile(tauriConfigPath(descriptor, appRoot))));
    const pick = (value) => (typeof value === "string" && value ? value : undefined);
    return { productName: pick(parsed?.productName), identifier: pick(parsed?.identifier) };
  } catch { return null; }
}

/**
 * The `tauri-devid` descriptor, validated. Accepts the config root or the descriptor itself.
 * Fail-closed on identity: the destination must be exactly `/Applications/<productName>.app`, where productName is the
 * tauri.conf.json productName (install.productName must equal it) and, when the release config declares
 * `targets.mac.notarize.app`, the basename of that app must agree. There is no other destination.
 * With `readFile` (the install and reset paths) the tauri config must be readable (`tauri.conf.json` beside the manifest, or
 * `install.tauriConfig`) or `install.bundleId` must be set explicitly; without it only the descriptor's shape is checked.
 * `releaseConfig` is the release config root when `config` is the bare descriptor.
 */
export function resolveTauriDevIdDescriptor(config, { appRoot = "", readFile, releaseConfig } = {}) {
  const descriptor = config?.development?.targets?.mac?.install ?? config;
  if (!descriptor || descriptor.kind !== TAURI_DEVID_KIND) throw new TauriDevIdError("TAURI_DEVID_CONFIG_INVALID", `development.targets.mac.install.kind must be "${TAURI_DEVID_KIND}"`);
  if (typeof descriptor.productName !== "string" || !descriptor.productName) throw new TauriDevIdError("TAURI_DEVID_CONFIG_INVALID", "install.productName is required");
  if (typeof descriptor.destination !== "string" || !P.isAbsolute(descriptor.destination) || !descriptor.destination.endsWith(".app") || descriptor.destination.split("/").includes("..")) {
    throw new TauriDevIdError("TAURI_DEVID_CONFIG_INVALID", "install.destination must be an absolute /…/<Product>.app path (the one fixed install path, for example /Applications/<Product>.app)");
  }
  if (descriptor.tauriConfig !== undefined && (typeof descriptor.tauriConfig !== "string" || !descriptor.tauriConfig || P.isAbsolute(descriptor.tauriConfig) || /^[A-Za-z]:/.test(descriptor.tauriConfig) || descriptor.tauriConfig.split(/[\/]/).includes("..") || !/\.json$/i.test(descriptor.tauriConfig))) {
    throw invalid("install.tauriConfig must be a relative path to a .json file inside the app");
  }
  const destination = P.normalize(descriptor.destination);
  const release = releaseConfig ?? (config?.development ? config : undefined);
  const notarizeApp = release?.targets?.mac?.notarize?.app;
  const notarizeName = typeof notarizeApp === "string" && notarizeApp.replaceAll("\\", "/").replace(/\/+$/, "").endsWith(".app") ? P.basename(notarizeApp.replaceAll("\\", "/").replace(/\/+$/, ""), ".app") : undefined;
  if (readFile) {
    const tauri = readTauriConfig(descriptor, appRoot, readFile);
    if (!tauri && descriptor.tauriConfig !== undefined) throw invalid(`install.tauriConfig ${descriptor.tauriConfig} cannot be read or is not JSON; the production identity is read from it`);
    if (!tauri && !descriptor.bundleId) throw invalid("cannot establish the production identity: tauri.conf.json (beside the manifest, or install.tauriConfig) is unreadable and install.bundleId is not set; set either so the install can be checked against it");
    if (tauri?.productName !== undefined && tauri.productName !== descriptor.productName) throw invalid(`install.productName ${JSON.stringify(descriptor.productName)} differs from the tauri.conf.json productName ${JSON.stringify(tauri.productName)}: ${SAME_IDENTITY_RULE}`);
  }
  if (notarizeName !== undefined && notarizeName !== descriptor.productName) throw invalid(`install.productName ${JSON.stringify(descriptor.productName)} disagrees with targets.mac.notarize.app (${notarizeName}.app): ${SAME_IDENTITY_RULE}`);
  if (destination !== `/Applications/${descriptor.productName}.app`) {
    throw invalid(`install.destination ${destination} is not the production install path /Applications/${descriptor.productName}.app: ${SAME_IDENTITY_RULE}`);
  }
  for (const key of ["devIdentity", "refuseSystemApplications"]) {
    if (descriptor[key] !== undefined) throw invalid(`install.${key} is not supported: ${SAME_IDENTITY_RULE}`);
  }
  if (descriptor.signing !== undefined) throw new TauriDevIdError("TAURI_DEVID_CONFIG_INVALID", `install.signing is not allowed for ${TAURI_DEVID_KIND}: ad hoc signing (signing: "-") changes the code identity every build and resets privacy grants; dev builds sign with the same Developer ID as release`);
  const binaries = descriptor.verifyBinaries ?? [];
  if (!Array.isArray(binaries) || binaries.some((item) => typeof item !== "string" || !item || P.isAbsolute(item) || item.split("/").includes(".."))) {
    throw new TauriDevIdError("TAURI_DEVID_CONFIG_INVALID", "install.verifyBinaries must be relative paths inside the app, for example Contents/MacOS/<name>");
  }
  // Shape of the per-binary signing keys (identifiers, identifierTemplate, entitlements, timestamp, assert, assertTeamSignedNested).
  try { readDevSigningSettings(descriptor, config?.development ? config : {}); }
  catch (error) { throw new TauriDevIdError("TAURI_DEVID_CONFIG_INVALID", error.message); }
  return { ...descriptor, destination, verifyBinaries: binaries, appRoot };
}

/** The built .app's CFBundleIdentifier must equal the production identifier (tauri.conf.json, and install.bundleId when set). Fail-closed. */
function assertBuiltIdentity({ source, descriptor, appRoot, env, deps }) {
  const production = readTauriConfig(descriptor, appRoot, deps.readFile)?.identifier ?? null;
  if (descriptor.bundleId && production && descriptor.bundleId !== production) {
    throw invalid(`install.bundleId ${descriptor.bundleId} differs from the tauri.conf.json identifier ${production}: ${SAME_IDENTITY_RULE}`);
  }
  const expected = production ?? descriptor.bundleId;
  if (!expected) throw invalid(`no production bundle identifier: the tauri.conf.json identifier is unreadable and install.bundleId is not set, so the built app cannot be checked: ${SAME_IDENTITY_RULE}`);
  const read = deps.run("/usr/bin/plutil", ["-extract", "CFBundleIdentifier", "raw", "-o", "-", P.join(source, "Contents", "Info.plist")], { cwd: appRoot, env, encoding: "utf8", windowsHide: true, shell: false });
  const actual = read.error || read.status !== 0 ? null : String(read.stdout ?? "").trim();
  if (actual !== expected) {
    throw new TauriDevIdError("TAURI_DEVID_BUNDLE_ID_MISMATCH", `built ${source} CFBundleIdentifier ${actual === null ? "(unreadable)" : JSON.stringify(actual)} is not the production identifier ${JSON.stringify(expected)}: ${SAME_IDENTITY_RULE}`, { builtBundleId: actual, productionBundleId: expected });
  }
}

const builtPathOf = (built, appRoot) => {
  const value = typeof built === "string" ? built : built?.app;
  if (typeof value !== "string" || !value.replace(/\/+$/, "").endsWith(".app")) throw new TauriDevIdError("TAURI_DEVID_BUILT_INVALID", "built must be the path of the built .app (a string, or { app })");
  return absolute(appRoot, value.replace(/\/+$/, ""));
};

const digestOf = (deps, file) => createHash("sha256").update(deps.readFile(file)).digest("hex");

function verifyInstalled({ appRoot, destination, staged, expectedRequirement, binaries, env, deps, descriptor, config, signing }) {
  const strict = deps.run("codesign", ["--verify", "--strict", "--deep", destination], { cwd: appRoot, env, encoding: "utf8", windowsHide: true, shell: false });
  if (strict.error || strict.status !== 0) throw new TauriDevIdError("TAURI_DEVID_VERIFY_FAILED", `installed ${destination} failed codesign --verify --strict --deep: ${strict.error?.message ?? text(strict)}`);
  const display = deps.run("codesign", ["--display", "-r-", destination], { cwd: appRoot, env, encoding: "utf8", windowsHide: true, shell: false });
  const actual = /^designated => (.+)$/m.exec(text(display))?.[1]?.trim();
  if (display.status !== 0 || actual !== expectedRequirement) {
    throw new TauriDevIdError("TAURI_DEVID_VERIFY_FAILED", `installed ${destination} designated requirement ${JSON.stringify(actual)} is not the signed ${JSON.stringify(expectedRequirement)}`, { installedRequirement: actual, designatedRequirement: expectedRequirement });
  }
  // Declared identity assertions (install.assert, install.assertTeamSignedNested) against the installed copy; a failure rolls back.
  const settings = readDevSigningSettings(descriptor, config?.development ? config : {});
  if (settings.assertions.length || settings.assertTeamSignedNested) {
    try {
      deps.assertIdentities({ root: appRoot, bundle: destination, settings, teamIdentifier: signing.teamIdentifier, identity: signing.identity, env, run: deps.run, join: P.join, label: "installed copy",
        ...(deps.listFiles ? { listFiles: deps.listFiles } : {}), ...(deps.isMachO ? { isMachO: deps.isMachO } : {}) });
    } catch (error) { throw new TauriDevIdError("TAURI_DEVID_VERIFY_FAILED", `installed ${destination} failed its identity assertions: ${error.message}`, { problems: error.problems }); }
  }
  for (const binary of binaries) {
    let want; let got;
    try { want = digestOf(deps, P.join(staged, binary)); got = digestOf(deps, P.join(destination, binary)); }
    catch (error) { throw new TauriDevIdError("TAURI_DEVID_VERIFY_FAILED", `cannot read ${binary} to verify the install: ${error.message}`); }
    if (want !== got) throw new TauriDevIdError("TAURI_DEVID_VERIFY_FAILED", `installed ${binary} does not match the signed staged copy`);
  }
}

/**
 * @param {{appRoot: string, config: object, built: string|{app: string}, deps?: object, env?: object}} options
 * `config` is the app's release config (development.targets.mac.install is the `tauri-devid` descriptor).
 * @returns {Promise<{installed: string, designatedRequirement: string, identity: string, teamIdentifier: string, signing: object}>}
 */
export async function installTauriDevId({ appRoot, config, built, deps = {}, env = {}, releaseConfig }) {
  const d = { ...defaultDeps(), ...deps };
  const descriptor = resolveTauriDevIdDescriptor(config, { appRoot, readFile: d.readFile, releaseConfig });
  const destination = descriptor.destination;
  const source = builtPathOf(built, appRoot);
  if (!d.exists(source)) throw new TauriDevIdError("TAURI_DEVID_BUILT_INVALID", `built app not found: ${source}`);
  // Same identity as production: the built bundle id must be the production identifier, or nothing is copied or signed.
  assertBuiltIdentity({ source, descriptor, appRoot, env, deps: d });
  const name = P.basename(destination);
  const tempRoot = d.makeTemp();
  // The staging copy carries the final bundle name so installApp installs <dest dir>/<name> without renaming.
  const staged = P.join(tempRoot, name);
  try {
    d.copyApp(source, staged);
    // 1. Sign: nested code first, then the app, with the stable Developer ID (development-signing orders it deepest first).
    const signing = await d.sign({ root: appRoot, bundle: staged, descriptor, config, env, ...(d.listFiles ? { listFiles: d.listFiles } : {}), ...(d.isMachO ? { isMachO: d.isMachO } : {}) });
    if (signing.adhoc || !signing.designatedRequirement) throw new TauriDevIdError("TAURI_DEVID_SIGNING_UNSTABLE", `signing produced no stable Developer ID designated requirement for ${staged}; refusing to install`);
    // 2. Same identity as the installed copy, or fail: a changed designated requirement resets every privacy grant.
    if (d.exists(destination)) await d.assertRequirement({ root: appRoot, installed: destination, expected: signing.designatedRequirement, env, run: d.run });
    // 3. Install at the fixed path. The replaced copy is NOT kept anywhere (no <dest>.previous, no backup of any kind).
    await d.installApp({ app: staged, dev: true, dest: P.dirname(destination), appName: name, bundleId: descriptor.bundleId ?? null, relaunch: null }, { env });
    // A stale <dest>.previous from an older RightKit must not survive a dev install either.
    d.remove(previousPathOf(destination));
    // 4. Verify the installed copy; a failure removes the bad copy and leaves the path absent: reinstall from an older DMG.
    try {
      verifyInstalled({ appRoot, destination, staged, expectedRequirement: signing.designatedRequirement, binaries: descriptor.verifyBinaries, env, deps: d, descriptor, config, signing });
    } catch (error) {
      d.remove(destination);
      error.rolledBack = "removed-failed-install";
      error.message = `${error.message}; the failed install was removed and no old copy is kept: reinstall from an older DMG`;
      throw error;
    }
    d.log(`right-release dev: installed ${destination}; identity=${signing.identity}; TeamIdentifier=${signing.teamIdentifier}; designated=${signing.designatedRequirement}`);
    return { installed: destination, designatedRequirement: signing.designatedRequirement, identity: signing.identity, teamIdentifier: signing.teamIdentifier, signing };
  } finally { d.remove(tempRoot); }
}

/**
 * Deliberate reset of a fixed install: mode "remove" (required, there is no default) deletes the install and any stale
 * `.previous` (and, with `resetPrivacy`, runs `tccutil reset All <bundleId>`). There is no restore mode: dev installs keep
 * no old copy (Orthic-Labs/rightkit#348), reinstall from an older DMG instead.
 * Quit the app first; nothing here kills processes.
 */
export async function resetTauriDevId({ appRoot, config, mode, resetPrivacy = false, deps = {}, env = {}, releaseConfig }) {
  const d = { ...defaultDeps(), ...deps };
  const descriptor = resolveTauriDevIdDescriptor(config, { appRoot, readFile: d.readFile, releaseConfig });
  const destination = descriptor.destination;
  if (mode !== "remove") throw new TauriDevIdError("TAURI_DEVID_RESET_MODE_INVALID", `mode must be "remove" (restore was removed: dev installs keep no old copy), got ${JSON.stringify(mode)}`);
  d.remove(destination);
  d.remove(previousPathOf(destination));
  let privacyReset = false;
  if (resetPrivacy) {
    if (!descriptor.bundleId) throw new TauriDevIdError("TAURI_DEVID_CONFIG_INVALID", "resetPrivacy requires install.bundleId");
    const reset = d.run("tccutil", ["reset", "All", descriptor.bundleId], { cwd: appRoot, env, encoding: "utf8", windowsHide: true, shell: false });
    if (reset.error || reset.status !== 0) throw new TauriDevIdError("TAURI_DEVID_RESET_FAILED", `tccutil reset failed: ${reset.error?.message ?? text(reset)}`);
    privacyReset = true;
  }
  d.log(`right-release dev: removed ${destination}${privacyReset ? " and reset its privacy grants" : ""}`);
  return { mode, destination, removed: true, privacyReset };
}
