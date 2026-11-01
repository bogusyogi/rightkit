// GB-51 (GATE Mac/Dell broker-only): declared native build tools (xcodegen, xcodebuild, Gradle, CMake) run as snapshot-bound jobs of their constrained adapters
// (native-adapters.mjs); a stage that uses one is planned only when the broker advertises the capability, an unsupported custom command refuses at PLANNING naming the capability,
// and nothing ever runs a tool around the broker.
import "./test-support/broker-guard.mjs";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { BUILD_LAUNCH_REFUSED, buildLaunch } from "./build-launch.mjs";
import { routePlan } from "./bin/rightkit-gate.mjs";
import { nativeToolRefusal, nativeToolUses } from "./native-tool-planning.mjs";
import { NATIVE_ADAPTERS, NATIVE_ADAPTER_CAPABILITIES } from "./native-adapters.mjs";
import { managedRuntimeClient } from "./managed-roots.mjs";

const ios = { development: { targets: { ios: { projectSpec: "ios/project.yml", build: [] } } } };
const android = { development: { targets: { android: { build: [{ cmd: "./gradlew", args: [":app:assembleDebug"], cwd: "android" }] } } } };
const cmakeSidecar = { sidecars: { sidecars: [{ kind: "cmake", name: "whisper", repo: "https://x/y", commit: "a".repeat(40) }] } };
const supported = { development: { targets: { mac: { build: [{ cmd: "pnpm", args: ["exec", "tauri", "build"] }, { cmd: "cargo", args: ["build"] }] } } }, sidecars: { sidecars: [{ kind: "cargo-bin", package: "eng" }] } };

test("[GB-51] each declared native tool is named with its missing capability: iOS xcodegen/xcodebuild, Android Gradle, a CMake sidecar; the supported tools name nothing", () => {
  assert.deepEqual(nativeToolUses({ config: ios, to: "dev", devPlatform: "ios" }).map((u) => [u.tool, u.capability]), [["xcodegen", "adapter:rightkit-xcodegen"], ["xcodebuild", "adapter:rightkit-xcodebuild"]]);
  assert.deepEqual(nativeToolUses({ config: android, to: "dev", devPlatform: "android" }).map((u) => [u.tool, u.capability]), [["gradlew", "adapter:rightkit-gradle"]]);
  assert.deepEqual(nativeToolUses({ config: cmakeSidecar, to: "compile" }).map((u) => u.tool), ["cmake"], "a cmake sidecar is in play for every stage that stages sidecars");
  assert.deepEqual(nativeToolUses({ config: supported, to: "dev", devPlatform: "mac" }), []);
  assert.deepEqual(nativeToolUses({ config: android, to: "compile" }), [], "a dev target's steps are not a compile stage's");
  assert.deepEqual(nativeToolUses({ config: ios, to: "dev", devPlatform: "mac" }), [], "another platform's target is not in play");
  assert.equal(nativeToolRefusal([]), null);
  // A broker that does not advertise the capability (or whose capabilities were not queried) refuses, naming each; one that advertises them plans.
  assert.match(nativeToolRefusal(nativeToolUses({ config: ios, to: "dev", devPlatform: "ios" })), /xcodegen at development\.targets\.ios\.projectSpec.*does not advertise adapter:rightkit-xcodegen.*xcodebuild.*does not advertise adapter:rightkit-xcodebuild.*nothing ran/);
  const all = { ok: true, capabilities: [...NATIVE_ADAPTER_CAPABILITIES] };
  assert.equal(nativeToolRefusal(nativeToolUses({ config: ios, to: "dev", devPlatform: "ios" }), { capabilities: all }), null);
  assert.match(nativeToolRefusal(nativeToolUses({ config: ios, to: "dev", devPlatform: "ios" }), { capabilities: { ok: true, capabilities: ["adapter:rightkit-xcodegen"] } }), /xcodebuild at .*does not advertise adapter:rightkit-xcodebuild/);
  assert.deepEqual(NATIVE_ADAPTER_CAPABILITIES, ["adapter:rightkit-xcodegen", "adapter:rightkit-xcodebuild", "adapter:rightkit-gradle", "adapter:rightkit-cmake"], "the names RustRight registers exactly (docs/GATE-CONTRACT.md)");
});

