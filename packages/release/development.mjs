import { discoverBrokerSocket, resolveManagedRoots } from './managed-roots.mjs';
import { spawnSync } from "./background-process.mjs";
import { createHash } from "node:crypto";
import { copyFileSync, cpSync, existsSync, mkdirSync, readFileSync, lstatSync, readdirSync, realpathSync, renameSync, rmSync, statSync } from "node:fs";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { assertPrimaryReleaseCheckout } from "./release-invocation.mjs";
import { assertReleaseExecutionAllowed } from "./execution-policy.mjs";
import { acquireManagedRunLease, managedRuntimeClient, managedRuntimeCommand, runCargoSync } from "./managed-cargo.mjs";
import { BUNDLE_MARKERS, bundleMarkerBuffer, readTauriBundleMarkers } from "./tauri-bundle-marker.mjs";
import { installPortableDevelopment } from "./portable-development.mjs";
import { previousPathOf, stagingPathOf, swapInPlace } from "./in-place-install.mjs";
import { stageDevelopmentBinaries, validateStageEntries } from "./stage-binary.mjs";
import { assertStableDesignatedRequirement, readBundleExecutable, requestedCiAdhocSigning, signMacosDevelopmentBundle } from "./development-signing.mjs";
import { macosCodesignEnvironment } from "./codesign-environment.mjs";
import { installTauriDevId, resolveTauriDevIdDescriptor, TAURI_DEVID_KIND } from "./install-tauri-devid.mjs";
import { androidDevelopmentNdkCommand, androidDevelopmentSerial, installAndroidDevelopment, validateAndroidDevelopment } from "./android-dev.mjs";

// One standard Cargo profile for every app (templates/cargo-profiles.toml): Cargo's built-in `dev`.
// `--profile dev` is valid and no flag also means dev.
export const DEVELOPMENT_PROFILE = "dev";
// Cargo writes the `dev` (and `test`) profile to target/<triple>/debug, never .../dev; `bench` goes to release.
// Every path built from a profile name must go through this.
export function cargoProfileDirectory(profile) {
  if (profile === "dev" || profile === "test") return "debug";
  if (profile === "bench") return "release";
  return profile;
}
const RELEASE_ONLY_APPLE_ENV = ["APPLE_ID", "APPLE_PASSWORD", "APPLE_API_KEY", "APPLE_API_ISSUER", "APPLE_API_KEY_PATH", "APPLE_CERTIFICATE", "APPLE_CERTIFICATE_PASSWORD"];

export function developmentEnvironment(env = process.env) {
  const result = {
    ...env,
    RIGHTKIT_BUILD_MODE: "dev",
    RIGHTKIT_CARGO_PROFILE: DEVELOPMENT_PROFILE,
    TAURI_SIGNING_PRIVATE_KEY: "",
    TAURI_SIGNING_PRIVATE_KEY_PASSWORD: "",
    CSC_IDENTITY_AUTO_DISCOVERY: "false",
  };
  for (const key of RELEASE_ONLY_APPLE_ENV) delete result[key];
  delete result.CARGO_INCREMENTAL;
  // A dev build is debug-signed: a release signing-properties path never reaches it.
  delete result.RIGHTKIT_ANDROID_SIGNING_PROPERTIES;
  const client = managedRuntimeClient(result);
  if (client) {
    const { agentBin } = resolveManagedRoots({ env: result });
    if (agentBin && existsSync(agentBin)) {
      const pathKey = Object.keys(result).find((key) => key.toLowerCase() === "path") ?? "PATH";
      result[pathKey] = [agentBin, result[pathKey]].filter(Boolean).join(path.delimiter);
    }
    // Same default as the agent-bin `rightkit` shim: children that run the managed client directly
    // (resolveCommand("rightkit"), build scripts) must not lose the broker endpoint.
    const socket = discoverBrokerSocket(result);
    if (!result.RIGHTKIT_BUILD_BROKER_SOCKET && socket && existsSync(socket)) result.RIGHTKIT_BUILD_BROKER_SOCKET = socket;
  }
  return result;
}

