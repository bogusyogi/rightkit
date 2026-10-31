import "./test-support/broker-guard.mjs";
import assert from "node:assert/strict";
import { mkdirSync, writeFileSync } from "node:fs";
import path from "node:path";
import test from "node:test";
import { tempRoot } from "./test-support/temp-roots.mjs";
import {
  IosDevelopmentError, assertIosDevelopmentSession, deviceReadiness, installIosDevelopment, iosDevelopmentApp, iosDevelopmentBuildCommands, iosDevelopmentDevice,
  iosUiTestCommands, listSimulators, prepareIosDevelopment, prepareIosUiTests, productsDirectory, runIosDevelopmentCommands, selectDevice, selectSimulator, validateIosDevelopment,
} from "./ios-dev.mjs";
import { createNativeRunner } from "./build-launch.mjs";
import { managedRuntimeClient } from "./managed-roots.mjs";
import { NATIVE_ADAPTERS, nativeManagedOutputs, nativeStateDir, nativeToolOf } from "./native-adapters.mjs";

const SIM_UDID = "A1B2C3D4-0000-4000-8000-1234567890AB";
const DEVICE_ID = "00008110-001A2B3C4D5E801E";
const root = path.resolve("fixture-app");
const base = (over = {}) => ({ project: "ios/Pulse.xcodeproj", projectSpec: "ios/project.yml", scheme: "Pulse", ...over });
const simulator = (over = {}) => ({ kind: "simulator", device: SIM_UDID, bundleId: "dev.orthic.pulse.ios", ...over });
const device = (over = {}) => ({ kind: "device", device: DEVICE_ID, bundleId: "dev.orthic.pulse.ios", ...over });
const coded = (code, fn) => assert.throws(fn, (error) => error instanceof IosDevelopmentError && error.code === code, code);
const slashed = (value) => String(value).replaceAll("\\", "/");

test("a simulator and a device config validate, with the build generated from them", () => {
  const sim = validateIosDevelopment(base({ install: simulator() }), { root });
  assert.equal(sim.configuration, "Debug");
  assert.equal(sim.install.productName, "Pulse");
  assert.deepEqual(sim.build.map((command) => command.cmd), ["xcodegen", "xcodebuild"]);
  const dev = validateIosDevelopment(base({ teamId: "6KLGD3LLKF", install: device() }), { root });
  assert.equal(dev.install.kind, "device");
  assert.equal(validateIosDevelopment(base(), { root }).install, null);
});

test("config is strict: unknown keys, a hand-written build, release configurations and release steps are refused", () => {
  coded("DEV_IOS_CONFIG_INVALID", () => validateIosDevelopment(undefined, { root }));
  coded("DEV_IOS_CONFIG_INVALID", () => validateIosDevelopment(base({ build: [{ cmd: "xcodebuild" }] }), { root }));
  coded("DEV_IOS_CONFIG_INVALID", () => validateIosDevelopment(base({ colour: 1 }), { root }));
  coded("DEV_IOS_CONFIG_INVALID", () => validateIosDevelopment(base({ install: { ...simulator(), colour: 1 } }), { root }));
  coded("DEV_IOS_RELEASE_CONFIGURATION", () => validateIosDevelopment(base({ configuration: "Release" }), { root }));
  coded("DEV_IOS_RELEASE_TASK", () => validateIosDevelopment(base({ prepare: [{ cmd: "xcodebuild", args: ["archive"] }] }), { root }));
  coded("DEV_IOS_RELEASE_TASK", () => validateIosDevelopment(base({ prepare: [{ cmd: "xcrun", args: ["altool", "--upload-package"] }] }), { root }));
  coded("DEV_IOS_INSTALL_KIND", () => validateIosDevelopment(base({ install: { ...simulator(), kind: "adb" } }), { root }));
  coded("DEV_IOS_CONFIG_INVALID", () => validateIosDevelopment(base({ project: "/abs/Pulse.xcodeproj" }), { root }));
  coded("DEV_IOS_CONFIG_INVALID", () => validateIosDevelopment(base({ extraBuildSettings: { AUTH_TOKEN: "x" } }), { root }));
  coded("DEV_IOS_CONFIG_INVALID", () => validateIosDevelopment(base({ extraBuildSettings: { CODE_SIGN_IDENTITY: "x" } }), { root }));
  // the same secret-value check as the release lane
  coded("DEV_IOS_CONFIG_INVALID", () => validateIosDevelopment(base({ extraBuildSettings: { SOME_URL: "Bearer abcdefghijkl" } }), { root }));
  coded("DEV_IOS_CONFIG_INVALID", () => validateIosDevelopment(base({ extraBuildSettings: { SOME_VALUE: "-----BEGIN " + "PRIVATE KEY-----" } }), { root }));
  coded("DEV_IOS_CONFIG_INVALID", () => validateIosDevelopment(base({ install: { ...simulator(), launchArgs: ["ok\nbad"] } }), { root }));
});

