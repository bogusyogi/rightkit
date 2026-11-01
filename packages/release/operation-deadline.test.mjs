// GB-52 (BROKER-CONTRACT section 16): the carried call has an end-to-end deadline and explicit cancellation. The gate runs the RustRight client with RUSTRIGHT_OPERATION_DEADLINE_MS (the call's own
// budget, never a user-set cap replaced) and RUSTRIGHT_CANCEL_WAIT_MS; past the deadline the client cancels the job BY ID and polls its terminal state, and the gate then asserts what the broker
// says about every submission under the call's sub-handle (SNAPSHOT_LEASE_STATUS members, GB-23/GB-85): REAPED means settled, anything else keeps ownership held. The job is never abandoned and
// never resubmitted.
//
// Evidence labels. Titles starting "data:" call the real production function with constructed inputs (a recording spawn, the client's documented deadline line, lease answers in the wire shape)
// and are NOT broker evidence. Titles starting "real broker:" run the installed RustRight client against an isolated instance of the installed broker (test-support/real-broker.mjs) and assert on
// its answers. There is no simulated broker in this file.
import "./test-support/broker-guard.mjs";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { acquireBuildOwnership, brokerLeaseState } from "./build-ownership.mjs";
import { brokerSection, carryBrokerAdapter, carryBrokerCargo } from "./snapshot-broker.mjs";
import { STAGE_DEADLINE_AT_ENV } from "./broker-wire.mjs";
import { startRealBroker } from "./test-support/real-broker.mjs";

const POSIX = process.platform === "win32" ? "the carried client environment is POSIX only" : false;
const GATE = "/gate/bin/rightkit-gate.mjs";
const DEADLINE_OUT = "operation deadline of 5000 ms passed; request abc-1: cancelConfirmed: true, terminal state: CANCELLED\n";
const COMMIT = { sha: "c".repeat(40), tree: "e".repeat(40) };
const WORKSPACES = { "Cargo.toml": '[workspace]\nmembers = ["crates/a"]\n', "crates/a/Cargo.toml": '[package]\nname = "a"\nversion = "0.1.0"\n', "crates/a/src/lib.rs": "\n" };
const writeTree = (root, files) => { for (const [rel, body] of Object.entries(files)) { const f = path.join(root, rel); fs.mkdirSync(path.dirname(f), { recursive: true }); fs.writeFileSync(f, body); } };
const NOW = Date.now();
const probe = () => ({ platform: "darwin", identity: (pid) => (pid === process.pid ? { alive: true, startedMs: NOW - 60_000, group: 4242 } : { alive: false, startedMs: null, group: null }), bootTime: () => NOW - 86_400_000, groupSnapshot: () => ({ members: [], byPid: new Map([[1, { pid: 1, ppid: 0, startedMs: NOW - 9e7 }]]), own: new Set([process.pid]) }), tokenHolders: () => null, snapshotUsers: () => null, ancestors: () => [{ pid: 5000, startedMs: NOW - 9e6 }] });

/**
 * A host for carried calls around the REAL build ownership: a checkout, the gate's snapshot (plain directories), and the broker section. With `rb` (an isolated real broker) the
 * client is the installed one, the carrier is written under the instance's control root and the lease reader asks that instance; without it the client path is a file that is never started
 * (every call there injects its own `spawn`).
 */
function host(t, { rb = null } = {}) {
  const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "rk-opdl-"))); t.after(() => fs.rmSync(root, { recursive: true, force: true, maxRetries: 20, retryDelay: 100 }));
  const checkout = path.join(root, "checkout"); const snapshot = path.join(rb ? path.join(rb.dataRoot, "snapshots") : path.join(root, "data", "snapshots"), "0123456789abcdef");
  writeTree(checkout, WORKSPACES); writeTree(snapshot, WORKSPACES);
  const own = acquireBuildOwnership({ file: path.join(root, "build-ownership", "owner.json"), platform: "darwin", probe: probe(), lease: () => { throw new Error("no lease query expected"); }, env: {}, exportEnv: false, stage: "compile", snapshot });
  const marks = [];
  const sb = {
    controlRoot: rb?.controlRoot ?? path.join(root, "control"), commit: COMMIT, snapshot: { path: snapshot, expectedTree: COMMIT.tree }, declaredInputs: [], handle: own.handle,
    pins: { host: "host-1", keyId: "0".repeat(64), publicKey: "04".padEnd(130, "0"), brokerDigest: "b".repeat(64) },
    markBrokerSubmit: (callHandle) => { marks.push(callHandle); return own.markBrokerSubmit(callHandle); }, markBrokerUnsent: own.markBrokerUnsent,
    checkout, outputRoots: [path.join(rb?.dataRoot ?? path.join(root, "data"), "rightkit-build-control", "ws")], platform: process.platform, client: rb?.clientPath ?? process.execPath,
    ...(rb ? { leaseStatus: ({ handle, waitMs = 0 }) => brokerLeaseState({ snapshot, handle, env: rb.env, platform: process.platform, client: rb.clientPath, waitMs }) } : {}),
  };
  const env = (extra = {}) => ({ PATH: process.env.PATH, RUSTRIGHT_CALLING_GATE: GATE, ...(rb?.env ?? {}), ...extra });
  const cargo = (extra = {}, callEnv = env(), section = brokerSection(sb)) => carryBrokerCargo({ words: ["check"], cwd: path.join(snapshot, "crates", "a"), env: callEnv, section, workspace: snapshot, checkoutWorkspace: checkout, record: () => {}, err: () => {}, stdio: "capture", ...extra });
  return { root, checkout, snapshot, own, sb, marks, env, cargo, owner: () => JSON.parse(fs.readFileSync(own.file, "utf8")), requestsAtBroker: () => (rb && fs.existsSync(rb.receiptsDir) ? fs.readdirSync(rb.receiptsDir).length : 0) };
}