export function validateDevelopmentConfig(config, platform, { root = process.cwd() } = {}) {
  const target = config?.development?.targets?.[platform];
  if (!target || !Array.isArray(target.build) || target.build.length === 0) throw new Error(`development.targets.${platform}.build must be a non-empty command array`);
  for (const [index, command] of target.build.entries()) validateCommand(command, `development.targets.${platform}.build[${index}]`);
  if (target.install?.kind === "command") validateCommand(target.install, `development.targets.${platform}.install`);
  else if (target.install?.kind === "tauri") {
    if (!target.install.productName || !target.install.targetTriple) throw new Error(`development.targets.${platform}.install requires productName and targetTriple`);
    if (target.install.manifestPath && path.isAbsolute(target.install.manifestPath)) throw new Error("install.manifestPath must be relative to app root");
  } else if (target.install?.kind === "portable") {
    const descriptor = target.install;
    const absoluteInstallRoot = platform === "win" ? path.win32.isAbsolute(descriptor.installRoot ?? "") : path.isAbsolute(descriptor.installRoot ?? "");
    if (descriptor.activation?.kind !== "current" || !absoluteInstallRoot) throw new Error("portable development installer requires absolute installRoot and current activation");
    for (const key of ["sourceDirectory", "entrypoint"]) {
      if (typeof descriptor[key] !== "string" || !descriptor[key] || path.isAbsolute(descriptor[key]) || path.win32.isAbsolute(descriptor[key]) || descriptor[key].split(/[\\/]/).includes("..")) throw new Error(`portable development ${key} must be a contained relative path`);
    }
  } else if (target.install?.kind === TAURI_DEVID_KIND) {
    if (platform !== "mac") throw new Error(`the ${TAURI_DEVID_KIND} development installer is for --platform mac`);
    resolveTauriDevIdDescriptor(target.install, { appRoot: root });
    if (!target.install.targetTriple && !target.install.bundleDirectory) throw new Error(`development.targets.${platform}.install requires targetTriple (or bundleDirectory) to find the built .app`);
  } else if (target.install?.kind === "adb") {
    if (platform !== "android") throw new Error("the adb development installer is for --platform android");
  } else if (target.install) throw new Error(`unsupported development installer kind: ${target.install.kind}`);
  validateStageEntries(target.stage, `development.targets.${platform}.stage`);
  if (platform === "android") return { ...target, ...validateAndroidDevelopment(target, { root }) };
  return target;
}

