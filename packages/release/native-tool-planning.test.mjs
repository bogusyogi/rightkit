// GB-51 (GATE Mac/Dell broker-only): declared native build tools with no carried adapter refuse at PLANNING, with the missing capability named; the
// supported tools and every non-broker host are untouched; the door's launch-time refusal stays the backstop.
import "./test-support/broker-guard.mjs";
import assert from "node:assert/strict";
import test from "node:test";
import { BUILD_LAUNCH_REFUSED, buildLaunch } from "./build-launch.mjs";
import { routePlan } from "./bin/rightkit-gate.mjs";
import { nativeToolRefusal, nativeToolUses } from "./native-tool-planning.mjs";

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
  assert.match(nativeToolRefusal(nativeToolUses({ config: ios, to: "dev", devPlatform: "ios" })), /xcodegen at development\.targets\.ios\.projectSpec.*xcodebuild.*missing adapter:rightkit-xcodegen, adapter:rightkit-xcodebuild.*nothing ran/);
});

test("[GB-51] the route planner refuses before anything runs on a broker host, and only there; a supported config and a direct host plan as before", () => {
  const broker = { kind: "broker", cargo: ["rustright", "cargo"] };
  const o = { env: {}, platform: "darwin", spawn: () => ({ status: 1 }), exists: () => false, readFile: () => "", snapshotBroker: { kind: "x" }, resolveExecutor: () => broker };
  const caps = { ok: true, capabilities: ["snapshot-readonly-v1", "snapshot-adapter-v1"] };
  const plan = (config, to, devPlatform, executor = broker) => routePlan({ ...o, resolveExecutor: () => executor }, { to, stages: [], devTest: to === "dev" || to === "test", executor, capabilities: caps, appConfig: { config, devPlatform, readFile: () => null } });
  const refusal = plan(ios, "dev", "ios").refusal;
  assert.equal(refusal.code, "RK_BROKER_CAPABILITY_MISSING"); assert.match(refusal.message, /adapter:rightkit-xcodebuild/);
  assert.equal(plan(android, "dev", "android").refusal.code, "RK_BROKER_CAPABILITY_MISSING");
  assert.equal(plan(cmakeSidecar, "compile", null).refusal.code, "RK_BROKER_CAPABILITY_MISSING", "a CMake sidecar refuses the compile stage at planning");
  assert.notEqual(plan(supported, "dev", "mac").refusal?.code, "RK_BROKER_CAPABILITY_MISSING", "supported tools never raise the missing-adapter refusal (any other refusal is the signing policy's own)");
  assert.equal(plan(supported, "compile", null).refusal, null);
  assert.notEqual(plan(ios, "dev", "ios", { kind: "direct", cargo: ["cargo"] }).refusal?.code, "RK_BROKER_CAPABILITY_MISSING", "a direct host has no broker whose adapters are missing");
  assert.equal(plan(cmakeSidecar, "check", null).refusal, null, "Class A compiles nothing");
  // The launch-time door refusal is still the backstop for a step that reaches it.
  const managed = { env: { PATH: "/managed/agent-bin" }, platform: "darwin", exists: (p) => p === "/managed/agent-bin/rustright", which: (c) => (c === "rustright" ? ["/managed/agent-bin/rustright"] : []) };
  for (const tool of ["xcodegen", "xcodebuild", "gradlew", "cmake"]) assert.throws(() => buildLaunch(tool, ["x"], managed), (e) => e.code === BUILD_LAUNCH_REFUSED && /no broker adapter runs this command yet/.test(e.message), tool);
});
