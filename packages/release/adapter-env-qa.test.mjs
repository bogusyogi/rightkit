// Two door rules for a snapshot stage's steps (rustright#190, rightkit#1301):
//  1. A carried adapter step's own environment rides the request: the door names it in RUSTRIGHT_ADAPTER_ENV, the carrier reads the forwarded
//     map back, binds it into the request core's `env` (so the signed digest covers the values) and ledgers it for the gate's own re-verification.
//  2. QA is not an adapter job: it runs an already-built artifact and compiles nothing, so on a snapshot stage it is a direct, gate-run step
//     (its GUI session and unscrubbed environment), and a QA step whose argv would build is refused.
// The broker is the fixture (test-support/fake-broker.mjs); no Cargo, no real broker.
import "./test-support/broker-guard.mjs";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { ADAPTER_ENV_RESERVED, ADAPTER_ENV_UNSET, ADAPTER_ENV_VAR, adapterEnvLaunch, forwardedAdapterEnv, isReservedAdapterEnvKey } from "./adapter-env.mjs";
import { readLedger, UNSCOPED } from "./cargo-ledger.mjs";
import { BUILD_LAUNCH_REFUSED, buildLaunch } from "./build-launch.mjs";
import { SHIM_MAP_SCHEMA } from "./cargo-target-shim.mjs";
import { CARRIED_CARGO_MAP_ENV, installCarrier, runCarriedAdapter } from "./carried-cargo.mjs";
import { spawnSync } from "./background-process.mjs";
import { brokerSection, verifyBrokerLedger } from "./snapshot-broker.mjs";
import { requestDigestOf, snapshotRequestCore } from "./snapshot-request.mjs";
import { makeFakeBroker } from "./test-support/fake-broker.mjs";

const POSIX = process.platform === "win32" ? "the fixture client and adapter PATH are POSIX only" : false;
const GATE = "/gate/bin/rightkit-gate.mjs";
const HERE = path.dirname(fileURLToPath(import.meta.url));
const ADAPTER_RUN = path.join(HERE, "carried-adapter-run.mjs");
const ARGS = ["run", "build"];
const REGISTERED = { registeredEnv: { "rightkit-pnpm": ["APP_FLAG", "OTHER_FLAG"] } };

function host(t) {
  const fx = makeFakeBroker(t, { readOnlySnapshots: true });
  const installed = installCarrier({ base: fs.mkdtempSync(path.join(fx.root, "carrier-")), snapshotRoot: fx.snapshot, checkoutDirs: { ".": fx.checkout }, section: brokerSection(fx.sb), platform: process.platform, env: {} });
  t.after(() => installed.cleanup());
  const env = { PATH: process.env.PATH, HOME: process.env.HOME, RUSTRIGHT_CALLING_GATE: GATE, ...installed.env };
  const map = JSON.parse(fs.readFileSync(installed.env[CARRIED_CARGO_MAP_ENV], "utf8"));
  const adapter = (name, args, over = {}) => runCarriedAdapter(name, args, { cwd: fx.snapshot, env: { ...env, ...over }, stdio: "capture" }, { map, err: () => {} });
  const entries = () => readLedger(installed.ledgerDir, UNSCOPED);
  return { fx, installed, env, adapter, entries };
}

// ---- the request environment ----

test("the reserved names are the broker's: RUSTRIGHT_*, RIGHTKIT_*, CARGO*, RUST*, loader and interpreter-path variables, in any case", () => {
  for (const key of ["RUSTRIGHT_JOB_ID", "rustright_x", "RIGHTKIT_QA_TIER", "RIGHTKIT_BUILD_MODE", "CARGO_HOME", "cargo_target_dir", "RUSTFLAGS", "LD_PRELOAD", "DYLD_INSERT_LIBRARIES", "PATH", "Home", "NODE_OPTIONS", "SCCACHE_DIR", "", "A=B"]) assert.equal(isReservedAdapterEnvKey(key), true, key);
  for (const key of ["HEARDRIGHT_QA_APP_ROOT", "HEARDRIGHT_QA_KEEP_EVIDENCE", "APP_FLAG", "PYTHONHASHSEED"]) assert.equal(isReservedAdapterEnvKey(key), false, key);
});