export function runDevelopment({ root, platform, config, buildOnly = false, dryRun = false, env = process.env, run = spawnSync,
  codesignEnvironment = macosCodesignEnvironment, serial }) {
  if (!new Set(["win", "mac", "android"]).has(platform)) throw new Error("--platform must be win, mac or android");
  assertNativeDevelopmentHost(platform);
  const target = validateDevelopmentConfig(config, platform, { root });
  const repoRoot = git(root, ["rev-parse", "--show-toplevel"]);
  assertPrimaryReleaseCheckout(repoRoot, env);
  if (dryRun) return { built: false, installed: false, plan: { ...(target.ndk ? { ndk: androidDevelopmentNdkCommand(target.ndk) } : {}), build: target.build, install: buildOnly ? null : target.install ?? null, profile: DEVELOPMENT_PROFILE } };
  assertReleaseExecutionAllowed({ cwd: repoRoot, environment: env, command: `right-release dev --platform ${platform}` });
  // Refuse an invalid or off-CI ad hoc opt-in before the (long) build, not after it.
  if (!buildOnly && platform === "mac" && target.install?.kind === "tauri") requestedCiAdhocSigning({ descriptor: target.install, env });
  // Android: the device serial is required, so a missing one is refused before the (long) build too.
  if (!buildOnly && platform === "android" && target.install) androidDevelopmentSerial({ descriptor: target.install, serial, env });
  const childEnv = codesignEnvironment(developmentEnvironment(env));
  // One lease spans every managed request this run makes, so a broker cutover cannot land between them.
  const lease = acquireManagedRunLease({ command: `right-release dev --platform ${platform}`, env });
  let releaseLease = true;
  try {
    const startedMs = Date.now();
    // Android: cargo-ndk (Cargo dev profile) fills the generated jniLibs, then the Gradle debug build packages it.
    if (target.ndk) { rmSync(target.ndk.jniLibs, { recursive: true, force: true }); execute(androidDevelopmentNdkCommand(target.ndk), { root, env: childEnv, dryRun, run, label: "native build" }); }
    for (const command of target.build) execute(command, { root, env: childEnv, dryRun, run, label: "build" });
    // Dev and QA binaries run from one stable staged path, copied in place (<name>.previous kept), never from the
    // broker's per-generation target folder or a hashed dev-targets folder.
    const staged = target.stage?.length ? stageDevelopmentBinaries(target.stage, { root, platform: platform === "win" ? "win32" : process.platform }) : [];
    for (const file of staged) console.log(`right-release dev: staged ${file}`);
    if (buildOnly || !target.install) return { built: true, installed: false, ...(staged.length ? { staged } : {}) };
    if (target.install.kind === TAURI_DEVID_KIND) {
      // installTauriDevId is async: the lease is held until it settles, so a broker cutover cannot land mid-install.
      releaseLease = false;
      const built = resolveTauriArtifact(target.install, { root, platform, env: childEnv });
      return installTauriDevId({ appRoot: root, config: target.install, built, env: childEnv })
        .then((outcome) => ({ built: true, installed: true, ...(staged.length ? { staged } : {}), devid: outcome }))
        .finally(() => lease.release());
    }
    const signing = install(target.install, { root, platform, config, env: childEnv, run, sinceMs: startedMs, serial });
    return { built: true, installed: true, ...(staged.length ? { staged } : {}), ...(platform === "mac" && target.install.kind === "tauri" ? { signing } : {}) };
  } finally { if (releaseLease) lease.release(); }
}

export function assertNativeDevelopmentHost(platform, host = process.platform) {
  if ((platform === "win" && host !== "win32") || (platform === "mac" && host !== "darwin")) throw new Error(`development target ${platform} requires its native host`);
}

function install(descriptor, { root, platform, config, env, run, sinceMs = null, serial }) {
  if (descriptor.kind === "adb") return installAndroidDevelopment(descriptor, { root, env, run, serial });
  if (descriptor.kind === "command") return execute(descriptor, { root, env, run, label: "install" });
  if (descriptor.kind === "portable") return installPortableDevelopment(descriptor, { root, platform: platform === "win" ? "win32" : "darwin", env });
  if (platform === "win") {
    if (!descriptor.expectedInstalledPath) throw new Error("Windows Tauri installer requires expectedInstalledPath for installed verification");
    const { bundleRoot, installer: app } = resolveTauriBundleDirectory(descriptor, { root, platform, env, sinceMs });
    const built = path.resolve(bundleRoot, "..", "..", path.basename(descriptor.expectedInstalledPath));
    assertNonEmptyExecutable(built);
    // Tauri's silent installer force-kills a running copy of the app (utils.nsh: IfSilent kill). When it
    // was running, pass /R so the installer relaunches it as the user (RunAsUser, outside this process tree).
    const running = isWindowsProcessRunning(path.basename(descriptor.expectedInstalledPath), { run });
    if (running) console.log(`right-release dev: ${descriptor.productName} is running; the installer will close it and relaunch it`);
    // The silent installer updates the fixed path in place. Keep the copy it replaces once, as <exe>.previous (never run).
    const installedBefore = path.resolve(root, descriptor.expectedInstalledPath);
    if (existsSync(installedBefore) && statSync(installedBefore).isFile()) copyFileSync(installedBefore, previousPathOf(installedBefore));
    const result = run(app, windowsInstallerArguments({ running }), { cwd: root, env, stdio: "inherit", windowsHide: true, shell: false });
    if (result.status !== 0) throw new Error(`development installer failed (${result.status}): ${app}`);
    const installed = path.resolve(root, descriptor.expectedInstalledPath);
    assertInstalledExecutableMatches(built, installed);
    return;
  }
  const app = resolveTauriArtifact(descriptor, { root, platform, env });
  // One fixed install path per app: macOS keys privacy grants to it. No /Applications/<name> guess.
  if (!descriptor.destination) throw new DevelopmentInstallError("DEV_INSTALL_DESTINATION_REQUIRED", "development.targets.mac.install.destination is required: the one fixed path this app installs to (for example /Applications/<Product>.app)");
  const destination = path.resolve(descriptor.destination);
  mkdirSync(path.dirname(destination), { recursive: true });
  // Fixed staging name (not per-pid): the copy is signed and verified here, then renamed into place. Nothing runs from it.
  const staging = stagingPathOf(destination);
  const previous = previousPathOf(destination);
  rmSync(staging, { recursive: true, force: true });
  cpSync(app, staging, { recursive: true, verbatimSymlinks: true });
  let signing;
  let executableName;
  try {
    signing = signMacosDevelopmentBundle({ root, bundle: staging, descriptor, config, env, run });
    // Same identity as the installed copy, or fail: a changed designated requirement resets every privacy grant.
    if (!signing.adhoc && existsSync(destination)) assertStableDesignatedRequirement({ root, installed: destination, expected: signing.designatedRequirement, env, run });
    executableName = readBundleExecutable(staging, { root, env, run });
  } catch (error) { rmSync(staging, { recursive: true, force: true }); throw error; }
  swapInstalledMacosBundle({ staging, destination, previous, executableName });
  if (signing.provisioningProfile) signing.provisioningProfile = path.join(destination, "Contents", "embedded.provisionprofile");
  if (signing.adhoc) console.log(`right-release dev: installed ${destination}; signing=ci-adhoc (ad hoc signed on a CI runner; NOT a release artifact); signature=adhoc; TeamIdentifier=none`);
  else console.log(`right-release dev: installed ${destination}; identity=${signing.identity}; TeamIdentifier=${signing.teamIdentifier}; profile=${signing.provisioningProfile ?? "none"}; designated=${signing.designatedRequirement}`);
  return signing;
}

