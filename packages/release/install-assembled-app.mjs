// `install.kind: "assembled-app"` for `right-release dev`: assemble the composite app declared in `assembly.<mac|win>` from the
// dev-profile build outputs and install it IN PLACE at the one fixed path, with no backup of the old copy
// (Orthic-Labs/rightkit#348). macOS signs with the stable Developer ID; Windows signs the payload exes through sign-windows.
//
// macOS (installAssembledMac): resolve DerivedData (rustright storage) -> xcodegen + xcodebuild the outer app -> assemble in a
// private temp folder -> sign inside-out (signMacosDevelopmentBundle) -> VERIFY THE STAGED BUNDLE (strict deep, required files,
// launchd BundleProgram, identity assertions) -> same designated requirement as the installed copy -> installApp (dev) ->
// verify the installed copy. Nothing touches the destination before the staged bundle verified.
//
// Windows (installAssembledWindows): assemble the payload at `<installDirectory>.incoming` -> sign it -> prove it -> lock ->
// stop the background task and every process in the install folder -> rename the folder aside, the payload in -> restart ->
// delete the old folder. Failures before the swap leave the install untouched.
//
// Every external effect is injectable through `deps`; the defaults are the real RightKit pieces.
import { createHash } from "node:crypto";
import nodeFs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { spawn, spawnSync } from "./background-process.mjs";
import { assertSignedIdentities, assertStableDesignatedRequirement, readDevSigningSettings, signMacosDevelopmentBundle } from "./development-signing.mjs";
import { installApp } from "./install-app.mjs";
import { previousPathOf } from "./in-place-install.mjs";
import { resolveXcodeStorage } from "./ios-host.mjs";
import { resolveTargetRoot } from "./cargo-target.mjs";
import { ASSEMBLED_APP_KIND, AssembledAppError, assembleMacBundle, assembleWindowsPayload, containedRelative, loadStageReceipt, platformPath, resolveAssembly, resolveNestedApp, verifyMacBundle, verifyWindowsPayload } from "./assembled-app.mjs";

export { ASSEMBLED_APP_KIND };
const P = path.posix;
const refuse = (code, message, details) => { throw new AssembledAppError(code, message, details); };
const invalid = (message) => refuse("ASSEMBLY_CONFIG_INVALID", message);
const isObject = (value) => value !== null && typeof value === "object" && !Array.isArray(value);
const text = (result) => `${result?.stdout ?? ""}\n${result?.stderr ?? ""}`.trim();
const digest = (fs, file) => createHash("sha256").update(fs.readFileSync(file)).digest("hex");

const MAC_KEYS = ["kind", "destination", "bundleId", "identifiers", "identifierTemplate", "entitlements", "assert", "assertTeamSignedNested", "timestamp", "verifyBinaries"];
const WIN_KEYS = ["kind", "installDirectory", "stop", "restart", "carryOver"];
const SAME_IDENTITY_RULE = "dev builds use the SAME install path and bundle id as production (else permission issues), replaced in place with no backup copy";

function assertKeys(object, allowed, where) {
  if (!isObject(object)) invalid(`${where} must be an object`);
  const unknown = Object.keys(object).filter((key) => !allowed.includes(key));
  if (unknown.length) invalid(`${where} has unknown key(s): ${unknown.join(", ")} (allowed: ${allowed.join(", ")})`);
}

