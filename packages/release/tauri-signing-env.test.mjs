// GB-06 (BROKER-CONTRACT section 16, rustright test installed_helpers_and_signing_variables_are_not_admitted_through_the_generic_routes): signing material never rides an adapter request.
// RustRight reserves TAURI_SIGNING_* (even the empty value that suppresses signing), TAURI_PRIVATE_KEY and TAURI_KEY_PASSWORD: they cannot be registered under an adapter's `env` and a request
// naming one is refused. So the gate never forwards them to an adapter or to the client that carries the call, planning refuses a step that declares one with a value, and a carried Tauri build
// runs unsigned (the updater seal and its read-back stay in the provisioned signer session). Other Tauri variables are not signing material.
import "./test-support/broker-guard.mjs";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { ADAPTER_ENV_RESERVED, ADAPTER_ENV_SIGNING, adapterEnvLaunch, forwardedAdapterEnv, isReservedAdapterEnvKey, isSigningEnvKey, withoutSigningEnv } from "./adapter-env.mjs";
import { BUILD_LAUNCH_REFUSED, buildLaunch } from "./build-launch.mjs";
import { brokerSection, carryBrokerAdapter, carryBrokerCargo } from "./snapshot-broker.mjs";
import { OWNERSHIP_ENV, OWNERSHIP_FILE_ENV } from "./build-ownership.mjs";
import { startRealBroker } from "./test-support/real-broker.mjs";

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

const WORKSPACE = { "Cargo.toml": '[workspace]\nmembers = ["crates/a"]\n', "crates/a/Cargo.toml": '[package]\nname = "a"\nversion = "0.1.0"\n', "crates/a/src/lib.rs": "\n" };
const writeTree = (root, files) => { for (const [rel, body] of Object.entries(files)) { const f = path.join(root, rel); fs.mkdirSync(path.dirname(f), { recursive: true }); fs.writeFileSync(f, body); } };
const filesUnder = (dir) => (fs.existsSync(dir) ? fs.readdirSync(dir, { withFileTypes: true }).flatMap((e) => (e.isDirectory() ? filesUnder(path.join(dir, e.name)) : [path.join(dir, e.name)])) : []);

test("[GB-06] the real client of a carried call never receives signing material from the caller's environment: the real broker names none, and no secret lands in its carriers, receipts or the outcome", { skip: POSIX }, async (t) => {
  const broker = await startRealBroker({ name: "tauri-signing-env" });
  t.after(() => broker.stop());
  const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "rk-tauri-signing-")));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const checkout = path.join(root, "checkout"); const snapshotDir = path.join(broker.dataRoot, "snapshots", "0123456789abcdef");
  writeTree(checkout, WORKSPACE); writeTree(snapshotDir, WORKSPACE);
  const snapshot = fs.realpathSync.native(snapshotDir);
  const handle = "own-7";
  const ownership = path.join(root, "build-ownership", "owner.json");
  fs.mkdirSync(path.dirname(ownership), { recursive: true });
  fs.writeFileSync(ownership, JSON.stringify({ handle, broker: { submitted: false } }));
  const commit = { sha: "c".repeat(40), tree: "e".repeat(40) };
  // The pins are placeholders: this test judges what the client is handed, never a receipt (the outcome is whatever the real broker makes of an unenrolled snapshot).
  const section = { client: broker.clientPath, controlRoot: broker.controlRoot, commit, snapshot: { path: snapshot, expectedTree: commit.tree }, handle, checkout, pins: { host: "host-1", keyId: "0".repeat(64), publicKey: "04".padEnd(130, "0"), brokerDigest: "b".repeat(64) }, outputRoots: [path.join(broker.dataRoot, "rightkit-build-control", "ws")], platform: process.platform, sentinel: null };
  const env = { PATH: process.env.PATH, ...broker.env, RUSTRIGHT_CALLING_GATE: "/gate/bin/rightkit-gate.mjs", TAURI_SIGNING_PRIVATE_KEY: SECRET, TAURI_SIGNING_PRIVATE_KEY_PASSWORD: "", TAURI_PRIVATE_KEY: SECRET, TAURI_KEY_PASSWORD: "pw", TAURI_DEBUG: "1", [OWNERSHIP_ENV]: handle, [OWNERSHIP_FILE_ENV]: ownership };
  const cargo = carryBrokerCargo({ words: ["check"], cwd: path.join(snapshot, "crates", "a"), env, section, workspace: snapshot, checkoutWorkspace: checkout, record: () => {}, err: () => {}, stdio: "capture" });
  const adapter = carryBrokerAdapter({ adapter: "rightkit-pnpm", words: ["run", "build"], cwd: snapshot, env, section, record: () => {}, err: () => {}, stdio: "capture" });
  for (const [name, call] of [["cargo", cargo], ["adapter", adapter]]) {
    const seen = JSON.stringify(call.outcome ?? call);
    assert.ok(!seen.includes(SECRET) && !seen.includes("pw\""), `${name}: no secret in the outcome`);
    assert.doesNotMatch(seen, /TAURI_(SIGNING|PRIVATE|KEY)/, `${name}: the real broker was never handed a signing variable to refuse`);
  }
  // Whatever the real broker wrote for these two requests (carriers, receipts, diagnostics) holds no signing value.
  for (const dir of [broker.controlRoot, broker.receiptsDir]) for (const file of filesUnder(dir)) {
    const body = fs.readFileSync(file, "utf8");
    assert.ok(!body.includes(SECRET), `${file} holds no signing secret`);
    assert.doesNotMatch(body, /TAURI_(SIGNING_PRIVATE_KEY|PRIVATE_KEY|KEY_PASSWORD)/, `${file} names no signing variable`);
  }
});