test("[GB-51] the route planner refuses before anything runs on a broker host, and only there; a supported config and a direct host plan as before", () => {
  const broker = { kind: "broker", cargo: ["rustright", "cargo"] };
  const o = { env: {}, platform: "darwin", spawn: () => ({ status: 1 }), exists: () => false, readFile: () => "", snapshotBroker: { kind: "x" }, resolveExecutor: () => broker };
  const caps = { ok: true, capabilities: ["snapshot-readonly-v1", "snapshot-adapter-v1"] };
  const withNative = { ok: true, capabilities: ["snapshot-readonly-v1", "snapshot-adapter-v1", ...NATIVE_ADAPTER_CAPABILITIES] };
  const plan = (config, to, devPlatform, executor = broker, capabilities = caps, readFile = () => null) => routePlan({ ...o, resolveExecutor: () => executor }, { to, stages: [], devTest: to === "dev" || to === "test", executor, capabilities, appConfig: { config, devPlatform, readFile } });
  const refusal = plan(ios, "dev", "ios").refusal;
  assert.equal(refusal.code, "RK_BROKER_CAPABILITY_MISSING"); assert.match(refusal.message, /adapter:rightkit-xcodebuild/);
  assert.equal(plan(android, "dev", "android").refusal.code, "RK_BROKER_CAPABILITY_MISSING");
  assert.equal(plan(cmakeSidecar, "compile", null).refusal.code, "RK_BROKER_CAPABILITY_MISSING", "a CMake sidecar refuses the compile stage at planning");
  assert.notEqual(plan(supported, "dev", "mac").refusal?.code, "RK_BROKER_CAPABILITY_MISSING", "supported tools never raise the missing-adapter refusal (any other refusal is the signing policy's own)");
  assert.equal(plan(supported, "compile", null).refusal, null);
  assert.notEqual(plan(ios, "dev", "ios", { kind: "direct", cargo: ["cargo"] }).refusal?.code, "RK_BROKER_CAPABILITY_MISSING", "a direct host has no broker whose adapters are missing");
  assert.equal(plan(cmakeSidecar, "check", null).refusal, null, "Class A compiles nothing");
  // With the adapters advertised the stage plans: the iOS and CMake uses go through, Gradle needs the committed wrapper's distribution pinned.
  assert.equal(plan(ios, "dev", "ios", broker, withNative).refusal, null);
  assert.equal(plan(cmakeSidecar, "compile", null, broker, withNative).refusal, null);
  const pinned = (rel) => (rel === "android/gradle/wrapper/gradle-wrapper.properties" ? `distributionUrl=x\ndistributionSha256Sum=${"a".repeat(64)}\n` : null);
  assert.equal(plan(android, "dev", "android", broker, withNative, pinned).refusal, null);
  const unpinned = plan(android, "dev", "android", broker, withNative, (rel) => (rel.endsWith("gradle-wrapper.properties") ? "distributionUrl=x\n" : null)).refusal;
  assert.equal(unpinned.code, "RK_BROKER_CAPABILITY_MISSING"); assert.match(unpinned.message, /carries no distributionSha256Sum/);
  assert.match(plan(android, "dev", "android", broker, withNative).refusal.message, /is not in the commit/);
  // An unsupported custom command refuses at planning (an output override, a release task), naming the capability.
  const custom = (step) => ({ development: { targets: { android: { build: [step] } } } });
  assert.match(plan(custom({ cmd: "./gradlew", args: [":app:assembleRelease"], cwd: "android" }), "dev", "android", broker, withNative, pinned).refusal.message, /positional argument ":app:assembleRelease" is not allowed.*adapter:rightkit-gradle/);
  assert.match(plan(custom({ cmd: "./gradlew", args: ["--gradle-user-home", "/x", ":app:assembleDebug"], cwd: "android" }), "dev", "android", broker, withNative, pinned).refusal.message, /--gradle-user-home names where the build writes/);
  // The door's launch of a tool with no route in a direct-session stays refused: only the adapters run native tools on a broker host.
  const managed = { env: { PATH: "/managed/agent-bin" }, platform: "darwin", exists: (p) => p === "/managed/agent-bin/rustright", which: (c) => (c === "rustright" ? ["/managed/agent-bin/rustright"] : []) };
  assert.throws(() => buildLaunch("make", ["x"], managed), (e) => e.code === BUILD_LAUNCH_REFUSED && /no broker adapter runs this command yet/.test(e.message));
});