test("the door names exactly the step's declared variables in RUSTRIGHT_ADAPTER_ENV; a step with no env gets none", () => {
  assert.deepEqual(adapterEnvLaunch(undefined), {});
  assert.deepEqual(adapterEnvLaunch({}), {});
  assert.deepEqual(adapterEnvLaunch({ OTHER_FLAG: "2", APP_FLAG: "1" }), { [ADAPTER_ENV_VAR]: "APP_FLAG,OTHER_FLAG", APP_FLAG: "1", OTHER_FLAG: "2" });
  assert.throws(() => adapterEnvLaunch({ RIGHTKIT_BUILD_MODE: "release" }), (e) => e.code === ADAPTER_ENV_RESERVED && /RIGHTKIT_BUILD_MODE/.test(e.message));
  assert.throws(() => adapterEnvLaunch({ APP_FLAG: undefined }), (e) => e.code === ADAPTER_ENV_UNSET);
  assert.throws(() => adapterEnvLaunch({ "A,B": "1" }), (e) => e.code === "RK_ADAPTER_ENV_INVALID");
  // The carrier reads back what the client would forward, with the client's own refusals.
  assert.deepEqual(forwardedAdapterEnv({ [ADAPTER_ENV_VAR]: "APP_FLAG, OTHER_FLAG", APP_FLAG: "1", OTHER_FLAG: "" }), { APP_FLAG: "1", OTHER_FLAG: "" });
  assert.deepEqual(forwardedAdapterEnv({}), {});
  assert.deepEqual(forwardedAdapterEnv({ [ADAPTER_ENV_VAR]: "" }), {});
  assert.throws(() => forwardedAdapterEnv({ [ADAPTER_ENV_VAR]: "CARGO_HOME", CARGO_HOME: "x" }), (e) => e.code === ADAPTER_ENV_RESERVED);
  assert.throws(() => forwardedAdapterEnv({ [ADAPTER_ENV_VAR]: "APP_FLAG" }), (e) => e.code === ADAPTER_ENV_UNSET);
});

test("build-launch: a carried adapter step's launch carries RUSTRIGHT_ADAPTER_ENV for exactly the env the step declares, a step with none carries no such variable, and a reserved name is refused at the door", (t) => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "rk-env-"));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  const mapFile = path.join(dir, "map.json");
  fs.writeFileSync(mapFile, JSON.stringify({ schema: SHIM_MAP_SCHEMA, mode: "broker", broker: {}, ledgerDir: path.join(dir, "ledger"), workspaces: {} }));
  const CLIENT = "/managed/agent-bin/rustright";
  const managed = { platform: "darwin", exists: (p) => p === CLIENT, which: (c) => (c === "rustright" ? [CLIENT] : []) };
  const snapshot = { ...managed, env: { PATH: "/managed/agent-bin", [CARRIED_CARGO_MAP_ENV]: mapFile } };
  const plain = { ...managed, env: { PATH: "/managed/agent-bin" } };
  // With env: carried on the snapshot route, and the client's `run` elsewhere.
  assert.deepEqual(buildLaunch("pnpm", ARGS, { ...snapshot, stepEnv: { APP_FLAG: "1" } }), { command: process.execPath, args: [ADAPTER_RUN, "rightkit-pnpm", "--", ...ARGS], env: { [ADAPTER_ENV_VAR]: "APP_FLAG", APP_FLAG: "1" } });
  assert.deepEqual(buildLaunch("pnpm", ARGS, { ...plain, stepEnv: { APP_FLAG: "1" } }), { command: CLIENT, args: ["run", "rightkit-pnpm", "--", ...ARGS], env: { [ADAPTER_ENV_VAR]: "APP_FLAG", APP_FLAG: "1" } });
  assert.equal(buildLaunch("node", ["scripts/x.mjs"], { ...snapshot, stepEnv: { APP_FLAG: "1" }, cwd: "/app" }).env[ADAPTER_ENV_VAR], "APP_FLAG");
  // The same step through `rustright run <adapter> --` keeps its env.
  assert.equal(buildLaunch("rustright", ["run", "rightkit-pnpm", "--", ...ARGS], { ...snapshot, stepEnv: { APP_FLAG: "1" } }).env[ADAPTER_ENV_VAR], "APP_FLAG");
  // No env declared: no RUSTRIGHT_ADAPTER_ENV (and no env key at all), whatever else the step's own environment holds.
  for (const stepEnv of [undefined, {}]) {
    const launch = buildLaunch("pnpm", ARGS, { ...snapshot, stepEnv });
    assert.equal(Object.hasOwn(launch, "env"), false);
    assert.equal(JSON.stringify(launch).includes(ADAPTER_ENV_VAR), false);
  }
  assert.equal(Object.hasOwn(buildLaunch("pnpm", ARGS, { ...plain, env: { ...plain.env, APP_FLAG: "ambient" } }), "env"), false, "an ambient variable is not a step's declared env");
  // A reserved name is refused at the door with the coded reason.
  for (const name of ["RIGHTKIT_BUILD_MODE", "CARGO_HOME", "NODE_OPTIONS", "RUSTRIGHT_ADAPTER_ENV"]) {
    assert.throws(() => buildLaunch("pnpm", ARGS, { ...snapshot, stepEnv: { [name]: "x" } }), (e) => e.code === BUILD_LAUNCH_REFUSED && e.reason === ADAPTER_ENV_RESERVED && e.message.includes(name), name);
  }
});