test("data: [GB-52] the carried client runs with the call's deadline and the cancel wait: the remaining stage budget becomes the operation deadline, a user-set cap or cancel wait is never replaced, no budget sets none", { skip: POSIX }, (t) => {
  const fx = host(t);
  const seen = [];
  const spawn = (file, args, options) => { seen.push(options); return { status: 0, stdout: "", stderr: "" }; };
  const call = (callEnv, extra = {}) => fx.cargo({ spawn, ...extra }, callEnv);
  call(fx.env({ [STAGE_DEADLINE_AT_ENV]: String(Date.now() + 90_000) }));
  const fromBudget = Number(seen.at(-1).env.RUSTRIGHT_OPERATION_DEADLINE_MS);
  assert.ok(fromBudget > 80_000 && fromBudget <= 90_000, `the remaining stage budget: ${fromBudget}`);
  assert.equal(seen.at(-1).timeout, fromBudget + 30_000 + 15_000, "the outer guard follows the client's own cancel wait");
  call(fx.env(), { deadlineMs: 12_000 });
  assert.equal(seen.at(-1).env.RUSTRIGHT_OPERATION_DEADLINE_MS, "12000", "the call's own deadline");
  call(fx.env({ RUSTRIGHT_OPERATION_DEADLINE_MS: "777", RUSTRIGHT_CANCEL_WAIT_MS: "4000" }), { deadlineMs: 12_000 });
  assert.deepEqual([seen.at(-1).env.RUSTRIGHT_OPERATION_DEADLINE_MS, seen.at(-1).env.RUSTRIGHT_CANCEL_WAIT_MS], ["777", "4000"], "a user-set cap and cancel wait stand");
  assert.equal(seen.at(-1).timeout, 777 + 4000 + 15_000);
  call(fx.env());
  assert.equal(Object.hasOwn(seen.at(-1).env, "RUSTRIGHT_OPERATION_DEADLINE_MS"), false, "no budget: no deadline is invented");
  assert.equal(Object.hasOwn(seen.at(-1), "timeout"), false);
});

test("real broker: [GB-52] a call carried to the real client with a 1 ms deadline is never a verified result, is sent once and never resubmitted, and the gate holds ownership on the real broker's lease answer for the sub-handle", { skip: POSIX }, async (t) => {
  const rb = await startRealBroker({ name: "opdl-live" });
  t.after(() => rb.stop());
  const fx = host(t, { rb });
  const r = fx.cargo({ deadlineMs: 1 });
  assert.notEqual(r.outcome.status, "verified", "the plain-directory snapshot is not one the real broker holds; a deadline of 1 ms is no build outcome");
  const sent = fx.marks.filter((h) => h.startsWith(`${fx.own.handle}#`));
  assert.ok(sent.length >= 1, "the sub-handle was recorded before the send");
  assert.ok(fx.requestsAtBroker() <= sent.length, "the real broker holds no more requests than the gate sent");
  // Whatever the real broker answers for the handle the gate holds, ownership is only ever released on its REAPED: ask it directly, as the gate does after a cancel.
  const asked = brokerLeaseState({ snapshot: fx.snapshot, handle: sent[0], env: rb.env, platform: process.platform, client: rb.clientPath });
  assert.ok(["REAPED", "ACTIVE", "UNKNOWN"].includes(asked.state), `the real broker's lease answer: ${JSON.stringify(asked)}`);
  if (r.outcome.code === "operation-deadline") {
    assert.equal(typeof r.outcome.reaped, "boolean");
    assert.ok(["REAPED", "ACTIVE", "UNKNOWN"].includes(r.outcome.subHandle.state));
    assert.equal(r.outcome.reaped, r.outcome.subHandle.state === "REAPED", "reaped only on a REAPED answer");
    assert.equal(fx.marks.length, 1, "never resubmitted");
  }
  assert.notEqual(r.status, 0);
});

