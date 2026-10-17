// `install.kind: "tauri-devid"`: install a freshly built Tauri .app at ONE fixed path, signed with the stable
// Developer ID, so macOS privacy grants (TCC) and Keychain code requirements survive every rebuild.
//
// Pipeline (installTauriDevId): copy the built .app to a private staging copy -> sign nested code, then the app,
// with the stable Developer ID (signMacosDevelopmentBundle) -> assert the designated requirement equals the
// installed copy's (assertStableDesignatedRequirement) -> install at the fixed path keeping `<dest>.previous`
// (installApp, dev mode) -> verify the installed copy; any verify failure restores `.previous`.
// resetTauriDevId restores `.previous` or removes the install, only when asked.
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
import { previousPathOf } from "./in-place-install.mjs";

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

/** The `tauri-devid` descriptor, validated. Accepts the config root or the descriptor itself. */
export function resolveTauriDevIdDescriptor(config, { appRoot = "" } = {}) {
  const descriptor = config?.development?.targets?.mac?.install ?? config;
  if (!descriptor || descriptor.kind !== TAURI_DEVID_KIND) throw new TauriDevIdError("TAURI_DEVID_CONFIG_INVALID", `development.targets.mac.install.kind must be "${TAURI_DEVID_KIND}"`);
  if (typeof descriptor.productName !== "string" || !descriptor.productName) throw new TauriDevIdError("TAURI_DEVID_CONFIG_INVALID", "install.productName is required");
  if (typeof descriptor.destination !== "string" || !P.isAbsolute(descriptor.destination) || !descriptor.destination.endsWith(".app") || descriptor.destination.split("/").includes("..")) {
    throw new TauriDevIdError("TAURI_DEVID_CONFIG_INVALID", "install.destination must be an absolute /…/<Product>.app path (the one fixed install path, for example /Applications/<Product>.app)");
  }
  if (descriptor.signing !== undefined) throw new TauriDevIdError("TAURI_DEVID_CONFIG_INVALID", `install.signing is not allowed for ${TAURI_DEVID_KIND}: ad hoc signing changes every build and resets privacy grants`);
  const binaries = descriptor.verifyBinaries ?? [];
  if (!Array.isArray(binaries) || binaries.some((item) => typeof item !== "string" || !item || P.isAbsolute(item) || item.split("/").includes(".."))) {
    throw new TauriDevIdError("TAURI_DEVID_CONFIG_INVALID", "install.verifyBinaries must be relative paths inside the app, for example Contents/MacOS/<name>");
  }
  // Shape of the per-binary signing keys (identifiers, identifierTemplate, entitlements, timestamp, assert, assertTeamSignedNested).
  try { readDevSigningSettings(descriptor, config?.development ? config : {}); }
  catch (error) { throw new TauriDevIdError("TAURI_DEVID_CONFIG_INVALID", error.message); }
  return { ...descriptor, destination: P.normalize(descriptor.destination), verifyBinaries: binaries, appRoot };
}

const builtPathOf = (built, appRoot) => {
  const value = typeof built === "string" ? built : built?.app;
  if (typeof value !== "string" || !value.replace(/\/+$/, "").endsWith(".app")) throw new TauriDevIdError("TAURI_DEVID_BUILT_INVALID", "built must be the path of the built .app (a string, or { app })");
  return absolute(appRoot, value.replace(/\/+$/, ""));
};

const digestOf = (deps, file) => createHash("sha256").update(deps.readFile(file)).digest("hex");

/** Put `.previous` back at the fixed path. Renames only; the bad copy is removed last. */
function restorePrevious(destination, deps) {
  const previous = previousPathOf(destination);
  if (!deps.exists(previous)) throw new TauriDevIdError("TAURI_DEVID_NO_PREVIOUS", `no rollback copy at ${previous}`);
  const aside = `${destination}.reset`;
  deps.remove(aside);
  const hadCurrent = deps.exists(destination);
  if (hadCurrent) deps.rename(destination, aside);
  try { deps.rename(previous, destination); }
  catch (error) { if (hadCurrent) deps.rename(aside, destination); throw error; }
  deps.remove(aside);
  return { destination, restoredFrom: previous };
}

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
 * @returns {Promise<{installed: string, previous: string|null, designatedRequirement: string, identity: string, teamIdentifier: string, signing: object}>}
 */
export async function installTauriDevId({ appRoot, config, built, deps = {}, env = {} }) {
  const d = { ...defaultDeps(), ...deps };
  const descriptor = resolveTauriDevIdDescriptor(config, { appRoot });
  const destination = descriptor.destination;
  const source = builtPathOf(built, appRoot);
  if (!d.exists(source)) throw new TauriDevIdError("TAURI_DEVID_BUILT_INVALID", `built app not found: ${source}`);
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
    // 3. Install at the fixed path; the replaced copy stays as <dest>.previous.
    const outcome = await d.installApp({ app: staged, dev: true, dest: P.dirname(destination), appName: name, bundleId: descriptor.bundleId ?? null, relaunch: null }, { env });
    // 4. Verify the installed copy; a failure puts .previous back (or removes a first install) so the path is never left broken.
    try {
      verifyInstalled({ appRoot, destination, staged, expectedRequirement: signing.designatedRequirement, binaries: descriptor.verifyBinaries, env, deps: d, descriptor, config, signing });
    } catch (error) {
      let rolledBack = "none";
      if (outcome?.previous && d.exists(previousPathOf(destination))) { restorePrevious(destination, d); rolledBack = "restored-previous"; }
      else { d.remove(destination); rolledBack = "removed-first-install"; }
      error.rolledBack = rolledBack;
      throw error;
    }
    d.log(`right-release dev: installed ${destination}; identity=${signing.identity}; TeamIdentifier=${signing.teamIdentifier}; designated=${signing.designatedRequirement}`);
    return { installed: destination, previous: outcome?.previous ?? null, designatedRequirement: signing.designatedRequirement, identity: signing.identity, teamIdentifier: signing.teamIdentifier, signing };
  } finally { d.remove(tempRoot); }
}

/**
 * Deliberate reset of a fixed install. mode "restore" (default) makes `.previous` the install again; mode "remove"
 * deletes the install and its `.previous` (and, with `resetPrivacy`, runs `tccutil reset All <bundleId>`).
 * Quit the app first; nothing here kills processes.
 */
export async function resetTauriDevId({ appRoot, config, mode = "restore", resetPrivacy = false, deps = {}, env = {} }) {
  const d = { ...defaultDeps(), ...deps };
  const descriptor = resolveTauriDevIdDescriptor(config, { appRoot });
  const destination = descriptor.destination;
  if (mode === "restore") {
    const result = restorePrevious(destination, d);
    d.log(`right-release dev: restored ${destination} from ${result.restoredFrom}`);
    return { mode, ...result };
  }
  if (mode !== "remove") throw new TauriDevIdError("TAURI_DEVID_RESET_MODE_INVALID", `mode must be "restore" or "remove", got ${JSON.stringify(mode)}`);
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
