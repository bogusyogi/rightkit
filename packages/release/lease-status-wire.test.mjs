// GB-23 / GB-85 (BROKER-CONTRACT section 16): SNAPSHOT_LEASE_STATUS is consumed exactly as RustRight answers it, wherever it is read (the release, the reclaim, the submission accounting)
// and as the real broker produces it (an isolated instance of the installed RustRight broker, test-support/real-broker.mjs). The answer is {type, schema, state, snapshot, ownership, members[], waitedMs, waitTimedOut}: `members` is one row per submission under the handle
// (the aggregate `state` REAPED needs at least one member and every member REAPED); `waitMs` (the CLI's --wait-ms, 0 to 30000, clamped) makes the broker answer as soon as the aggregate is
// REAPED or at the bound with the state it has; `waitedMs` is the time it waited and `waitTimedOut` is true exactly when a wait was asked and the state is not REAPED. A gate releases
// ownership only on a well-formed REAPED answer and keeps it UNKNOWN, with the reason, on waitTimedOut, on any contradiction and on any answer it cannot read.
import "./test-support/broker-guard.mjs";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { OWNERSHIP_ENV, OWNERSHIP_FILE_ENV, acquireBuildOwnership, brokerLeaseState, markCarriedSend } from "./build-ownership.mjs";
import { enrollBrokerPins } from "./broker-pins.mjs";
import { CARRIED_CARGO_MAP_ENV, installCarrier } from "./carried-cargo.mjs";
import { runCargoSync } from "./managed-cargo.mjs";
import { syncSnapshot } from "./snapshot.mjs";
import { brokerSection, createSnapshotBroker } from "./snapshot-broker.mjs";
import { spawn, spawnSync } from "./background-process.mjs";
import { startRealBroker } from "./test-support/real-broker.mjs";