test("a device build needs a configured team, and boot is for simulators only", () => {
  coded("DEV_IOS_CONFIG_INVALID", () => validateIosDevelopment(base({ install: device() }), { root }));
  coded("DEV_IOS_CONFIG_INVALID", () => validateIosDevelopment(base({ teamId: "6KLGD3LLKF", install: device({ boot: true }) }), { root }));
});

test("the device is chosen by id and the only attached device is never assumed", () => {
  coded("DEV_IOS_DEVICE_REQUIRED", () => iosDevelopmentDevice({ descriptor: { kind: "device" }, env: {} }));
  assert.equal(iosDevelopmentDevice({ descriptor: device(), env: {} }), DEVICE_ID);
  assert.equal(iosDevelopmentDevice({ descriptor: { kind: "device" }, serial: DEVICE_ID, env: {} }), DEVICE_ID);
  assert.equal(iosDevelopmentDevice({ descriptor: { kind: "device" }, env: { RIGHTKIT_IOS_DEVICE: DEVICE_ID } }), DEVICE_ID);
  assert.equal(iosDevelopmentDevice({ descriptor: { kind: "simulator" }, env: { RIGHTKIT_IOS_DEVICE: "iPhone 17 Pro" } }), "iPhone 17 Pro");
  coded("DEV_IOS_DEVICE_INVALID", () => iosDevelopmentDevice({ descriptor: { kind: "device" }, serial: "bad id; rm", env: {} }));
});

test("build commands: simulator builds sign nothing special, device builds register the device with the configured team, and none names where a build writes", () => {
  const sim = iosDevelopmentBuildCommands(validateIosDevelopment(base({ install: simulator() }), { root }), { root, destinationId: SIM_UDID });
  const simBuild = sim.at(-1).args;
  assert.equal(simBuild[simBuild.indexOf("-destination") + 1], `platform=iOS Simulator,id=${SIM_UDID}`);
  assert.ok(!simBuild.includes("-allowProvisioningDeviceRegistration"));
  assert.equal(simBuild.at(-1), "build");
  assert.deepEqual(sim[0].args.slice(0, 1), ["generate"]);
  const dev = iosDevelopmentBuildCommands(validateIosDevelopment(base({ teamId: "6KLGD3LLKF", install: device() }), { root }), { root, destinationId: DEVICE_ID });
  const devBuild = dev.at(-1).args;
  assert.equal(devBuild[devBuild.indexOf("-destination") + 1], `id=${DEVICE_ID}`);
  for (const flag of ["-allowProvisioningUpdates", "-allowProvisioningDeviceRegistration", "DEVELOPMENT_TEAM=6KLGD3LLKF"]) assert.ok(devBuild.includes(flag), flag);
  assert.ok(!devBuild.some((arg) => /authenticationKey|\.p8/.test(arg)), "no ASC credential in a dev build");
  assert.ok(!devBuild.includes("archive"));
  for (const args of [simBuild, devBuild]) for (const owned of ["-derivedDataPath", "-resultBundlePath", "-archivePath", "-exportPath"]) assert.ok(!args.includes(owned), `${owned} is the broker's`);
  assert.equal(productsDirectory({ configuration: "Debug", install: { kind: "device" } }), "Debug-iphoneos");
  assert.equal(productsDirectory({ configuration: "Debug", install: { kind: "simulator" } }), "Debug-iphonesimulator");
});

test("xcodegen is part of the build when a project.yml is named, and absent otherwise", () => {
  assert.equal(iosDevelopmentBuildCommands(validateIosDevelopment(base({ projectSpec: undefined }), { root }), { root, destinationId: "x" })[0].cmd, "xcodebuild");
});

// ---- simulators and devices ---------------------------------------------------------------------------------------
const simctlJson = (devices) => JSON.stringify({ devices: { "com.apple.CoreSimulator.SimRuntime.iOS-26-0": devices } });
const SIMS = [
  { udid: SIM_UDID, name: "iPhone 17 Pro", state: "Shutdown", isAvailable: true },
  { udid: "F0F0F0F0-0000-4000-8000-000000000001", name: "iPhone 17", state: "Booted", isAvailable: true },
  { udid: "F0F0F0F0-0000-4000-8000-000000000002", name: "iPhone 17", state: "Shutdown", isAvailable: true },
  { udid: "F0F0F0F0-0000-4000-8000-000000000003", name: "iPhone 14", state: "Shutdown", isAvailable: false },
];
test("simulators are selected by UDID or a unique exact name; unavailable ones are not listed", () => {
  const run = () => ({ status: 0, stdout: simctlJson(SIMS) });
  const list = listSimulators(run);
  assert.equal(list.length, 3);
  assert.equal(selectSimulator(list, SIM_UDID.toLowerCase()).name, "iPhone 17 Pro");
  assert.equal(selectSimulator(list, "iPhone 17 Pro").udid, SIM_UDID);
  coded("DEV_IOS_DEVICE_AMBIGUOUS", () => selectSimulator(list, "iPhone 17"));
  coded("DEV_IOS_DEVICE_NOT_FOUND", () => selectSimulator(list, "iPhone 14"));
  coded("DEV_IOS_SIMCTL_FAILED", () => listSimulators(() => ({ status: 1, stdout: "" })));
});

