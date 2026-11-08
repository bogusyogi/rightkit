// The process-liveness probe that outlived the build-ownership record (scripts/ci/rk-pipeline.mjs acquireLock reads it): alive, start time, boot time.
import test from "node:test";
import assert from "node:assert/strict";
import { lockBootTimeMs, lockProcessIdentity } from "./lock-identity.mjs";

const gone = () => { const e = new Error("no such process"); e.code = "ESRCH"; throw e; };
const denied = () => { const e = new Error("not permitted"); e.code = "EPERM"; throw e; };
const alive = () => true;
const out = (stdout, status = 0) => () => ({ status, stdout });
const winOut = (stdout) => (cmd, args) => { assert.equal(cmd, "powershell.exe"); assert.ok(args.includes("-EncodedCommand")); return { status: 0, stdout }; };

test("boot time: Linux btime, macOS kern.boottime, Windows LastBootUpTime; anything else throws", () => {
  assert.equal(lockBootTimeMs({ platform: "linux", readFile: () => "cpu 1 2\nbtime 1700000000\nprocesses 9\n" }), 1_700_000_000_000);
  assert.throws(() => lockBootTimeMs({ platform: "linux", readFile: () => "cpu 1 2\n" }), /no btime/);
  assert.equal(lockBootTimeMs({ platform: "darwin", spawn: out("{ sec = 1762682400, usec = 12345 } Sun Nov  9 10:00:00 2025\n") }), 1_762_682_400_000);
  assert.throws(() => lockBootTimeMs({ platform: "darwin", spawn: out("", 1) }), /kern\.boottime/);
  assert.throws(() => lockBootTimeMs({ platform: "darwin", spawn: out("garbled") }), /kern\.boottime/);
  assert.equal(lockBootTimeMs({ platform: "win32", spawn: winOut("BOOT:1762682400123\r\n") }), 1_762_682_400_123);
  assert.throws(() => lockBootTimeMs({ platform: "win32", spawn: out("", 1) }), /boot time/);
  assert.throws(() => lockBootTimeMs({ platform: "freebsd" }), /no boot time/);
});

test("process identity on Unix: a missing pid is not alive, a live one reports its start, an unreadable start throws (the caller then fails closed)", () => {
  assert.deepEqual(lockProcessIdentity(4242, { platform: "darwin", kill: gone }), { alive: false, startedMs: null });
  const live = lockProcessIdentity(4242, { platform: "darwin", kill: alive, spawn: (cmd, args, options) => {
    assert.deepEqual(args, ["-o", "lstart=", "-p", "4242"]);
    assert.equal(options.env.LC_ALL, "C");
    return { status: 0, stdout: "Wed Nov  5 14:57:59 2026\n" };
  } });
  assert.deepEqual(live, { alive: true, startedMs: Date.parse("Wed Nov  5 14:57:59 2026") });
  assert.equal(lockProcessIdentity(4242, { platform: "linux", kill: denied, spawn: out("Wed Nov  5 14:57:59 2026\n") }).alive, true, "EPERM means the process exists");
  assert.deepEqual(lockProcessIdentity(4242, { platform: "linux", kill: alive, spawn: out("", 1) }), { alive: false, startedMs: null }, "ps found no such pid");
  assert.throws(() => lockProcessIdentity(4242, { platform: "linux", kill: alive, spawn: out("", 2) }), /no start time/);
  assert.throws(() => lockProcessIdentity(4242, { platform: "linux", kill: alive, spawn: out("not a date\n") }), /cannot read the start time/);
  assert.throws(() => lockProcessIdentity(4242, { platform: "linux", kill: () => { const e = new Error("boom"); e.code = "EIO"; throw e; } }), /boom/);
});

test("process identity on Windows: this process from its time origin, another from Get-Process", () => {
  assert.deepEqual(lockProcessIdentity(process.pid, { platform: "win32" }), { alive: true, startedMs: Math.round(performance.timeOrigin) });
  assert.deepEqual(lockProcessIdentity(4242, { platform: "win32", kill: alive, spawn: winOut("START:1762682400123\r\n") }), { alive: true, startedMs: 1_762_682_400_123 });
  assert.deepEqual(lockProcessIdentity(4242, { platform: "win32", kill: alive, spawn: winOut("GONE\r\n") }), { alive: false, startedMs: null });
  assert.deepEqual(lockProcessIdentity(4242, { platform: "win32", kill: gone }), { alive: false, startedMs: null });
  assert.throws(() => lockProcessIdentity(4242, { platform: "win32", kill: alive, spawn: winOut("???") }), /cannot read the start time/);
});
