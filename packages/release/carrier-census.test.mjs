// GB-76 (BROKER-CONTRACT section 16): for each per-call carrier `<control root>/snapshot-requests/<name>.json` the gate FIRST writes `<name>.json.owner.json`
// ({"schema":"rustright.carrier-owner/1","gate":{"pid","startTimeMs"},"nonce","handle","createdAtMs"}: its own process identity as RustRight reads it, the call's attestation nonce and
// ownership handle, the creation time in UNIX epoch milliseconds), then the carrier, and removes both in its `finally`. The broker's maintenance pass retires a carrier only when its owner
// record parses, its gate is gone or reused, and the broker holds no active or unknown submission under its handle; age alone never retires anything. The gate's own sweep follows the same rules
// over the same wire: nothing is deleted without proof, every entry is accounted for, and removal is compare-token.
import "./test-support/broker-guard.mjs";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { CARRIER_OWNER_SCHEMA, FINISHED_CARRIER_RETENTION_MS, ownerFileOf, removeCarrier, sweepSnapshotRequests, writeCarrierOwner } from "./carrier-census.mjs";
import { darwinStartTimeMs, linuxStartTimeMs, processStartTimeMs } from "./lock-identity.mjs";
import { writeSnapshotRequest } from "./snapshot-request.mjs";

const NOW = Date.now();
const tmp = (t) => { const d = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "rk-carrier-census-"))); t.after(() => fs.rmSync(d, { recursive: true, force: true })); return d; };
// Process identity as the broker reads it: a live process has a start time, a pid with no process is gone, a start time that is not the recorded one is a reused pid.
const identity = { probe: (pid) => (pid === process.pid ? { alive: true, startTimeMs: NOW - 60_000 } : pid === 4321 ? { alive: true, startTimeMs: NOW - 5_000 } : pid === 5555 ? { alive: true, startTimeMs: null } : { alive: false, startTimeMs: null }) };
/** Plain request inputs for writeSnapshotRequest: a control root, a checkout, and a commit with its snapshot path. Data only; no broker is involved. */
const requestInputs = (t) => {
  const root = tmp(t);
  const controlRoot = path.join(root, "control"); const checkout = path.join(root, "checkout"); const snapshot = path.join(root, "snapshots", "0123456789abcdef");
  for (const d of [controlRoot, checkout, snapshot]) fs.mkdirSync(d, { recursive: true });
  const commit = { sha: "c".repeat(40), tree: "e".repeat(40) };
  return { controlRoot, checkout, commit, snapshot: { path: snapshot, expectedTree: commit.tree }, handle: "own-7" };
};
const GATE_DEAD = { pid: 999_999, startTimeMs: NOW - 50_000 };

/** A carrier on disk with an owner record in RustRight's wire written beside it. */
function leftover(root, name, { gate = GATE_DEAD, createdAtMs = NOW - 2 * FINISHED_CARRIER_RETENTION_MS, handle = "stage#call-1", body = null, owner = true, ownerBody = null } = {}) {
  const dir = path.join(root, "snapshot-requests"); fs.mkdirSync(dir, { recursive: true });
  const file = path.join(dir, `${name}.json`);
  fs.writeFileSync(file, body ?? JSON.stringify({ schema: "rustright.snapshot-request/1", snapshot: { path: "/snap/x" }, ownership: { handle } }));
  if (owner) fs.writeFileSync(`${file}.owner.json`, ownerBody ?? JSON.stringify({ schema: CARRIER_OWNER_SCHEMA, gate, nonce: "n".repeat(64), handle, createdAtMs }));
  return file;
}
const lease = (answers) => (q) => answers[q.handle] ?? { state: "UNKNOWN", why: "x", members: [] };
const sweep = (root, answers = {}, extra = {}) => sweepSnapshotRequests({ controlRoot: root, lease: lease(answers), now: NOW, identity, snapshot: "/snap/x", ...extra });
const reaped = (handle) => ({ state: "REAPED", members: [{ handle, state: "REAPED" }] });