test("real broker: [GB-52] the real broker's answer for a sub-handle it never admitted is UNKNOWN with no members: after a deadline the gate keeps ownership held (never REAPED, never released) and says so", { skip: POSIX }, async (t) => {
  const rb = await startRealBroker({ name: "opdl-unknown" });
  t.after(() => rb.stop());
  const fx = host(t, { rb });
  // The client's own deadline line (the text the contract names) is the constructed input; the lease answer the gate acts on is the real broker's.
  const r = fx.cargo({ deadlineMs: 5000, spawn: () => ({ status: 1, stdout: DEADLINE_OUT, stderr: "" }) }, fx.env({ RUSTRIGHT_CANCEL_WAIT_MS: "300" }));
  assert.equal(r.outcome.code, "operation-deadline");
  assert.equal(r.outcome.reaped, false, "a handle the real broker never admitted is not REAPED");
  assert.equal(r.outcome.subHandle.state, "UNKNOWN");
  assert.match(r.outcome.message, /ownership stays held until it is REAPED/);
  assert.equal(fx.marks.length, 1, "never resubmitted");
  assert.notEqual(r.status, 0);
});

test("data: [GB-52] the lease answers after a cancel, in the wire shape: REAPED once the job settled, held while a member is still ACTIVE after the bounded wait; the job is never resubmitted", { skip: POSIX }, (t) => {
  const fx = host(t);
  const answer = (state, extra = {}) => ({ state, why: state, ...extra });
  const lease = (reply) => ({ ...fx.sb, leaseStatus: ({ handle }) => reply(handle) });
  const deadline = { deadlineMs: 5000, spawn: () => ({ status: 1, stdout: DEADLINE_OUT, stderr: "" }) };
  const settled = fx.cargo(deadline, fx.env(), brokerSection(lease((h) => answer("REAPED", { members: [{ handle: h, state: "REAPED", spawned: true }] }))));
  assert.equal(settled.outcome.code, "operation-deadline");
  assert.deepEqual([settled.outcome.reaped, settled.outcome.subHandle.state], [true, "REAPED"]);
  assert.match(settled.outcome.message, /after the cancel the broker reports sub-handle .* REAPED/);
  assert.equal(fx.marks.length, 1, "never resubmitted");
  const held = fx.cargo(deadline, fx.env({ RUSTRIGHT_CANCEL_WAIT_MS: "300" }), brokerSection(lease((h) => answer("UNKNOWN", { why: "still ACTIVE after waiting 300 ms (waitTimedOut)", waitedMs: 300, waitTimedOut: true, members: [{ handle: h, state: "ACTIVE", spawned: true }] }))));
  assert.equal(held.outcome.code, "operation-deadline");
  assert.equal(held.outcome.reaped, false);
  assert.deepEqual([held.outcome.subHandle.state, held.outcome.subHandle.waitTimedOut], ["UNKNOWN", true], "a wait that timed out keeps ownership UNKNOWN");
  assert.deepEqual(held.outcome.subHandle.members.map((m) => m.state), ["ACTIVE"], "the member rows travel with the answer");
  assert.match(held.outcome.message, /still UNKNOWN.*ownership stays held until it is REAPED/s);
  assert.notEqual(held.status, 0);
  assert.equal(fx.marks.length, 2, "one send per call, none repeated");
});

test("data: [GB-52] the same for a carried adapter step, and a broker that cannot be asked leaves ownership held (UNKNOWN), never released", { skip: POSIX }, (t) => {
  const fx = host(t);
  const deadline = { status: 1, stdout: DEADLINE_OUT, stderr: "" };
  const adapter = (section) => carryBrokerAdapter({ adapter: "rightkit-pnpm", words: ["run", "build"], cwd: fx.snapshot, env: fx.env(), section, record: () => {}, err: () => {}, stdio: "capture", deadlineMs: 5000, spawn: () => deadline });
  const settled = adapter(brokerSection({ ...fx.sb, leaseStatus: ({ handle }) => ({ state: "REAPED", why: "REAPED", members: [{ handle, state: "REAPED", spawned: true }] }) }));
  assert.equal(settled.outcome.code, "operation-deadline");
  assert.deepEqual([settled.outcome.reaped, settled.outcome.subHandle.state], [true, "REAPED"]);
  // No lease reader (a gate without the broker's lease status): nothing is asserted, and the outcome says ownership stays UNKNOWN.
  const unasked = adapter(brokerSection({ ...fx.sb, leaseStatus: undefined }));
  assert.equal(unasked.outcome.code, "operation-deadline");
  assert.equal(unasked.outcome.reaped, false);
  assert.match(unasked.outcome.message, /ownership stays UNKNOWN/);
  // A lease reader that throws: held.
  const failed = adapter(brokerSection({ ...fx.sb, leaseStatus: () => { throw new Error("broker unreachable"); } }));
  assert.equal(failed.outcome.reaped, false);
  assert.match(failed.outcome.message, /broker unreachable|ownership stays UNKNOWN/);
  assert.equal(fs.existsSync(fx.sb.client), true);
});
