// right-release dev --platform android: a DEBUG build installed on an emulator or a device through adb. Never a release lane:
// no keystore, no password, no upload key and no AAB. The release lane (android-release.mjs) and the upload (android-play-upload.mjs)
// refuse everything this file accepts.
//
// Config (development.targets.android):
//   {
//     ndk?: { abis?: ["arm64-v8a", "x86_64"], jniLibs: "android/app/src/main/jniLibs", manifestPath?, package?, features?, apiLevel? },
//     build: [{ cmd: "./gradlew", args: [":app:assembleDebug"], cwd: "android" }],     // debug tasks only (no bundle/assemble Release)
//     install: { kind: "adb", apk: "android/app/build/outputs/apk/debug/app-debug.apk", package: "com.orthiclabs.heardright.dev",
//                activity?: ".MainActivity", serial?: "emulator-5554" },
//   }
// The device is chosen by serial only, never "the only device": `serial` here, `--serial` on the command line, or
// RIGHTKIT_ANDROID_SERIAL. The package's last segment must be dev, qa, test or debug, so a dev install can coexist with the
// production app and can never replace it (rightkit-qa requirement, docs/design/android-minimum.md section 5).
import { existsSync, statSync } from "node:fs";
import path from "node:path";
import { spawnSync } from "./background-process.mjs";
import { androidNdkCommand, validateAndroidNdk } from "./android-ndk.mjs";

export class AndroidDevelopmentError extends Error {
  constructor(code, message) { super(message); this.name = "AndroidDevelopmentError"; this.code = code; }
}
const refuse = (code, message) => { throw new AndroidDevelopmentError(code, message); };

const SERIAL = /^[A-Za-z0-9][A-Za-z0-9._:-]{0,63}$/;
const PACKAGE_NAME = /^[A-Za-z][A-Za-z0-9_]*(?:\.[A-Za-z][A-Za-z0-9_]*)+$/;
const ACTIVITY = /^\.?[A-Za-z][A-Za-z0-9_.$]*$/;
const TEST_SEGMENT = /^(?:dev|qa|test|debug)$/i;
const RELEASE_TASK = /(?:bundle|assemble|install)[A-Za-z0-9]*Release|publish|upload/i;
const inside = (root, child) => { const rel = path.relative(path.resolve(root), path.resolve(child)); return rel !== "" && !rel.startsWith("..") && !path.isAbsolute(rel); };

export function androidDevelopmentSerial({ descriptor, serial, env = process.env }) {
  const chosen = serial ?? descriptor?.serial ?? env.RIGHTKIT_ANDROID_SERIAL;
  if (!chosen) refuse("DEV_ANDROID_SERIAL_REQUIRED", "name the device by serial (install.serial, --serial or RIGHTKIT_ANDROID_SERIAL); the only attached device is never assumed");
  if (!SERIAL.test(chosen)) refuse("DEV_ANDROID_SERIAL_INVALID", "the adb serial must be a plain device or emulator serial");
  return chosen;
}

// Validates development.targets.android in place of the generic build/install rules. Returns { ndk } (normalised or null).
export function validateAndroidDevelopment(target, { root }) {
  for (const [index, command] of target.build.entries()) {
    const tokens = [command.cmd, ...(command.args ?? [])].map(String);
    const bad = tokens.find((token) => !token.startsWith("-") && RELEASE_TASK.test(token));
    if (bad !== undefined) refuse("DEV_ANDROID_RELEASE_TASK", `development.targets.android.build[${index}] runs ${bad}; a dev build is a debug build (assembleDebug), never a release or upload task`);
    if (tokens.some((token) => /pass(?:word|phrase)?\s*=|storepass|keypass|signing\.(?:store|key)|RIGHTKIT_ANDROID_SIGNING/i.test(token))) {
      refuse("DEV_ANDROID_SIGNING_ARGUMENT", `development.targets.android.build[${index}] carries a signing property; a dev build uses the debug keystore only`);
    }
  }
  if (target.install !== undefined && target.install.kind !== "adb") refuse("DEV_ANDROID_INSTALL_KIND", "development.targets.android.install.kind must be adb");
  const ndk = target.ndk === undefined ? null : validateAndroidNdk(target.ndk, { root, kind: "dev" });
  if (target.install) {
    const { apk, package: pkg, activity } = target.install;
    if (typeof apk !== "string" || !/\.apk$/i.test(apk) || path.isAbsolute(apk) || path.win32.isAbsolute(apk) || !inside(root, path.resolve(root, apk))) refuse("DEV_ANDROID_APK_INVALID", "install.apk must be a relative .apk path inside the app");
    if (/(^|[\\/])release([\\/]|$)|[-_.]release\.apk$/i.test(apk)) refuse("DEV_ANDROID_APK_INVALID", "install.apk names a release output; a dev install is a debug APK");
    if (typeof pkg !== "string" || !PACKAGE_NAME.test(pkg)) refuse("DEV_ANDROID_PACKAGE_INVALID", "install.package must be the Android applicationId");
    if (!TEST_SEGMENT.test(pkg.split(".").at(-1))) refuse("DEV_ANDROID_PACKAGE_NOT_TEST", `install.package ${pkg} must end in .dev, .qa, .test or .debug so it never replaces the production app`);
    if (activity !== undefined && (typeof activity !== "string" || !ACTIVITY.test(activity))) refuse("DEV_ANDROID_ACTIVITY_INVALID", "install.activity must be a class name such as .MainActivity");
    if (target.install.serial !== undefined && !SERIAL.test(String(target.install.serial))) refuse("DEV_ANDROID_SERIAL_INVALID", "install.serial must be a plain device or emulator serial");
  }
  return { ndk };
}