/** The macOS install descriptor, validated against the assembly it installs. */
export function resolveAssembledMacDescriptor(descriptor, spec, { config = {} } = {}) {
  assertKeys(descriptor, MAC_KEYS, "development.targets.mac.install");
  if (descriptor.kind !== ASSEMBLED_APP_KIND) invalid(`development.targets.mac.install.kind must be "${ASSEMBLED_APP_KIND}"`);
  if (typeof descriptor.destination !== "string" || !P.isAbsolute(descriptor.destination) || !descriptor.destination.endsWith(".app") || descriptor.destination.split("/").includes("..")) invalid("install.destination must be an absolute /…/<Product>.app path");
  const destination = P.normalize(descriptor.destination);
  if (destination !== `/Applications/${spec.productName}.app`) invalid(`install.destination ${destination} is not the production install path /Applications/${spec.productName}.app: ${SAME_IDENTITY_RULE}`);
  if (descriptor.bundleId !== undefined && descriptor.bundleId !== spec.bundleId) invalid(`install.bundleId ${descriptor.bundleId} differs from assembly.mac.bundleId ${spec.bundleId}: ${SAME_IDENTITY_RULE}`);
  const binaries = descriptor.verifyBinaries ?? [];
  if (!Array.isArray(binaries) || binaries.some((item) => typeof item !== "string")) invalid("install.verifyBinaries must be bundle-relative file paths");
  const verifyBinaries = binaries.map((item, index) => containedRelative(item, `install.verifyBinaries[${index}]`));
  try { readDevSigningSettings({ ...descriptor, bundleId: spec.bundleId }, config ?? {}); }
  catch (error) { invalid(error.message); }
  return { ...descriptor, destination, bundleId: spec.bundleId, verifyBinaries };
}

const WINDOWS_VARS = { LOCALAPPDATA: "LOCALAPPDATA", PROGRAMFILES: "ProgramFiles" };
/** Expand `%LOCALAPPDATA%` / `%PROGRAMFILES%` in the install folder; the result must be an absolute non-root path. */
export function expandInstallDirectory(value, env = process.env) {
  if (typeof value !== "string" || !value.trim()) invalid("install.installDirectory is required");
  const expanded = value.replace(/%([A-Za-z]+)%/g, (whole, name) => {
    const key = WINDOWS_VARS[name.toUpperCase()];
    if (!key) invalid(`install.installDirectory may only expand %LOCALAPPDATA% and %PROGRAMFILES% (found ${whole})`);
    const found = env[key] ?? env[name.toUpperCase()];
    if (!found) invalid(`${whole} is not set; cannot resolve install.installDirectory`);
    return found;
  });
  const normalized = path.normalize(expanded);
  if (!path.isAbsolute(normalized) || path.parse(normalized).root === normalized || normalized.split(/[\\/]/).includes("..")) invalid(`install.installDirectory must be an absolute, non-root folder: ${normalized}`);
  return normalized.replace(/[\\/]+$/, "");
}