const DEVICES = [
  { identifier: DEVICE_ID, deviceProperties: { name: "Adrian's iPhone", ddiServicesAvailable: true, developerModeStatus: "enabled" }, hardwareProperties: { productType: "iPhone16,2", deviceType: "iPhone" }, connectionProperties: { tunnelState: "connected" } },
  { identifier: "00008030-OTHER", deviceProperties: { name: "Old iPhone", ddiServicesAvailable: false }, hardwareProperties: { deviceType: "iPhone" }, connectionProperties: { tunnelState: "disconnected" } },
  { identifier: "MAC-1", deviceProperties: { name: "My Mac" }, hardwareProperties: { deviceType: "Mac" }, connectionProperties: { tunnelState: "connected" } },
];
test("devicectl devices: a named ready device is accepted; an unready one, an unknown one and the Mac are refused", () => {
  assert.equal(selectDevice(DEVICES, DEVICE_ID).identifier, DEVICE_ID);
  assert.equal(selectDevice(DEVICES, "Adrian's iPhone").identifier, DEVICE_ID);
  coded("DEV_IOS_DEVICE_NOT_READY", () => selectDevice(DEVICES, "00008030-OTHER"));
  coded("DEV_IOS_DEVICE_NOT_FOUND", () => selectDevice(DEVICES, "MAC-1"));
  coded("DEV_IOS_DEVICE_NOT_FOUND", () => selectDevice(DEVICES, "unknown"));
  assert.equal(deviceReadiness(DEVICES[1]).ready, false);
});

function fakeXcrun({ failStep, sims = SIMS, devices = DEVICES, launchResult = { result: { process: { processIdentifier: 42 } } }, rustright } = {}) {
  const calls = [];
  const run = (cmd, args, options = {}) => {
    calls.push({ cmd, args: [...args], options });
    if (cmd === "rustright") return rustright ?? { status: 1, stdout: "" };
    if (args[0] === "simctl" && args[1] === "list") return { status: 0, stdout: simctlJson(sims) };
    if (args[0] === "devicectl" && args[1] === "list") { writeFileSync(args[args.indexOf("--json-output") + 1], JSON.stringify({ result: { devices } })); return { status: 0, stdout: "" }; }
    if (args[0] === "devicectl" && args.includes("launch")) { writeFileSync(args[args.indexOf("--json-output") + 1], JSON.stringify(launchResult)); }
    const step = args.slice(0, 3).join(" ");
    if (failStep && step.includes(failStep)) return { status: 1, stdout: "", stderr: "failed" };
    return { status: 0, stdout: "" };
  };
  return { calls, run };
}

test("prepare resolves the simulator or device before any build and resolves no storage: the broker owns the output root", () => {
  const tmp = tempRoot("rk-ios-dev-");
  const xcrun = fakeXcrun({ rustright: { status: 0, stdout: JSON.stringify(["-derivedDataPath", "/runs/1/DerivedData", "-resultBundlePath", "/runs/1/r.xcresult"]) } });
  const sim = prepareIosDevelopment(validateIosDevelopment(base({ install: simulator({ device: "iPhone 17 Pro" }) }), { root }), { root, env: { RIGHTKIT_RUN_ROOT: "/runs/1" }, run: xcrun.run, tmpRoot: tmp });
  assert.equal(sim.install.resolved.id, SIM_UDID);
  assert.equal(sim.brokerOwned, true);
  assert.ok(!xcrun.calls.some((call) => call.cmd === "rustright"), "no rustright storage query: the host does not choose DerivedData");
  assert.equal(sim.storage, undefined);
  const buildArgs = sim.build.at(-1).args;
  assert.ok(!buildArgs.includes("-derivedDataPath") && !buildArgs.includes("-resultBundlePath"));
  assert.equal(sim.install.app, null, "the built .app is known only from the receipt's managed output");
  // The .app is read back from the managed derived-data role of the xcodebuild call's output directory (relative cwd "" is the snapshot root).
  const outputDir = path.join(tmp, "job-out");
  assert.equal(slashed(sim.appFromOutputDir(outputDir)), slashed(path.join(nativeStateDir(outputDir, "xcodebuild", ""), "derived-data", "Build", "Products", "Debug-iphonesimulator", "Pulse.app")));
  assert.equal(slashed(sim.appFromOutputDir(outputDir, { relativeCwd: "ios" })), slashed(path.join(nativeStateDir(outputDir, "xcodebuild", "ios"), "derived-data", "Build", "Products", "Debug-iphonesimulator", "Pulse.app")));
  const fallback = fakeXcrun();
  const dev = prepareIosDevelopment(validateIosDevelopment(base({ teamId: "6KLGD3LLKF", install: device() }), { root }), { root, env: {}, run: fallback.run, tmpRoot: tmp });
  assert.equal(dev.install.resolved.id, DEVICE_ID);
  assert.equal(slashed(dev.appFromOutputDir(outputDir)).endsWith("Debug-iphoneos/Pulse.app"), true);
  assert.equal(prepareIosDevelopment(validateIosDevelopment(base(), { root }), { root, env: {}, run: fallback.run, tmpRoot: tmp }).appFromOutputDir, undefined, "no install, no app to find");
});

