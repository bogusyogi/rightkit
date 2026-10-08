import { spawnSync } from "./background-process.mjs";
import { createHash } from "node:crypto";
import { cpSync, existsSync, mkdirSync, readFileSync, readdirSync, realpathSync, renameSync, rmSync, statSync } from "node:fs";
import path from "node:path";
import { pathToFileURL } from "node:url";
import { assertPrimaryReleaseCheckout } from "./release-invocation.mjs";
import { assertReleaseExecutionAllowed } from "./execution-policy.mjs";
import { acquireManagedRunLease, managedRuntimeClient, runCargoSync } from "./managed-cargo.mjs";
import { BUNDLE_MARKERS, bundleMarkerBuffer, readTauriBundleMarkers } from "./tauri-bundle-marker.mjs";
import { installPortableDevelopment } from "./portable-development.mjs";
import { signMacosDevelopmentBundle } from "./development-signing.mjs";
import { macosCodesignEnvironment } from "./codesign-environment.mjs";

export const DEVELOPMENT_PROFILE = "release-iterate";
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
  const client = managedRuntimeClient(result);
  if (client) {
    const managedRoot = result.RIGHTKIT_MANAGED_ROOT ?? result.RIGHTKIT_EXTERNAL_ROOT
      ?? (process.platform === "win32" ? "D:\\.rightkit-managed" : process.platform === "darwin" ? "/Volumes/D/.rightkit-managed" : undefined);
    const agentBin = managedRoot && path.join(managedRoot, "agent-bin");
    if (agentBin && existsSync(agentBin)) {
      const pathKey = Object.keys(result).find((key) => key.toLowerCase() === "path") ?? "PATH";
      result[pathKey] = [agentBin, result[pathKey]].filter(Boolean).join(path.delimiter);
    }
    // Same default as the agent-bin `rightkit` shim: children that run the managed client directly
    // (resolveCommand("rightkit"), build scripts) must not lose the broker endpoint.
    const socket = managedRoot && path.join(managedRoot, "broker.sock");
    if (!result.RIGHTKIT_BUILD_BROKER_SOCKET && socket && existsSync(socket)) result.RIGHTKIT_BUILD_BROKER_SOCKET = socket;
  }
  return result;
}

export function validateDevelopmentConfig(config, platform) {
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
  } else if (target.install) throw new Error(`unsupported development installer kind: ${target.install.kind}`);
  return target;
}

export function runDevelopment({ root, platform, config, buildOnly = false, dryRun = false, env = process.env, run = spawnSync,
  codesignEnvironment = macosCodesignEnvironment }) {
  if (!new Set(["win", "mac"]).has(platform)) throw new Error("--platform must be win or mac");
  assertNativeDevelopmentHost(platform);
  const target = validateDevelopmentConfig(config, platform);
  const repoRoot = git(root, ["rev-parse", "--show-toplevel"]);
  assertPrimaryReleaseCheckout(repoRoot, env);
  if (dryRun) return { built: false, installed: false, plan: { build: target.build, install: buildOnly ? null : target.install ?? null, profile: DEVELOPMENT_PROFILE } };
  assertReleaseExecutionAllowed({ cwd: repoRoot, environment: env, command: `right-release dev --platform ${platform}` });
  const childEnv = codesignEnvironment(developmentEnvironment(env));
  // One lease spans every managed request this run makes, so a broker cutover cannot land between them.
  const lease = acquireManagedRunLease({ command: `right-release dev --platform ${platform}`, env });
  try {
    const startedMs = Date.now();
    for (const command of target.build) execute(command, { root, env: childEnv, dryRun, run, label: "build" });
    if (buildOnly || !target.install) return { built: true, installed: false };
    const signing = install(target.install, { root, platform, config, env: childEnv, run, sinceMs: startedMs });
    return { built: true, installed: true, ...(platform === "mac" && target.install.kind === "tauri" ? { signing } : {}) };
  } finally { lease.release(); }
}

export function assertNativeDevelopmentHost(platform, host = process.platform) {
  if ((platform === "win" && host !== "win32") || (platform === "mac" && host !== "darwin")) throw new Error(`development target ${platform} requires its native host`);
}

