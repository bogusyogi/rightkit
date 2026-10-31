// GB-06 (BROKER-CONTRACT section 16, rustright test installed_helpers_and_signing_variables_are_not_admitted_through_the_generic_routes): signing material never rides an adapter request.
// RustRight reserves TAURI_SIGNING_* (even the empty value that suppresses signing), TAURI_PRIVATE_KEY and TAURI_KEY_PASSWORD: they cannot be registered under an adapter's `env` and a request
// naming one is refused. So the gate never forwards them to an adapter or to the client that carries the call, planning refuses a step that declares one with a value, and a carried Tauri build
// runs unsigned (the updater seal and its read-back stay in the provisioned signer session). Other Tauri variables are not signing material.
import "./test-support/broker-guard.mjs";
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import test from "node:test";
import { ADAPTER_ENV_RESERVED, ADAPTER_ENV_SIGNING, adapterEnvLaunch, forwardedAdapterEnv, isReservedAdapterEnvKey, isSigningEnvKey, withoutSigningEnv } from "./adapter-env.mjs";
import { BUILD_LAUNCH_REFUSED, buildLaunch } from "./build-launch.mjs";
import { brokerSection, carryBrokerAdapter, carryBrokerCargo } from "./snapshot-broker.mjs";
import { makeFakeBroker } from "./test-support/fake-broker.mjs";

const POSIX = process.platform === "win32" ? "the fixture client is POSIX only" : false;
const SECRET = "dW50cnVzdGVkIGNvbW1lbnQ6IHNlY3JldCBrZXk=";
const RESERVED = ["TAURI_SIGNING_PRIVATE_KEY", "TAURI_SIGNING_PRIVATE_KEY_PASSWORD", "tauri_signing_private_key", "TAURI_SIGNING_X", "TAURI_PRIVATE_KEY", "TAURI_KEY_PASSWORD"];

test("[GB-06] the signing variables are reserved exactly as RustRight reserves them, whatever the case; other Tauri variables are not", () => {
  for (const key of RESERVED) { assert.equal(isSigningEnvKey(key), true, key); assert.equal(isReservedAdapterEnvKey(key), true, key); }
  for (const key of ["TAURI_DEBUG", "TAURI_CONFIG", "TAURI_ENV_PLATFORM", "TAURI", "MY_TAURI_SIGNING_KEY", "TAURI_SIGN"]) { assert.equal(isSigningEnvKey(key), false, key); assert.equal(isReservedAdapterEnvKey(key), false, key); }
  assert.deepEqual(withoutSigningEnv({ PATH: "/bin", TAURI_SIGNING_PRIVATE_KEY: SECRET, tauri_key_password: "x", TAURI_DEBUG: "1" }), { PATH: "/bin", TAURI_DEBUG: "1" });
});