export function androidDevelopmentNdkCommand(ndk) {
  return androidNdkCommand(ndk, { profile: "dev" });
}

export function resolveAdb(env = process.env, { exists = existsSync, platform = process.platform } = {}) {
  if (env.RIGHTKIT_ADB) return env.RIGHTKIT_ADB;
  for (const home of [env.ANDROID_HOME, env.ANDROID_SDK_ROOT]) {
    if (!home) continue;
    const candidate = path.join(home, "platform-tools", platform === "win32" ? "adb.exe" : "adb");
    if (exists(candidate)) return candidate;
  }
  return "adb";
}

// Pure: the adb argument lists, in order. No secret is ever part of them.
export function androidInstallPlan(descriptor, { root, serial }) {
  const apk = path.resolve(root, descriptor.apk);
  const base = ["-s", serial];
  return {
    apk,
    serial,
    steps: [
      { id: "state", args: [...base, "get-state"] },
      { id: "install", args: [...base, "install", "-r", "-d", apk] },
      { id: "verify", args: [...base, "shell", "pm", "path", descriptor.package] },
      ...(descriptor.activity ? [{ id: "launch", args: [...base, "shell", "am", "start", "-n", `${descriptor.package}/${descriptor.activity}`] }] : []),
    ],
  };
}

// `run` is spawnSync-shaped (tests inject a fake). Every step is bounded; the first failure stops and throws.
export function installAndroidDevelopment(descriptor, { root, env = process.env, run = spawnSync, serial, adb = resolveAdb(env), log = console.log, stat = statSync } = {}) {
  const chosen = androidDevelopmentSerial({ descriptor, serial, env });
  const plan = androidInstallPlan(descriptor, { root, serial: chosen });
  let size = 0;
  try { size = stat(plan.apk).size; } catch { /* reported below */ }
  if (!size) refuse("DEV_ANDROID_APK_MISSING", `the debug APK was not produced: ${descriptor.apk}`);
  const timeouts = { state: 30_000, install: 5 * 60_000, verify: 30_000, launch: 30_000 };
  for (const step of plan.steps) {
    const result = run(adb, step.args, { cwd: root, env, encoding: "utf8", windowsHide: true, shell: false, timeout: timeouts[step.id] });
    const out = `${result.stdout ?? ""}`.trim();
    if (result.error || result.status !== 0) refuse(`DEV_ANDROID_${step.id.toUpperCase()}_FAILED`, `adb ${step.id} failed (${result.status ?? "unavailable"}) for ${chosen}${result.error ? `: ${result.error.message}` : ""}`);
    if (step.id === "state" && out !== "device") refuse("DEV_ANDROID_DEVICE_NOT_READY", `${chosen} is not ready (adb state: ${out || "unknown"})`);
    if (step.id === "install" && !/Success/i.test(out)) refuse("DEV_ANDROID_INSTALL_FAILED", `adb install did not report Success for ${chosen}`);
    if (step.id === "verify" && !out.includes("package:")) refuse("DEV_ANDROID_VERIFY_FAILED", `${descriptor.package} is not installed on ${chosen} after the install`);
  }
  log(`right-release dev: installed ${descriptor.package} on ${chosen} (debug build; not a release artifact)`);
  return { serial: chosen, package: descriptor.package, apk: plan.apk };
}