const POSIX = process.platform === "win32" ? "the carried adapter PATH is POSIX only" : false;
const GATE = "/gate/bin/rightkit-gate.mjs";
const SCHEMA = "rustright.snapshot-lease-status.v1";
// ---- the real broker host: the installed RustRight broker (an isolated instance), a real git checkout, the gate's real snapshot of it, the enrolled pins of THAT broker, and the production snapshot broker over them ----
const HANDLE = "own-7";
const PLATFORM_KEY = process.platform === "win32" ? "win32" : process.platform === "darwin" ? "darwin" : "linux";
const LOCK = (name) => `version = 4\n\n[[package]]\nname = "${name}"\nversion = "0.1.0"\n`;
const BASE_FILES = {
  ".gitignore": "target/\nnode_modules/\n",
  "Cargo.toml": '[workspace]\nmembers = ["crates/a"]\nresolver = "2"\n', "Cargo.lock": LOCK("a"), "crates/a/Cargo.toml": '[package]\nname = "a"\nversion = "0.1.0"\nedition = "2021"\n', "crates/a/src/lib.rs": "\n",
  "native/Cargo.toml": '[workspace]\nmembers = []\n[package]\nname = "b"\nversion = "0.1.0"\nedition = "2021"\n', "native/Cargo.lock": LOCK("b"), "native/src/lib.rs": "\n",
  "packages/release/package.json": '{"name":"@x/release","version":"0.0.0"}\n',
};
const gitIn = (cwd, ...args) => { const r = spawnSync("git", ["-c", "user.name=t", "-c", "user.email=t@t", ...args], { cwd, encoding: "utf8" }); if (r.status !== 0) throw new Error(`git ${args.join(" ")}: ${r.stderr}`); return String(r.stdout).trim(); };
/** The enrolled pins of the broker instance `rb`: its own attestation key (public fields only), host and broker digest as its own `doctor --json` reports them. */
function instancePins(rb) {
  const key = JSON.parse(fs.readFileSync(path.join(rb.controlRoot, "attestation", "key.json"), "utf8"));
  const run = rb.run(["doctor", "--json", "--no-scan"], {});
  const doctor = JSON.parse(run.stdout);
  const find = (value, name) => { if (value && typeof value === "object") { if (typeof value[name] === "string") return value[name]; for (const inner of Object.values(value)) { const hit = find(inner, name); if (hit) return hit; } } return null; };
  const host = find(doctor, "hostName") ?? find(doctor, "host"); const brokerDigest = find(doctor, "brokerDigest");
  if (!host || !brokerDigest) throw new Error(`the isolated broker's doctor report names no host or brokerDigest to enroll: ${run.stdout.slice(0, 300)}`);
  return { publicKey: key.publicKey, keyId: key.keyId, host, brokerDigest };
}
/** `files` are committed over BASE_FILES; the snapshot is the gate's own (syncSnapshot) inside the broker's data root; `sb` is createSnapshotBroker's. */
async function realBrokerHost(t, name, files = {}) {
  const rb = await startRealBroker({ name });
  t.after(() => rb.stop());
  const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), `rk-${name}-`)));
  t.after(() => fs.rmSync(root, { recursive: true, force: true, maxRetries: 20, retryDelay: 100 }));
  const checkout = path.join(root, "checkout"); fs.mkdirSync(checkout);
  for (const [rel, body] of Object.entries({ ...BASE_FILES, ...files })) { const f = path.join(checkout, rel); fs.mkdirSync(path.dirname(f), { recursive: true }); fs.writeFileSync(f, body); }
  gitIn(checkout, "init", "-q", "-b", "main"); gitIn(checkout, "add", "-A"); gitIn(checkout, "commit", "-q", "-m", "c");
  const snap = syncSnapshot({ checkout, rev: "HEAD", root: path.join(rb.dataRoot, "snapshots"), managedRoot: rb.dataRoot });
  enrollBrokerPins({ dataRoot: rb.dataRoot, platform: PLATFORM_KEY, ...instancePins(rb) });
  const ownership = path.join(root, "build-ownership", "owner.json");
  fs.mkdirSync(path.dirname(ownership), { recursive: true });
  fs.writeFileSync(ownership, JSON.stringify({ handle: HANDLE, broker: { submitted: false } }));
  const submits = [];
  const sb = createSnapshotBroker({
    env: rb.env, platform: process.platform, checkout, commit: { sha: snap.commit, tree: snap.tree }, snapshot: { path: snap.path, expectedTree: snap.tree },
    ownership: { handle: HANDLE, markBrokerSubmit: (callHandle) => { submits.push(callHandle); return markCarriedSend({ file: ownership, handle: HANDLE, callHandle }); } },
    client: rb.clientPath, gateFile: path.join(checkout, "x", "rightkit-gate.mjs"), dataRoot: rb.dataRoot, lease: (q) => brokerLeaseState(q),
  });
  return { rb, root, checkout, snapshot: snap.path, commit: { sha: snap.commit, tree: snap.tree }, sb, submits, ownershipEnv: { [OWNERSHIP_ENV]: HANDLE, [OWNERSHIP_FILE_ENV]: ownership } };
}

/** The release's lease query over the REAL client wire (the isolated broker instance), as the production reader spawns it. */
const viaRealClient = (rb) => (_file, args) => rb.run(args, {});
const tmp = (t, p) => { const d = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), p))); t.after(() => fs.rmSync(d, { recursive: true, force: true, maxRetries: 20, retryDelay: 100 })); return d; };
/** The real client's `snapshot-lease status`, against the isolated real broker instance; the answer is its last stdout line. */
const ask = (rb, handle, wait = null, snapshot = rb.dataRoot) => {
  const r = rb.run(["snapshot-lease", "status", "--snapshot", snapshot, "--ownership", handle, ...(wait === null ? [] : ["--wait-ms", String(wait)])], {});
  return { status: r.status, answer: r.status === 0 ? JSON.parse(String(r.stdout).trim().split("\n").pop()) : null, stderr: String(r.stderr) };
};
const answerOf = (over = {}) => ({ schema: SCHEMA, state: "REAPED", snapshot: "/s", ownership: "h", members: [{ handle: "h#1", state: "REAPED", spawned: true }], waitedMs: 0, waitTimedOut: false, ...over });
const reading = (answer, { waitMs = 0 } = {}) => brokerLeaseState({ snapshot: "/s", handle: "h", client: "/c", waitMs, spawn: () => ({ status: 0, stdout: `${JSON.stringify(answer)}\n`, stderr: "" }) });