test("a declared environment is forwarded, bound into the signed request core and ledgered; the gate's own re-verification recomputes the same digest", { skip: POSIX }, async (t) => {
  const h = host(t);
  h.fx.setBehavior(REGISTERED);
  const r = h.adapter("rightkit-pnpm", ARGS, { [ADAPTER_ENV_VAR]: "APP_FLAG,OTHER_FLAG", APP_FLAG: "1", OTHER_FLAG: "two" });
  assert.equal(r.status, 0, r.stderr);
  const [job] = h.fx.adapterCalls();
  assert.deepEqual(job.forwardedEnv, { APP_FLAG: "1", OTHER_FLAG: "two" }, "the client forwarded exactly the named variables");
  assert.deepEqual(job.jobEnvNames, ["APP_FLAG", "HOME", "OTHER_FLAG", "PATH", "RUSTRIGHT_PARENT_CAPABILITY_TOKEN", "RUSTRIGHT_PARENT_REQUEST_ID"], "and the broker's job has them");
  const { entries, problems } = h.entries();
  assert.deepEqual(problems, []);
  assert.equal(entries[0].status, "verified");
  assert.deepEqual(entries[0].adapterEnv, { APP_FLAG: "1", OTHER_FLAG: "two" });
  assert.deepEqual(await h.installed.problems(h.fx.sb), [], "the gate recomputes the digest with the ledgered env and the receipt verifies");
  // The values are in the digest: another value, another name or none changes it.
  const carrier = { nonce: "n", commit: h.fx.sb.commit, snapshot: h.fx.sb.snapshot, declaredInputs: { inputs: [] }, ownership: { handle: h.fx.sb.handle } };
  const digest = (adapterEnv) => requestDigestOf(snapshotRequestCore({ argv: ARGS, workspace: h.fx.checkout, carrier, adapter: "rightkit-pnpm", adapterEnv }));
  assert.equal(snapshotRequestCore({ argv: ARGS, workspace: h.fx.checkout, carrier, adapter: "rightkit-pnpm", adapterEnv: { APP_FLAG: "1" } }).env.APP_FLAG, "1");
  assert.notEqual(digest({ APP_FLAG: "1" }), digest({ APP_FLAG: "2" }));
  assert.notEqual(digest({ APP_FLAG: "1" }), digest({ OTHER_FLAG: "1" }));
  assert.notEqual(digest({ APP_FLAG: "1" }), digest({}));
  assert.equal(digest({}), requestDigestOf(snapshotRequestCore({ argv: ARGS, workspace: h.fx.checkout, carrier, adapter: "rightkit-pnpm" })), "no env is the empty object");
  // A cargo request carries no request environment whatever is passed.
  assert.deepEqual(snapshotRequestCore({ argv: ["build"], workspace: "/w", carrier, adapterEnv: { X: "1" } }).env, {});
  // A ledger that claims another environment than the one the broker signed fails the gate's verification; so does a broker that signed another one.
  const forged = entries.map((e) => ({ ...e, adapterEnv: { APP_FLAG: "9", OTHER_FLAG: "two" } }));
  assert.match((await verifyBrokerLedger(forged, h.fx.sb)).join("\n"), /the gate's own verification of the broker's receipt failed/);
  const h2 = host(t);
  h2.fx.setBehavior({ ...REGISTERED, tamper: "adapterEnvChanged" });
  const r2 = h2.adapter("rightkit-pnpm", ARGS, { [ADAPTER_ENV_VAR]: "APP_FLAG", APP_FLAG: "1" });
  assert.notEqual(r2.status, 0);
  assert.match(r2.stderr, /RK_ADAPTER_BROKER_UNVERIFIED/);
});