test("prepare refuses a missing device id and an unready device before any build", () => {
  const tmp = tempRoot("rk-ios-dev-");
  const target = validateIosDevelopment(base({ teamId: "6KLGD3LLKF", install: { kind: "device", bundleId: "dev.orthic.pulse.ios" } }), { root });
  coded("DEV_IOS_DEVICE_REQUIRED", () => prepareIosDevelopment(target, { root, env: {}, run: fakeXcrun().run, tmpRoot: tmp }));
  coded("DEV_IOS_DEVICE_NOT_READY", () => prepareIosDevelopment(target, { root, env: {}, serial: "00008030-OTHER", run: fakeXcrun().run, tmpRoot: tmp }));
  coded("DEV_IOS_DEVICE_NOT_FOUND", () => prepareIosDevelopment(target, { root, env: {}, serial: "someone-elses-phone", run: fakeXcrun().run, tmpRoot: tmp }));
});

// ---- install ------------------------------------------------------------------------------------------------------
function prepared(kind, over = {}) {
  const tmp = tempRoot("rk-ios-dev-");
  const xcrun = fakeXcrun(over);
  const dev = validateIosDevelopment(base({ teamId: "6KLGD3LLKF", install: kind === "device" ? device(over.install) : simulator(over.install) }), { root });
  const result = prepareIosDevelopment(dev, { root, env: {}, run: xcrun.run, tmpRoot: tmp });
  result.install.app = result.appFromOutputDir(path.join(tmp, "job-out"));
  mkdirSync(result.install.app, { recursive: true });
  xcrun.calls.length = 0;
  return { install: result.install, xcrun, tmp };
}

test("simulator install: boot a shut-down simulator, install, launch and terminate a running copy", () => {
  const p = prepared("simulator");
  const logs = [];
  const out = installIosDevelopment(p.install, { root, run: p.xcrun.run, tmpRoot: p.tmp, log: (line) => logs.push(line) });
  assert.deepEqual(p.xcrun.calls.map((call) => call.args.slice(0, 3).join(" ")), [`simctl boot ${SIM_UDID}`, `simctl install ${SIM_UDID}`, "simctl launch --terminate-running-process"]);
  assert.equal(p.xcrun.calls[2].args.at(-1), "dev.orthic.pulse.ios");
  const withArgs = prepared("simulator", { install: { launchArgs: ["--device", "x"] } });
  installIosDevelopment(withArgs.install, { root, run: withArgs.xcrun.run, tmpRoot: withArgs.tmp, log: () => {} });
  assert.deepEqual(withArgs.xcrun.calls.at(-1).args.slice(-4), ["dev.orthic.pulse.ios", "--", "--device", "x"], "an app argument can never be read as a simctl option");
  assert.equal(out.kind, "simulator");
  assert.match(logs[0], /debug build; not a release artifact/);
});

test("an already booted simulator is not booted again; boot:false and launch:false are honoured", () => {
  const booted = prepared("simulator", { install: { device: "F0F0F0F0-0000-4000-8000-000000000001" } });
  installIosDevelopment(booted.install, { root, run: booted.xcrun.run, tmpRoot: booted.tmp, log: () => {} });
  assert.ok(!booted.xcrun.calls.some((call) => call.args[1] === "boot"));
  const quiet = prepared("simulator", { install: { boot: false, launch: false } });
  installIosDevelopment(quiet.install, { root, run: quiet.xcrun.run, tmpRoot: quiet.tmp, log: () => {} });
  assert.deepEqual(quiet.xcrun.calls.map((call) => call.args[1]), ["install"]);
});