test("data: [GB-76] the owner record is RustRight's wire: <carrier>.owner.json beside the carrier, exactly {schema, gate:{pid,startTimeMs}, nonce, handle, createdAtMs}, written BEFORE the carrier", (t) => {
  const fx = requestInputs(t);
  const order = []; const write = fs.writeFileSync;
  fs.writeFileSync = (file, ...rest) => { order.push(path.basename(String(file))); return write(file, ...rest); };
  let made; const before = Date.now();
  try { made = writeSnapshotRequest({ controlRoot: fx.controlRoot, commit: fx.commit, snapshot: fx.snapshot, declaredInputs: [], handle: fx.handle, primaryCwd: fx.checkout, argv: ["check"], nonce: "a".repeat(64) }); } finally { fs.writeFileSync = write; }
  const after = Date.now();
  assert.equal(ownerFileOf(made.file), `${made.file}.owner.json`, "named after the carrier file, extension included");
  const written = order.filter((n) => n.includes(path.basename(made.file)));
  assert.deepEqual(written, [`${path.basename(made.file)}.owner.json`, path.basename(made.file)], "the owner record first, then the carrier");
  const owner = JSON.parse(fs.readFileSync(ownerFileOf(made.file), "utf8"));
  assert.deepEqual(Object.keys(owner).sort(), ["createdAtMs", "gate", "handle", "nonce", "schema"], "no field RustRight's reader would refuse (it denies unknown fields)");
  assert.deepEqual(Object.keys(owner.gate).sort(), ["pid", "startTimeMs"]);
  assert.equal(owner.schema, "rustright.carrier-owner/1"); assert.equal(owner.schema, CARRIER_OWNER_SCHEMA);
  assert.equal(owner.gate.pid, process.pid);
  assert.ok(owner.gate.startTimeMs === null || (Number.isInteger(owner.gate.startTimeMs) && owner.gate.startTimeMs > 1_000_000_000_000), "UNIX epoch milliseconds, or null when this host cannot read it");
  assert.equal(owner.nonce, "a".repeat(64)); assert.equal(owner.handle, made.carrier.ownership.handle);
  assert.ok(Number.isInteger(owner.createdAtMs) && owner.createdAtMs >= before && owner.createdAtMs <= after, "creation time in UNIX epoch milliseconds");
  const live = sweepSnapshotRequests({ controlRoot: fx.controlRoot, lease: () => ({ state: "REAPED", members: [] }) });
  assert.deepEqual(live.retired, [], "this process is alive: its carrier is not retired even though the broker would settle it");
  assert.match(live.retained[0].why, /gate is alive|creator is alive/);
  removeCarrier(made.file);
  assert.deepEqual(fs.readdirSync(path.join(fx.controlRoot, "snapshot-requests")), [], "the finally removes both");
});

test("data: [GB-76] negative: an owner record that cannot be written leaves no carrier, and an existing owner record is never overwritten", (t) => {
  const fx = requestInputs(t);
  const dir = path.join(fx.controlRoot, "snapshot-requests"); fs.mkdirSync(dir, { recursive: true });
  const uuid = () => "11111111-1111-4111-8111-111111111111";
  fs.writeFileSync(path.join(dir, `${uuid()}.json.owner.json`), "taken");
  assert.throws(() => writeSnapshotRequest({ controlRoot: fx.controlRoot, commit: fx.commit, snapshot: fx.snapshot, declaredInputs: [], handle: fx.handle, primaryCwd: fx.checkout, argv: ["check"], nonce: "b".repeat(64), uuid }), /owner record/);
  assert.equal(fs.existsSync(path.join(dir, `${uuid()}.json`)), false, "no carrier without its owner record");
  assert.equal(fs.readFileSync(path.join(dir, `${uuid()}.json.owner.json`), "utf8"), "taken", "never overwritten");
});

