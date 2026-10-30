// GB-51 (GATE Mac/Dell broker-only): declared native build tools (xcodegen, xcodebuild, Gradle, CMake) run as snapshot-bound jobs of their constrained adapters
// (native-adapters.mjs); a stage that uses one is planned only when the broker advertises the capability, an unsupported custom command refuses at PLANNING naming the capability,
// and nothing ever runs a tool around the broker.
import "./test-support/broker-guard.mjs";
import assert from "node:assert/strict";
import test from "node:test";
import { BUILD_LAUNCH_REFUSED, buildLaunch } from "./build-launch.mjs";
import { routePlan } from "./bin/rightkit-gate.mjs";
import { nativeToolRefusal, nativeToolUses } from "./native-tool-planning.mjs";
import { NATIVE_ADAPTERS, NATIVE_ADAPTER_CAPABILITIES } from "./native-adapters.mjs";

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

test("[GB-51] the launch route: each declared tool is one carried job of its own adapter; paths are made relative to the step's directory; output overrides, unregistered flags, signing identities and a step's own environment are refused before anything is sent", () => {
  const managed = { env: { PATH: "/managed/agent-bin" }, platform: "darwin", exists: (p) => p === "/managed/agent-bin/rustright", which: (c) => (c === "rustright" ? ["/managed/agent-bin/rustright"] : []), cwd: "/app" };
  const go = (tool, args, over = {}) => buildLaunch(tool, args, { ...managed, ...over });
  const run = (tool, args, over) => go(tool, args, over).args;
  assert.deepEqual(run("xcodegen", ["generate", "--spec", "/app/ios/project.yml", "--project", "/app/ios"]), ["run", "rightkit-xcodegen", "--", "generate", "--spec", "ios/project.yml", "--project", "ios"]);
  assert.deepEqual(run("xcodebuild", ["-project", "/app/ios/A.xcodeproj", "-scheme", "A", "-configuration", "Debug", "-destination", "generic/platform=iOS Simulator", "DEVELOPMENT_TEAM=ABCDE12345", "build"]), ["run", "rightkit-xcodebuild", "--", "-project", "ios/A.xcodeproj", "-scheme", "A", "-configuration", "Debug", "-destination", "generic/platform=iOS Simulator", "DEVELOPMENT_TEAM=ABCDE12345", "build"]);
  assert.deepEqual(run("./gradlew", [":app:assembleDebug", "--offline"], { cwd: "/app/android" }), ["run", "rightkit-gradle", "--", ":app:assembleDebug", "--offline"]);
  assert.deepEqual(run("cmake", ["-S", "/app/src", "-B", "/app/build", "-DCMAKE_BUILD_TYPE=Release", "-G", "Ninja"]), ["run", "rightkit-cmake", "--", "-S", "src", "-B", "build", "-DCMAKE_BUILD_TYPE=Release", "-G", "Ninja"]);
  assert.deepEqual(run("cmake", ["--build", "/app/build", "--config", "Release", "--target", "whisper", "--parallel"]), ["run", "rightkit-cmake", "--", "--build", "build", "--config", "Release", "--target", "whisper", "--parallel"]);
  // Negative cases per tool: an output override, an unregistered flag, a path outside the step's directory, a signing identity, a release task, a step environment.
  const refused = (tool, args, re, over) => assert.throws(() => go(tool, args, over), (e) => e.code === BUILD_LAUNCH_REFUSED && re.test(e.message), `${tool} ${args.join(" ")}`);
  refused("xcodebuild", ["-project", "/app/A.xcodeproj", "-scheme", "A", "-derivedDataPath", "/tmp/dd", "build"], /-derivedDataPath names where the build writes.*adapter:rightkit-xcodebuild/);
  refused("xcodebuild", ["-project", "/app/A.xcodeproj", "-scheme", "A", "-resultBundlePath", "/tmp/r.xcresult", "build"], /-resultBundlePath names where the build writes/);
  refused("xcodebuild", ["-project", "/app/A.xcodeproj", "-scheme", "A", "CODE_SIGN_IDENTITY=Apple Development", "build"], /positional argument .* is not allowed/);
  refused("xcodebuild", ["-project", "/app/A.xcodeproj", "-scheme", "A", "SYMROOT=/tmp/out", "build"], /positional argument .* is not allowed/);
  refused("xcodebuild", ["-project", "/etc/A.xcodeproj", "-scheme", "A", "build"], /is outside the step's directory/);
  refused("xcodebuild", ["-project", "/app/A.xcodeproj", "-exportArchive", "build"], /not registered/);
  refused("xcodegen", ["generate", "--spec", "/app/p.yml", "--cache-path", "/tmp/c"], /--cache-path names where the build writes/);
  refused("xcodegen", ["dump"], /subcommand "dump" is not registered/);
  refused("./gradlew", [":app:assembleDebug", "--init-script", "/tmp/i.gradle"], /--init-script names where the build writes/, { cwd: "/app/android" });
  refused("./gradlew", [":app:bundleRelease"], /is not allowed/, { cwd: "/app/android" });
  refused("./gradlew", [":app:assembleDebug", "-Dorg.gradle.jvmargs=-Xmx9g"], /not registered/, { cwd: "/app/android" });
  refused("cmake", ["-S", "/app/src", "-B", "/app/build", "--toolchain", "/tmp/t.cmake"], /--toolchain names where the build writes/);
  refused("cmake", ["-S", "/app/src", "-B", "/app/build", "-DCMAKE_COMMAND=/tmp/evil"], /never admits|wrong kind/);
  refused("cmake", ["-E", "remove_directory", "x"], /not registered/);
  refused("cmake", ["-S", "/app/src", "-B", "/tmp/elsewhere"], /outside the step's directory/);
});

// ---- the carried jobs, end to end through the fixture broker ----
import fs from "node:fs";
import path from "node:path";
import { spawnSync } from "./background-process.mjs";
import { CARRIED_CARGO_MAP_ENV, installCarrier } from "./carried-cargo.mjs";
import { brokerSection } from "./snapshot-broker.mjs";
import { makeFakeBroker } from "./test-support/fake-broker.mjs";

test("[GB-51] each native tool runs as one signed, ledgered, snapshot-bound adapter job whose output directory is the broker's (never named by the step); a step with its own environment is refused", { skip: process.platform === "win32" ? "POSIX fixture" : false }, (t) => {
  const fx = makeFakeBroker(t, { readOnlySnapshots: true });
  fx.setBehavior({});
  const installed = installCarrier({ base: fs.mkdtempSync(path.join(fx.root, "carrier-")), snapshotRoot: fx.snapshot, checkoutDirs: { ".": fx.checkout }, section: brokerSection(fx.sb), platform: process.platform, env: {} });
  t.after(() => installed.cleanup());
  const env = { PATH: fx.root, HOME: process.env.HOME, RUSTRIGHT_CALLING_GATE: "/gate/bin/rightkit-gate.mjs", ...fx.ownershipEnv, ...installed.env };
  const door = (tool, args, over = {}) => buildLaunch(tool, args, { env, platform: "darwin", exists: (p) => p === path.join(fx.root, "rustright"), cwd: fx.snapshot, ...over });
  const cases = [
    ["xcodegen", ["generate", "--spec", `${fx.snapshot}/project.yml`, "--project", fx.snapshot], "rightkit-xcodegen", ["generate", "--spec", "project.yml", "--project", "."]],
    ["xcodebuild", ["-project", `${fx.snapshot}/A.xcodeproj`, "-scheme", "A", "build"], "rightkit-xcodebuild", ["-project", "A.xcodeproj", "-scheme", "A", "build"]],
    ["./gradlew", [":app:assembleDebug"], "rightkit-gradle", [":app:assembleDebug"]],
    ["cmake", ["-S", `${fx.snapshot}/src`, "-B", `${fx.snapshot}/build`], "rightkit-cmake", ["-S", "src", "-B", "build"]],
  ];
  for (const [tool, args, adapter, sent] of cases) {
    const launch = door(tool, args);
    assert.deepEqual(launch.args.slice(0, 3), [path.join(path.dirname(new URL(import.meta.url).pathname), "carried-adapter-run.mjs"), adapter, "--"].slice(0, 3), tool);
    const r = spawnSync(launch.command, launch.args, { cwd: fx.snapshot, env: { ...env, ...(launch.env ?? {}) }, encoding: "utf8" });
    assert.equal(r.status, 0, `${tool}: ${r.stderr}`);
    const job = fx.adapterCalls().at(-1);
    assert.equal(job.adapter, adapter); assert.deepEqual(job.argv, sent, `${tool}: the job received the relative, registered argv`);
  }
  const verified = JSON.parse(`[${fs.readdirSync(installed.ledgerDir).flatMap((f) => fs.readFileSync(path.join(installed.ledgerDir, f), "utf8").trim().split("\n")).filter(Boolean).join(",")}]`).filter((e) => e.status === "verified");
  assert.equal(verified.length, 4); assert.ok(verified.every((e) => e.shim === "adapter" && typeof e.outputDir === "string" && e.outputDir.startsWith(fx.wsRoot)), "each signed receipt names the broker's managed output directory");
  assert.throws(() => door("cmake", ["--build", `${fx.snapshot}/build`], { stepEnv: { MY_FLAG: "1" } }), (e) => e.code === BUILD_LAUNCH_REFUSED && e.reason === "RK_ADAPTER_ENV_UNREGISTERED");
  // The `rustright run <adapter> --` spelling of the same steps is judged by the same rules.
  assert.deepEqual(door("rustright", ["run", "rightkit-cmake", "--", "--build", `${fx.snapshot}/build`]).args.slice(1, 3), ["rightkit-cmake", "--"]);
  assert.throws(() => door("rustright", ["run", "rightkit-cmake", "--", "-B", "/tmp/elsewhere", "-S", `${fx.snapshot}/src`]), (e) => e.code === BUILD_LAUNCH_REFUSED);
  assert.ok(NATIVE_ADAPTERS.cmake.executable.length > 0);
});