function install(descriptor, { root, platform, config, env, run, sinceMs = null }) {
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
    const result = run(app, windowsInstallerArguments({ running }), { cwd: root, env, stdio: "inherit", windowsHide: true, shell: false });
    if (result.status !== 0) throw new Error(`development installer failed (${result.status}): ${app}`);
    const installed = path.resolve(root, descriptor.expectedInstalledPath);
    assertInstalledExecutableMatches(built, installed);
    return;
  }
  const app = resolveTauriArtifact(descriptor, { root, platform, env });
  const destination = path.resolve(descriptor.destination ?? path.join("/Applications", path.basename(app)));
  const destinationRoot = path.dirname(destination);
  mkdirSync(destinationRoot, { recursive: true });
  const staging = `${destination}.rightkit-dev-${process.pid}`;
  const backup = `${destination}.rightkit-backup-${process.pid}`;
  rmSync(staging, { recursive: true, force: true });
  rmSync(backup, { recursive: true, force: true });
  cpSync(app, staging, { recursive: true, verbatimSymlinks: true });
  let signing;
  try { signing = signMacosDevelopmentBundle({ root, bundle: staging, descriptor, config, env, run }); }
  catch (error) { rmSync(staging, { recursive: true, force: true }); throw error; }
  let backedUp = false;
  try {
    if (existsSync(destination)) { renameSync(destination, backup); backedUp = true; }
    renameSync(staging, destination);
    if (!existsSync(path.join(destination, "Contents", "Info.plist"))) throw new Error(`installed app verification failed: ${destination}`);
    if (backedUp) rmSync(backup, { recursive: true, force: true });
  } catch (error) {
    rmSync(destination, { recursive: true, force: true });
    if (backedUp && existsSync(backup)) renameSync(backup, destination);
    rmSync(staging, { recursive: true, force: true });
    throw error;
  }
  if (signing.provisioningProfile) signing.provisioningProfile = path.join(destination, "Contents", "embedded.provisionprofile");
  console.log(`right-release dev: installed ${destination}; identity=${signing.identity}; TeamIdentifier=${signing.teamIdentifier}; profile=${signing.provisioningProfile ?? "none"}`);
  return signing;
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
  return path.join(targetDirectory, descriptor.targetTriple, DEVELOPMENT_PROFILE, "bundle", platformBundleKind(descriptor));
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
    const socket = path.join(path.dirname(path.dirname(path.dirname(path.dirname(client)))), "broker.sock");
    const childEnv = !env.RIGHTKIT_BUILD_BROKER_SOCKET && existsSync(socket) ? { ...env, RIGHTKIT_BUILD_BROKER_SOCKET: socket } : undefined;
    return { command: process.execPath, args: [client, ...args], ...(childEnv ? { env: childEnv } : {}) };
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
  let platform, configPath = "right-release.config.mjs", dryRun = false, buildOnly = false;
  for (let i = 0; i < args.length; i++) {
    if (args[i] === "-h" || args[i] === "--help") { console.log("usage: right-release dev --platform win|mac [--config right-release.config.mjs] [--dry-run] [--build-only]"); return; }
    if (args[i] === "--platform") platform = args[++i];
    else if (args[i] === "--config") configPath = args[++i];
    else if (args[i] === "--dry-run") dryRun = true;
    else if (args[i] === "--build-only") buildOnly = true;
    else throw new Error(`unknown dev option: ${args[i]}`);
  }
  if (!platform) throw new Error("right-release dev requires --platform win|mac");
  const absoluteConfig = path.resolve(configPath);
  const safeEnv = developmentEnvironment(process.env);
  for (const [key, value] of Object.entries(safeEnv)) process.env[key] = value;
  for (const key of RELEASE_ONLY_APPLE_ENV) delete process.env[key];
  delete process.env.CARGO_INCREMENTAL;
  const pathKey = Object.keys(process.env).find((key) => key.toLowerCase() === "path");
  if (pathKey) process.env[pathKey] = safeEnv[pathKey] ?? process.env[pathKey];
  const loaded = await import(pathToFileURL(absoluteConfig));
  const config = loaded.default ?? loaded.config ?? loaded;
  const result = runDevelopment({ root: path.dirname(absoluteConfig), platform, config, dryRun, buildOnly });
  if (dryRun) console.log(JSON.stringify(result.plan, null, 2));
  return result;
}

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) {
  main().catch((error) => { console.error(`right-release dev: ${error.code ? `[${error.code}] ` : ""}${error.message}`); process.exitCode = 1; });
}