test("device install: devicectl install, then launch with --terminate-existing and a process result", () => {
  const p = prepared("device", { install: { launchArgs: ["--onboarding-step", "2"] } });
  const out = installIosDevelopment(p.install, { root, run: p.xcrun.run, tmpRoot: p.tmp, log: () => {} });
  assert.deepEqual(p.xcrun.calls[0].args.slice(0, 5), ["devicectl", "device", "install", "app", "--device"]);
  assert.equal(p.xcrun.calls[0].args[5], DEVICE_ID);
  const launch = p.xcrun.calls[1].args;
  assert.ok(launch.includes("--terminate-existing") && launch.includes("--no-activate"));
  assert.deepEqual(launch.slice(-4), ["dev.orthic.pulse.ios", "--", "--onboarding-step", "2"], "app arguments follow a -- terminator");
  assert.equal(out.kind, "device");
});

test("install failures stop at the failing step; a launch without a process result and a missing app are errors", () => {
  const failing = prepared("simulator", { failStep: "simctl install" });
  coded("DEV_IOS_INSTALL_FAILED", () => installIosDevelopment(failing.install, { root, run: failing.xcrun.run, tmpRoot: failing.tmp, log: () => {} }));
  assert.ok(!failing.xcrun.calls.some((call) => call.args[1] === "launch"));
  const noProcess = prepared("device", { launchResult: { result: {} } });
  coded("DEV_IOS_LAUNCH_FAILED", () => installIosDevelopment(noProcess.install, { root, run: noProcess.xcrun.run, tmpRoot: noProcess.tmp, log: () => {} }));
  const missing = prepared("simulator");
  coded("DEV_IOS_APP_MISSING", () => installIosDevelopment({ ...missing.install, app: path.join(missing.tmp, "nope.app") }, { root, run: missing.xcrun.run, tmpRoot: missing.tmp, log: () => {} }));
});

test("a device build needs the GUI signing session; a simulator build does not", () => {
  const dev = validateIosDevelopment(base({ teamId: "6KLGD3LLKF", install: device() }), { root });
  const sim = validateIosDevelopment(base({ install: simulator() }), { root });
  coded("IOS_GUI_SESSION_REQUIRED", () => assertIosDevelopmentSession(dev, { env: {}, run: () => assert.fail("no command"), detect: () => ({ guiRunner: true, reason: "SSH security session" }) }));
  assert.equal(assertIosDevelopmentSession(sim, { env: {}, detect: () => assert.fail("a simulator build never checks the keychain session") }).skipped, true);
  assert.equal(assertIosDevelopmentSession(dev, { env: {}, detect: () => ({ guiRunner: false, reason: "Aqua" }) }).routed, false);
});

test("a paired device whose idle tunnel dropped is woken and read again; only a device still not ready, or unavailable, is refused (rightkit#1228)", async () => {
  const { selectReadyDevice } = await import("./ios-dev.mjs");
  const phone = (tunnel, ddi) => ({ identifier: "DEV-1", deviceProperties: { name: "Adrian's iPhone", ddiServicesAvailable: ddi, developerModeStatus: "enabled" }, hardwareProperties: { deviceType: "iPhone", productType: "iPhone17,2", udid: "UDID-1" }, connectionProperties: { tunnelState: tunnel } });
  // idle: disconnected on the first read, connected after the wake
  const reads = [[phone("disconnected", false)], [phone("connected", true)]]; const woken = [];
  const got = selectReadyDevice({ list: () => reads.shift(), wake: (id) => woken.push(id), wanted: "UDID-1" });
  assert.equal(got.identifier, "DEV-1");
  assert.deepEqual(woken, ["DEV-1"]);
  // still disconnected after the wake: refused with the device detail
  const still = [[phone("disconnected", false)], [phone("disconnected", false)]];
  assert.throws(() => selectReadyDevice({ list: () => still.shift(), wake: () => {}, wanted: "UDID-1" }), (e) => e.code === "DEV_IOS_DEVICE_NOT_READY" && /tunnel=disconnected/.test(e.message));
  // unavailable (not reachable): refused at once, no wake
  const none = []; 
  assert.throws(() => selectReadyDevice({ list: () => [phone("unavailable", false)], wake: (id) => none.push(id), wanted: "UDID-1" }), (e) => e.code === "DEV_IOS_DEVICE_NOT_READY");
  assert.deepEqual(none, []);
  // already connected: no wake
  const quiet = [];
  assert.equal(selectReadyDevice({ list: () => [phone("connected", true)], wake: (id) => quiet.push(id), wanted: "UDID-1" }).identifier, "DEV-1");
  assert.deepEqual(quiet, []);
});