// ---- the real broker answers ----
test("[GB-85] the real broker's answer carries members, waitedMs and waitTimedOut: no wait answers at once and is not a timeout; a wait that cannot be satisfied ends at its bound with the state it has and says so", async (t) => {
  const rb = await startRealBroker({ name: "lease-status-wire" });
  t.after(() => rb.stop());
  const plain = ask(rb, "nobody").answer;
  assert.deepEqual(Object.keys(plain).sort(), ["members", "ownership", "schema", "snapshot", "state", "type", "waitTimedOut", "waitedMs"], "every field of the answer, always");
  assert.equal(plain.schema, SCHEMA);
  assert.deepEqual([plain.state, plain.waitTimedOut, Number.isInteger(plain.waitedMs) && plain.waitedMs >= 0 && plain.waitedMs < 150, plain.members], ["UNKNOWN", false, true, []], "a handle the broker never held is UNKNOWN, never REAPED");
  const started = Date.now();
  const timed = ask(rb, "nobody", 150).answer;
  assert.deepEqual([timed.state, timed.waitTimedOut, timed.waitedMs >= 150], ["UNKNOWN", true, true], JSON.stringify(timed));
  assert.ok(Date.now() - started < 10_000);
  assert.notEqual(ask(rb, "nobody", "x").status, 0, "a wait that is not a number is refused by the client");
});

// These two run a real carried compile and a real adapter job on the real broker through the production seams, which launch the installed client: they need RIGHTKIT_TEST_REAL_BROKER=1
// (the broker guard's opt-in) to be allowed to.
const SLEEP_SCRIPT = 'setTimeout(() => process.exit(0), 3000);\n';
async function leaseHost(t) {
  const fx = await realBrokerHost(t, "lease-status", { "scripts/sleep.mjs": SLEEP_SCRIPT });
  const installed = installCarrier({ base: fs.mkdtempSync(path.join(fx.root, "carrier-")), snapshotRoot: fx.snapshot, checkoutDirs: { ".": fx.checkout }, section: brokerSection(fx.sb), platform: process.platform, env: {} });
  t.after(() => installed.cleanup());
  const env = { ...fx.rb.env, PATH: process.env.PATH, HOME: process.env.HOME, RUSTRIGHT_CALLING_GATE: GATE, ...fx.ownershipEnv, ...installed.env };
  return { fx, env, installed };
}

test("[GB-85] a settled handle answers at once on the real broker, whatever the wait: REAPED, not a timeout, with the carried call's sub-handle as its member", { skip: POSIX }, async (t) => {
  const h = await leaseHost(t);
  const r = runCargoSync(["metadata", "--format-version", "1", "--no-deps"], { cwd: h.fx.snapshot, env: h.env, encoding: "utf8" }, { platform: process.platform });
  assert.equal(r.status, 0, r.stderr);
  assert.equal(h.fx.submits.length, 1);
  const reaped = ask(h.fx.rb, HANDLE, 5000, h.fx.snapshot).answer;
  assert.deepEqual([reaped.state, reaped.waitTimedOut, reaped.waitedMs < 1000, reaped.members.map((m) => m.handle)], ["REAPED", false, true, h.fx.submits], JSON.stringify(reaped));
});

