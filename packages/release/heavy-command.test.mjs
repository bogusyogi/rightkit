import assert from "node:assert/strict";
import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { mkdtempSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";

import {
  acquireHeavyWorkSlot,
  formatResourceSnapshot,
  heavyChildDetached,
  heavyCommandEnvironment,
  heavyWorkRoot,
  parseMacResourceSnapshot,
  parseWindowsResourceSnapshot,
  resourceBlockers,
  runHeavyCommand,
  systemResourceSnapshot,
  terminateProcessTree,
  watchOwnedProcessTree,
} from "./heavy-command.mjs";

test("heavy-work root is machine-wide rather than repository-local", () => {
  assert.equal(heavyWorkRoot({ platform: "mac", home: "/Users/test", env: {} }), "/Users/test/Library/Caches/RightSuite/heavy-work");
  assert.equal(heavyWorkRoot({ platform: "win", env: { LOCALAPPDATA: "C:\\Users\\test\\AppData\\Local" } }), "C:\\Users\\test\\AppData\\Local\\RightSuite\\heavy-work");
});

test("heavy commands default compiler and test concurrency to two", () => {
  const env = heavyCommandEnvironment({});
  assert.equal(env.CARGO_BUILD_JOBS, "2");
  assert.equal(env.RUST_TEST_THREADS, "2");
  assert.equal(env.RIGHTSUITE_HEAVY_WORK_OWNER, "1");
  assert.equal(heavyCommandEnvironment({ CARGO_BUILD_JOBS: "3", RUST_TEST_THREADS: "1" }).CARGO_BUILD_JOBS, "3");
});

test("only the slot owner creates a new POSIX process group", () => {
  assert.equal(heavyChildDetached({ slot: {}, platform: "mac" }), true);
  assert.equal(heavyChildDetached({ slot: null, platform: "mac" }), false);
  assert.equal(heavyChildDetached({ slot: {}, platform: "win" }), false);
});

test("mac resource parser reports memory, swap and thermal limiting", () => {
  const snapshot = parseMacResourceSnapshot({
    memory: "System-wide memory free percentage: 9%",
    swap: "vm.swapusage: total = 8192.00M used = 7750.62M free = 441.38M",
    thermal: "CPU_Speed_Limit = 70",
  });
  assert.equal(snapshot.freePercent, 9);
  assert.equal(Math.round(snapshot.swapUsedBytes / 1024 ** 2), 7751);
  assert.equal(snapshot.thermalLimited, true);
  assert.deepEqual(resourceBlockers(snapshot), ["memory free 9% < 15%", "macOS reports thermal/performance limiting"]);
});

test("Windows admission uses physical memory and CPU load with a Node memory fallback", () => {
  const snapshot = parseWindowsResourceSnapshot({
    system: JSON.stringify({ TotalBytes: 16 * 1024 ** 3, FreeBytes: 4 * 1024 ** 3, CpuLoadPercent: 94 }),
  });
  assert.equal(snapshot.freePercent, 25);
  assert.equal(snapshot.cpuLoadPercent, 94);
  assert.deepEqual(resourceBlockers(snapshot), ["CPU load 94% > 90%"]);

  const fallback = parseWindowsResourceSnapshot({ system: "not-json", fallbackTotalBytes: 8 * 1024 ** 3, fallbackFreeBytes: 2 * 1024 ** 3 });
  assert.equal(fallback.freePercent, 25);
  assert.equal(fallback.cpuLoadPercent, null);
});

test("Windows resource snapshot queries PowerShell without requiring optional modules", () => {
  const calls = [];
  const snapshot = systemResourceSnapshot({
    platform: "win",
    run: (command, args) => { calls.push({ command, args }); return { status: 0, stdout: '{"TotalBytes":8589934592,"FreeBytes":1073741824,"CpuLoadPercent":42}' }; },
    totalmem: () => 1,
    freemem: () => 1,
  });
  assert.equal(calls[0].command, "powershell.exe");
  assert.deepEqual(calls[0].args.slice(0, 3), ["-NoLogo", "-NoProfile", "-NonInteractive"]);
  assert.equal(snapshot.freePercent, 12.5);
  assert.equal(snapshot.cpuLoadPercent, 42);
  assert.equal(snapshot.thermalLimited, false);
  assert.equal(formatResourceSnapshot(snapshot), "memory-free=12.5% cpu-load=42%");
});

test("nested heavy command preserves ownership without another slot", async () => {
  const code = await runHeavyCommand(["--", process.execPath, "-e", "process.exit(0)"], { env: { ...process.env, RIGHTSUITE_HEAVY_WORK_OWNER: "1" } });
  assert.equal(code, 0);
});

test("owned process-tree termination targets a POSIX group or Windows task tree", () => {
  const signals = [];
  terminateProcessTree(202, { platform: "mac", kill: (pid, signal) => signals.push([pid, signal]), treeAlive: () => true, pause: () => {} });
  assert.deepEqual(signals, [[-202, "SIGTERM"], [-202, "SIGKILL"]]);
  const calls = [];
  terminateProcessTree(303, { platform: "win", run: (command, args) => calls.push([command, args]) });
  assert.deepEqual(calls, [["taskkill", ["/PID", "303", "/T", "/F"]]]);
});

test("slot serializes contenders and removes stale owners", () => {
  const root = mkdtempSync(path.join(os.tmpdir(), "rightsuite-heavy-"));
  const quiet = () => {};
  const snapshot = () => ({ freePercent: 50, swapUsedBytes: 0, swapTotalBytes: 0, thermalLimited: false });
  const first = acquireHeavyWorkSlot({ root, pid: 101, alive: (pid) => pid === 101, snapshot, log: quiet });
  assert.throws(
    () => acquireHeavyWorkSlot({ root, pid: 202, waitMs: 1, pollMs: 1, alive: (pid) => pid === 101, pause: () => {}, snapshot, log: quiet }),
    /holder pid 101/,
  );
  first.release();

  const lockDir = path.join(root, "slot");
  mkdirSync(lockDir);
  writeFileSync(path.join(lockDir, "owner.json"), JSON.stringify({ pid: 303, token: "stale" }));
  const replacement = acquireHeavyWorkSlot({ root, pid: 404, alive: () => false, snapshot, log: quiet });
  replacement.release();
});

test("dead slot owner reaps its recorded matching child tree", () => {
  const root = mkdtempSync(path.join(os.tmpdir(), "rightsuite-heavy-owned-"));
  const killed = [];
  const options = {
    root, snapshot: () => ({ freePercent: 50, thermalLimited: false }), log: () => {},
    alive: (pid) => pid === 202,
    processStartedAt: (pid) => pid === 202 ? 1234 : null,
    terminate: (pid) => killed.push(pid),
    startWatcher: () => {},
  };
  const first = acquireHeavyWorkSlot({ ...options, pid: 101 });
  first.trackChild(202, "cargo");
  assert.equal(JSON.parse(readFileSync(path.join(root, "slot", "child.json"), "utf8")).childPid, 202);
  const replacement = acquireHeavyWorkSlot({ ...options, pid: 404 });
  assert.deepEqual(killed, [202]);
  replacement.release();
});

test("dead slot owner refuses to kill a reused child PID", () => {
  const root = mkdtempSync(path.join(os.tmpdir(), "rightsuite-heavy-reused-"));
  let observedStart = 1234;
  const options = {
    root, snapshot: () => ({ freePercent: 50, thermalLimited: false }), log: () => {},
    alive: (pid) => pid === 202,
    processStartedAt: (pid) => pid === 202 ? observedStart : null,
    terminate: () => assert.fail("reused PID must not be terminated"),
    startWatcher: () => {},
  };
  const first = acquireHeavyWorkSlot({ ...options, pid: 101 });
  first.trackChild(202, "cargo");
  observedStart = 5678;
  assert.throws(() => acquireHeavyWorkSlot({ ...options, pid: 404 }), /identity changed/);
  first.release();
});

test("detached watcher reaps an owned child as soon as its owner disappears", async () => {
  const root = mkdtempSync(path.join(os.tmpdir(), "rightsuite-heavy-watch-"));
  const lockDir = path.join(root, "slot");
  mkdirSync(lockDir);
  writeFileSync(path.join(lockDir, "owner.json"), JSON.stringify({ pid: 101, token: "owned" }));
  writeFileSync(path.join(lockDir, "child.json"), JSON.stringify({ childPid: 202, childStartedAtMs: 1234 }));
  const killed = [];
  const result = await watchOwnedProcessTree(
    { root, token: "owned", ownerPid: 101, childPid: 202, childStartedAtMs: 1234 },
    { alive: (pid) => pid === 202, startedAt: () => 1234, terminate: (pid) => killed.push(pid), pause: async () => {} },
  );
  assert.equal(result, true);
  assert.deepEqual(killed, [202]);
  assert.equal(existsSync(lockDir), false);
});
