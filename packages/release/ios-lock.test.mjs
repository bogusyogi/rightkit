import assert from "node:assert/strict";
import nodeFs from "node:fs";
import path from "node:path";
import test from "node:test";
import { acquireFileLock } from "./ios-lock.mjs";
import { tempRoot } from "./test-support/temp-roots.mjs";

const locked = (fn) => assert.throws(fn, (error) => error.code === "IOS_UPLOAD_LOCKED");
const lockFile = () => path.join(tempRoot("rk-ios-lock-"), "locks", "x.lock.json");

test("a live holder blocks a second lock; release lets the next one in", () => {
  const file = lockFile();
  const first = acquireFileLock(file, "a", { isAlive: () => true, pid: 1 });
  locked(() => acquireFileLock(file, "b", { isAlive: () => true, pid: 2 }));
  first.release();
  acquireFileLock(file, "c", { isAlive: () => true, pid: 3 }).release();
  assert.ok(!nodeFs.existsSync(file));
});

test("a stale lock is renamed to a tombstone and replaced; no tombstone is left", () => {
  const file = lockFile();
  nodeFs.mkdirSync(path.dirname(file), { recursive: true });
  nodeFs.writeFileSync(file, JSON.stringify({ pid: 99, label: "dead" }));
  const events = [];
  const spy = { ...nodeFs, renameSync: (from, to) => { events.push(`rename ${path.basename(from)} -> ${path.basename(to).split(".stale-")[0]}.stale`); return nodeFs.renameSync(from, to); }, unlinkSync: (target) => { events.push(`unlink ${path.basename(target).includes(".stale-") ? "tombstone" : "lock"}`); return nodeFs.unlinkSync(target); } };
  const lock = acquireFileLock(file, "a", { fs: spy, isAlive: () => false, pid: 1 });
  assert.deepEqual(events, ["rename x.lock.json -> x.lock.json.stale", "unlink tombstone"]);
  assert.equal(JSON.parse(nodeFs.readFileSync(file, "utf8")).pid, 1);
  assert.deepEqual(nodeFs.readdirSync(path.dirname(file)), ["x.lock.json"]);
  lock.release();
});

test("two processes that both judged the lock stale: only the rename winner proceeds, the other retries the exclusive create and is refused", () => {
  const file = lockFile();
  nodeFs.mkdirSync(path.dirname(file), { recursive: true });
  const stale = JSON.stringify({ pid: 99, label: "dead" });
  nodeFs.writeFileSync(file, stale);
  let winner;
  // B reads the stale lock; before B renames, A replaces it with its own live lock (rename + create). B's rename then takes A's NEW lock.
  const spy = {
    ...nodeFs,
    renameSync: (from, to) => {
      if (!winner) {
        winner = acquireFileLock(file, "A", { fs: nodeFs, isAlive: (pid) => pid === 1, pid: 1 }); // A wins first
      }
      return nodeFs.renameSync(from, to);
    },
  };
  locked(() => acquireFileLock(file, "B", { fs: spy, isAlive: (pid) => pid === 1, pid: 2 }));
  assert.equal(JSON.parse(nodeFs.readFileSync(file, "utf8")).pid, 1, "A's lock is back in place after B took it by mistake");
  assert.deepEqual(nodeFs.readdirSync(path.dirname(file)), ["x.lock.json"], "no tombstone left");
  winner.release();
});

test("a rename that loses the race (ENOENT) retries the exclusive create instead of failing or stealing", () => {
  const file = lockFile();
  nodeFs.mkdirSync(path.dirname(file), { recursive: true });
  nodeFs.writeFileSync(file, JSON.stringify({ pid: 99 }));
  let first = true;
  const spy = { ...nodeFs, renameSync: (from, to) => { if (first) { first = false; nodeFs.unlinkSync(from); } return nodeFs.renameSync(from, to); } };
  const lock = acquireFileLock(file, "a", { fs: spy, isAlive: () => false, pid: 1 });
  assert.equal(JSON.parse(nodeFs.readFileSync(file, "utf8")).pid, 1);
  lock.release();
});

test("a just-created, still-empty lock is held until it is older than the grace period", () => {
  const file = lockFile();
  nodeFs.mkdirSync(path.dirname(file), { recursive: true });
  nodeFs.writeFileSync(file, "");
  const mtime = nodeFs.statSync(file).mtimeMs;
  locked(() => acquireFileLock(file, "b", { isAlive: () => false, pid: 2, now: () => mtime + 100, graceMs: 5000 }));
  assert.ok(nodeFs.existsSync(file), "the new lock is left alone");
  locked(() => acquireFileLock(file, "b", { isAlive: () => false, pid: 2, now: () => mtime + 4999, graceMs: 5000 }));
  nodeFs.writeFileSync(file, "{not json");
  const old = acquireFileLock(file, "b", { isAlive: () => false, pid: 2, now: () => nodeFs.statSync(file).mtimeMs + 6000, graceMs: 5000 });
  assert.equal(JSON.parse(nodeFs.readFileSync(file, "utf8")).pid, 2, "an old damaged lock is stale");
  old.release();
});
