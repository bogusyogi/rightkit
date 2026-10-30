import "./test-support/broker-guard.mjs";
// GATE-5 gate-side proofs (gate-vs-broker audit 2026-10-30): M31 (route refuses an ndk app on a host with no `cargo ndk` registration) and M40
// (client discovery: the installed runtime first, PATH second, in every place the gate looks for the RustRight client).
import assert from "node:assert/strict";
import test from "node:test";
import { queryBrokerCapabilities, routePlan, rustrightClientOf } from "./bin/rightkit-gate.mjs";
import { adapterRegistered, declaresNdk, externalCommandRegistered, liveBrokerConfig } from "./broker-registrations.mjs";

const HOME = "/Users/x";
const INSTALLED = `${HOME}/Library/Application Support/RustRight/runtime/rustright`;
const STALE = "/stale/agent-bin/rustright";
const NDK_APP = { config: { development: { targets: { android: { ndk: { abis: ["arm64-v8a"], jniLibs: "android/app/src/main/jniLibs" }, build: [{ cmd: "./gradlew", args: [":app:assembleDebug"] }] } } } } };
const PLAIN_APP = { config: { development: { targets: { mac: { build: [] } } } } };
const registered = { externalCommands: [{ shim: "cargo", command: "ndk", toolId: "cargo-ndk", lane: "BUILD", digest: "d" }], adapters: { "rightkit-pnpm": {} } };
const hostOf = (config) => ({
  env: { HOME }, platform: "darwin", snapshotBroker: {}, exists: () => false, spawn: () => ({ status: 1, stdout: "", stderr: "" }),
  resolveExecutor: () => ({ kind: "broker" }), readFile: () => JSON.stringify(config),
});
// Other refusals (the signing policy of a dev build on this simulated Mac) are not this test's subject: it asks only whether the ndk registration refused.
const missing = (decision) => decision.refusal?.code === "RK_BROKER_CAPABILITY_MISSING";
const plan = (o, to, appConfig) => routePlan(o, { to, stages: [], devTest: to === "dev" || to === "test", executor: { kind: "broker" }, appConfig });

test("M31: the registration reader sees externalCommands and adapters; an ndk declaration is found at any depth; an unreadable config is null", () => {
  const live = liveBrokerConfig({ env: { HOME }, platform: "darwin", readFile: () => JSON.stringify(registered) });
  assert.equal(externalCommandRegistered(live, "cargo", "ndk"), true);
  assert.equal(externalCommandRegistered({ externalCommands: [] }, "cargo", "ndk"), false);
  assert.equal(externalCommandRegistered({}, "cargo", "ndk"), false);
  assert.equal(adapterRegistered(live, "rightkit-pnpm"), true);
  assert.equal(adapterRegistered(live, "rightkit-gradle"), false);
  assert.equal(liveBrokerConfig({ env: { HOME }, platform: "darwin", readFile: () => { throw new Error("ENOENT"); } }), null);
  assert.equal(liveBrokerConfig({ env: { HOME }, platform: "darwin", readFile: () => "not json" }), null);
  assert.equal(declaresNdk(NDK_APP.config), true);
  assert.equal(declaresNdk({ targets: { android: { ndk: { jniLibs: "x" } } } }), true);
  assert.equal(declaresNdk(PLAIN_APP.config), false);
  assert.equal(declaresNdk({ notes: { ndk: "text" } }), false);
});

test("M31: the route refuses an ndk app (dev and package) on a broker host whose config registers no `cargo ndk`; a registered host, a non-ndk app and an unreadable config pass", () => {
  for (const to of ["dev", "package"]) {
    const refused = plan(hostOf({ externalCommands: [], adapters: {} }), to, NDK_APP);
    assert.equal(refused.refusal?.code, "RK_BROKER_CAPABILITY_MISSING", to);
    assert.match(refused.refusal.message, /cargo ndk/);
    assert.equal(refused.refusal.stage, to);
    assert.equal(missing(plan(hostOf(registered), to, NDK_APP)), false, `${to}: registered`);
    assert.equal(missing(plan(hostOf({ externalCommands: [] }), to, PLAIN_APP)), false, `${to}: an app with no ndk needs none`);
  }
  const unreadable = { ...hostOf({}), readFile: () => { throw new Error("ENOENT"); } };
  assert.equal(missing(plan(unreadable, "dev", NDK_APP)), false, "unverified is not a refusal: the broker still refuses an unregistered command itself");
  assert.equal(missing(plan(hostOf({ externalCommands: [] }), "check", NDK_APP)), false, "--to check compiles nothing");
  // A direct (Linux) host has no broker registration to need.
  const linux = { ...hostOf({ externalCommands: [] }), platform: "linux" };
  assert.equal(missing(routePlan(linux, { to: "dev", stages: [], devTest: true, executor: { kind: "direct" }, appConfig: NDK_APP })), false);
});

test("M40: the installed runtime's client wins over a PATH rustright; PATH is the fallback; neither is a clear failure", () => {
  const which = () => [STALE];
  const withInstalled = { env: { HOME }, platform: "darwin", exists: (p) => p === INSTALLED };
  assert.equal(rustrightClientOf(withInstalled, which), INSTALLED);
  assert.equal(rustrightClientOf({ ...withInstalled, exists: () => false }, which), STALE, "no installed runtime: PATH");
  assert.equal(rustrightClientOf({ ...withInstalled, exists: () => false }, () => ["relative/rustright"]), undefined, "only an absolute client is used");
  assert.equal(rustrightClientOf({ env: { HOME }, platform: "linux", exists: () => true }, which), STALE, "a direct host has no installed runtime client to prefer");
});

test("M40: the capability query asks the installed runtime's client first, and names both places when it finds none", () => {
  const calls = [];
  const spawn = (cmd, args) => {
    calls.push([cmd, ...(args ?? [])]);
    if (cmd === "which" || cmd === "where") return { status: 0, stdout: `${STALE}\n`, stderr: "" };
    return { status: 0, stdout: JSON.stringify({ broker: { capabilities: ["snapshot-readonly-v1", "snapshot-adapter-v1"] } }), stderr: "" };
  };
  const o = { env: { HOME }, platform: "darwin", exists: (p) => p === INSTALLED, spawn, readFile: () => "" };
  const answer = queryBrokerCapabilities(o);
  const asked = calls.filter(([cmd]) => cmd !== "which" && cmd !== "where");
  assert.ok(asked.length >= 1);
  assert.ok(asked.every((call) => !call.includes(STALE)), `the stale PATH client was never asked: ${JSON.stringify(asked)}`);
  assert.ok(asked.some((call) => call.includes(INSTALLED)), "the installed client was asked");
  void answer;
  const none = queryBrokerCapabilities({ env: { HOME }, platform: "darwin", exists: () => false, spawn: () => ({ status: 1, stdout: "", stderr: "" }), readFile: () => "" });
  assert.equal(none.ok, false);
  assert.match(none.problem, /neither the installed runtime's nor on PATH/);
});