test("[GB-51] the launch route: each declared tool is one carried job of its own adapter; paths are made relative to the step's directory; output overrides, unregistered flags, signing identities and a step's own environment are refused before anything is sent", (t) => {
  // A real application directory (the broker's file rules are judged on it: the Gradle wrapper's pin) and a Mac host whose installed runtime client is where managedRuntimeClient finds it; the
  // `rustright` on PATH is another file and is never the one a native step launches.
  const APP = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "rk-native-route-")));
  t.after(() => fs.rmSync(APP, { recursive: true, force: true }));
  fs.mkdirSync(path.join(APP, "android", "gradle", "wrapper"), { recursive: true });
  fs.writeFileSync(path.join(APP, "android", "gradle", "wrapper", "gradle-wrapper.properties"), `distributionSha256Sum=${"c".repeat(64)}\n`);
  fs.writeFileSync(path.join(APP, "ExportOptions.plist"), '<?xml version="1.0"?><plist><dict><key>destination</key><string>export</string></dict></plist>');
  const MAC_ENV = { PATH: "/managed/agent-bin", HOME: "/Users/rk" };
  const CLIENT = managedRuntimeClient(MAC_ENV, "darwin", () => true);
  const managed = { env: MAC_ENV, platform: "darwin", exists: (p) => p === CLIENT, which: (c) => (c === "rustright" ? ["/managed/agent-bin/rustright"] : []), cwd: APP };
  const go = (tool, args, over = {}) => buildLaunch(tool, args, { ...managed, ...over });
  const run = (tool, args, over) => { const launch = go(tool, args, over); assert.equal(launch.command, CLIENT, `${tool}: the executor's managed runtime client`); return launch.args; };
  assert.deepEqual(run("xcodegen", ["generate", "--spec", `${APP}/ios/project.yml`, "--project", `${APP}/ios`]), ["run", "rightkit-xcodegen", "--", "generate", "--spec", "ios/project.yml", "--project", "ios"]);
  assert.deepEqual(run("xcodebuild", ["-project", `${APP}/ios/A.xcodeproj`, "-scheme", "A", "-configuration", "Debug", "-destination", "generic/platform=iOS Simulator", "DEVELOPMENT_TEAM=ABCDE12345", "build"]), ["run", "rightkit-xcodebuild", "--", "-project", "ios/A.xcodeproj", "-scheme", "A", "-configuration", "Debug", "-destination", "generic/platform=iOS Simulator", "DEVELOPMENT_TEAM=ABCDE12345", "build"]);
  assert.deepEqual(run("./gradlew", [":app:assembleDebug", "--offline"], { cwd: `${APP}/android` }), ["run", "rightkit-gradle", "--", ":app:assembleDebug", "--offline"]);
  // CMake: the configure names its source only (the broker names the build tree and the install prefix); `--build` and `--install` are modes that name no directory.
  assert.deepEqual(run("cmake", ["-S", `${APP}/src`, "-DCMAKE_BUILD_TYPE=Release", "-G", "Ninja"]), ["run", "rightkit-cmake", "--", "-S", "src", "-DCMAKE_BUILD_TYPE=Release", "-G", "Ninja"]);
  assert.deepEqual(run("cmake", ["--build", "--config", "Release", "--target", "whisper", "--parallel"]), ["run", "rightkit-cmake", "--", "--build", "--config", "Release", "--target", "whisper", "--parallel"]);
  assert.deepEqual(run("cmake", ["--install", "--config", "Release"]), ["run", "rightkit-cmake", "--", "--install", "--config", "Release"]);
  // The archive, export and test shapes the broker now takes (RA-09), with the options file made relative.
  assert.deepEqual(run("xcodebuild", ["-project", `${APP}/ios/A.xcodeproj`, "-scheme", "A", "archive"]).slice(-1), ["archive"]);
  assert.deepEqual(run("xcodebuild", ["-exportArchive", "-exportOptionsPlist", `${APP}/ExportOptions.plist`]), ["run", "rightkit-xcodebuild", "--", "-exportArchive", "-exportOptionsPlist", "ExportOptions.plist"]);
  assert.deepEqual(run("xcodebuild", ["-scheme", "A", "-testPlan", "UI", "-only-testing:AUITests/Login", "test"]).slice(3), ["-scheme", "A", "-testPlan", "UI", "-only-testing:AUITests/Login", "test"]);
  // Negative cases per tool: an output override, an unregistered flag, a path outside the step's directory, a signing identity, a release task, a step environment.
  const refused = (tool, args, re, over) => assert.throws(() => go(tool, args, over), (e) => e.code === BUILD_LAUNCH_REFUSED && re.test(e.message), `${tool} ${args.join(" ")}`);
  refused("xcodebuild", ["-project", `${APP}/A.xcodeproj`, "-scheme", "A", "-derivedDataPath", "/tmp/dd", "build"], /-derivedDataPath names where the build writes.*adapter:rightkit-xcodebuild/);
  refused("xcodebuild", ["-project", `${APP}/A.xcodeproj`, "-scheme", "A", "-resultBundlePath", "/tmp/r.xcresult", "build"], /-resultBundlePath names where the build writes/);
  refused("xcodebuild", ["-project", `${APP}/A.xcodeproj`, "-scheme", "A", "-archivePath", "/tmp/a.xcarchive", "archive"], /-archivePath names where the build writes/);
  refused("xcodebuild", ["-exportArchive", "-exportPath", "/tmp/e", "-exportOptionsPlist", "ExportOptions.plist"], /-exportPath names where the build writes/);
  refused("xcodebuild", ["-project", `${APP}/A.xcodeproj`, "-scheme", "A", "CODE_SIGN_IDENTITY=Apple Development", "build"], /positional argument .* is not allowed/);
  refused("xcodebuild", ["-project", `${APP}/A.xcodeproj`, "-scheme", "A", "SYMROOT=/tmp/out", "build"], /positional argument .* is not allowed/);
  refused("xcodebuild", ["-project", "/etc/A.xcodeproj", "-scheme", "A", "build"], /is outside the step's directory/);
  refused("xcodebuild", ["-project", `${APP}/A.xcodeproj`, "-exportArchive", "build"], /takes no action or build setting/);
  refused("xcodegen", ["generate", "--spec", `${APP}/p.yml`, "--cache-path", "/tmp/c"], /--cache-path names where the build writes/);
  refused("xcodegen", ["dump"], /subcommand "dump" is not registered/);
  refused("./gradlew", [":app:assembleDebug", "--init-script", "/tmp/i.gradle"], /--init-script names where the build writes/, { cwd: `${APP}/android` });
  refused("./gradlew", [":app:bundleRelease"], /is not allowed/, { cwd: `${APP}/android` });
  refused("./gradlew", [":app:assembleDebug", "-Dorg.gradle.jvmargs=-Xmx9g"], /not registered/, { cwd: `${APP}/android` });
  refused("cmake", ["-S", `${APP}/src`, "--toolchain", "/tmp/t.cmake"], /--toolchain names where the build writes/);
  refused("cmake", ["-S", `${APP}/src`, "-DCMAKE_COMMAND=/tmp/evil"], /never admits|wrong kind/);
  refused("cmake", ["-E", "remove_directory", "x"], /not registered/);
  refused("cmake", ["-S", "/tmp/elsewhere"], /outside the step's directory/);
  // The build tree is the broker's: a caller's `-B`, a `--build <dir>` and an install prefix refuse, naming the managed route.
  refused("cmake", ["-S", `${APP}/src`, "-B", `${APP}/build`], /-B names where the build writes/);
  refused("cmake", ["--build", `${APP}/build`], /positional argument .* is not allowed/);
  refused("cmake", ["-S", `${APP}/src`, "-DCMAKE_INSTALL_PREFIX=/opt/x"], /moves where CMake writes/);
});

