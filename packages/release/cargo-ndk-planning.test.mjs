// GB-62 (GATE commit exactness; BR20, rustright externalCommands): `cargo ndk` is a pinned EXTERNAL tool, never a free subcommand. A stage that builds Android JNI libraries is planned only
// when the broker advertises the pinned registration (capability `external:cargo-ndk`: RustRight's `externalCommands` entry for shim cargo / command ndk, with its digest) and the
// declared ndk block is one RustRight's ndk grammar accepts (ABIs arm64-v8a and x86_64, a generated `jniLibs` directory that holds no committed file), BEFORE Class A or any build.
import "./test-support/broker-guard.mjs";
import assert from "node:assert/strict";
import test from "node:test";
import { routePlan, routeAppConfig } from "./bin/rightkit-gate.mjs";
import { androidNdkCommand, validateAndroidNdk } from "./android-ndk.mjs";
import { NDK_EXTERNAL_CAPABILITY, nativeToolRefusal, nativeToolUses } from "./native-tool-planning.mjs";

const JNI = "android/app/src/main/jniLibs";
const devConfig = (ndk) => ({ development: { targets: { android: { ndk, build: [] } } } });
const releaseConfig = (ndk) => ({ targets: { android: { ndk } } });
const caps = (...extra) => ({ ok: true, capabilities: ["snapshot-readonly-v1", "snapshot-adapter-v1", ...extra] });
const broker = { kind: "broker", cargo: ["rustright", "cargo"] };
const o = { env: {}, platform: "darwin", spawn: () => ({ status: 1 }), exists: () => false, readFile: () => "", snapshotBroker: { kind: "x" }, resolveExecutor: () => broker };
const plan = (config, to, nativePlatform, capabilities, listFiles = () => []) => routePlan(o, { to, stages: [], devTest: to === "dev" || to === "test", executor: broker, capabilities, appConfig: { config, devPlatform: null, nativePlatform, readFile: () => null, listFiles } });

test("[GB-62] the capability is named from the commit's own config, for a dev/test android target and a release android target, and only for them", () => {
  assert.equal(NDK_EXTERNAL_CAPABILITY, "external:cargo-ndk");
  const dev = nativeToolUses({ config: devConfig({ jniLibs: JNI }), to: "dev", devPlatform: "android" });
  assert.deepEqual(dev.map((u) => [u.tool, u.capability]), [["cargo-ndk", "external:cargo-ndk"]]);
  assert.deepEqual(nativeToolUses({ config: releaseConfig({ jniLibs: JNI }), to: "release", devPlatform: null, platform: "android" }).map((u) => u.tool), ["cargo-ndk"]);
  assert.deepEqual(nativeToolUses({ config: releaseConfig({ jniLibs: JNI }), to: "package", devPlatform: null, platform: "android" }).map((u) => u.tool), ["cargo-ndk"]);
  assert.deepEqual(nativeToolUses({ config: devConfig({ jniLibs: JNI }), to: "dev", devPlatform: "mac" }), [], "another platform does not use it");
  assert.deepEqual(nativeToolUses({ config: devConfig({ jniLibs: JNI }), to: "dev", devPlatform: null }), [], "no platform, no android use");
  assert.deepEqual(nativeToolUses({ config: devConfig({ jniLibs: JNI }), to: "compile", devPlatform: null }), [], "a compile stage does not run it");
  assert.deepEqual(nativeToolUses({ config: devConfig(undefined), to: "dev", devPlatform: "android" }), [], "no ndk block, no use");
});

test("[GB-62] negative: without the pinned registration the stage refuses at planning naming it; with it the stage plans", () => {
  const uses = nativeToolUses({ config: devConfig({ jniLibs: JNI }), to: "dev", devPlatform: "android" });
  assert.match(nativeToolRefusal(uses, { capabilities: caps() }), /cargo-ndk at development\.targets\.android\.ndk: the broker does not advertise external:cargo-ndk.*nothing ran/);
  assert.match(nativeToolRefusal(uses, { capabilities: null }), /does not advertise external:cargo-ndk \(its capabilities were not queried\)/);
  assert.match(nativeToolRefusal(uses, { capabilities: { ok: false, problem: "doctor failed" } }), /doctor failed/);
  assert.equal(nativeToolRefusal(uses, { capabilities: caps("external:cargo-ndk") }), null);
  // The route refuses before Class A or any build, on a broker host, for both the dev and the release stage.
  const dev = plan(devConfig({ jniLibs: JNI }), "dev", "android", caps()).refusal;
  assert.equal(dev.code, "RK_BROKER_CAPABILITY_MISSING"); assert.match(dev.message, /external:cargo-ndk/);
  const release = plan(releaseConfig({ jniLibs: JNI }), "release", "android", caps()).refusal;
  assert.equal(release.code, "RK_BROKER_CAPABILITY_MISSING");
  assert.equal(plan(devConfig({ jniLibs: JNI }), "dev", "android", caps("external:cargo-ndk")).refusal, null);
  assert.equal(plan(releaseConfig({ jniLibs: JNI }), "release", "android", caps("external:cargo-ndk")).refusal, null);
});

