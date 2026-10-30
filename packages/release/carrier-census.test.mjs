// GB-76: carrier crash leftovers are retired by owner proof, never by age alone, and every entry is accounted for.
import "./test-support/broker-guard.mjs";
import assert from "node:assert/strict";
import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { CARRIER_OWNER_SCHEMA, CARRIER_RETENTION_MS, ownerFileOf, removeCarrier, sweepSnapshotRequests, writeCarrierOwner } from "./carrier-census.mjs";
import { writeSnapshotRequest } from "./snapshot-request.mjs";
import { makeFakeBroker } from "./test-support/fake-broker.mjs";

const NOW = Date.now();
const C = "c".repeat(40); const T = "e".repeat(40);
const identity = { self: () => ({ pid: process.pid, startedMs: NOW - 60_000, bootId: "boot-1" }), bootId: () => "boot-1", probe: (pid) => (pid === process.pid ? { alive: true, startedMs: NOW - 60_000 } : pid === 4321 ? { alive: true, startedMs: NOW - 5_000 } : { alive: false, startedMs: null }) };
const tmp = (t) => { const d = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "rk-carrier-census-"))); t.after(() => fs.rmSync(d, { recursive: true, force: true })); return d; };
const sha = (b) => crypto.createHash("sha256").update(b).digest("hex");

/** A carrier on disk with an owner record written by `creator` (pid, start, boot, host) at `createdAt`. */
function leftover(root, name, { creator = { pid: 999_999, startedMs: NOW - 50_000, bootId: "boot-1" }, host = os.hostname(), createdAt = NOW - 2 * CARRIER_RETENTION_MS, handle = "stage#call-1", body = null, owner = true } = {}) {
  const dir = path.join(root, "snapshot-requests"); fs.mkdirSync(dir, { recursive: true });
  const file = path.join(dir, `${name}.json`);
  const bytes = Buffer.from(body ?? JSON.stringify({ schema: "rustright.snapshot-request/1", snapshot: { path: "/snap/x" }, ownership: { handle } }));
  fs.writeFileSync(file, bytes);
  if (owner) fs.writeFileSync(ownerFileOf(file), JSON.stringify({ schema: CARRIER_OWNER_SCHEMA, ...creator, host, handle, nonce: "n".repeat(64), sha256: sha(bytes), createdAt: new Date(createdAt).toISOString() }));
  return file;
}
const lease = (answers) => (q) => answers[q.handle] ?? { state: "UNKNOWN", why: "x", members: [] };
const sweep = (root, answers, extra = {}) => sweepSnapshotRequests({ controlRoot: root, lease: lease(answers), now: NOW, identity, snapshot: "/snap/x", ...extra });

test("[GB-76] a carrier whose creator is dead and whose sub-handle the broker has completed, or never saw, retires with its owner record", (t) => {
  const root = tmp(t);
  const done = leftover(root, "a1", { handle: "s#done" });
  const never = leftover(root, "a2", { handle: "s#never" });
  const result = sweep(root, { "s#done": { state: "REAPED", members: [{ handle: "s#done", state: "REAPED" }] } });
  assert.deepEqual(result.retired.sort(), [done, never].sort());
  assert.equal(fs.readdirSync(path.join(root, "snapshot-requests")).length, 0, "the owner records go with them");
  assert.equal(result.examined, 2);
});

test("[GB-76] every other carrier is retained and counted: a live creator, a pid reused, another host, ACTIVE or UNKNOWN-with-members at the broker, a young carrier of a dead creator", (t) => {
  const root = tmp(t);
  const files = {
    live: leftover(root, "b1", { creator: { pid: process.pid, startedMs: NOW - 60_000, bootId: "boot-1" }, handle: "s#live" }),
    host: leftover(root, "b2", { host: "elsewhere.example", handle: "s#host" }),
    active: leftover(root, "b3", { handle: "s#active" }),
    known: leftover(root, "b4", { handle: "s#known" }),
    young: leftover(root, "b5", { handle: "s#young", createdAt: NOW - 1000 }),
  };
  const result = sweep(root, { "s#active": { state: "ACTIVE", members: [{ handle: "s#active", state: "ACTIVE" }] }, "s#known": { state: "UNKNOWN", members: [{ handle: "s#known", state: "UNKNOWN" }] } });
  assert.deepEqual(result.retired, []);
  const why = Object.fromEntries(result.retained.map((r) => [path.basename(r.file, ".json"), r.why]));
  assert.match(why.b1, /creator is alive/); assert.match(why.b2, /cannot be proven dead/); assert.match(why.b3, /ACTIVE/); assert.match(why.b4, /UNKNOWN/); assert.match(why.b5, /younger than the retention/);
  for (const f of Object.values(files)) assert.ok(fs.existsSync(f), "kept on disk");
  assert.equal(result.retained.length, 5, "every entry is accounted for");
});

