// rightkit#1367 step 2: a carried scan's run limit is a stopwatch that does not tick while the broker holds the scan QUEUED. Every test here drives a fake clock and an
// injected live status sequence: nothing depends on wall-clock timing.
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { QUEUED_CEILING_FACTOR, STATUS_POLL_CONCURRENCY, brokerStateOf, createBrokerLive, createRunClock, createStatusPoller, requestIdsOf, watchBrokerAdmission } from "./broker-admission.mjs";

/** A manual clock: `advance(ms)` moves time and fires the timers that fall due, in order. */
function manualTime() {
  let now = 1_000; let next = 1; const timers = new Map();
  return {
    now: () => now,
    setTimer: (fn, ms) => { const id = next++; timers.set(id, { at: now + ms, fn }); return id; },
    clearTimer: (id) => { timers.delete(id); },
    advance(ms) {
      const end = now + ms;
      for (;;) {
        const due = [...timers.entries()].filter(([, t]) => t.at <= end).sort((a, b) => a[1].at - b[1].at)[0];
        if (!due) break;
        now = due[1].at; timers.delete(due[0]); due[1].fn();
      }
      now = end;
    },
    pending: () => timers.size,
  };
}

const refFile = (dir, name, requestId) => {
  const file = path.join(dir, name);
  fs.writeFileSync(file, JSON.stringify({ schema: "rustright.request-ref.v1", requestId, workspace: path.resolve("/ws") }));
  return file;
};
const tmp = (t) => { const d = fs.mkdtempSync(path.join(os.tmpdir(), "rk-admission-")); t.after(() => fs.rmSync(d, { recursive: true, force: true })); return d; };

test("run clock: expires after its limit of RUNNING time; paused time (queued at the broker) is not counted", () => {
  const time = manualTime(); let expired = 0;
  const clock = createRunClock({ limitMs: 100, onExpire: () => { expired += 1; }, ...time });
  time.advance(60);
  clock.pause();
  assert.equal(clock.running, false);
  time.advance(10_000);
  assert.equal(expired, 0, "ten seconds queued do not use up a 100 ms limit");
  assert.equal(clock.queuedMs, 10_000);
  clock.resume();
  assert.equal(clock.remainingMs, 40, "60 of 100 ms were run before the queue");
  time.advance(39);
  assert.equal(expired, 0);
  time.advance(1);
  assert.equal(expired, 1);
  assert.equal(clock.expired, true);
  clock.pause(); clock.resume(); time.advance(1000);
  assert.equal(expired, 1, "fires once");
});

test("run clock: stop ends it for good; pause and resume are idempotent", () => {
  const time = manualTime(); let expired = 0;
  const clock = createRunClock({ limitMs: 50, onExpire: () => { expired += 1; }, ...time });
  clock.pause(); clock.pause(); time.advance(500); clock.resume(); clock.resume();
  assert.equal(clock.remainingMs, 50);
  clock.stop();
  time.advance(500);
  assert.equal(expired, 0);
  assert.equal(time.pending(), 0, "no timer left behind");
  clock.resume();
  assert.equal(clock.running, false, "a stopped clock does not restart");
});

test("brokerStateOf reads the live state of this request's STATUS only; anything else is null", () => {
  assert.equal(brokerStateOf(JSON.stringify({ type: "STATUS", id: "r1", state: "QUEUED", receipt: null }), "r1"), "QUEUED");
  assert.equal(brokerStateOf(JSON.stringify({ type: "STATUS", id: "r1", state: "RUNNING" }), "r1"), "RUNNING");
  assert.equal(brokerStateOf(JSON.stringify({ type: "STATUS", id: "r2", state: "QUEUED" }), "r1"), null, "another request's status");
  assert.equal(brokerStateOf("not json", "r1"), null);
  assert.equal(brokerStateOf(JSON.stringify({ type: "STATUS", id: "r1" }), "r1"), null, "no state");
  assert.equal(brokerStateOf(JSON.stringify({ type: "STATUS", id: "r1", state: "UNAVAILABLE" }), "r1"), null);
});

test("createBrokerLive asks the client `status <id>` and `cancel <id>` through the given asynchronous spawn", async () => {
  const calls = [];
  const spawn = async (file, args) => { calls.push([file, ...args]); return args[0] === "status" ? { status: 0, stdout: JSON.stringify({ type: "STATUS", id: args[1], state: "QUEUED", receipt: null }), stderr: "" } : { status: 0, stdout: "", stderr: "" }; };
  const live = createBrokerLive({ client: "/managed/rustright", env: {}, spawn, platform: "darwin" });
  assert.equal(await live.state("job-9"), "QUEUED");
  await live.cancel("job-9");
  assert.deepEqual(calls, [["/managed/rustright", "status", "job-9"], ["/managed/rustright", "cancel", "job-9"]]);
  const failing = createBrokerLive({ client: "/x", env: {}, spawn: async () => ({ error: new Error("spawn failed") }), platform: "darwin" });
  assert.equal(await failing.state("job-9"), null, "a client that cannot answer is no state, never a throw");
  const throwing = createBrokerLive({ client: "/x", env: {}, spawn: async () => { throw new Error("boom"); }, platform: "darwin" });
  assert.equal(await throwing.state("job-9"), null);
});