export class DevelopmentInstallError extends Error {
  constructor(code, message) { super(message); this.name = "DevelopmentInstallError"; this.code = code; }
}

const NODE_FS = { existsSync, lstatSync, mkdirSync, copyFileSync, readFileSync, renameSync, rmSync, statSync };

/**
 * Move the signed staging bundle into place, keeping the previous app as `<destination>.previous`, and verify it: Info.plist exists and
 * the installed main executable (`executableName`, the staged CFBundleExecutable read through plutil by the caller) exists with the same SHA-256 as the staged one. Any failure removes the
 * new install, renames `.previous` back and throws a DevelopmentInstallError (code DEV_INSTALL_VERIFICATION_FAILED,
 * DEV_INSTALL_EXECUTABLE_MISSING or DEV_INSTALL_EXECUTABLE_MISMATCH). `fs` is the file-system seam (tests inject it).
 */
export function swapInstalledMacosBundle({ staging, destination, previous = previousPathOf(destination), executableName, fs = NODE_FS }) {
  if (typeof executableName !== "string" || !executableName) throw new DevelopmentInstallError("DEV_INSTALL_VERIFICATION_FAILED", "swapInstalledMacosBundle requires the staged CFBundleExecutable name");
  const executable = path.join("Contents", "MacOS", executableName);
  const digest = (file) => createHash("sha256").update(fs.readFileSync(file)).digest("hex");
  let expected;
  try { expected = digest(path.join(staging, executable)); }
  catch (error) {
    fs.rmSync(staging, { recursive: true, force: true });
    throw new DevelopmentInstallError("DEV_INSTALL_EXECUTABLE_MISSING", `staged app has no main executable ${executable}: ${staging} (${error.message})`);
  }
  // The old app becomes `<destination>.previous` (replacing any older one) and stays there for rollback; any failure renames it back.
  swapInPlace({
    staging, destination, previous, fs,
    verify: () => {
      if (!fs.existsSync(path.join(destination, "Contents", "Info.plist"))) throw new DevelopmentInstallError("DEV_INSTALL_VERIFICATION_FAILED", `installed app verification failed: ${destination}`);
      const installed = path.join(destination, executable);
      let actual;
      try { actual = digest(installed); }
      catch { throw new DevelopmentInstallError("DEV_INSTALL_EXECUTABLE_MISSING", `installed app verification failed: main executable ${executable} is missing in ${destination}`); }
      if (actual !== expected) throw new DevelopmentInstallError("DEV_INSTALL_EXECUTABLE_MISMATCH", `installed app verification failed: ${executable} SHA-256 ${actual} does not match the staged ${expected}`);
    },
  });
}