test("[GB-76] the gate's identity is the number RustRight reads: Linux boot time plus the stat start ticks, macOS's kernel start time in microseconds floored to ms, Windows's creation time as Unix ms", () => {
  // Linux: /proc/<pid>/stat field 22 (the comm may hold spaces and parentheses), /proc/stat btime, CLK_TCK.
  const stat = `4242 (my (odd) cmd) S 1 4242 4242 0 -1 4194560 100 0 0 0 1 2 0 0 20 0 1 0 ${12_345_678} 1000 2 18446744073709551615 0 0 0 0 0 0 0 0 0 0 0 0 17 0 0 0 0 0 0`;
  assert.equal(linuxStartTimeMs(stat, "cpu 1 2 3\nbtime 1730000000\nprocesses 5\n", 100), 1_730_000_000_000 + Math.floor(12_345_678 * 1000 / 100));
  assert.equal(linuxStartTimeMs(stat, "cpu 1 2 3\nbtime 1730000000\n", 250), 1_730_000_000_000 + Math.floor(12_345_678 * 1000 / 250));
  assert.equal(linuxStartTimeMs("garbage", "btime 1\n", 100), null);
  assert.equal(linuxStartTimeMs(stat, "no boot time here\n", 100), null);
  assert.equal(linuxStartTimeMs(stat, "btime 1\n", 0), null);
  // macOS: kinfo_proc begins with the process's start timeval (int64 seconds, int32 microseconds).
  const kinfo = Buffer.alloc(648); kinfo.writeBigInt64LE(1_730_000_123n, 0); kinfo.writeInt32LE(456_789, 8);
  assert.equal(darwinStartTimeMs(kinfo), 1_730_000_123_456);
  assert.equal(darwinStartTimeMs(Buffer.alloc(4)), null);
  assert.equal(darwinStartTimeMs(Buffer.alloc(0)), null);
  // Through the platform dispatch with fixtures (a simulated host).
  assert.equal(processStartTimeMs(4242, { platform: "linux", readFile: (p) => (p === "/proc/4242/stat" ? stat : p === "/proc/stat" ? "btime 1730000000\n" : (() => { throw Object.assign(new Error("ENOENT"), { code: "ENOENT" }); })()), hz: 100 }), 1_730_000_000_000 + 123_456_780);
  const calls = [];
  assert.equal(processStartTimeMs(77, { platform: "darwin", spawn: (cmd, args) => { calls.push([path.basename(cmd), ...args]); return { status: 0, stdout: kinfo }; } }), 1_730_000_123_456);
  assert.deepEqual(calls, [["sysctl", "-b", "kern.proc.pid.77"]]);
  assert.equal(processStartTimeMs(77, { platform: "darwin", spawn: () => ({ status: 1, stdout: Buffer.alloc(0) }) }), null);
  const script = [];
  assert.equal(processStartTimeMs(88, { platform: "win32", spawn: (cmd, args) => { script.push([cmd, ...args]); return { status: 0, stdout: "START:1730000999123\r\n" }; } }), 1_730_000_999_123);
  assert.match(script[0][0], /powershell/i);
  assert.equal(processStartTimeMs(88, { platform: "win32", spawn: () => ({ status: 0, stdout: "GONE" }) }), null);
  assert.equal(processStartTimeMs(88, { platform: "win32", spawn: () => ({ status: 1, stdout: "", stderr: "x" }) }), null);
});

test("[GB-76] this host's own start time equals an independent reading of /proc (Linux), so a live gate reads Same at the broker, never Reused", { skip: process.platform === "linux" ? false : "the independent reading is /proc" }, () => {
  const stat = fs.readFileSync("/proc/self/stat", "utf8");
  const ticks = Number(stat.slice(stat.lastIndexOf(")") + 1).trim().split(/\s+/)[19]);
  const btime = Number(/^btime (\d+)/m.exec(fs.readFileSync("/proc/stat", "utf8"))[1]);
  assert.equal(processStartTimeMs(process.pid), btime * 1000 + Math.floor(ticks * 1000 / 100));
});

test("[GB-76] writeCarrierOwner records the creator's identity, the call's nonce and handle and the creation time; exclusive, never overwritten", (t) => {
  const root = tmp(t); const file = path.join(root, "x.json");
  const rec = writeCarrierOwner({ file, handle: "h#1", nonce: "n1", identity: () => ({ pid: 7, startTimeMs: 123 }), now: NOW });
  assert.deepEqual(rec, { schema: CARRIER_OWNER_SCHEMA, gate: { pid: 7, startTimeMs: 123 }, nonce: "n1", handle: "h#1", createdAtMs: NOW });
  assert.deepEqual(JSON.parse(fs.readFileSync(`${file}.owner.json`, "utf8")), rec);
  assert.throws(() => writeCarrierOwner({ file, handle: "h#1", nonce: "n1" }), /EEXIST/);
});