test("UI test journeys: uiTests validates strictly and generates build-for-testing then test-without-building of exactly the listed journeys on the dev destination, naming no output location", () => {
  const journeys = ["PulseUITests/PulseScreensJourney", "PulseUITests/PulseLiveJourney"];
  const dev = validateIosDevelopment(base({ teamId: "6KLGD3LLKF", install: device(), uiTests: { onlyTesting: journeys } }), { root });
  assert.deepEqual(dev.uiTests, { scheme: "Pulse", onlyTesting: journeys });
  const [build, run] = iosUiTestCommands(dev, { root, destinationId: DEVICE_ID });
  assert.equal(build.args.at(-1), "build-for-testing");
  assert.equal(run.args.at(-1), "test-without-building");
  for (const c of [build, run]) {
    assert.equal(c.cmd, "xcodebuild");
    assert.equal(c.args[c.args.indexOf("-destination") + 1], `id=${DEVICE_ID}`);
    assert.ok(c.args.includes("DEVELOPMENT_TEAM=6KLGD3LLKF"));
    for (const owned of ["-derivedDataPath", "-resultBundlePath", "-archivePath", "-exportPath"]) assert.ok(!c.args.includes(owned), `${owned} is the broker's`);
  }
  assert.deepEqual(run.args.filter((a) => a.startsWith("-only-testing:")), journeys.map((j) => `-only-testing:${j}`));
  const sim = validateIosDevelopment(base({ install: simulator(), uiTests: { onlyTesting: ["PulseUITests"], testPlan: "Journeys" } }), { root });
  const [simBuild] = iosUiTestCommands(sim, { root, destinationId: SIM_UDID });
  assert.equal(simBuild.args[simBuild.args.indexOf("-destination") + 1], `platform=iOS Simulator,id=${SIM_UDID}`);
  assert.equal(simBuild.args[simBuild.args.indexOf("-testPlan") + 1], "Journeys");
  // Strict: no install, an empty or malformed list, or an unknown key is refused.
  coded("DEV_IOS_CONFIG_INVALID", () => validateIosDevelopment(base({ uiTests: { onlyTesting: journeys } }), { root }));
  coded("DEV_IOS_CONFIG_INVALID", () => validateIosDevelopment(base({ install: simulator(), uiTests: { onlyTesting: [] } }), { root }));
  coded("DEV_IOS_CONFIG_INVALID", () => validateIosDevelopment(base({ install: simulator(), uiTests: { onlyTesting: ["a b"] } }), { root }));
  coded("DEV_IOS_CONFIG_INVALID", () => validateIosDevelopment(base({ install: simulator(), uiTests: { onlyTesting: journeys, retries: 3 } }), { root }));
  assert.throws(() => iosUiTestCommands(validateIosDevelopment(base({ install: simulator() }), { root }), { root, destinationId: SIM_UDID }), /uiTests is not declared/);
  // prepare for tests resolves the device and hands the commands; the result bundle is the broker's run-unique role, so there is no path to pick.
  const tmp = tempRoot("rk-ios-dev-");
  const prepared = prepareIosUiTests(dev, { root, env: {}, run: fakeXcrun().run, tmpRoot: tmp });
  assert.equal(prepared.resultBundle, null);
  assert.equal(prepared.tests.length, 2);
  assert.equal(prepared.tests[0].args[prepared.tests[0].args.indexOf("-destination") + 1], `id=${DEVICE_ID}`);
});

