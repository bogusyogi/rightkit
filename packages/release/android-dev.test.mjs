import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import test from "node:test";
import { androidDevelopmentNdkCommand, androidDevelopmentSerial, androidInstallPlan, installAndroidDevelopment, resolveAdb, validateAndroidDevelopment } from "./android-dev.mjs";
import { developmentEnvironment, validateDevelopmentConfig } from "./development.mjs";

const APK = "android/app/build/outputs/apk/debug/app-debug.apk";
const JNI = "android/app/src/main/jniLibs";
const install = (over = {}) => ({ kind: "adb", apk: APK, package: "com.orthiclabs.heardright.dev", serial: "emulator-5554", ...over });
const build = [{ cmd: "./gradlew", args: [":app:assembleDebug"], cwd: "android" }];

function app(t) {
  const root = mkdtempSync(path.join(tmpdir(), "rk-android-dev-"));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  mkdirSync(path.join(root, path.dirname(APK)), { recursive: true });
  writeFileSync(path.join(root, APK), "apk bytes");
  return root;
}
const config = (target) => ({ development: { targets: { android: target } } });
const code = (pattern) => (error) => pattern.test(error.code ?? error.message);

test("the dev config accepts a debug build, the emulator and device ABIs, and an adb install of a .dev package", (t) => {
  const root = app(t);
  const target = validateDevelopmentConfig(config({ ndk: { jniLibs: JNI }, build, install: install() }), "android", { root });
  assert.deepEqual(target.ndk.abis, ["arm64-v8a", "x86_64"]);
  assert.equal(target.build.length, 1);
  const command = androidDevelopmentNdkCommand(target.ndk);
  assert.equal(command.cmd, "cargo");
  assert.ok(!command.args.includes("--release"));
  assert.equal(validateDevelopmentConfig(config({ build }), "android", { root }).ndk, null);
  for (const suffix of ["dev", "qa", "test", "debug"]) assert.doesNotThrow(() => validateAndroidDevelopment({ build, install: install({ package: `com.orthiclabs.heardright.${suffix}` }) }, { root }));
});

test("release tasks, signing arguments, production packages and release outputs are refused in dev", (t) => {
  const root = app(t);
  const bad = (target, pattern) => assert.throws(() => validateDevelopmentConfig(config(target), "android", { root }), code(pattern));
  bad({ build: [{ cmd: "./gradlew", args: [":app:bundleRelease"] }], install: install() }, /DEV_ANDROID_RELEASE_TASK/);
  bad({ build: [{ cmd: "./gradlew", args: [":app:assembleRelease"] }] }, /DEV_ANDROID_RELEASE_TASK/);
  bad({ build: [{ cmd: "./gradlew", args: [":app:assembleDebug", "-Pandroid.injected.signing.store.password=x"] }] }, /DEV_ANDROID_SIGNING_ARGUMENT/);
  bad({ build, install: { kind: "command", cmd: "adb" } }, /DEV_ANDROID_INSTALL_KIND/);
  bad({ build, install: install({ package: "com.orthiclabs.heardright" }) }, /DEV_ANDROID_PACKAGE_NOT_TEST/);
  bad({ build, install: install({ package: "heardright" }) }, /DEV_ANDROID_PACKAGE_INVALID/);
  bad({ build, install: install({ apk: "android/app/build/outputs/apk/release/app-release.apk" }) }, /DEV_ANDROID_APK_INVALID/);
  bad({ build, install: install({ apk: "../app-debug.apk" }) }, /DEV_ANDROID_APK_INVALID/);
  bad({ build, install: install({ apk: "android/app/build/app.aab" }) }, /DEV_ANDROID_APK_INVALID/);
  bad({ build, install: install({ activity: "Main Activity" }) }, /DEV_ANDROID_ACTIVITY_INVALID/);
  bad({ build, install: install({ serial: "bad serial" }) }, /DEV_ANDROID_SERIAL_INVALID/);
  bad({ build, ndk: { jniLibs: JNI, args: ["--release"] } }, /not free-form/);
  assert.throws(() => validateDevelopmentConfig({ development: { targets: { win: { build, install: install() } } } }, "win", { root }), /adb development installer is for --platform android/);
});

test("the device is chosen by serial only: config, --serial or the environment, never the only attached device", () => {
  assert.equal(androidDevelopmentSerial({ descriptor: { serial: "emulator-5554" }, env: {} }), "emulator-5554");
  assert.equal(androidDevelopmentSerial({ descriptor: { serial: "emulator-5554" }, serial: "R58M1234567", env: {} }), "R58M1234567");
  assert.equal(androidDevelopmentSerial({ descriptor: {}, env: { RIGHTKIT_ANDROID_SERIAL: "192.168.1.9:5555" } }), "192.168.1.9:5555");
  assert.throws(() => androidDevelopmentSerial({ descriptor: {}, env: {} }), code(/DEV_ANDROID_SERIAL_REQUIRED/));
  assert.throws(() => androidDevelopmentSerial({ descriptor: {}, serial: "; rm -rf", env: {} }), code(/DEV_ANDROID_SERIAL_INVALID/));
});