test("[GB-76] a carrier whose gate is dead and whose sub-handle the broker has completed, or never saw, retires with its owner record", (t) => {
  const root = tmp(t);
  const done = leftover(root, "a1", { handle: "s#done" });
  const never = leftover(root, "a2", { handle: "s#never" });
  const result = sweep(root, { "s#done": reaped("s#done") });
  assert.deepEqual(result.retired.sort(), [done, never].sort());
  assert.equal(fs.readdirSync(path.join(root, "snapshot-requests")).length, 0, "the owner records go with them");
  assert.equal(result.examined, 2);
});

test("[GB-76] every other carrier is retained and counted: a live gate, ACTIVE or UNKNOWN-with-members at the broker, a gate whose identity cannot be read, a young carrier of a dead gate", (t) => {
  const root = tmp(t);
  const files = {
    live: leftover(root, "b1", { gate: { pid: process.pid, startTimeMs: NOW - 60_000 }, handle: "s#live", createdAtMs: NOW - 1000 }),
    unreadable: leftover(root, "b2", { gate: { pid: 5555, startTimeMs: NOW - 1 }, handle: "s#unreadable" }),
    active: leftover(root, "b3", { handle: "s#active" }),
    known: leftover(root, "b4", { handle: "s#known" }),
    young: leftover(root, "b5", { handle: "s#young", createdAtMs: NOW - 1000 }),
    nostart: leftover(root, "b6", { gate: { pid: process.pid, startTimeMs: null }, handle: "s#nostart" }),
  };
  const result = sweep(root, { "s#active": { state: "ACTIVE", members: [{ handle: "s#active", state: "ACTIVE" }] }, "s#known": { state: "UNKNOWN", members: [{ handle: "s#known", state: "UNKNOWN" }] } });
  assert.deepEqual(result.retired, []);
  const why = Object.fromEntries(result.retained.map((r) => [path.basename(r.file, ".json"), r.why]));
  assert.match(why.b1, /gate is alive/); assert.match(why.b2, /cannot be read|cannot be proven/); assert.match(why.b3, /ACTIVE/); assert.match(why.b4, /UNKNOWN/); assert.match(why.b5, /younger than the retention/); assert.match(why.b6, /cannot be proven|cannot be read/);
  for (const f of Object.values(files)) assert.ok(fs.existsSync(f) && fs.existsSync(`${f}.owner.json`), "kept on disk with its owner record");
  assert.equal(result.retained.length, 6, "every entry is accounted for");
});

test("[GB-76] a dead gate of either kind (pid gone, pid reused by a process that started at another time) retires; a live gate's carrier retires only an hour after the call finished and only when the broker holds nothing under it", (t) => {
  const root = tmp(t);
  leftover(root, "c1", { gate: { pid: 999_998, startTimeMs: NOW - 50_000 }, handle: "s#1" });
  leftover(root, "c2", { gate: { pid: 4321, startTimeMs: NOW - 90_000 }, handle: "s#2" });
  assert.equal(sweep(root).retired.length, 2);
  const live = tmp(t);
  const oldLive = leftover(live, "d1", { gate: { pid: process.pid, startTimeMs: NOW - 60_000 }, handle: "s#old", createdAtMs: NOW - 2 * FINISHED_CARRIER_RETENTION_MS });
  const heldLive = leftover(live, "d2", { gate: { pid: process.pid, startTimeMs: NOW - 60_000 }, handle: "s#held", createdAtMs: NOW - 2 * FINISHED_CARRIER_RETENTION_MS });
  const result = sweep(live, { "s#old": reaped("s#old"), "s#held": { state: "ACTIVE", members: [{ handle: "s#held", state: "ACTIVE" }] } });
  assert.deepEqual(result.retired, [oldLive]);
  assert.ok(fs.existsSync(heldLive));
});