test("[GB-06] planning: a step that declares signing material is refused naming the variable and never the value; the recipe's empty suppressors carry nothing and are not forwarded", () => {
  for (const key of ["TAURI_SIGNING_PRIVATE_KEY", "TAURI_PRIVATE_KEY", "TAURI_KEY_PASSWORD"]) {
    assert.throws(() => adapterEnvLaunch({ [key]: SECRET }, { registered: [] }), (e) => e.code === ADAPTER_ENV_SIGNING && e.message.includes(key) && !e.message.includes(SECRET), key);
  }
  assert.throws(() => adapterEnvLaunch({ TAURI_SIGNING_PRIVATE_KEY: "", TAURI_KEY_PASSWORD: "x" }, { registered: [] }), (e) => e.code === ADAPTER_ENV_SIGNING && /TAURI_KEY_PASSWORD/.test(e.message) && !/TAURI_SIGNING_PRIVATE_KEY"/.test(e.message), "one with a value among empty ones still refuses");
  // The quiet suppressors (empty) and a registered knob: only the knob is forwarded; the dropped names are reported, not hidden.
  const launch = adapterEnvLaunch({ TAURI_SIGNING_PRIVATE_KEY: "", TAURI_SIGNING_PRIVATE_KEY_PASSWORD: "", RK_ISOLATED_CHECKOUT: "1" }, { registered: ["RK_ISOLATED_CHECKOUT"] });
  assert.deepEqual(launch, { RUSTRIGHT_ADAPTER_ENV: "RK_ISOLATED_CHECKOUT", RK_ISOLATED_CHECKOUT: "1" });
  assert.deepEqual(launch.signingEnvDropped, ["TAURI_SIGNING_PRIVATE_KEY", "TAURI_SIGNING_PRIVATE_KEY_PASSWORD"]);
  assert.deepEqual(adapterEnvLaunch({ TAURI_SIGNING_PRIVATE_KEY: "" }, { registered: [] }), {}, "nothing left to forward: no RUSTRIGHT_ADAPTER_ENV at all");
  assert.equal(adapterEnvLaunch({ TAURI_SIGNING_PRIVATE_KEY: "" }, { registered: [] }).signingEnvDropped[0], "TAURI_SIGNING_PRIVATE_KEY");
});

test("[GB-06] a request environment that names signing material is refused before anything is sent: RUSTRIGHT_ADAPTER_ENV never names it", () => {
  for (const key of RESERVED) assert.throws(() => forwardedAdapterEnv({ RUSTRIGHT_ADAPTER_ENV: key, [key]: SECRET }), (e) => e.code === ADAPTER_ENV_SIGNING && !e.message.includes(SECRET), key);
  assert.throws(() => forwardedAdapterEnv({ RUSTRIGHT_ADAPTER_ENV: "CARGO_HOME", CARGO_HOME: "/x" }), (e) => e.code === ADAPTER_ENV_RESERVED);
  assert.deepEqual(forwardedAdapterEnv({ RUSTRIGHT_ADAPTER_ENV: "RK_ISOLATED_CHECKOUT", RK_ISOLATED_CHECKOUT: "1", TAURI_SIGNING_PRIVATE_KEY: SECRET }), { RK_ISOLATED_CHECKOUT: "1" }, "an ambient signing variable the request does not name is not forwarded either");
});

test("[GB-06] through the door: a carried step with signing material refuses (BUILD_LAUNCH_REFUSED), one with the recipe's empty suppressors is launched without them and says which it dropped", () => {
  const broker = { env: { PATH: "/managed/agent-bin" }, platform: "darwin", exists: (p) => p === "/managed/agent-bin/rustright", which: () => ["/managed/agent-bin/rustright"], readFile: () => "", cwd: "/app" };
  assert.throws(() => buildLaunch("pnpm", ["run", "build"], { ...broker, stepEnv: { TAURI_SIGNING_PRIVATE_KEY: SECRET } }), (e) => e.code === BUILD_LAUNCH_REFUSED && e.reason === ADAPTER_ENV_SIGNING && !e.message.includes(SECRET));
  const quiet = buildLaunch("pnpm", ["run", "build"], { ...broker, stepEnv: { TAURI_SIGNING_PRIVATE_KEY: "", TAURI_SIGNING_PRIVATE_KEY_PASSWORD: "" } });
  assert.equal(quiet.env, undefined, "no request environment");
  assert.deepEqual(quiet.signingEnvDropped, ["TAURI_SIGNING_PRIVATE_KEY", "TAURI_SIGNING_PRIVATE_KEY_PASSWORD"]);
  assert.deepEqual(quiet.args.slice(0, 3), ["run", "rightkit-pnpm", "--"]);
});

test("[GB-06] the client of a carried call never receives signing material from the caller's environment, cargo or adapter; the request carries none, and the other variables are untouched", { skip: POSIX }, (t) => {
  const fx = makeFakeBroker(t);
  const env = { PATH: process.env.PATH, RUSTRIGHT_CALLING_GATE: "/gate/bin/rightkit-gate.mjs", TAURI_SIGNING_PRIVATE_KEY: SECRET, TAURI_SIGNING_PRIVATE_KEY_PASSWORD: "", TAURI_PRIVATE_KEY: SECRET, TAURI_KEY_PASSWORD: "pw", TAURI_DEBUG: "1", ...fx.ownershipEnv };
  const root = fs.realpathSync.native(fx.snapshot);
  const cargo = carryBrokerCargo({ words: ["check"], cwd: path.join(root, "crates", "a"), env, section: brokerSection(fx.sb), workspace: root, checkoutWorkspace: fx.checkout, record: () => {}, err: () => {}, stdio: "capture" });
  assert.equal(cargo.outcome.status, "verified", JSON.stringify(cargo.outcome));
  const adapter = carryBrokerAdapter({ adapter: "rightkit-pnpm", words: ["run", "build"], cwd: fx.snapshot, env, section: brokerSection(fx.sb), record: () => {}, err: () => {}, stdio: "capture" });
  assert.equal(adapter.outcome.status, "verified", JSON.stringify(adapter.outcome));
  for (const call of [fx.calls().at(-1), fx.adapterCalls().at(-1)]) {
    assert.deepEqual(call.signingEnvNames, [], "the client process held no signing variable");
    assert.equal(call.debugEnvSeen, true, "TAURI_DEBUG is not signing material and still arrives");
  }
  assert.deepEqual(fx.adapterCalls().at(-1).forwardedEnvNames ?? [], [], "the adapter request carries no environment");
});
