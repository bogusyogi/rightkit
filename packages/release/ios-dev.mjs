// right-release dev --platform ios: a Debug build installed on a simulator or a physical device. Never a release lane: it
// archives nothing, uploads nothing and reads no App Store Connect credential. The simulator flow follows ScrapeRight's
// scripts/run-e2e.sh (simctl boot, install, launch --terminate-running-process); the device flow follows HeardRight's
// device-smoke.sh (devicectl install, process launch --terminate-existing --no-activate).
//
// Config (development.targets.ios):
//   {
//     project: "ios/Pulse.xcodeproj",
//     projectSpec?: "ios/project.yml",         // when named, `xcodegen generate` runs first and is REQUIRED (no fallback project)
//     scheme: "Pulse", configuration?: "Debug", teamId?: "ABCDE12345",   // teamId is required for a device build
//     extraBuildSettings?: { NAME: "value" },    // non-secret
//     install: { kind: "simulator", device: "<UDID or exact name>", bundleId, productName?, boot?: true, launch?: true, launchArgs?: [] }
//           | { kind: "device",     device: "<devicectl identifier>", bundleId, productName?, launch?: true, launchArgs?: [] },
//   }
// The build commands are generated from the config (a hand-written `build` is refused). The device is chosen by id only, never
// "the only device": `install.device`, `--serial` or RIGHTKIT_IOS_DEVICE. DerivedData comes from `rightkit storage xcode-args`.
import * as nodeFs from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "./background-process.mjs";
import { assertGuiSigningSession, IosHostError, outputTail, resolveXcodeStorage } from "./ios-host.mjs";
import { SECRET_VALUE } from "./ios-package-recipe.mjs";

export class IosDevelopmentError extends Error {
  constructor(code, message) { super(message); this.name = "IosDevelopmentError"; this.code = code; }
}
const refuse = (code, message) => { throw new IosDevelopmentError(code, message); };
const isObject = (value) => value !== null && typeof value === "object" && !Array.isArray(value);

const TARGET_KEYS = ["project", "projectSpec", "scheme", "configuration", "teamId", "extraBuildSettings", "prepare", "install"];
const INSTALL_KEYS = ["kind", "device", "bundleId", "productName", "boot", "launch", "launchArgs"];
const UDID = /^[0-9A-Fa-f]{8}-(?:[0-9A-Fa-f]{4}-){3}[0-9A-Fa-f]{12}$/;
const DEVICE_ID = /^[A-Za-z0-9][A-Za-z0-9._:-]{3,80}$/;
const RESERVED_SETTING = /^(?:DEVELOPMENT_TEAM|CODE_SIGN\w*|PROVISIONING\w*|OTHER_CODE_SIGN_FLAGS|ARCHS?|SDKROOT)$/;
const SECRET_SETTING = /SECRET|TOKEN|PASSWORD|PASSWD|BEARER|PRIVATE|CREDENTIAL|API_?KEY|AUTH/i;
const RELEASE_WORD = /^-{0,2}(?:archive|exportArchive|altool|upload|upload-app|upload-package|testflight)$/i;