test("[GB-76] corrupt, unreadable, oversize, linked, wrong-schema, extra-field or owner-less entries are counted and never deleted; the owner records themselves and grant files are never carriers", (t) => {
  const root = tmp(t);
  const noOwner = leftover(root, "e1", { owner: false });
  const wrongSchema = leftover(root, "e2", { ownerBody: JSON.stringify({ schema: "other", gate: GATE_DEAD, nonce: "n", handle: "s#x", createdAtMs: 0 }) });
  const junk = leftover(root, "e3", { ownerBody: "{not json" });
  const extra = leftover(root, "e4", { ownerBody: JSON.stringify({ schema: CARRIER_OWNER_SCHEMA, gate: GATE_DEAD, nonce: "n", handle: "s#y", createdAtMs: 0, sha256: "x" }) });
  const badGate = leftover(root, "e5", { ownerBody: JSON.stringify({ schema: CARRIER_OWNER_SCHEMA, gate: { pid: -1, startTimeMs: 1 }, nonce: "n", handle: "s#z", createdAtMs: 0 }) });
  const oversize = leftover(root, "e6", { ownerBody: JSON.stringify({ schema: CARRIER_OWNER_SCHEMA, gate: GATE_DEAD, nonce: "n".repeat(70_000), handle: "s#big", createdAtMs: 0 }) });
  const grant = path.join(root, "snapshot-requests", "grant-0b6e1a9c-1111-4111-8111-111111111111.json"); fs.writeFileSync(grant, "{}");
  let link = null;
  if (process.platform !== "win32") { link = path.join(root, "snapshot-requests", "e7.json"); fs.symlinkSync(noOwner, link); }
  const result = sweep(root);
  assert.deepEqual(result.retired, []);
  assert.ok(result.retained.some((r) => r.file === noOwner && /no owner record/.test(r.why)));
  for (const f of [wrongSchema, junk, extra, badGate, oversize]) assert.ok(result.corrupt.some((c) => c.file === f), `${path.basename(f)} is corrupt`);
  if (link) assert.ok(result.corrupt.some((c) => c.file === link && /not a regular file/.test(c.why)), "a link is never followed");
  for (const f of [noOwner, wrongSchema, junk, extra, badGate, oversize, grant]) assert.ok(fs.existsSync(f), `${path.basename(f)} kept`);
  assert.equal(result.examined, result.corrupt.length + result.retained.length, "every examined entry is accounted for");
  assert.ok(!result.corrupt.concat(result.retained).some((e) => e.file === grant || String(e.file).endsWith(".owner.json")), "grant files and owner records are not carriers");
});

test("[GB-76] removal is compare-token: a carrier or owner record that changed after it was judged is kept, and a symlinked directory is refused", (t) => {
  const root = tmp(t);
  const file = leftover(root, "f1", { handle: "s#race" });
  const result = sweep(root, {}, { lease: (q) => { fs.appendFileSync(file, " "); return { state: "REAPED", members: [{ handle: q.handle, state: "REAPED" }] }; } });
  assert.deepEqual(result.retired, []);
  assert.match(result.retained[0].why, /changed while/);
  assert.ok(fs.existsSync(file));
  const again = leftover(tmp(t), "f2", { handle: "s#race2" });
  const raceRoot = path.dirname(path.dirname(again));
  const owner = `${again}.owner.json`;
  const result2 = sweep(raceRoot, {}, { lease: (q) => { fs.writeFileSync(owner, fs.readFileSync(owner, "utf8").replace("s#race2", "s#race2")); fs.appendFileSync(owner, "\n"); return { state: "REAPED", members: [{ handle: q.handle, state: "REAPED" }] }; } });
  assert.deepEqual(result2.retired, []);
  if (process.platform !== "win32") {
    const linked = tmp(t); const real = path.join(linked, "real"); fs.mkdirSync(real); fs.symlinkSync(real, path.join(linked, "snapshot-requests"));
    assert.match(sweepSnapshotRequests({ controlRoot: linked, lease: () => ({ state: "REAPED" }) }).corrupt[0].why, /not a real directory/);
  }
  assert.deepEqual(sweepSnapshotRequests({ controlRoot: path.join(tmp(t), "none"), lease: () => ({ state: "REAPED" }) }), { retired: [], retained: [], corrupt: [], examined: 0 });
});