test("[GB-85] the real broker's wait is the real wait: it answers as soon as the aggregate is REAPED (waitedMs below the bound, not a timeout), and at the bound with ACTIVE and waitTimedOut while a job still holds its lease", { skip: POSIX }, async (t) => {
  const h = await leaseHost(t);
  const runner = path.join(path.dirname(fileURLToPath(import.meta.url)), "carried-adapter-run.mjs");
  const job = spawn(process.execPath, [runner, "rightkit-node", "--", "scripts/sleep.mjs"], { cwd: h.fx.snapshot, env: h.env, stdio: "ignore" });
  const done = new Promise((resolve) => job.once("exit", resolve));
  let active = null;
  for (let i = 0; i < 200 && active === null; i += 1) { const a = ask(h.fx.rb, HANDLE, null, h.fx.snapshot).answer; if (a.state === "ACTIVE") active = a; else await new Promise((r) => setTimeout(r, 100)); }
  assert.ok(active, "the job's lease became ACTIVE at the broker");
  const early = ask(h.fx.rb, HANDLE, 40, h.fx.snapshot).answer;
  assert.deepEqual([early.state, early.waitTimedOut], ["ACTIVE", true], "the wait ended before the lease completed");
  assert.deepEqual([ask(h.fx.rb, HANDLE, null, h.fx.snapshot).answer.state, ask(h.fx.rb, HANDLE, null, h.fx.snapshot).answer.waitTimedOut], ["ACTIVE", false], "no wait: the state it has, not a timeout");
  const settled = ask(h.fx.rb, HANDLE, 25_000, h.fx.snapshot).answer;
  assert.deepEqual([settled.state, settled.waitTimedOut, settled.waitedMs < 25_000], ["REAPED", false, true], JSON.stringify(settled));
  assert.ok(settled.members.length >= 1 && settled.members.every((m) => m.state === "REAPED"));
  await done;
});

// ---- the gate consumes it exactly (data: the production reader on constructed answers; never broker evidence) ----
test("data: [GB-85] a well-formed answer is read whole: the aggregate state, the member rows, waitedMs and waitTimedOut:false", () => {
  const ok = reading(answerOf({ waitedMs: 37 }), { waitMs: 500 });
  assert.deepEqual([ok.state, ok.waitTimedOut, ok.waitedMs], ["REAPED", false, 37]);
  assert.deepEqual(ok.members, [{ handle: "h#1", state: "REAPED", spawned: true }]);
  const asked = [];
  brokerLeaseState({ snapshot: "/s", handle: "h", client: "/c", waitMs: 99_999, spawn: (c, a) => { asked.push(a); return { status: 0, stdout: JSON.stringify(answerOf()) }; } });
  assert.equal(asked[0].at(-1), "30000", "waitMs is clamped to the broker's 30000");
  const plain = [];
  brokerLeaseState({ snapshot: "/s", handle: "h", client: "/c", spawn: (c, a) => { plain.push(a); return { status: 0, stdout: JSON.stringify(answerOf()) }; } });
  assert.equal(plain[0].includes("--wait-ms"), false, "no wait asked: none sent");
});

test("data: [GB-85] negative: an answer that contradicts itself or lacks what the real broker always sends is UNKNOWN with the reason, never REAPED and never an ACTIVE the gate would wait on", () => {
  const cases = [
    ["waitTimedOut missing after a wait was asked", answerOf({ waitTimedOut: undefined }), /waitTimedOut/],
    ["waitedMs missing after a wait was asked", answerOf({ waitedMs: undefined }), /waitedMs/],
    ["waitTimedOut not a boolean", answerOf({ waitTimedOut: "false" }), /waitTimedOut/],
    ["waitedMs negative", answerOf({ waitedMs: -1 }), /waitedMs/],
    ["waitedMs a fraction", answerOf({ waitedMs: 1.5 }), /waitedMs/],
    ["REAPED yet waitTimedOut", answerOf({ waitTimedOut: true, waitedMs: 15000 }), /waitTimedOut|contradict/],
    ["not REAPED after a wait without waitTimedOut", answerOf({ state: "ACTIVE", members: [{ handle: "h#1", state: "ACTIVE", spawned: true }] }), /contradict|waitTimedOut/],
    ["REAPED with no member", answerOf({ members: [] }), /no member|REAPED/],
    ["REAPED with a member that is not", answerOf({ members: [{ handle: "h#1", state: "REAPED", spawned: true }, { handle: "h#2", state: "UNKNOWN", spawned: true }] }), /aggregate|REAPED/],
    ["members absent", answerOf({ members: undefined }), /members/],
  ];
  for (const [name, answer, why] of cases) {
    const got = reading(answer, { waitMs: 500 });
    assert.equal(got.state, "UNKNOWN", name);
    assert.match(got.why, why, name);
  }
  // Without a wait the broker still sends the fields; a plain query that lacks them is read for what it is (the aggregate and the members), but a malformed one is refused all the same.
  assert.equal(reading(answerOf({ waitTimedOut: "no" })).state, "UNKNOWN");
  assert.equal(reading(answerOf({ waitedMs: "5" })).state, "UNKNOWN");
});