/** The Windows install descriptor, validated; `installDirectory` stays unexpanded (the environment decides at run time). */
export function resolveAssembledWindowsDescriptor(descriptor, spec) {
  assertKeys(descriptor, WIN_KEYS, "development.targets.win.install");
  if (descriptor.kind !== ASSEMBLED_APP_KIND) invalid(`development.targets.win.install.kind must be "${ASSEMBLED_APP_KIND}"`);
  if (typeof descriptor.installDirectory !== "string" || !descriptor.installDirectory.trim()) invalid("install.installDirectory is required");
  const stop = descriptor.stop ?? {};
  assertKeys(stop, ["scheduledTask", "process", "timeoutMs"], "install.stop");
  for (const key of ["scheduledTask", "process"]) if (stop[key] !== undefined && (typeof stop[key] !== "string" || !stop[key].trim() || /["'`$;&|<>]/.test(stop[key]))) invalid(`install.stop.${key} must be a plain name`);
  if (stop.timeoutMs !== undefined && (!Number.isInteger(stop.timeoutMs) || stop.timeoutMs < 1000 || stop.timeoutMs > 120000)) invalid("install.stop.timeoutMs must be 1000..120000");
  const restart = descriptor.restart ?? {};
  assertKeys(restart, ["scheduledTask", "launch"], "install.restart");
  if (restart.scheduledTask !== undefined && typeof restart.scheduledTask !== "boolean") invalid("install.restart.scheduledTask must be true or false");
  if (restart.scheduledTask && !stop.scheduledTask) invalid("install.restart.scheduledTask needs install.stop.scheduledTask (the task to run again)");
  if (restart.launch !== undefined) containedRelative(restart.launch, "install.restart.launch");
  const carry = descriptor.carryOver ?? [];
  if (!Array.isArray(carry)) invalid("install.carryOver must be an array of file names");
  const carryOver = carry.map((item, index) => containedRelative(item, `install.carryOver[${index}]`));
  if (!spec.sign.length) invalid("the Windows dev install signs the payload exes: assembly.win.sign must list them");
  return { ...descriptor, stop: { timeoutMs: 15000, ...stop }, restart: { ...restart }, carryOver };
}

/** The xcodegen + xcodebuild argv commands of the outer app, in order (pure). */
export function outerBuildCommands({ root, spec, storage, profile }) {
  const outer = spec.outer;
  const commands = [];
  if (outer.projectSpec) commands.push({ cmd: "xcodegen", name: "xcodegen generate", args: ["generate", "--spec", path.resolve(root, outer.projectSpec), "--project", path.resolve(root, path.posix.dirname(outer.project))] });
  commands.push({
    cmd: "xcodebuild", name: `xcodebuild ${profile}`,
    args: ["-quiet", "-project", path.resolve(root, outer.project), "-scheme", outer.scheme, "-configuration", outer.configuration, "-destination", "generic/platform=macOS", "-derivedDataPath", storage.derivedData,
      ...(storage.resultBundle ? ["-resultBundlePath", storage.resultBundle] : []), "CODE_SIGNING_ALLOWED=NO", ...Object.entries(outer.extraBuildSettings).map(([name, value]) => `${name}=${value}`), "build"],
  });
  return commands;
}
export const outerProductPath = ({ spec, storage }) => path.join(storage.derivedData, "Build", "Products", spec.outer.configuration, spec.outer.product);

/** Run the outer build: unset variables removed from the environment, argv only, no shell. */
export function buildOuterApp({ root, spec, storage, profile, env = process.env, run = spawnSync }) {
  const clean = { ...env };
  for (const name of spec.outer.unsetEnv) delete clean[name];
  for (const command of outerBuildCommands({ root, spec, storage, profile })) {
    const result = run(command.cmd, command.args, { cwd: root, env: clean, stdio: "inherit", windowsHide: true, shell: false });
    if (result.error?.code === "ENOENT") refuse("ASSEMBLY_TOOL_MISSING", `${command.cmd} is not installed`);
    if (result.error || result.status !== 0) refuse("ASSEMBLY_BUILD_FAILED", `${command.name} failed (${result.status ?? result.error?.message})`);
  }
}

export function resolveAssemblyStorage({ root, profile, env, run }) {
  return resolveXcodeStorage({ env, run, cwd: root, fallbackRoot: path.join(root, ".right-release", "assembled-app", profile) });
}

// ---- macOS --------------------------------------------------------------------------------------------------------

function macDefaults() {
  return {
    fs: nodeFs, run: spawnSync, sign: signMacosDevelopmentBundle, assertRequirement: assertStableDesignatedRequirement, assertIdentities: assertSignedIdentities,
    installApp, storage: resolveAssemblyStorage, resolveTargetRoot, buildOuter: buildOuterApp, assemble: assembleMacBundle,
    makeTemp: () => nodeFs.mkdtempSync(path.join(os.tmpdir(), "rightkit-assembled-app-")), log: (line) => console.log(line),
  };
}

/** Read-only verification of a staged or installed bundle with /usr/bin/codesign (never the door shim). */
export function verifyMacSignature({ root, bundle, expectedRequirement, env, run, label }) {
  const capture = (args) => run("/usr/bin/codesign", args, { cwd: root, env, encoding: "utf8", windowsHide: true, shell: false });
  const strict = capture(["--verify", "--strict", "--deep", bundle]);
  if (strict.error || strict.status !== 0) refuse("ASSEMBLY_VERIFY_FAILED", `${label} ${bundle} failed codesign --verify --strict --deep: ${strict.error?.message ?? text(strict)}`);
  if (expectedRequirement) {
    const display = capture(["--display", "-r-", bundle]);
    const actual = /^designated => (.+)$/m.exec(text(display))?.[1]?.trim();
    if (display.status !== 0 || actual !== expectedRequirement) refuse("ASSEMBLY_VERIFY_FAILED", `${label} ${bundle} designated requirement ${JSON.stringify(actual)} is not the signed ${JSON.stringify(expectedRequirement)}`);
  }
}

/**
 * @param {{root: string, config: object, descriptor: object, spec: object, env?: object, deps?: object}} options
 * `descriptor` is development.targets.mac.install (already resolved by resolveAssembledMacDescriptor or the raw block).
 */
export async function installAssembledMac({ root, config, descriptor: rawDescriptor, spec, env = {}, deps = {} }) {
  const d = { ...macDefaults(), ...deps };
  const descriptor = resolveAssembledMacDescriptor(rawDescriptor, spec, { config });
  const destination = descriptor.destination;
  const storage = d.storage({ root, profile: "dev", env, run: d.run });
  d.buildOuter({ root, spec, storage, profile: "dev", env, run: d.run });
  const outerApp = outerProductPath({ spec, storage });
  const nestedApps = spec.nested.map((entry) => resolveNestedApp({ root, entry, spec, profile: "dev", deps: { fs: d.fs, resolveTargetRoot: d.resolveTargetRoot } }));
  const receipt = loadStageReceipt({ root, spec, profile: "dev", fs: d.fs });
  const tempRoot = d.makeTemp();
  const name = P.basename(destination);
  const staged = path.join(tempRoot, name);
  try {
    d.assemble({ root, spec, profile: "dev", out: staged, outerApp, nestedApps, receipt, env, deps: { fs: d.fs, run: d.run, log: d.log, ...(d.commitCount ? { commitCount: d.commitCount } : {}) } });
    // 1. Sign inside-out with the stable Developer ID (identifiers/entitlements/assertions are the install.* keys).
    const signing = await d.sign({ root, bundle: staged, descriptor, config, env, run: d.run, ...(d.listFiles ? { listFiles: d.listFiles } : {}), ...(d.isMachO ? { isMachO: d.isMachO } : {}) });
    if (signing.adhoc || !signing.designatedRequirement) refuse("ASSEMBLY_SIGNING_UNSTABLE", `signing produced no stable Developer ID designated requirement for ${staged}; refusing to install`);
    // 2. Verify the STAGED bundle before the destination is touched.
    verifyMacBundle({ root, spec, bundle: staged, profile: "dev", signed: false, env, deps: { fs: d.fs, run: d.run } });
    verifyMacSignature({ root, bundle: staged, expectedRequirement: signing.designatedRequirement, env, run: d.run, label: "staged" });
    const stagedDigests = new Map();
    for (const binary of descriptor.verifyBinaries) {
      try { stagedDigests.set(binary, digest(d.fs, platformPath(staged, binary))); }
      catch (error) { refuse("ASSEMBLY_VERIFY_FAILED", `cannot read ${binary} of the staged bundle: ${error.message}`); }
    }
    // 3. Same identity as the installed copy, or fail: a changed designated requirement resets every privacy grant.
    if (d.fs.existsSync(destination)) await d.assertRequirement({ root, installed: destination, expected: signing.designatedRequirement, env, run: d.run });
    // 4. Install in place (no backup), then verify the installed copy; a failure removes the bad copy.
    await d.installApp({ app: staged, dev: true, dest: P.dirname(destination), appName: name, bundleId: spec.bundleId, relaunch: null }, { env });
    d.fs.rmSync(previousPathOf(destination), { recursive: true, force: true });
    try {
      verifyMacSignature({ root, bundle: destination, expectedRequirement: signing.designatedRequirement, env, run: d.run, label: "installed" });
      const settings = readDevSigningSettings(descriptor, config ?? {});
      if (settings.assertions.length || settings.assertTeamSignedNested) {
        d.assertIdentities({ root, bundle: destination, settings, teamIdentifier: signing.teamIdentifier, identity: signing.identity, env, run: d.run, label: "installed copy", ...(d.listFiles ? { listFiles: d.listFiles } : {}), ...(d.isMachO ? { isMachO: d.isMachO } : {}) });
      }
      for (const [binary, want] of stagedDigests) {
        let got;
        try { got = digest(d.fs, platformPath(destination, binary)); } catch (error) { refuse("ASSEMBLY_VERIFY_FAILED", `cannot read installed ${binary}: ${error.message}`); }
        if (got !== want) refuse("ASSEMBLY_VERIFY_FAILED", `installed ${binary} does not match the signed staged copy`);
      }
    } catch (error) {
      d.fs.rmSync(destination, { recursive: true, force: true });
      error.rolledBack = "removed-failed-install";
      error.message = `${error.message}; the failed install was removed and no old copy is kept: reinstall from an older DMG`;
      throw error;
    }
    d.log(`right-release dev: installed ${destination}; identity=${signing.identity}; TeamIdentifier=${signing.teamIdentifier}; designated=${signing.designatedRequirement}`);
    return { installed: destination, designatedRequirement: signing.designatedRequirement, identity: signing.identity, teamIdentifier: signing.teamIdentifier, signing };
  } finally { d.fs.rmSync(tempRoot, { recursive: true, force: true }); }
}

// ---- Windows ------------------------------------------------------------------------------------------------------

const SIGN_WINDOWS = fileURLToPath(new URL("./sign-windows.mjs", import.meta.url));
const BUSY = ["EBUSY", "EPERM", "EPIPE", "EACCES"];
const quoted = (value) => `'${String(value).replaceAll("'", "''")}'`;

function windowsDefaults(env) {
  const system = env.SystemRoot ?? env.SYSTEMROOT ?? "C:\\Windows";
  const run = spawnSync;
  const ps = (command) => run(path.win32.join(system, "System32", "WindowsPowerShell", "v1.0", "powershell.exe"), ["-NoProfile", "-NonInteractive", "-Command", command], { encoding: "utf8", windowsHide: true, shell: false });
  const schtasks = (args) => run(path.win32.join(system, "System32", "schtasks.exe"), args, { encoding: "utf8", windowsHide: true, shell: false });
  const signTool = (extra, files) => run(process.execPath, [SIGN_WINDOWS, ...extra, ...files], { stdio: "inherit", windowsHide: true, shell: false });
  return {
    fs: nodeFs, run, platform: process.platform, pid: process.pid,
    sleep: (ms) => Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, ms),
    isAlive: (pid) => { try { process.kill(pid, 0); return true; } catch (error) { return error.code === "EPERM"; } },
    sign: (files) => signTool([], files), verifySigned: (files) => signTool(["--verify-only"], files),
    isRunning: (exe) => ps(`if (@(Get-Process -ErrorAction SilentlyContinue | Where-Object { $_.Path -eq ${quoted(exe)} }).Count -gt 0) { exit 0 } else { exit 1 }`).status === 0,
    stopInDirectory: (dir) => ps(`Get-Process -ErrorAction SilentlyContinue | Where-Object { $_.Path -and $_.Path.StartsWith(${quoted(`${dir}\\`)}, [StringComparison]::OrdinalIgnoreCase) } | Stop-Process -Force`),
    anyInDirectory: (dir) => ps(`if (@(Get-Process -ErrorAction SilentlyContinue | Where-Object { $_.Path -and $_.Path.StartsWith(${quoted(`${dir}\\`)}, [StringComparison]::OrdinalIgnoreCase) }).Count -eq 0) { exit 1 } else { exit 0 }`).status === 0,
    endTask: (name) => schtasks(["/end", "/tn", name]), runTask: (name) => schtasks(["/run", "/tn", name]),
    launch: (exe) => { spawn(exe, [], { detached: true, stdio: "ignore", windowsHide: false, shell: false }).unref(); },
    assemble: assembleWindowsPayload, log: (line) => console.log(line),
  };
}

const exists = (fs, file) => { try { fs.lstatSync(file); return true; } catch (error) { if (error?.code === "ENOENT") return false; throw error; } };
function listFiles(fs, root, dir = root) {
  return fs.readdirSync(dir, { withFileTypes: true }).flatMap((entry) => {
    const file = path.join(dir, entry.name);
    return entry.isDirectory() ? listFiles(fs, root, file) : [path.relative(root, file).split(path.sep).join("/")];
  }).sort();
}

/**
 * The Windows dev install. Synchronous and ordered: failures before the swap leave the install untouched; once the folders are
 * swapped the install is a whole new payload and later problems are reported as `warnings` (the swap is not undone).
 * @returns {{installDirectory: string, files: number, signed: number, warnings: string[], launched: boolean}}
 */
export function installAssembledWindows({ root, config, descriptor: rawDescriptor, spec, env = process.env, deps = {} }) {
  const d = { ...windowsDefaults(env), ...deps };
  const descriptor = resolveAssembledWindowsDescriptor(rawDescriptor, spec);
  const fs = d.fs;
  const dir = expandInstallDirectory(descriptor.installDirectory, env);
  const incoming = `${dir}.incoming`;
  const replaced = `${dir}.replaced`;
  const lockFile = `${dir}.install-lock`;
  const warnings = [];
  const fail = (message) => refuse("ASSEMBLY_INSTALL_FAILED", `${message}`);
  const removeOrAside = (target) => {
    try { fs.rmSync(target, { recursive: true, force: true }); return true; }
    catch (error) {
      if (!exists(fs, target)) return true;
      try { fs.renameSync(target, `${target}-aside-${Date.now()}`); return true; }
      catch { d.log(`right-release dev: ${target} could not be removed or moved aside (${error.code})`); return false; }
    }
  };
  // One install at a time: the lock holds this pid; a lock whose process is gone is stale and taken over.
  let locked = false;
  for (let attempt = 0; !locked; attempt += 1) {
    try { fs.writeFileSync(lockFile, String(d.pid), { flag: "wx" }); locked = true; }
    catch (error) {
      if (error.code !== "EEXIST" || attempt >= 2) fail(`cannot take the install lock ${lockFile} (${error.code}); the install was not touched`);
      let holder = 0;
      try { holder = Number.parseInt(fs.readFileSync(lockFile, "utf8"), 10); } catch { continue; }
      if (Number.isInteger(holder) && holder > 0 && d.isAlive(holder)) fail(`another install (pid ${holder}) is running; the install was not touched`);
      try { fs.rmSync(lockFile, { force: true }); } catch { /* retry */ }
    }
  }
  try {
    // A run that stopped between its two renames: the missing install folder gets its old copy back first.
    if (!exists(fs, dir) && exists(fs, replaced)) { fs.renameSync(replaced, dir); d.log(`right-release dev: put back ${dir} from an install that stopped between its two renames`); }
    for (const leftover of [replaced, incoming, previousPathOf(dir)]) if (!removeOrAside(leftover)) fail(`${leftover} could not be removed or moved aside; the install was not touched`);

    // 1. Assemble, sign and prove the incoming payload beside the install (same volume); the running install is not touched.
    const abandon = (message) => { removeOrAside(incoming); fail(`${message}; the install was not touched`); };
    let assembled;
    try {
      const receipt = loadStageReceipt({ root, spec, profile: "dev", fs });
      assembled = d.assemble({ root, spec, profile: "dev", out: incoming, receipt, deps: { fs, log: d.log } });
    } catch (error) { removeOrAside(incoming); throw error; }
    const before = listFiles(fs, incoming);
    const signFiles = spec.sign.map((file) => platformPath(incoming, file));
    const unsigned = before.filter((file) => !spec.sign.map((s) => s.toLowerCase()).includes(file.toLowerCase())).map((file) => [file, digest(fs, platformPath(incoming, file))]);
    const signResult = d.sign(signFiles);
    if (signResult?.status !== 0) abandon("signing the payload failed");
    const verifyResult = d.verifySigned(signFiles);
    if (verifyResult?.status !== 0) abandon("Authenticode verification of the incoming payload failed");
    if (listFiles(fs, incoming).join("\n") !== before.join("\n")) abandon("the incoming payload changed its file list while signing");
    for (const [file, want] of unsigned) if (digest(fs, platformPath(incoming, file)) !== want) abandon(`${file} changed while signing`);
    try { verifyWindowsPayload({ spec, payload: incoming, profile: "dev", fs }); } catch (error) { removeOrAside(incoming); throw error; }

    // 2. Stop: the background task and everything that runs from the install folder.
    const exe = descriptor.stop.process ? path.join(dir, descriptor.stop.process) : null;
    const wasRunning = exe ? d.isRunning(exe) : false;
    if (descriptor.stop.scheduledTask) d.endTask(descriptor.stop.scheduledTask);
    d.stopInDirectory(dir);
    const relaunch = () => {
      if (descriptor.restart.scheduledTask) d.runTask(descriptor.stop.scheduledTask);
      if (wasRunning && descriptor.restart.launch && !d.isRunning(path.join(dir, descriptor.restart.launch))) d.launch(path.join(dir, descriptor.restart.launch));
    };
    const deadline = d.now ? d.now() + descriptor.stop.timeoutMs : Date.now() + descriptor.stop.timeoutMs;
    const nowMs = () => (d.now ? d.now() : Date.now());
    while (exists(fs, dir) && d.anyInDirectory(dir)) {
      if (nowMs() > deadline) { relaunch(); abandon(`processes still run from the install folder after ${descriptor.stop.timeoutMs / 1000} s; the old copy was started again`); }
      d.sleep(500);
      d.stopInDirectory(dir);
    }

    // 3. Swap by rename; Windows refuses while a process (a chat hook, say) runs from the folder, so retry after stopping it again.
    let movedAside = !exists(fs, dir);
    for (let attempt = 1; !movedAside; attempt += 1) {
      try { fs.renameSync(dir, replaced); movedAside = true; }
      catch (error) {
        if (attempt >= 6 || !BUSY.includes(error.code)) { relaunch(); abandon(`the install folder could not be moved aside (${error.code}); the old copy was started again`); }
        d.log(`right-release dev: swap attempt ${attempt} hit ${error.code}; stopping what runs from the install folder and retrying`);
        d.stopInDirectory(dir);
        d.sleep(700);
      }
    }
    try { fs.renameSync(incoming, dir); }
    catch (error) {
      try { if (exists(fs, replaced)) fs.renameSync(replaced, dir); }
      catch (back) { fail(`the swap failed (${error.code}) and the old copy could not be put back (${back.code}); it is at ${replaced}. Nothing was started; run the install again, which puts it back first`); }
      relaunch();
      abandon(`the new copy could not be moved into place (${error.code}); the old copy was put back and started again`);
    }
    // 4. Installer-owned files the payload lacks (Uninstall.exe) come from the folder that was moved aside, before it is deleted.
    for (const name of descriptor.carryOver) {
      if (assembled.bound.some((item) => item.dest === name) || before.includes(name) || !exists(fs, platformPath(replaced, name)) || exists(fs, platformPath(dir, name))) continue;
      try { fs.renameSync(platformPath(replaced, name), platformPath(dir, name)); } catch (error) { warnings.push(`${name} could not be carried over (${error.code}); the replaced copy is kept at ${replaced}`); }
    }
    // 5. Start the new copy; the old folder goes only when nothing needs it (no backup is kept).
    if (descriptor.restart.scheduledTask) d.runTask(descriptor.stop.scheduledTask);
    let launched = !wasRunning;
    if (wasRunning && descriptor.restart.launch) {
      d.launch(path.join(dir, descriptor.restart.launch));
      for (let waited = 0; waited < 10 && !launched; waited += 1) { d.sleep(1000); launched = d.isRunning(path.join(dir, descriptor.restart.launch)); }
      if (!launched) warnings.push(`the new ${descriptor.restart.launch} did not start within 10 s; the replaced copy is kept at ${replaced}`);
    }
    if (!warnings.length) {
      try { fs.rmSync(replaced, { recursive: true, force: true }); } catch (error) { warnings.push(`${replaced} could not be deleted (${error.code}); the next run removes it`); }
    }
    for (const warning of warnings) d.log(`right-release dev: WARNING (swap succeeded): ${warning}`);
    d.log(`right-release dev: ${dir}: whole payload swapped in (${before.length} files, ${signFiles.length} signed and verified)`);
    return { installDirectory: dir, files: before.length, signed: signFiles.length, warnings, launched };
  } finally { try { fs.rmSync(lockFile, { force: true }); } catch { /* a leftover lock of a dead pid is taken over */ } }
}

/** Dispatch used by development.mjs: the validated assembly + descriptor for this platform. */
export function resolveAssembledInstall({ config, platform }) {
  const spec = resolveAssembly(config, platform);
  const descriptor = config.development.targets[platform].install;
  if (platform === "mac") resolveAssembledMacDescriptor(descriptor, spec, { config });
  else resolveAssembledWindowsDescriptor(descriptor, spec);
  return { spec, descriptor };
}
