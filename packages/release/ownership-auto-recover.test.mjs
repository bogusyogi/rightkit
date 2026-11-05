// rightkit#1513: a dead owner's reclaim recovers by itself when the broker has run continuously since before the ownership was acquired.
import test from "node:test";
import assert from "node:assert";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { judgeOwner } from "./build-ownership.mjs";

const OWNER = "a1b2"; const LOST = `${OWNER}#lost`; const DONE = `${OWNER}#done`;
const ACQUIRED = "2026-11-04T23:56:10.802Z";
function setup() {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "rk-auto-")); const file = path.join(dir, "k", "owner.json");
  fs.mkdirSync(path.dirname(file), { recursive: true });
  const observed = JSON.stringify({ schema: "rightkit.build-ownership/1", handle: OWNER, host: os.hostname(), pid: 4242, startedMs: 1, containment: {}, snapshot: "/snap/x", acquiredAt: ACQUIRED, broker: { submitted: true, handles: [DONE, LOST] } });
  fs.writeFileSync(file, observed); return { file, observed };
}
const probe = { identity: () => ({ alive: false }), bootTime: () => Date.now() };
const leaseWith = (lost = { state: "UNKNOWN", members: [] }, ownerMembers = [{ handle: DONE, state: "REAPED" }]) => (q) =>
  q.handle === OWNER ? { state: "REAPED", why: "", members: ownerMembers } : q.handle === DONE ? { state: "REAPED", why: "", members: [{ handle: DONE, state: "REAPED" }] } : { why: "no record", ...lost };
const before = { pid: 51701, startedMs: Date.parse(ACQUIRED) - 3600_000 };
const judge = (ctx, broker, lease = leaseWith()) => judgeOwner(ctx.observed, { file: ctx.file, probe, lease, broker });

test("broker up since before the ownership: the UNKNOWN sub-handle never reached it; the reclaim recovers", () => {
  const j = judge(setup(), before);
  assert.ok(j.proofs.some((p) => /1 sub-handle\(s\) never reached the broker/.test(p)));
  assert.ok(j.proofs.some((p) => /1 REAPED, 0 proven never started, 1 never reached the broker/.test(p)));
});
test("broker restarted after the ownership was acquired: UNKNOWN proves nothing; refused", () => {
  assert.throws(() => judge(setup(), { pid: 1, startedMs: Date.parse(ACQUIRED) + 60_000 }), /lease is UNKNOWN/);
});
test("broker identity unknown: refused", () => { assert.throws(() => judge(setup(), null), /lease is UNKNOWN/); });
test("ACTIVE work still holds, whatever the broker's age", () => {
  assert.throws(() => judge(setup(), before, leaseWith({ state: "ACTIVE", members: [{ handle: LOST, state: "ACTIVE" }] })), /ACTIVE/);
});
test("a member row the broker lists still holds", () => {
  assert.throws(() => judge(setup(), before, leaseWith({ state: "UNKNOWN", members: [{ handle: LOST, state: "UNKNOWN" }] })), /UNKNOWN/);
});
test("listed as non-REAPED in the owner's answer still holds", () => {
  assert.throws(() => judge(setup(), before, leaseWith(undefined, [{ handle: DONE, state: "REAPED" }, { handle: LOST, state: "ACTIVE" }])), /ACTIVE/);
});