test("requestIdsOf: only complete request references count; a missing or half-written file names none", (t) => {
  const dir = tmp(t);
  const good = refFile(dir, "a.json", "req-a");
  fs.writeFileSync(path.join(dir, "half.json"), "{\"schema\":");
  assert.deepEqual(requestIdsOf([good, path.join(dir, "half.json"), path.join(dir, "missing.json")]), ["req-a"]);
});

test("watching a scan: the stopwatch pauses while every request is QUEUED and runs when any is not, following an injected status sequence", async (t) => {
  const dir = tmp(t); const time = manualTime(); let expired = 0;
  const clock = createRunClock({ limitMs: 100, onExpire: () => { expired += 1; }, ...time });
  const sequence = [["QUEUED"], ["QUEUED"], ["QUEUED", "RUNNING"], ["RUNNING", "RUNNING"], [null, "QUEUED"], ["QUEUED", "QUEUED"]];
  let at = 0; const files = [refFile(dir, "one.json", "r1")]; const asked = [];
  const live = { state: async (id) => { asked.push(id); const row = sequence[Math.min(at, sequence.length - 1)]; return row[id === "r1" ? 0 : 1] ?? null; }, cancel: async () => {} };
  const watch = watchBrokerAdmission({ clock, refFiles: () => files, live, ...time });
  assert.deepEqual(await watch.poll(), ["QUEUED"]); assert.equal(clock.running, false, "queued: paused"); time.advance(500); at += 1;
  assert.deepEqual(await watch.poll(), ["QUEUED"]); assert.equal(clock.running, false); time.advance(500); at += 1;
  files.push(refFile(dir, "two.json", "r2"));
  assert.deepEqual(await watch.poll(), ["QUEUED", "RUNNING"]); assert.equal(clock.running, true, "one request admitted: the scan is running"); at += 1;
  time.advance(30);
  assert.deepEqual(await watch.poll(), ["RUNNING", "RUNNING"]); assert.equal(clock.running, true); at += 1;
  assert.deepEqual(await watch.poll(), [null, "QUEUED"]); assert.equal(clock.running, true, "a state the gate cannot read counts as running (fail closed)"); at += 1;
  assert.equal(expired, 0);
  time.advance(69);
  assert.equal(expired, 0, "30 + 69 of 100 ms run so far");
  assert.deepEqual(await watch.poll(), ["QUEUED", "QUEUED"]); // both queued again: paused with 1 ms left
  assert.equal(clock.running, false);
  time.advance(3_600_000);
  assert.equal(expired, 0, "an hour queued is not run time");
  assert.equal(clock.queuedMs >= 3_601_000, true);
  watch.stop();
  assert.equal(await watch.poll(), null, "a stopped watch asks nothing");
  assert.deepEqual([...new Set(asked)].sort(), ["r1", "r2"]);
});

test("watching a scan with no request reference yet: the stopwatch runs (a scan that never reaches the broker is bounded by its own limit)", async (t) => {
  const dir = tmp(t); const time = manualTime(); let expired = 0;
  const clock = createRunClock({ limitMs: 100, onExpire: () => { expired += 1; }, ...time });
  let asked = 0;
  const watch = watchBrokerAdmission({ clock, refFiles: () => [path.join(dir, "later.json")], live: { state: async () => { asked += 1; return "QUEUED"; }, cancel: async () => {} }, ...time });
  assert.deepEqual(await watch.poll(), []);
  assert.equal(asked, 0);
  time.advance(100);
  assert.equal(expired, 1);
});

test("cancelKnown cancels, by id, every request the watch has seen, also after its reference file is gone", async (t) => {
  const dir = tmp(t); const time = manualTime();
  const clock = createRunClock({ limitMs: 100, onExpire: () => {}, ...time });
  const cancelled = [];
  const file = refFile(dir, "a.json", "req-a");
  const watch = watchBrokerAdmission({ clock, refFiles: () => [file], live: { state: async () => "QUEUED", cancel: async (id) => { cancelled.push(id); } }, ...time });
  await watch.poll();
  fs.rmSync(file);
  await watch.cancelKnown();
  assert.deepEqual(cancelled, ["req-a"]);
});