test("adb is found from the environment or the SDK, else the PATH", () => {
  assert.equal(resolveAdb({ RIGHTKIT_ADB: "/opt/adb" }), "/opt/adb");
  assert.equal(resolveAdb({ ANDROID_HOME: "/sdk" }, { exists: () => true, platform: "linux" }), path.join("/sdk", "platform-tools", "adb"));
  assert.equal(resolveAdb({ ANDROID_SDK_ROOT: "C:\\sdk" }, { exists: () => true, platform: "win32" }), path.join("C:\\sdk", "platform-tools", "adb.exe"));
  assert.equal(resolveAdb({}, { exists: () => false }), "adb");
});

test("the install plan is state, install, verify and an optional launch, every step pinned to the serial", () => {
  const plan = androidInstallPlan(install({ activity: ".MainActivity" }), { root: "/app", serial: "emulator-5554" });
  assert.deepEqual(plan.steps.map((step) => step.id), ["state", "install", "verify", "launch"]);
  for (const step of plan.steps) assert.deepEqual(step.args.slice(0, 2), ["-s", "emulator-5554"]);
  assert.deepEqual(plan.steps[1].args.slice(2, 5), ["install", "-r", "-d"]);
  assert.deepEqual(plan.steps[2].args.slice(2), ["shell", "pm", "path", "com.orthiclabs.heardright.dev"]);
  assert.deepEqual(plan.steps[3].args.slice(2), ["shell", "am", "start", "-n", "com.orthiclabs.heardright.dev/.MainActivity"]);
  assert.equal(androidInstallPlan(install(), { root: "/app", serial: "x" }).steps.length, 3);
});

function fakeAdb({ state = "device", installOut = "Performing Streamed Install\nSuccess", pm = "package:/data/app/base.apk", failAt } = {}) {
  const calls = [];
  const run = (command, args, options) => {
    calls.push({ command, args, options });
    const id = args.includes("get-state") ? "state" : args.includes("install") ? "install" : args.includes("path") ? "verify" : "launch";
    if (failAt === id) return { status: 1, stdout: "", stderr: "" };
    return { status: 0, stdout: id === "state" ? `${state}\n` : id === "install" ? installOut : id === "verify" ? pm : "Starting: Intent" };
  };
  return { calls, run };
}

test("installAndroidDevelopment runs each adb step without a shell and verifies the package is installed", (t) => {
  const root = app(t);
  const adb = fakeAdb();
  const logs = [];
  const result = installAndroidDevelopment(install({ activity: ".MainActivity" }), { root, env: {}, run: adb.run, adb: "adb", log: (line) => logs.push(line) });
  assert.equal(result.serial, "emulator-5554");
  assert.equal(adb.calls.length, 4);
  for (const call of adb.calls) {
    assert.equal(call.command, "adb");
    assert.equal(call.options.shell, false);
    assert.ok(call.options.timeout > 0);
  }
  assert.equal(adb.calls[1].args.at(-1), path.join(root, ...APK.split("/")));
  assert.match(logs[0], /installed com\.orthiclabs\.heardright\.dev on emulator-5554 \(debug build; not a release artifact\)/);
});

test("a missing APK, a device that is not ready, a failed or silent install and a missing package each stop the install", (t) => {
  const root = app(t);
  const run = (adb, descriptor = install()) => () => installAndroidDevelopment(descriptor, { root, env: {}, run: adb.run, adb: "adb", log: () => {} });
  assert.throws(run(fakeAdb(), install({ apk: "android/app/build/outputs/apk/debug/missing.apk" })), code(/DEV_ANDROID_APK_MISSING/));
  assert.throws(run(fakeAdb({ state: "offline" })), code(/DEV_ANDROID_DEVICE_NOT_READY/));
  assert.throws(run(fakeAdb({ failAt: "state" })), code(/DEV_ANDROID_STATE_FAILED/));
  assert.throws(run(fakeAdb({ failAt: "install" })), code(/DEV_ANDROID_INSTALL_FAILED/));
  assert.throws(run(fakeAdb({ installOut: "Failure [INSTALL_FAILED_UPDATE_INCOMPATIBLE]" })), code(/DEV_ANDROID_INSTALL_FAILED/));
  assert.throws(run(fakeAdb({ pm: "" })), code(/DEV_ANDROID_VERIFY_FAILED/));
  const stopped = fakeAdb({ state: "offline" });
  assert.throws(run(stopped));
  assert.equal(stopped.calls.length, 1);
  assert.throws(() => installAndroidDevelopment(install({ serial: undefined }), { root, env: {}, run: fakeAdb().run, adb: "adb" }), code(/DEV_ANDROID_SERIAL_REQUIRED/));
});

test("the dev environment never carries a release signing-properties path", () => {
  const env = developmentEnvironment({ RIGHTKIT_ANDROID_SIGNING_PROPERTIES: "/tmp/signing.properties", KEEP: "yes" });
  assert.equal(env.RIGHTKIT_ANDROID_SIGNING_PROPERTIES, undefined);
  assert.equal(env.KEEP, "yes");
  assert.equal(env.RIGHTKIT_BUILD_MODE, "dev");
});