test("a step with no env sends an empty request environment: nothing is forwarded, whatever else the caller's environment holds", { skip: POSIX }, async (t) => {
  const h = host(t);
  h.fx.setBehavior(REGISTERED);
  const r = h.adapter("rightkit-pnpm", ARGS, { APP_FLAG: "ambient", HEARDRIGHT_QA_APP_ROOT: "/x" });
  assert.equal(r.status, 0, r.stderr);
  assert.deepEqual(h.fx.adapterCalls()[0].forwardedEnv, {});
  assert.deepEqual(h.entries().entries[0].adapterEnv, {});
  assert.deepEqual(await h.installed.problems(h.fx.sb), []);
});

test("a reserved or an unset declared name is refused before any request is made (a ledgered refusal), and the broker refuses a name its registration does not list", { skip: POSIX }, (t) => {
  for (const [over, code] of [
    [{ [ADAPTER_ENV_VAR]: "CARGO_HOME", CARGO_HOME: "/x" }, ADAPTER_ENV_RESERVED],
    [{ [ADAPTER_ENV_VAR]: "RIGHTKIT_QA_TIER", RIGHTKIT_QA_TIER: "fast" }, ADAPTER_ENV_RESERVED],
    [{ [ADAPTER_ENV_VAR]: "PATH" }, ADAPTER_ENV_RESERVED],
    [{ [ADAPTER_ENV_VAR]: "APP_FLAG" }, ADAPTER_ENV_UNSET],
    [{ [ADAPTER_ENV_VAR]: "APP_FLAG,OTHER_FLAG", APP_FLAG: "1" }, ADAPTER_ENV_UNSET],
  ]) {
    const h = host(t);
    h.fx.setBehavior(REGISTERED);
    const r = h.adapter("rightkit-pnpm", ARGS, over);
    assert.notEqual(r.status, 0, code);
    assert.equal(r.refusal.code, code);
    assert.match(r.stderr, new RegExp(code));
    assert.equal(h.fx.adapterCalls().length, 0, "no request reached the broker");
    assert.equal(h.entries().entries[0].status, "refused");
  }
  // A name the registration does not list: the broker's check_environment refuses it, so the call has no verified receipt.
  const h = host(t);
  h.fx.setBehavior({ registeredEnv: { "rightkit-pnpm": ["APP_FLAG"] } });
  const r = h.adapter("rightkit-pnpm", ARGS, { [ADAPTER_ENV_VAR]: "OTHER_FLAG", OTHER_FLAG: "1" });
  assert.notEqual(r.status, 0);
  assert.equal(r.outputDir, undefined, "no verified receipt");
  assert.match(r.stderr, /RK_ADAPTER_ENV_DENIED/);
  assert.equal(h.fx.adapterCalls().length, 0);
});

// ---- a terminal receipt that is not SUCCEEDED ----

test("a tracked write refused at exit (`snapshot exit bind: ...`) is a terminal FAILED receipt and fails the stage, like any status but SUCCEEDED", { skip: POSIX }, async (t) => {
  const h = host(t);
  h.fx.setBehavior({ tamper: "exitBindRefused" });
  const r = h.adapter("rightkit-pnpm", ARGS);
  assert.notEqual(r.status, 0);
  assert.match(r.stderr, /RK_ADAPTER_BROKER_UNVERIFIED.*the broker signed FAILED \(exit 1; snapshot exit bind: tracked path src\/main\.rs was written during the job\), not SUCCEEDED with exit 0/);
  assert.doesNotMatch(r.stderr, /changed during execution/);
  const { entries, problems } = h.entries();
  assert.equal(entries[0].status, "unverified");
  assert.equal(entries[0].code, "result");
  assert.ok(problems.length >= 1, "the stage's ledger reports the call");
  assert.equal(r.outputDir, undefined, "an unverified call names no output directory");
  // Every other terminal status is the same failure, with or without a message.
  for (const tamper of ["failed", "noResult"]) {
    const g = host(t);
    g.fx.setBehavior({ tamper });
    assert.notEqual(g.adapter("rightkit-pnpm", ARGS).status, 0, tamper);
    assert.equal(g.entries().entries[0].status, "unverified", tamper);
  }
});

// QA on the snapshot route (a direct step, the carried build of a run-tests step, the path rules): qa-snapshot.test.mjs.