export function resolveTauriArtifact(descriptor, { root, platform, env = process.env, runCargo = runCargoSync }) {
  return resolveTauriBundleDirectory(descriptor, { root, platform, env, runCargo }).installer;
}

export function resolveTauriBundleDirectory(descriptor, { root, platform, env = process.env, runCargo = runCargoSync, sinceMs = null }) {
  const bundleRoot = descriptor.bundleDirectory
    ? path.resolve(root, descriptor.bundleDirectory)
    : resolveBundleRoot(descriptor, { root, env, runCargo });
  if (descriptor.bundleDirectory && !inside(root, bundleRoot)) throw new Error("Tauri bundleDirectory must stay inside app root");
  const entries = readdirSync(bundleRoot, { withFileTypes: true });
  const candidates = platform === "mac"
    ? entries.filter((entry) => entry.isDirectory() && entry.name === `${descriptor.productName}.app`).map((entry) => path.join(bundleRoot, entry.name))
    // Older installers are kept for history: select this product's installer written by this run.
    : entries.filter((entry) => entry.isFile() && entry.name.toLowerCase().endsWith(".exe") && entry.name.toLowerCase().includes("setup") && entry.name.startsWith(`${descriptor.productName}_`))
      .map((entry) => path.join(bundleRoot, entry.name))
      .filter((file) => sinceMs === null || statSync(file).mtimeMs >= sinceMs);
  if (candidates.length !== 1) throw new Error(`expected exactly one ${platform} Tauri installer for ${descriptor.productName} in ${bundleRoot}${sinceMs === null ? "" : " written by this run"}; found ${candidates.length}`);
  if (!statSync(candidates[0]).isFile() && platform === "win") throw new Error(`installer is not a file: ${candidates[0]}`);
  return { bundleRoot, installer: candidates[0] };
}

export function windowsInstallerArguments({ running = false } = {}) { return running ? ["/S", "/R"] : ["/S"]; }

export function isWindowsProcessRunning(imageName, { run = spawnSync, systemRoot = process.env.SystemRoot ?? "C:\\Windows" } = {}) {
  const result = run(path.join(systemRoot, "System32", "tasklist.exe"), ["/FI", `IMAGENAME eq ${imageName}`, "/FO", "CSV", "/NH"], { encoding: "utf8", windowsHide: true, shell: false });
  if (result.status !== 0 || typeof result.stdout !== "string") return false;
  return result.stdout.toLowerCase().includes(`"${imageName.toLowerCase()}"`);
}

export function assertInstalledExecutableMatches(built, installed, read = readFileSync) {
  assertNonEmptyExecutable(built);
  assertNonEmptyExecutable(installed);
  const digest = (file) => createHash("sha256").update(withUnknownBundleMarker(file, read)).digest("hex");
  if (digest(built) !== digest(installed)) throw new Error(`installed executable does not match this dev build: ${installed}`);
}
// Tauri's NSIS bundler rewrites the packaged copy's bundle marker UNK -> NSS; compare both copies as UNK.
function withUnknownBundleMarker(file, read) {
  const bytes = Buffer.from(read(file));
  const nsis = readTauriBundleMarkers(file, { read: () => bytes }).markers.nsis ?? [];
  if (nsis.length > 1) throw new Error(`installed executable verification failed: ${nsis.length} NSIS bundle markers in ${file}`);
  if (nsis.length === 1) bundleMarkerBuffer(BUNDLE_MARKERS.unknown).copy(bytes, nsis[0]);
  return bytes;
}
function assertNonEmptyExecutable(file) { if (!existsSync(file) || !statSync(file).isFile() || statSync(file).size === 0) throw new Error(`installed executable verification failed: missing or empty file ${file}`); }