// ---- the carried jobs: the door's own answers as data, then the real broker running the real tools ----
import { spawnSync } from "./background-process.mjs";
import { CARRIED_CARGO_MAP_ENV, installCarrier } from "./carried-cargo.mjs";
import { brokerSection, createSnapshotBroker, signedNativeOutputs } from "./snapshot-broker.mjs";
import { createNativeRunner } from "./build-launch.mjs";
import { nativeManagedOutputs, nativeStateDir } from "./native-adapters.mjs";
import { syncSnapshot } from "./snapshot.mjs";
import { startRealBroker } from "./test-support/real-broker.mjs";

/** The PUBLIC half of one isolated instance's own attestation key and its runtime's broker digest (the pins a gate enrolls for it); a receipt never supplies its own. */
function enrolledPinsOf(broker) {
  const key = JSON.parse(fs.readFileSync(path.join(broker.controlRoot, "attestation", "key.json"), "utf8"));
  const doctor = JSON.parse(broker.run(["doctor", "--json", "--no-scan"]).stdout);
  const find = (v, name) => { if (v && typeof v === "object") { if (typeof v[name] === "string") return v[name]; for (const x of Object.values(v)) { const r = find(x, name); if (r) return r; } } return null; };
  const brokerDigest = find(doctor, "brokerDigest"); const host = find(doctor, "host") ?? os.hostname();
  assert.match(String(brokerDigest), /^[0-9a-f]{64}$/, "the real client reports the installed broker's digest");
  return { host, keyId: key.keyId, publicKey: key.publicKey, brokerDigest };
}