test("[GB-76] a dead creator of any proof kind (pid gone, pid reused, rebooted) retires; a creator with a reused pid that is alive at another start is dead, not live", (t) => {
  const root = tmp(t);
  leftover(root, "c1", { creator: { pid: 999_998, startedMs: NOW - 50_000, bootId: "boot-1" }, handle: "s#1" });
  leftover(root, "c2", { creator: { pid: 4321, startedMs: NOW - 90_000, bootId: "boot-1" }, handle: "s#2" });
  leftover(root, "c3", { creator: { pid: process.pid, startedMs: NOW - 60_000, bootId: "boot-0" }, handle: "s#3" });
  assert.equal(sweep(root, {}).retired.length, 3);
});

test("[GB-76] corrupt, unreadable, oversize, linked or owner-less entries are counted and never deleted; the digest the owner record holds must match the bytes", (t) => {
  const root = tmp(t);
  const noOwner = leftover(root, "d1", { owner: false });
  const tampered = leftover(root, "d2", { handle: "s#t" }); fs.writeFileSync(tampered, "{\"changed\":true}");
  const badOwner = leftover(root, "d3"); fs.writeFileSync(ownerFileOf(badOwner), JSON.stringify({ schema: "other" }));
  const junk = leftover(root, "d4"); fs.writeFileSync(ownerFileOf(junk), "{not json");
  let link = null;
  if (process.platform !== "win32") { link = path.join(root, "snapshot-requests", "d5.json"); fs.symlinkSync(noOwner, link); }
  const result = sweep(root, {});
  assert.deepEqual(result.retired, []);
  assert.ok(result.retained.some((r) => r.file === noOwner && /no owner record/.test(r.why)));
  assert.ok(result.retained.some((r) => r.file === junk && /unreadable/.test(r.why)));
  assert.ok(result.corrupt.some((c) => c.file === tampered && /not the bytes/.test(c.why)));
  assert.ok(result.corrupt.some((c) => c.file === badOwner && /not one this gate wrote/.test(c.why)));
  if (link) assert.ok(result.corrupt.some((c) => c.file === link && /not a regular file/.test(c.why)), "a link is never followed");
  for (const f of [noOwner, tampered, badOwner, junk]) assert.ok(fs.existsSync(f));
  assert.equal(result.corrupt.length + result.retained.length, result.examined, "every examined entry is accounted for");
});

test("[GB-76] a real carrier carries its creator record; the call's normal cleanup removes both; a sweep never touches a live creator's carrier, and a symlinked directory is refused", (t) => {
  const fx = makeFakeBroker(t);
  const made = writeSnapshotRequest({ controlRoot: fx.controlRoot, commit: fx.sb.commit, snapshot: fx.sb.snapshot, declaredInputs: [], handle: fx.handle, primaryCwd: fx.checkout, argv: ["check"], nonce: "a".repeat(64) });
  const owner = JSON.parse(fs.readFileSync(ownerFileOf(made.file), "utf8"));
  assert.equal(owner.schema, CARRIER_OWNER_SCHEMA); assert.equal(owner.pid, process.pid); assert.equal(owner.handle, made.carrier.ownership.handle); assert.equal(owner.sha256, sha(fs.readFileSync(made.file)));
  const live = sweepSnapshotRequests({ controlRoot: fx.controlRoot, lease: () => ({ state: "REAPED", members: [] }) });
  assert.deepEqual(live.retired, [], "this process is alive: its carrier is not retired even though the broker would settle it");
  assert.match(live.retained[0].why, /creator is alive/);
  removeCarrier(made.file);
  assert.deepEqual(fs.readdirSync(path.join(fx.controlRoot, "snapshot-requests")), []);
  if (process.platform !== "win32") {
    const root = tmp(t); const real = path.join(root, "real"); fs.mkdirSync(real); fs.symlinkSync(real, path.join(root, "snapshot-requests"));
    assert.match(sweepSnapshotRequests({ controlRoot: root, lease: () => ({ state: "REAPED" }) }).corrupt[0].why, /not a real directory/);
  }
  assert.deepEqual(sweepSnapshotRequests({ controlRoot: path.join(tmp(t), "none"), lease: () => ({ state: "REAPED" }) }), { retired: [], retained: [], corrupt: [], examined: 0 });
});

test("[GB-76] writeCarrierOwner records the creator by the lock identity rules (pid, start, boot, host)", (t) => {
  const root = tmp(t); const file = path.join(root, "x.json"); fs.writeFileSync(file, "{}");
  const rec = writeCarrierOwner({ file, bytes: Buffer.from("{}"), handle: "h#1", nonce: "n", identity: () => ({ pid: 7, startedMs: 123, bootId: "b" }), now: NOW, host: "h" });
  assert.deepEqual({ pid: rec.pid, startedMs: rec.startedMs, bootId: rec.bootId, host: rec.host, handle: rec.handle }, { pid: 7, startedMs: 123, bootId: "b", host: "h", handle: "h#1" });
  assert.throws(() => writeCarrierOwner({ file, bytes: Buffer.from("{}"), handle: "h#1", nonce: "n" }), /EEXIST/, "exclusive: an owner record is never overwritten");
});