function resolveBundleRoot(descriptor, { root, env, runCargo }) {
  if (!descriptor.manifestPath) throw new Error("Tauri installer requires manifestPath when bundleDirectory is omitted");
  const manifest = path.resolve(root, descriptor.manifestPath);
  const cwd = process.cwd();
  let metadata;
  try {
    process.chdir(root);
    metadata = runCargo(["metadata", "--offline", "--no-deps", "--format-version", "1", "--manifest-path", manifest], { cwd: root, env, encoding: "utf8", windowsHide: true, shell: false });
  } finally { process.chdir(cwd); }
  if (metadata.error || metadata.status !== 0) throw new Error(`RightKit Cargo metadata failed (${metadata.status ?? "unavailable"})${metadata.error ? `: ${metadata.error.message}` : ""}`);
  let targetDirectory;
  try { targetDirectory = JSON.parse(metadata.stdout).target_directory; } catch { throw new Error("RightKit Cargo metadata returned invalid JSON"); }
  if (!targetDirectory || !path.isAbsolute(targetDirectory)) throw new Error("Cargo metadata omitted absolute target_directory");
  return path.join(targetDirectory, descriptor.targetTriple, cargoProfileDirectory(DEVELOPMENT_PROFILE), "bundle", platformBundleKind(descriptor));
}