const NATIVE_CASES = (snapshot) => [
  ["xcodegen", ["generate", "--spec", `${snapshot}/project.yml`, "--project", snapshot], "rightkit-xcodegen", ["generate", "--spec", "project.yml", "--project", "."]],
  ["xcodebuild", ["-project", `${snapshot}/A.xcodeproj`, "-scheme", "A", "build"], "rightkit-xcodebuild", ["-project", "A.xcodeproj", "-scheme", "A", "build"]],
  ["./gradlew", [":app:assembleDebug"], "rightkit-gradle", [":app:assembleDebug"]],
  ["cmake", ["-S", `${snapshot}/src`], "rightkit-cmake", ["-S", "src"]],
];

test("data: [GB-51] the door sends each native tool as one snapshot-bound adapter job with the registered, relative argv; a step with its own environment is refused; the `rustright run <adapter> --` spelling is judged by the same rules", () => {
  const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "rk-native-door-")));
  try {
    const checkout = path.join(root, "checkout"); const snapshot = path.join(root, "snapshots", "0123456789abcdef"); const client = path.join(root, "rustright");
    for (const d of [checkout, snapshot]) fs.mkdirSync(d, { recursive: true });
    fs.mkdirSync(path.join(snapshot, "gradle", "wrapper"), { recursive: true });
    fs.writeFileSync(path.join(snapshot, "gradle", "wrapper", "gradle-wrapper.properties"), `distributionSha256Sum=${"d".repeat(64)}\n`);
    const commit = { sha: "c".repeat(40), tree: "e".repeat(40) };
    const section = { client, controlRoot: path.join(root, "control"), commit, snapshot: { path: snapshot, expectedTree: commit.tree }, handle: "own-7", checkout, pins: { host: "host-1", keyId: "0".repeat(64), publicKey: "04".padEnd(130, "0"), brokerDigest: "b".repeat(64) }, outputRoots: [], platform: process.platform, sentinel: null };
    const installed = installCarrier({ base: fs.mkdtempSync(path.join(root, "carrier-")), snapshotRoot: snapshot, checkoutDirs: { ".": checkout }, section, platform: process.platform, env: {} });
    const env = { PATH: root, HOME: process.env.HOME, RUSTRIGHT_CALLING_GATE: "/gate/bin/rightkit-gate.mjs", ...installed.env };
    const door = (tool, args, over = {}) => buildLaunch(tool, args, { env, platform: "darwin", exists: (p) => p === client, cwd: snapshot, ...over });
    for (const [tool, args, adapter] of NATIVE_CASES(snapshot)) {
      const launch = door(tool, args);
      assert.equal(path.basename(launch.args[0]), "carried-adapter-run.mjs", tool);
      assert.deepEqual(launch.args.slice(1, 3), [adapter, "--"], tool);
    }
    assert.throws(() => door("cmake", ["--build"], { stepEnv: { MY_FLAG: "1" } }), (e) => e.code === BUILD_LAUNCH_REFUSED && e.reason === "RK_ADAPTER_ENV_UNREGISTERED");
    assert.deepEqual(door("rustright", ["run", "rightkit-cmake", "--", "--build", "--config", "Release"]).args.slice(1, 3), ["rightkit-cmake", "--"]);
    assert.throws(() => door("rustright", ["run", "rightkit-cmake", "--", "-B", "/tmp/elsewhere", "-S", `${snapshot}/src`]), (e) => e.code === BUILD_LAUNCH_REFUSED);
    assert.equal(createNativeRunner({ cwd: snapshot, env, platform: "darwin", exists: (p) => p === client }).run("./gradlew", [":app:assembleDebug"], { signingFile: "/private/x/signing.properties" }).refusal.code, "RK_NATIVE_SIGNING_FILE_UNSUPPORTED");
    assert.ok(NATIVE_ADAPTERS.cmake.executable.length > 0);
    installed.cleanup();
  } finally { fs.rmSync(root, { recursive: true, force: true }); }
});