test("data: [GB-85] a timeout reports how long the broker waited and keeps the state it had, and never reads as success", () => {
  const got = reading(answerOf({ state: "ACTIVE", waitTimedOut: true, waitedMs: 15000, members: [{ handle: "h#1", state: "ACTIVE", spawned: true }] }), { waitMs: 15_000 });
  assert.deepEqual([got.state, got.waitTimedOut, got.waitedMs], ["UNKNOWN", true, 15000]);
  assert.match(got.why, /after waiting 15000 ms/);
  assert.deepEqual(got.members.map((m) => m.state), ["ACTIVE"], "the member rows travel with the timeout");
});

// ---- release, reclaim and accounting ----
const NOW = Date.now();
const probe = () => ({ platform: "darwin", identity: (pid) => (pid === process.pid ? { alive: true, startedMs: NOW - 60_000, group: 4242 } : { alive: false, startedMs: null, group: null }), bootTime: () => NOW - 86_400_000, groupSnapshot: () => ({ members: [], byPid: new Map([[1, { pid: 1, ppid: 0, startedMs: NOW - 9e7 }]]), own: new Set([process.pid]) }), tokenHolders: () => null, snapshotUsers: () => null, ancestors: () => [] });
const acquire = (file, lease, extra = {}) => acquireBuildOwnership({ file, platform: "darwin", probe: probe(), lease, env: {}, exportEnv: false, stage: "compile", snapshot: "/snap/x", ...extra });

test("[GB-85] a stage's release asks the real broker once with its bounded wait, over the real client wire, and keeps ownership when the broker does not answer REAPED for the recorded sub-handle", async (t) => {
  const rb = await startRealBroker({ name: "lease-status-release" });
  t.after(() => rb.stop());
  const snapshot = fs.realpathSync(tmp(t, "rk-lease-snap-"));
  const file = path.join(tmp(t, "rk-lease-wire-"), "build-ownership", "owner.json");
  const own = acquire(file, (q) => brokerLeaseState({ ...q, env: rb.env, platform: process.platform, client: rb.clientPath, waitMs: 200, spawn: viaRealClient(rb) }), { snapshot });
  const sub = `${own.handle}#a`; own.markBrokerSubmit(sub);
  const released = own.release();
  assert.equal(released.released, false, "this broker never held that sub-handle: UNKNOWN, so ownership is kept, never released on a guess");
});

test("data: [GB-23] the release reconciles each recorded sub-handle against the members of the answer: a bare REAPED answer whose own member row for a recorded sub-handle is not REAPED keeps ownership", (t) => {
  const row = (handle, state) => ({ handle, state, spawned: true });
  const owned = (bare) => {
    const own = acquire(path.join(tmp(t, "rk-lease-members-"), "build-ownership", "owner.json"), (q) => (q.handle.includes("#") ? { state: "REAPED", why: "ok", members: [row(q.handle, "REAPED")] } : { state: "REAPED", why: "ok", members: bare(q.handle) }), { snapshot: "/snap/x" });
    own.markBrokerSubmit(`${own.handle}#a`); own.markBrokerSubmit(`${own.handle}#b`);
    return own;
  };
  // The aggregate said REAPED, but the answer's own row for #b is ACTIVE: contradictory, kept.
  const lying = owned((h) => [row(`${h}#a`, "REAPED"), row(`${h}#b`, "ACTIVE")]).release();
  assert.equal(lying.released, false);
  assert.match(String(lying.why), /ACTIVE|member/);
  // Every recorded sub-handle is a REAPED member of the answer: released.
  assert.equal(owned((h) => [row(`${h}#a`, "REAPED"), row(`${h}#b`, "REAPED")]).release().released, true);
});