const relativeInside = (value, label, extension) => {
  if (typeof value !== "string" || !value.trim()) refuse("DEV_IOS_CONFIG_INVALID", `${label} must be a relative path inside the app`);
  const normalized = value.replaceAll("\\", "/");
  if (path.isAbsolute(value) || path.win32.isAbsolute(value) || /^[A-Za-z]:/.test(value) || normalized.split("/").includes("..")) refuse("DEV_IOS_CONFIG_INVALID", `${label} must be relative and stay inside the app`);
  if (extension && !extension.test(normalized)) refuse("DEV_IOS_CONFIG_INVALID", `${label} must match ${extension}`);
  return normalized.replace(/^\.\//, "");
};

export function iosDevelopmentDevice({ descriptor, serial, env = process.env }) {
  const chosen = serial ?? descriptor?.device ?? env.RIGHTKIT_IOS_DEVICE;
  if (!chosen) refuse("DEV_IOS_DEVICE_REQUIRED", "name the device by id (install.device, --serial or RIGHTKIT_IOS_DEVICE); the only attached device is never assumed");
  const text = String(chosen);
  const ok = descriptor?.kind === "simulator" ? (UDID.test(text) || /^[A-Za-z0-9][A-Za-z0-9 ()._-]{1,80}$/.test(text)) : DEVICE_ID.test(text);
  if (!ok) refuse("DEV_IOS_DEVICE_INVALID", "the iOS device must be a simulator UDID or exact name, or a devicectl device identifier");
  return text;
}

/** Validates development.targets.ios in place of the generic build/install rules. Returns the normalized target. */
export function validateIosDevelopment(target, { root, env = process.env } = {}) {
  if (!isObject(target)) refuse("DEV_IOS_CONFIG_INVALID", "development.targets.ios must be an object");
  if (target.build !== undefined) refuse("DEV_IOS_CONFIG_INVALID", "development.targets.ios.build is generated from project, scheme and install; remove it");
  const unknown = Object.keys(target).filter((key) => !TARGET_KEYS.includes(key));
  if (unknown.length) refuse("DEV_IOS_CONFIG_INVALID", `development.targets.ios has unknown key(s): ${unknown.join(", ")} (allowed: ${TARGET_KEYS.join(", ")})`);
  const project = relativeInside(target.project, "development.targets.ios.project", /\.xcodeproj$/i);
  const projectSpec = target.projectSpec === undefined ? null : relativeInside(target.projectSpec, "development.targets.ios.projectSpec", /\.ya?ml$/i);
  if (projectSpec && path.posix.dirname(projectSpec) !== path.posix.dirname(project)) refuse("DEV_IOS_CONFIG_INVALID", "projectSpec and project must be in the same directory");
  if (typeof target.scheme !== "string" || !/^[A-Za-z0-9][A-Za-z0-9 ._-]{0,100}$/.test(target.scheme)) refuse("DEV_IOS_CONFIG_INVALID", "development.targets.ios.scheme is required");
  const configuration = target.configuration ?? "Debug";
  if (typeof configuration !== "string" || !/^[A-Za-z][A-Za-z0-9 _-]{0,40}$/.test(configuration)) refuse("DEV_IOS_CONFIG_INVALID", "configuration must be a build configuration name");
  if (/release/i.test(configuration)) refuse("DEV_IOS_RELEASE_CONFIGURATION", `development.targets.ios.configuration ${configuration} is a release configuration; a dev build is a Debug build`);
  if (target.teamId !== undefined && !/^[A-Z0-9]{10}$/.test(String(target.teamId))) refuse("DEV_IOS_CONFIG_INVALID", "teamId must be the 10 character Apple team id");
  const extraBuildSettings = {};
  if (target.extraBuildSettings !== undefined) {
    if (!isObject(target.extraBuildSettings)) refuse("DEV_IOS_CONFIG_INVALID", "extraBuildSettings must be an object of NAME: value");
    for (const [name, value] of Object.entries(target.extraBuildSettings)) {
      if (!/^[A-Z][A-Z0-9_]*$/.test(name) || RESERVED_SETTING.test(name)) refuse("DEV_IOS_CONFIG_INVALID", `extraBuildSettings.${name} is not allowed`);
      if (SECRET_SETTING.test(name)) refuse("DEV_IOS_CONFIG_INVALID", `extraBuildSettings.${name} looks like a secret; build settings travel on the command line`);
      if (typeof value !== "string" || /[\0\r\n]/.test(value) || SECRET_VALUE.test(value)) refuse("DEV_IOS_CONFIG_INVALID", `extraBuildSettings.${name} must be a single-line non-secret string`);
      extraBuildSettings[name] = value;
    }
  }
  const prepare = (target.prepare ?? []).map((item, index) => {
    if (!isObject(item) || typeof item.cmd !== "string" || !item.cmd.trim() || /\s/.test(item.cmd.trim()) || (item.args !== undefined && (!Array.isArray(item.args) || item.args.some((arg) => typeof arg !== "string")))) {
      refuse("DEV_IOS_CONFIG_INVALID", `development.targets.ios.prepare[${index}] must be { cmd, args?, cwd? }`);
    }
    const tokens = [item.cmd, ...(item.args ?? [])];
    if (tokens.some((token) => RELEASE_WORD.test(token))) refuse("DEV_IOS_RELEASE_TASK", `development.targets.ios.prepare[${index}] looks like a release, archive or upload step; a dev build never archives, uploads or distributes`);
    return { cmd: item.cmd.trim(), args: [...(item.args ?? [])], ...(item.cwd === undefined ? {} : { cwd: relativeInside(item.cwd, `prepare[${index}].cwd`) }) };
  });

  let install = null;
  if (target.install !== undefined) {
    const descriptor = target.install;
    if (!isObject(descriptor)) refuse("DEV_IOS_CONFIG_INVALID", "development.targets.ios.install must be an object");
    const bad = Object.keys(descriptor).filter((key) => !INSTALL_KEYS.includes(key));
    if (bad.length) refuse("DEV_IOS_CONFIG_INVALID", `development.targets.ios.install has unknown key(s): ${bad.join(", ")} (allowed: ${INSTALL_KEYS.join(", ")})`);
    if (descriptor.kind !== "simulator" && descriptor.kind !== "device") refuse("DEV_IOS_INSTALL_KIND", "development.targets.ios.install.kind must be simulator or device");
    if (typeof descriptor.bundleId !== "string" || !/^[A-Za-z0-9-]+(?:\.[A-Za-z0-9-]+)+$/.test(descriptor.bundleId)) refuse("DEV_IOS_CONFIG_INVALID", "install.bundleId must be the app bundle id");
    if (descriptor.productName !== undefined && (typeof descriptor.productName !== "string" || !/^[A-Za-z0-9][A-Za-z0-9 ._-]{0,80}$/.test(descriptor.productName))) refuse("DEV_IOS_CONFIG_INVALID", "install.productName must be the built .app name without the extension");
    for (const key of ["boot", "launch"]) if (descriptor[key] !== undefined && typeof descriptor[key] !== "boolean") refuse("DEV_IOS_CONFIG_INVALID", `install.${key} must be a boolean`);
    if (descriptor.kind === "device" && descriptor.boot !== undefined) refuse("DEV_IOS_CONFIG_INVALID", "install.boot applies to a simulator only");
    if (descriptor.launchArgs !== undefined && (!Array.isArray(descriptor.launchArgs) || descriptor.launchArgs.some((arg) => typeof arg !== "string" || /[\0\r\n]/.test(arg)))) refuse("DEV_IOS_CONFIG_INVALID", "install.launchArgs must be a list of strings");
    if (descriptor.device !== undefined) iosDevelopmentDevice({ descriptor, env: {} });
    if (descriptor.kind === "device" && !target.teamId) refuse("DEV_IOS_CONFIG_INVALID", "a device build needs development.targets.ios.teamId (there is no built-in default)");
    install = { ...descriptor, productName: descriptor.productName ?? target.scheme, launchArgs: [...(descriptor.launchArgs ?? [])] };
  }
  const normalized = { project, projectSpec, scheme: target.scheme, configuration, teamId: target.teamId, extraBuildSettings, prepare, install };
  return { ...normalized, build: iosDevelopmentBuildCommands(normalized, { root, storage: { derivedData: "<rightkit storage xcode-args -derivedDataPath>" }, destinationId: "<device or simulator id>" }) };
}

const destinationFor = (install, id) => (install?.kind === "device" ? `id=${id}` : `platform=iOS Simulator,id=${id}`);
export const productsDirectory = (dev) => `${dev.configuration}-${dev.install?.kind === "device" ? "iphoneos" : "iphonesimulator"}`;

/** Pure: the generated argv commands, in order. No secret is ever part of them. */
export function iosDevelopmentBuildCommands(dev, { root, storage, destinationId }) {
  const commands = [];
  if (dev.projectSpec) commands.push({ cmd: "xcodegen", args: ["generate", "--spec", path.resolve(root, dev.projectSpec), "--project", path.resolve(root, path.posix.dirname(dev.project))], name: "xcodegen generate" });
  for (const step of dev.prepare) commands.push({ ...step, name: step.cmd });
  const device = dev.install?.kind === "device";
  commands.push({
    cmd: "xcodebuild",
    name: "xcodebuild build",
    args: [
      "-project", path.resolve(root, dev.project),
      "-scheme", dev.scheme,
      "-configuration", dev.configuration,
      "-destination", dev.install ? destinationFor(dev.install, destinationId) : "generic/platform=iOS Simulator",
      "-derivedDataPath", storage.derivedData,
      ...(storage.resultBundle ? ["-resultBundlePath", storage.resultBundle] : []),
      ...(dev.teamId ? [`DEVELOPMENT_TEAM=${dev.teamId}`] : []),
      ...Object.entries(dev.extraBuildSettings).map(([name, value]) => `${name}=${value}`),
      ...(device ? ["-allowProvisioningUpdates", "-allowProvisioningDeviceRegistration"] : []),
      "build",
    ],
  });
  return commands;
}

// ---- device and simulator lookup --------------------------------------------------------------------------------
const xcrun = (run, args, options = {}) => run("xcrun", args, { encoding: "utf8", windowsHide: true, shell: false, timeout: 120_000, ...options });

export function listSimulators(run) {
  const result = xcrun(run, ["simctl", "list", "devices", "available", "--json"], { timeout: 60_000 });
  if (result.error || result.status !== 0) refuse("DEV_IOS_SIMCTL_FAILED", `simctl list failed (${result.status ?? "unavailable"})`);
  let parsed;
  try { parsed = JSON.parse(String(result.stdout)); } catch { refuse("DEV_IOS_SIMCTL_FAILED", "simctl list returned invalid JSON"); }
  return Object.entries(parsed.devices ?? {}).flatMap(([runtime, devices]) => (devices ?? []).filter((device) => device.isAvailable !== false).map((device) => ({ udid: String(device.udid), name: String(device.name), state: String(device.state), runtime })));
}

export function selectSimulator(devices, wanted) {
  const matches = UDID.test(wanted) ? devices.filter((device) => device.udid.toLowerCase() === wanted.toLowerCase()) : devices.filter((device) => device.name === wanted);
  if (!matches.length) refuse("DEV_IOS_DEVICE_NOT_FOUND", `no available simulator matches ${wanted}`);
  if (matches.length > 1) refuse("DEV_IOS_DEVICE_AMBIGUOUS", `${matches.length} simulators are named ${wanted}; use the UDID`);
  return matches[0];
}

/** Port of HeardRight's check-ios-device-ready.py readiness rule. */
export function deviceReadiness(device) {
  const props = device.deviceProperties ?? {};
  const hardware = device.hardwareProperties ?? {};
  const tunnel = device.connectionProperties?.tunnelState ?? "unknown";
  const ddi = props.ddiServicesAvailable;
  const ready = !["unavailable", "disconnected"].includes(tunnel) && ddi !== false;
  return { ready, developerMode: props.developerModeStatus ?? "unknown", detail: `${props.name ?? device.name ?? "device"} (${hardware.productType ?? hardware.marketingName ?? "unknown model"}) tunnel=${tunnel} ddi=${ddi} developerMode=${props.developerModeStatus ?? "unknown"}` };
}

export function selectDevice(devices, wanted) {
  const physical = devices.filter((device) => (device.deviceProperties?.name ?? device.name) !== "My Mac" && device.hardwareProperties?.deviceType !== "Mac");
  const found = physical.find((device) => [device.identifier, device.deviceProperties?.identifier, device.hardwareProperties?.udid, device.deviceProperties?.name, device.name].includes(wanted));
  if (!found) refuse("DEV_IOS_DEVICE_NOT_FOUND", `devicectl does not report a device ${wanted}`);
  const state = deviceReadiness(found);
  if (!state.ready) refuse("DEV_IOS_DEVICE_NOT_READY", `${state.detail}: connect it, unlock it, keep it awake, trust this Mac and enable Developer Mode`);
  return { identifier: String(found.identifier ?? found.deviceProperties?.identifier ?? wanted), detail: state.detail };
}

function listDevices({ run, fs, tmpRoot }) {
  const dir = fs.mkdtempSync(path.join(tmpRoot, "rightkit-ios-dev-"));
  try {
    const file = path.join(dir, "devices.json");
    const result = xcrun(run, ["devicectl", "list", "devices", "--json-output", file, "--timeout", "10"], { timeout: 60_000 });
    if (result.error || result.status !== 0) refuse("DEV_IOS_DEVICECTL_FAILED", `devicectl list devices failed (${result.status ?? "unavailable"})`);
    try { return JSON.parse(fs.readFileSync(file, "utf8")).result?.devices ?? []; } catch { return refuse("DEV_IOS_DEVICECTL_FAILED", "devicectl returned unreadable device JSON"); }
  } finally { fs.rmSync(dir, { recursive: true, force: true }); }
}

/**
 * Resolve the storage, the device or simulator and the final build commands BEFORE the (long) build, so a missing or
 * unready device is refused up front. Returns { build, install } with install carrying the resolved id and the .app path.
 */
export function prepareIosDevelopment(dev, { root, env = process.env, run = spawnSync, fs = nodeFs, serial, tmpRoot = os.tmpdir() }) {
  const storage = resolveXcodeStorage({ env, run, cwd: root, fallbackRoot: path.join(root, ".right-release", "ios-dev") });
  let resolved = null;
  if (dev.install) {
    const wanted = iosDevelopmentDevice({ descriptor: dev.install, serial, env });
    if (dev.install.kind === "simulator") {
      const simulator = selectSimulator(listSimulators(run), wanted);
      resolved = { id: simulator.udid, state: simulator.state, name: simulator.name };
    } else {
      const device = selectDevice(listDevices({ run, fs, tmpRoot }), wanted);
      resolved = { id: device.identifier, detail: device.detail };
    }
  }
  const build = iosDevelopmentBuildCommands(dev, { root, storage, destinationId: resolved?.id });
  const app = dev.install ? path.join(storage.derivedData, "Build", "Products", productsDirectory(dev), `${dev.install.productName}.app`) : null;
  return { build, storage, install: dev.install ? { ...dev.install, resolved, app } : null };
}

// App arguments follow a "--" terminator so an argument such as --device or --json-output is passed to the app and can never be
// read as an option of simctl or devicectl (the standard ArgumentParser terminator; unverified on a live Mac, see the docs).
const launchTail = (args) => (args.length ? ["--", ...args] : []);

// ---- install ------------------------------------------------------------------------------------------------------
/** `run` is spawnSync-shaped (tests inject a fake). Every step is bounded; the first failure stops and throws. */
export function installIosDevelopment(install, { root, env = process.env, run = spawnSync, fs = nodeFs, tmpRoot = os.tmpdir(), log = console.log } = {}) {
  if (!install?.app || !install.resolved) refuse("DEV_IOS_APP_MISSING", "the install was not prepared (no built .app)");
  if (!fs.existsSync(install.app)) refuse("DEV_IOS_APP_MISSING", `the Debug build did not produce ${path.basename(install.app)}`);
  const id = install.resolved.id;
  const step = (name, args, options) => {
    const result = xcrun(run, args, { cwd: root, env, ...options });
    if (result.error || result.status !== 0) refuse(`DEV_IOS_${name.toUpperCase()}_FAILED`, `${args.slice(0, 3).join(" ")} failed (${result.status ?? "unavailable"})\n${outputTail(result, 20)}`);
    return result;
  };
  if (install.kind === "simulator") {
    if (install.resolved.state !== "Booted" && install.boot !== false) step("boot", ["simctl", "boot", id], { timeout: 120_000 });
    step("install", ["simctl", "install", id, install.app], { timeout: 5 * 60_000 });
    if (install.launch !== false) step("launch", ["simctl", "launch", "--terminate-running-process", id, install.bundleId, ...launchTail(install.launchArgs)], { timeout: 60_000 });
    log(`right-release dev: installed ${install.bundleId} on simulator ${install.resolved.name} (${id}) (debug build; not a release artifact)`);
    return { kind: "simulator", device: id, bundleId: install.bundleId, app: install.app };
  }
  step("install", ["devicectl", "device", "install", "app", "--device", id, install.app], { timeout: 10 * 60_000 });
  if (install.launch !== false) {
    const dir = fs.mkdtempSync(path.join(tmpRoot, "rightkit-ios-dev-"));
    try {
      const out = path.join(dir, "launch.json");
      step("launch", ["devicectl", "device", "process", "launch", "--device", id, "--terminate-existing", "--no-activate", "--json-output", out, install.bundleId, ...launchTail(install.launchArgs)], { timeout: 120_000 });
      let payload = {};
      try { payload = JSON.parse(fs.readFileSync(out, "utf8")); } catch { /* reported below */ }
      const result = payload.result ?? {};
      if (!(result.process || result.processIdentifier || result.launchResult)) refuse("DEV_IOS_LAUNCH_FAILED", "devicectl launch did not report a process result");
    } finally { fs.rmSync(dir, { recursive: true, force: true }); }
  }
  log(`right-release dev: installed ${install.bundleId} on device ${id} (debug build; not a release artifact)`);
  return { kind: "device", device: id, bundleId: install.bundleId, app: install.app };
}

/** The GUI signing session check for a device build (the keychain must be unlocked); a simulator build signs nothing. */
export function assertIosDevelopmentSession(dev, { env = process.env, run = spawnSync, detect, route } = {}) {
  if (dev.install?.kind !== "device") return { routed: false, skipped: true };
  try { return assertGuiSigningSession({ operation: "right-release dev --platform ios (device build)", env, run, detect, route }); }
  catch (error) { if (error instanceof IosHostError) refuse(error.code, error.message); throw error; }
}