test("[GB-51] (real broker) xcodegen and cmake each run as one signed, ledgered, snapshot-bound adapter job of the REAL broker whose output directory is the broker's (never named by the step); the production runner returns the verified job, its output directory and the output mapping", { skip: process.platform === "win32" ? "POSIX fixture" : false }, async (t) => {
  const broker = await startRealBroker({ name: "native-tools" });
  t.after(() => broker.stop());
  const repo = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "rk-native-real-")));
  t.after(() => fs.rmSync(repo, { recursive: true, force: true }));
  const git = (...a) => { const r = spawnSync("git", a, { cwd: repo, encoding: "utf8" }); assert.equal(r.status, 0, r.stderr); return r.stdout.trim(); };
  git("init", "-q", "-b", "main"); git("config", "user.email", "t@example.test"); git("config", "user.name", "t");
  const files = { "project.yml": "name: A\n", "src/CMakeLists.txt": "cmake_minimum_required(VERSION 3.20)\nproject(A NONE)\n", "gradle/wrapper/gradle-wrapper.properties": `distributionSha256Sum=${"d".repeat(64)}\n` };
  for (const [rel, body] of Object.entries(files)) { fs.mkdirSync(path.dirname(path.join(repo, rel)), { recursive: true }); fs.writeFileSync(path.join(repo, rel), body); }
  git("add", "-A"); git("commit", "-q", "-m", "A");
  const synced = syncSnapshot({ checkout: repo, rev: git("rev-parse", "HEAD"), root: path.join(broker.dataRoot, "snapshots") });
  const handle = "own-7";
  const own = path.join(repo, "..", `owner-${process.pid}.json`);
  fs.writeFileSync(own, JSON.stringify({ handle, broker: { submitted: false } }));
  t.after(() => fs.rmSync(own, { force: true }));
  const sb = createSnapshotBroker({ env: broker.env, platform: process.platform, checkout: repo, commit: { sha: synced.commit, tree: synced.tree }, snapshot: { path: synced.path, expectedTree: synced.tree }, ownership: { handle, markBrokerSubmit: (h) => ({ handle, broker: { submitted: true, handles: [h] } }) }, client: broker.clientPath, dataRoot: broker.dataRoot, loadPins: () => enrolledPinsOf(broker) });
  const installed = installCarrier({ base: fs.mkdtempSync(path.join(os.tmpdir(), "rk-native-carrier-")), snapshotRoot: synced.path, checkoutDirs: { ".": repo }, section: brokerSection(sb), platform: process.platform, env: {} });
  t.after(() => installed.cleanup());
  const env = { PATH: process.env.PATH, HOME: process.env.HOME, ...broker.env, RUSTRIGHT_CALLING_GATE: "/gate/bin/rightkit-gate.mjs", RIGHTKIT_BUILD_OWNERSHIP: handle, RIGHTKIT_BUILD_OWNERSHIP_FILE: own, ...installed.env };
  const door = (tool, args, over = {}) => buildLaunch(tool, args, { env, platform: process.platform, cwd: synced.path, ...over });
  for (const [tool, args, adapter] of NATIVE_CASES(synced.path).filter(([name]) => name === "xcodegen" || name === "cmake")) {
    const launch = door(tool, args);
    const r = spawnSync(launch.command, launch.args, { cwd: synced.path, env: { ...env, ...(launch.env ?? {}) }, encoding: "utf8" });
    assert.equal(r.status, 0, `${tool} (${adapter}): ${r.stderr}`);
  }
  const verified = JSON.parse(`[${fs.readdirSync(installed.ledgerDir).flatMap((f) => fs.readFileSync(path.join(installed.ledgerDir, f), "utf8").trim().split("\n")).filter(Boolean).join(",")}]`).filter((e) => e.status === "verified");
  assert.equal(verified.length, 2);
  assert.ok(verified.every((e) => e.shim === "adapter" && typeof e.outputDir === "string" && e.outputDir.startsWith(path.join(broker.dataRoot, "rightkit-build-control", "ws"))), "each signed receipt names the broker's managed output directory");
  // The production runner takes the same route in-process: the carried call is verified, and the result names the job, its output directory and the output mapping by role.
  const runner = createNativeRunner({ cwd: synced.path, env, platform: process.platform });
  const result = runner.run("cmake", ["-S", `${synced.path}/src`]);
  assert.equal(result.ok, true, result.stderr);
  assert.equal(result.carried, true); assert.equal(result.tool, "cmake"); assert.deepEqual(result.argv, ["-S", "src"]);
  assert.equal(typeof result.requestId, "string"); assert.ok(result.outputDir.startsWith(path.join(broker.dataRoot, "rightkit-build-control", "ws")));
  assert.equal(result.outputs.ok, true);
  assert.equal(result.outputs.state, nativeStateDir(result.outputDir, "cmake", ""));
  assert.deepEqual(result.outputs.outputs, nativeManagedOutputs("cmake", ["-S", "src"], { state: result.outputs.state }).outputs);
});