test("[GB-62] negative: an ABI RustRight's ndk grammar refuses (armeabi-v7a, x86) is refused at planning, with the registration advertised", () => {
  const advertised = caps("external:cargo-ndk");
  for (const abi of ["armeabi-v7a", "x86"]) {
    const refusal = plan(devConfig({ jniLibs: JNI, abis: ["arm64-v8a", abi] }), "dev", "android", advertised).refusal;
    assert.equal(refusal.code, "RK_BROKER_CAPABILITY_MISSING", abi);
    assert.match(refusal.message, new RegExp(`ndk.abis.*${abi}.*only arm64-v8a, x86_64`), abi);
  }
  assert.equal(plan(devConfig({ jniLibs: JNI, abis: ["arm64-v8a", "x86_64"] }), "dev", "android", advertised).refusal, null);
  assert.equal(plan(devConfig({ jniLibs: JNI }), "dev", "android", advertised).refusal, null, "the default ABIs are the accepted ones");
  // The command the gate sends carries --locked at the inner boundary and exactly the accepted options.
  const ndk = validateAndroidNdk({ jniLibs: JNI, abis: ["x86_64"], apiLevel: 26 }, { root: "/app", kind: "release" });
  assert.deepEqual(androidNdkCommand(ndk).args.slice(0, 9), ["ndk", "--platform", "26", "-t", "x86_64", "-o", ndk.jniLibs, "build", "--locked"]);
});

test("[GB-62] negative: the JNI output must be a generated directory: a committed file under jniLibs refuses, and a malformed ndk block refuses naming its reason", () => {
  const advertised = caps("external:cargo-ndk");
  const committed = plan(devConfig({ jniLibs: JNI }), "dev", "android", advertised, (dir) => (dir === JNI ? [`${JNI}/arm64-v8a/libx.so`] : [])).refusal;
  assert.equal(committed.code, "RK_BROKER_CAPABILITY_MISSING");
  assert.match(committed.message, /jniLibs.*commits 1 file.*libx\.so.*generated/);
  assert.equal(plan(devConfig({ jniLibs: JNI }), "dev", "android", advertised, () => []).refusal, null, "an empty generated directory is the controlled ledger");
  assert.equal(plan(devConfig({ jniLibs: JNI }), "dev", "android", advertised, () => null).refusal, null, "a directory the commit does not have is generated");
  const bad = plan(devConfig({ jniLibs: "../outside/jniLibs" }), "dev", "android", advertised).refusal;
  assert.match(bad.message, /ndk.jniLibs must name a generated directory called jniLibs inside the app/);
  assert.match(plan(devConfig({ jniLibs: JNI, extra: 1 }), "dev", "android", advertised).refusal.message, /unsupported key\(s\) extra/);
});

test("[GB-62] routeAppConfig carries the requested platform so the android target is seen on the route (the dev platform stays mac/win for signing)", () => {
  const files = { "right-release.config.json": JSON.stringify(devConfig({ jniLibs: JNI })) };
  const git = (...args) => (args[0] === "show" ? { status: files[args[1].split(":")[1]] === undefined ? 1 : 0, stdout: files[args[1].split(":")[1]] ?? "" } : { status: 0, stdout: "" });
  const android = routeAppConfig({ git, commit: "c".repeat(40), platform: "android" });
  assert.equal(android.nativePlatform, "android"); assert.equal(android.devPlatform, null);
  assert.equal(routeAppConfig({ git, commit: "c".repeat(40), platform: "mac" }).devPlatform, "mac");
});