// ---- running the generated commands through the production native runner -----------------------------------------------------
// `realRunner` is the REAL production runner on a Mac host description: planning (the door, the adapter grammar, the capability check) and every refusal are the real code and launch nothing. Only the broker's
// EXECUTION is stood in for in `standIn`, with the output roles computed by the real grammar (what a carrier reads back from a signed receipt). It is a unit seam, never Mac evidence.
const MAC_ENV = { PATH: "/usr/bin", HOME: "/Users/rk" };
const MAC_CLIENT = managedRuntimeClient(MAC_ENV, "darwin", () => true);
const realRunner = (cwd, brokerCapabilities) => createNativeRunner({
  cwd, env: MAC_ENV, platform: "darwin", exists: (p) => p === MAC_CLIENT, which: (c) => (c === "rustright" ? ["/elsewhere/on/path/rustright"] : []), readFile: () => "",
  ...(brokerCapabilities ? { brokerCapabilities } : {}),
});
function standIn({ cwd, brokerCapabilities, execute = () => ({ status: 0 }), outputDir = path.join(cwd, "broker-output") }) {
  const real = realRunner(cwd, brokerCapabilities);
  const launched = [];
  let jobs = 0;
  return {
    launched, plan: (command, args, options = {}) => real.plan(command, args, { ...options, env: MAC_ENV }),
    run(command, args = [], opts = {}) {
      // The stand-in is its own host description (MAC_ENV): the caller's `env` is the step's launch environment, which the runner never forwards.
      const options = { ...opts, env: MAC_ENV };
      let launch;
      try { launch = real.plan(command, args, options); } catch { return real.run(command, args, options); }
      jobs += 1;
      const jobId = `job-${jobs}`;
      const { tool, args: sent } = launch.native;
      const state = nativeStateDir(outputDir, tool, "");
      const mapped = nativeManagedOutputs(tool, sent, { state, jobId });
      const byRole = Object.fromEntries((mapped.outputs ?? []).map((entry) => [entry.role, entry.path]));
      launched.push({ command, args: [...sent] });
      const raw = execute({ command, args: [...sent], byRole });
      const ok = raw.status === 0;
      return { ok, status: raw.status, signal: null, stdout: raw.stdout ?? "", stderr: raw.stderr ?? "", error: null, refusal: null, tool, adapter: NATIVE_ADAPTERS[tool].adapter, capability: NATIVE_ADAPTERS[tool].capability, argv: [...sent], carried: true, relativeCwd: "", requestId: jobId, outputDir, outputs: ok ? { ok: true, source: "receipt", state, outputs: mapped.outputs, byRole } : null, nestedChildren: null };
    },
  };
}

test("the dev build and the UI test journeys are admitted by the real native door and grammar exactly as generated, and map the managed derived-data and result-bundle roles", () => {
  const dir = tempRoot("rk-ios-dev-run-");
  const journeys = ["PulseUITests/PulseScreensJourney"];
  const dev = validateIosDevelopment(base({ teamId: "6KLGD3LLKF", install: device(), uiTests: { onlyTesting: journeys, testPlan: "Journeys" }, extraBuildSettings: { HR_FLAG: "1" } }), { root: dir });
  const runner = realRunner(dir);
  for (const command of [...iosDevelopmentBuildCommands(dev, { root: dir, destinationId: DEVICE_ID }), ...iosUiTestCommands(dev, { root: dir, destinationId: DEVICE_ID })]) {
    const launch = runner.plan(command.cmd, command.args, { cwd: dir });
    assert.equal(launch.command, MAC_CLIENT, command.name);
    assert.equal(launch.native.tool, command.cmd);
    for (const word of launch.native.args) assert.ok(!path.isAbsolute(word), `${word} is made relative to the step's directory`);
  }
  const state = nativeStateDir(path.join(dir, "out"), "xcodebuild", "");
  const [build, test] = iosUiTestCommands(dev, { root: dir, destinationId: DEVICE_ID }).map((command) => runner.plan(command.cmd, command.args, { cwd: dir }).native.args);
  assert.deepEqual(nativeManagedOutputs("xcodebuild", build, { state }).outputs.map((entry) => entry.role), ["derived-data", "result-bundle"], "build-for-testing is a test-mode call: the same managed DerivedData the test call reuses, and its own run-unique bundle");
  assert.deepEqual(nativeManagedOutputs("xcodebuild", test, { state, jobId: "job-9" }).outputs.map((entry) => entry.role), ["derived-data", "result-bundle"]);
  // the owned spellings the old generator emitted are refused by the real door
  assert.throws(() => runner.plan("xcodebuild", [...iosUiTestCommands(dev, { root: dir, destinationId: DEVICE_ID })[1].args, "-resultBundlePath", "/x/r.xcresult"], { cwd: dir }), (error) => error.code === "RK_BUILD_LAUNCH_REFUSED" && error.reason === "RK_NATIVE_TOOL_REFUSED");
});