test("start schedules passes on the injected timer until stop, and stop leaves no timer behind", async (t) => {
  const dir = tmp(t); const time = manualTime();
  const clock = createRunClock({ limitMs: 1000, onExpire: () => {}, ...time });
  let passes = 0;
  const watch = watchBrokerAdmission({ clock, refFiles: () => [refFile(dir, "a.json", "req-a")], live: { state: async () => { passes += 1; return "QUEUED"; }, cancel: async () => {} }, intervalMs: 50, firstMs: 10, ...time });
  watch.start();
  time.advance(10); await watch.done();
  assert.equal(passes, 1);
  time.advance(50); await watch.done();
  assert.equal(passes, 2);
  watch.stop();
  clock.stop();
  assert.equal(time.pending(), 0);
});

test("run clock ceiling: a wall-clock bound of limit x QUEUED_CEILING_FACTOR fires once while the scan sits queued, and never after expiry or stop (fake clock)", () => {
  assert.equal(QUEUED_CEILING_FACTOR, 4);
  const time = manualTime(); const fired = []; let expired = 0;
  const clock = createRunClock({ limitMs: 100, onExpire: () => { expired += 1; }, ceilingMs: 100 * QUEUED_CEILING_FACTOR, onCeiling: () => fired.push(time.now()), ...time });
  clock.pause();
  time.advance(399);
  assert.deepEqual(fired, [], "just under four times the limit: still waiting");
  time.advance(1);
  assert.deepEqual(fired, [1_400], "the ceiling fired at four times the limit, in wall-clock time, with the stopwatch paused throughout");
  assert.equal(clock.ceilingHit, true); assert.equal(clock.expired, true); assert.equal(expired, 0, "the run limit itself never expired: the scan never ran");
  assert.equal(clock.queuedMs, 400);
  time.advance(10_000);
  assert.equal(fired.length, 1, "fires once");
  assert.equal(time.pending(), 0);
  // A scan that ends first, or expires first, never trips the ceiling.
  const t2 = manualTime(); let hit2 = 0;
  const c2 = createRunClock({ limitMs: 100, onExpire: () => {}, ceilingMs: 400, onCeiling: () => { hit2 += 1; }, ...t2 });
  t2.advance(100); t2.advance(1_000);
  assert.equal(hit2, 0, "an expired run limit cancels the ceiling: one cancel only"); assert.equal(c2.ceilingHit, false);
  const t3 = manualTime(); let hit3 = 0;
  const c3 = createRunClock({ limitMs: 100, onExpire: () => {}, ceilingMs: 400, onCeiling: () => { hit3 += 1; }, ...t3 });
  c3.pause(); c3.stop(); t3.advance(1_000);
  assert.equal(hit3, 0); assert.equal(t3.pending(), 0, "stop leaves no timer behind");
});

test("one shared status poller: N watched scans cause at most STATUS_POLL_CONCURRENCY reads in flight, the first read after 1 s, then one pass every 5 s (fake clock)", async (t) => {
  assert.equal(STATUS_POLL_CONCURRENCY, 2);
  const dir = tmp(t); const time = manualTime();
  let active = 0; let peak = 0; const reads = [];
  const live = { state: async (id) => { active += 1; peak = Math.max(peak, active); reads.push({ at: time.now(), id }); await Promise.resolve(); await Promise.resolve(); active -= 1; return "QUEUED"; }, cancel: async () => {} };
  const poller = createStatusPoller({ live, ...time });
  const clocks = []; const watches = [];
  for (let n = 0; n < 6; n += 1) {
    const clock = createRunClock({ limitMs: 1_000_000, onExpire: () => {}, ...time }); clocks.push(clock);
    const watch = watchBrokerAdmission({ clock, refFiles: () => [refFile(dir, `${n}.json`, `req-${n}`)], live, poller, ...time });
    watch.start(); watches.push(watch);
  }
  assert.equal(poller.members, 6); assert.equal(time.pending(), 7, "one poller timer for six scans (plus the six run clocks)");
  time.advance(999); assert.equal(reads.length, 0, "nothing is read before the first second");
  time.advance(1); await poller.idle();
  assert.equal(reads.length, 6, "the first pass reads each request once");
  assert.ok(reads.every((r) => r.at === 2_000));
  assert.equal(peak, 2, "never more than two reads at once, however many scans are watched");
  assert.ok(clocks.every((c) => c.running === false), "every scan is queued: all paused");
  time.advance(4_999); await poller.idle(); assert.equal(reads.length, 6);
  time.advance(1); await poller.idle();
  assert.equal(reads.length, 12, "the next pass comes five seconds later");
  assert.equal(reads.at(-1).at, 7_000);
  for (const w of watches) w.stop();
  assert.equal(poller.members, 0);
  for (const c of clocks) c.stop();
  assert.equal(time.pending(), 0, "no timer left once every scan is done");
});