test("[RA-07/RA-09] the output mapping is read only from the broker's signed `executable.managedOutputs` (RustRight service.rs signs `payload.executable` with `argv` removed and mirrors it at `receipt.nativeOutputs`); a mirror alone, or a differing one, is refused", () => {
  const managed = { tool: "xcodebuild", state: "/ws/out/native/xcodebuild/abc", outputs: [{ role: "derived-data", path: "/ws/out/native/xcodebuild/abc/derived-data" }, { role: "archive", path: "/ws/out/native/xcodebuild/abc/archive/App.xcarchive" }] };
  const payload = { executable: { tool: "xcodebuild", digest: "x", managedOutputs: managed } };
  assert.deepEqual(signedNativeOutputs({ nativeOutputs: managed }, payload), managed);
  assert.deepEqual(signedNativeOutputs({}, payload), managed, "the signed copy is the mapping; the mirror is optional");
  assert.deepEqual(signedNativeOutputs({ nativeOutputs: { outputs: managed.outputs, state: managed.state, tool: "xcodebuild" } }, payload), managed, "key order is not a difference");
  assert.equal(signedNativeOutputs({}, { executable: { tool: "xcodegen" } }), null, "a call with no managed state (xcodegen without --use-cache, any non-native adapter) has none");
  assert.equal(signedNativeOutputs({}, {}), null);
  assert.throws(() => signedNativeOutputs({ nativeOutputs: managed }, { executable: { tool: "xcodebuild" } }), /not covered by the signature/);
  assert.throws(() => signedNativeOutputs({ nativeOutputs: { ...managed, state: "/elsewhere" } }, payload), /differs from the signed/);
  assert.throws(() => signedNativeOutputs({}, { executable: { managedOutputs: { tool: "xcodebuild", state: "/s", outputs: [{ role: "x" }] } } }), /is not \{ tool, state/);
  assert.throws(() => signedNativeOutputs({}, { executable: { managedOutputs: "x" } }), /is not \{ tool, state/);
});