function platformBundleKind(descriptor) { return descriptor.targetTriple.includes("apple") ? "macos" : "nsis"; }
function validateCommand(command, label) { if (!command || typeof command.cmd !== "string" || !command.cmd.trim() || (command.args !== undefined && !Array.isArray(command.args))) throw new Error(`${label} must declare cmd and args`); }
function execute(command, { root, env, dryRun, run, label }) {
  validateCommand(command, `development ${label}`);
  const cwd = path.resolve(root, command.cwd ?? ".");
  if (!inside(root, cwd)) throw new Error(`development ${label} cwd must stay inside app root`);
  const args = (command.args ?? []).map(String);
  if (dryRun) return { cmd: command.cmd, args, cwd };
  const launch = resolveCommand(command.cmd, args, { root, env });
  const result = run(launch.command, launch.args, { cwd, env: launch.env ?? env, stdio: "inherit", windowsHide: true, shell: false, timeout: command.timeoutMs });
  if (result.status !== 0) throw new Error(`development ${label} command failed (${result.status}): ${command.name ?? command.cmd}`);
}
export function resolveCommand(command, args, { root, env = process.env } = {}) {
  if (command === "node") return { command: process.execPath, args };
  if (command === "pnpm") return { command: process.execPath, args: [resolvePnpmCli({ root, env }), ...args] };
  if (command === "rightkit") {
    const client = managedRuntimeClient(env);
    if (!client) throw new Error("installed RightKit runtime client is unavailable for development command");
    const socket = discoverBrokerSocket(env);
    const childEnv = !env.RIGHTKIT_BUILD_BROKER_SOCKET && existsSync(socket) ? { ...env, RIGHTKIT_BUILD_BROKER_SOCKET: socket } : undefined;
    return { ...managedRuntimeCommand(client, args), ...(childEnv ? { env: childEnv } : {}) };
  }
  return { command, args };
}
export function resolvePnpmCli({ root, env = process.env } = {}) {
  const candidates = [];
  if (env.npm_execpath && /pnpm/i.test(path.basename(env.npm_execpath))) candidates.push(env.npm_execpath);
  const nodeDir = path.dirname(process.execPath);
  candidates.push(path.join(nodeDir, "node_modules", "corepack", "dist", "pnpm.js"));
  candidates.push(path.join(nodeDir, "node_modules", "pnpm", "bin", "pnpm.cjs"));
  for (let dir = path.resolve(root ?? process.cwd()); ; dir = path.dirname(dir)) {
    const store = path.join(dir, "node_modules", ".pnpm");
    if (existsSync(store)) {
      for (const entry of readdirSync(store).filter((name) => /^pnpm@/.test(name)).sort().reverse()) {
        candidates.push(path.join(store, entry, "node_modules", "pnpm", "bin", "pnpm.cjs"));
      }
    }
    const parent = path.dirname(dir);
    if (parent === dir) break;
  }
  // A global pnpm (no Corepack in Node >= 25) exposes its JS entry only through its PATH shim.
  for (const dir of String(env.PATH ?? env.Path ?? "").split(path.delimiter).filter(Boolean)) {
    const shim = path.join(dir, "pnpm.CMD");
    if (!existsSync(shim)) continue;
    const match = /"%~dp0\\([^"]*pnpm\.(?:mjs|cjs|js))"/i.exec(readFileSync(shim, "utf8"));
    if (match) candidates.push(path.resolve(dir, match[1]));
  }
  // Unix global pnpm exposes its JavaScript entry through a PATH symlink.
  for (const dir of String(env.PATH ?? env.Path ?? "").split(path.delimiter).filter(Boolean)) candidates.push(path.join(dir, "pnpm"));
  const selected = candidates.map(resolveJavaScriptEntrypoint).find(Boolean);
  if (!selected) throw new Error("could not resolve pnpm JavaScript CLI entrypoint; install/pin pnpm through Corepack");
  return selected;
}
function resolveJavaScriptEntrypoint(candidate) {
  try {
    const resolved = realpathSync(candidate);
    return /pnpm\.(?:mjs|cjs|js)$/i.test(resolved) && statSync(resolved).isFile() ? resolved : undefined;
  } catch {
    return undefined;
  }
}
function git(cwd, args) { const result = spawnSync("git", args, { cwd, encoding: "utf8", windowsHide: true }); if (result.status !== 0) throw new Error(`git ${args.join(" ")} failed`); return result.stdout.trim(); }
function inside(root, child) { const rel = path.relative(path.resolve(root), path.resolve(child)); return rel === "" || (rel !== ".." && !rel.startsWith(`..${path.sep}`) && !path.isAbsolute(rel)); }
export async function main(args = process.argv.slice(2)) {
  let platform, configPath = "right-release.config.mjs", dryRun = false, buildOnly = false, serial;
  for (let i = 0; i < args.length; i++) {
    if (args[i] === "-h" || args[i] === "--help") { console.log("usage: right-release dev --platform win|mac|android [--config right-release.config.mjs] [--dry-run] [--build-only] [--serial <adb serial>]"); return; }
    if (args[i] === "--platform") platform = args[++i];
    else if (args[i] === "--config") configPath = args[++i];
    else if (args[i] === "--dry-run") dryRun = true;
    else if (args[i] === "--build-only") buildOnly = true;
    else if (args[i] === "--serial") serial = args[++i];
    else throw new Error(`unknown dev option: ${args[i]}`);
  }
  if (!platform) throw new Error("right-release dev requires --platform win|mac|android");
  const absoluteConfig = path.resolve(configPath);
  const safeEnv = developmentEnvironment(process.env);
  for (const [key, value] of Object.entries(safeEnv)) process.env[key] = value;
  for (const key of RELEASE_ONLY_APPLE_ENV) delete process.env[key];
  delete process.env.CARGO_INCREMENTAL;
  const pathKey = Object.keys(process.env).find((key) => key.toLowerCase() === "path");
  if (pathKey) process.env[pathKey] = safeEnv[pathKey] ?? process.env[pathKey];
  const loaded = await import(pathToFileURL(absoluteConfig));
  const config = loaded.default ?? loaded.config ?? loaded;
  const result = await runDevelopment({ root: path.dirname(absoluteConfig), platform, config, dryRun, buildOnly, serial });
  if (dryRun) console.log(JSON.stringify(result.plan, null, 2));
  return result;
}

const realOrSelf = (file) => { try { return realpathSync(file); } catch { return path.resolve(file); } };
if (process.argv[1] && realOrSelf(process.argv[1]) === realOrSelf(fileURLToPath(import.meta.url))) {
  main().catch((error) => { console.error(`right-release dev: ${error.code ? `[${error.code}] ` : ""}${error.message}`); process.exitCode = 1; });
}
