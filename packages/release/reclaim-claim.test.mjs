// GB-56: a crash during a reclaim leaves a `.reclaim` claim; a later contender judges the claimant by the identity its body carries instead of treating the file as a
// live claim forever. Proven dead (pid gone, pid reused, rebooted) is dropped and the claim retried once; live, unreadable or unprovable is never stolen; release is token-checked.
import "./test-support/broker-guard.mjs";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { OwnershipBusy, acquireBuildOwnership, judgeClaimant } from "./build-ownership.mjs";

const tmp = (t) => { const d = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "rk-own-claim-"))); t.after(() => fs.rmSync(d, { recursive: true, force: true, maxRetries: 20, retryDelay: 100 })); return d; };
const NOW = Date.now();
const probe = () => ({ platform: "darwin", identity: (pid) => (pid === process.pid ? { alive: true, startedMs: NOW - 60_000, group: 4242 } : pid === 4321 ? { alive: true, startedMs: NOW - 5_000, group: 1 } : { alive: false, startedMs: null, group: null }), bootTime: () => NOW - 86_400_000, groupSnapshot: () => ({ members: [], byPid: new Map([[1, { pid: 1, ppid: 0, startedMs: NOW - 9e7 }]]), own: new Set([process.pid]) }), tokenHolders: () => null, snapshotUsers: () => null, ancestors: () => [{ pid: 5000, startedMs: NOW - 9e6 }] });
const noLease = () => { throw new Error("no lease expected"); };
const acquire = (file, extra = {}) => acquireBuildOwnership({ file, platform: "darwin", probe: probe(), lease: noLease, env: {}, exportEnv: false, stage: "compile", snapshot: "/snap/x", ...extra });
/** A held record whose gate crashed, and a `.reclaim` claim written by `claimant` (owner-record shaped, as the claimant publishes it). */
function crashedWithClaim(t, claimant) {
  const file = path.join(tmp(t), "build-ownership", "owner.json");
  const own = acquire(file);
  const rec = JSON.parse(fs.readFileSync(file, "utf8"));
  fs.writeFileSync(file, JSON.stringify({ ...rec, pid: 999_999, startedMs: NOW - 120_000, acquiredAt: new Date(NOW - 100_000).toISOString() }));
  const claim = { ...rec, handle: "claimant-handle", ...claimant };
  fs.writeFileSync(`${file}.reclaim`, typeof claimant === "string" ? claimant : JSON.stringify(claim));
  return { file, own, claim };
}

test("[GB-56] a claim whose claimant is proven dead (pid gone, pid reused by a process that started at another time, rebooted since) is dropped and the reclaim goes on, once", (t) => {
  for (const [name, claimant] of [["pid gone", { pid: 999_998, startedMs: NOW - 50_000 }], ["pid reused", { pid: 4321, startedMs: NOW - 90_000 }], ["host rebooted since", { pid: process.pid, startedMs: NOW - 90_000_000 }]]) {
    const { file } = crashedWithClaim(t, claimant);
    const next = acquire(file);
    assert.ok(next.reclaimed, name); assert.equal(fs.existsSync(`${file}.reclaim`), false, `${name}: the claim is gone after the reclaim`);
    assert.equal(JSON.parse(fs.readFileSync(file, "utf8")).handle, next.handle, name);
    next.release();
  }
});

test("[GB-56] a live, paused, malformed or foreign-host claimant is never stolen from: the contender fails closed and the claim stays", (t) => {
  for (const [name, claimant, why] of [["a paused live reclaimer (same pid, same start)", { pid: process.pid, startedMs: NOW - 60_000 }, /a live gate holds the reclaim claim/], ["another host", { host: "elsewhere.example", pid: 999_998, startedMs: NOW - 50_000 }, /cannot be proven dead/], ["a malformed record", "{not json", /cannot be proven dead/], ["a record with no start", { pid: 999_998, startedMs: null }, /cannot be proven dead/]]) {
    const { file } = crashedWithClaim(t, claimant);
    const before = fs.readFileSync(`${file}.reclaim`, "utf8");
    assert.throws(() => acquire(file), (e) => e instanceof OwnershipBusy && why.test(e.message), name);
    assert.equal(fs.readFileSync(`${file}.reclaim`, "utf8"), before, `${name}: the claim is untouched`);
  }
});

test("[GB-56] judgeClaimant: dead, live and unknown by the same identity rules as the record's judge", () => {
  const host = os.hostname();
  const rec = (over) => JSON.stringify({ schema: "rightkit.build-ownership/1", handle: "h", host, pid: 999_998, startedMs: NOW - 50_000, ...over });
  const p = probe();
  assert.equal(judgeClaimant(null, { probe: p, host }), "unknown");
  assert.equal(judgeClaimant("{}", { probe: p, host }), "unknown");
  assert.equal(judgeClaimant(rec({ schema: "other" }), { probe: p, host }), "unknown");
  assert.equal(judgeClaimant(rec({ host: "elsewhere" }), { probe: p, host }), "unknown");
  assert.equal(judgeClaimant(rec({}), { probe: p, host }), "dead", "pid gone");
  assert.equal(judgeClaimant(rec({ pid: process.pid, startedMs: NOW - 60_000 }), { probe: p, host }), "live");
  assert.equal(judgeClaimant(rec({ pid: process.pid, startedMs: NOW - 30_000 }), { probe: p, host }), "dead", "pid reused");
});

test("[GB-56] the claim is released by token: a reclaim whose claim was replaced by a successor's never removes the successor's", (t) => {
  const file = path.join(tmp(t), "build-ownership", "owner.json");
  const first = acquire(file);
  const rec = JSON.parse(fs.readFileSync(file, "utf8"));
  fs.writeFileSync(file, JSON.stringify({ ...rec, pid: 999_999, startedMs: NOW - 120_000, acquiredAt: new Date(NOW - 100_000).toISOString() }));
  void first;
  const successor = JSON.stringify({ ...rec, handle: "successor", pid: process.pid });
  const next = acquire(file, { hooks: { afterClaim: (claimFile) => fs.writeFileSync(claimFile, successor) } });
  assert.ok(next.reclaimed);
  assert.equal(fs.readFileSync(`${file}.reclaim`, "utf8"), successor, "the successor's claim stays");
  next.release();
});

test("[GB-56] two contenders over one dead claim end with exactly one owner: the loser is told it was taken first, never both", (t) => {
  const { file } = crashedWithClaim(t, { pid: 999_998, startedMs: NOW - 50_000 });
  const winner = acquire(file);
  assert.throws(() => acquire(file), (e) => e instanceof OwnershipBusy && /held by pid/.test(e.message), "the second contender sees a live owner, not a free host");
  assert.equal(JSON.parse(fs.readFileSync(file, "utf8")).handle, winner.handle);
  winner.release();
});