test("runIosDevelopmentCommands runs xcodegen and xcodebuild only as native adapter jobs, in order, and returns the managed derived-data and result-bundle read back from the outputs", () => {
  const dir = tempRoot("rk-ios-dev-run-");
  const dev = validateIosDevelopment(base({ install: simulator(), uiTests: { onlyTesting: ["PulseUITests/PulseScreensJourney"] } }), { root: dir });
  const broker = standIn({ cwd: dir });
  const direct = [];
  const build = runIosDevelopmentCommands(iosDevelopmentBuildCommands(dev, { root: dir, destinationId: SIM_UDID }), { native: broker, root: dir, env: { PATH: "/usr/bin" }, log: () => {}, direct: (command) => { direct.push(command.cmd); return { status: 0 }; } });
  assert.deepEqual(broker.launched.map((call) => call.command), ["xcodegen", "xcodebuild"]);
  assert.deepEqual(direct, [], "no prepare step in this config: nothing ran directly");
  assert.equal(build.steps.length, 2);
  const app = iosDevelopmentApp(dev, build.outputs);
  assert.equal(slashed(app), slashed(path.join(nativeStateDir(path.join(dir, "broker-output"), "xcodebuild", ""), "derived-data", "Build", "Products", "Debug-iphonesimulator", "Pulse.app")));
  const tests = runIosDevelopmentCommands(iosUiTestCommands(dev, { root: dir, destinationId: SIM_UDID }), { native: broker, root: dir, log: () => {} });
  assert.deepEqual(broker.launched.slice(2).map((call) => call.args.at(-1)), ["build-for-testing", "test-without-building"]);
  assert.equal(slashed(tests.resultBundle), slashed(path.join(nativeStateDir(path.join(dir, "broker-output"), "xcodebuild", ""), "results", "job-4.xcresult")));
  // a non-native prepare command is the caller's direct step, never a native-tool spawn
  const withPrepare = [{ cmd: "node", args: ["scripts/prep.mjs"], name: "node" }, ...iosDevelopmentBuildCommands(dev, { root: dir, destinationId: SIM_UDID }).slice(-1)];
  coded("DEV_IOS_DIRECT_STEP_UNHANDLED", () => runIosDevelopmentCommands(withPrepare, { native: standIn({ cwd: dir }), root: dir, log: () => {} }));
  const seen = [];
  runIosDevelopmentCommands(withPrepare, { native: standIn({ cwd: dir }), root: dir, log: () => {}, direct: (command) => { seen.push(command.cmd); return { status: 0 }; } });
  assert.deepEqual(seen, ["node"]);
});

test("a missing runner, an unadvertised capability and a refused argv stop the whole sequence before the FIRST launch (no spawn fallback)", () => {
  const dir = tempRoot("rk-ios-dev-run-");
  const dev = validateIosDevelopment(base({ install: simulator() }), { root: dir });
  const commands = iosDevelopmentBuildCommands(dev, { root: dir, destinationId: SIM_UDID });
  coded("DEV_IOS_NATIVE_REQUIRED", () => runIosDevelopmentCommands(commands, { root: dir, log: () => {} }));
  coded("DEV_IOS_NATIVE_REQUIRED", () => runIosDevelopmentCommands(commands, { native: { run: () => ({}) }, root: dir, log: () => {} }));
  // xcodegen is advertised, xcodebuild is not: planning every native command first means xcodegen is not launched either.
  const noBuild = standIn({ cwd: dir, brokerCapabilities: ["adapter:rightkit-xcodegen"] });
  assert.throws(() => runIosDevelopmentCommands(commands, { native: noBuild, root: dir, log: () => {} }), (error) => error instanceof IosDevelopmentError && error.code === "DEV_IOS_NATIVE_REFUSED" && /RK_NATIVE_CAPABILITY_MISSING[^]*adapter:rightkit-xcodebuild/.test(error.message));
  assert.equal(noBuild.launched.length, 0);
  // an argv with no managed form (an owned output) is refused by the real door before anything runs
  const owned = standIn({ cwd: dir });
  assert.throws(() => runIosDevelopmentCommands([...commands.slice(0, 1), { ...commands[1], args: [...commands[1].args, "-derivedDataPath", "/x/dd"] }], { native: owned, root: dir, log: () => {} }), (error) => error.code === "DEV_IOS_NATIVE_REFUSED" && /RK_NATIVE_TOOL_REFUSED/.test(error.message));
  assert.equal(owned.launched.length, 0);
  // a failing call is a typed build failure with its output tail; a refusal at run time is carried through
  const failing = standIn({ cwd: dir, execute: ({ command }) => (command === "xcodebuild" ? { status: 65, stderr: "** BUILD FAILED **" } : { status: 0 }) });
  assert.throws(() => runIosDevelopmentCommands(commands, { native: failing, root: dir, log: () => {} }), (error) => error.code === "DEV_IOS_BUILD_FAILED" && /BUILD FAILED/.test(error.message) && /\(65\)/.test(error.message));
});

test("the built app is never guessed: no reported derived-data role refuses", () => {
  const dev = validateIosDevelopment(base({ install: simulator() }), { root });
  coded("DEV_IOS_OUTPUT_UNKNOWN", () => iosDevelopmentApp(dev, null));
  coded("DEV_IOS_OUTPUT_UNKNOWN", () => iosDevelopmentApp(dev, { ok: false, problem: "no managedOutputs" }));
  coded("DEV_IOS_OUTPUT_UNKNOWN", () => iosDevelopmentApp(dev, { ok: true, byRole: { archive: "/x/a" } }));
  assert.equal(slashed(iosDevelopmentApp(dev, { ok: true, byRole: { "derived-data": "/m/dd" } })), "/m/dd/Build/Products/Debug-iphonesimulator/Pulse.app");
});
