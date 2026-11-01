import assert from "node:assert/strict";
import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { mkdtempSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";

import {
  acquireHeavyWorkSlot,
  acquireRunHeavySlot,
  summarizePreBrokerWaits,
  formatResourceSnapshot,
  heavyChildDetached,
  heavySlotDecision,
  heavyCommandEnvironment,
  heavyWorkRoot,
  parseMacResourceSnapshot,
  parseWindowsResourceSnapshot,
  resourceBlockers,
  runHeavyCommand,
  sweepTrackedHeavyWorkOrphan,
  systemResourceSnapshot,
  terminateProcessTree,
  watchOwnedProcessTree,
  withLocalHeavySlot,
} from "./heavy-command.mjs";

test("heavy-work root is machine-wide rather than repository-local", () => {
  assert.equal(heavyWorkRoot({ platform: "mac", home: "/Users/test", env: {} }), "/Users/test/Library/Caches/RightSuite/heavy-work");
  assert.equal(heavyWorkRoot({ platform: "win", env: { LOCALAPPDATA: "C:\\Users\\test\\AppData\\Local" } }), "C:\\Users\\test\\AppData\\Local\\RightSuite\\heavy-work");
});

test("heavy-work root ignores XDG_CACHE_HOME regardless of value", () => {
  assert.equal(
    heavyWorkRoot({ platform: "linux", env: { XDG_CACHE_HOME: "/one/path" }, home: "/home/u" }),
    heavyWorkRoot({ platform: "linux", env: {}, home: "/home/u" }),
  );
  assert.equal(heavyWorkRoot({ platform: "linux", env: {}, home: "/home/u" }), "/home/u/.cache/rightsuite/heavy-work");
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

test("preflight sweep reaps only a tracked child after an owner PID is reused", () => {
  const root = mkdtempSync(path.join(os.tmpdir(), "rightsuite-heavy-preflight-"));
  const lockDir = path.join(root, "slot");
  mkdirSync(lockDir);
  writeFileSync(path.join(lockDir, "owner.json"), JSON.stringify({ pid: 101, ownerStartedAtMs: 100, token: "stale" }));
  writeFileSync(path.join(lockDir, "child.json"), JSON.stringify({ childPid: 202, childStartedAtMs: 200 }));
  const killed = [];
  assert.equal(sweepTrackedHeavyWorkOrphan({
    root,
    alive: (pid) => pid === 101 || pid === 202,
    startedAt: (pid) => pid === 101 ? 5_000 : pid === 202 ? 200 : null,
    terminate: (pid) => killed.push(pid),
    log: () => {},
  }), true);
  assert.deepEqual(killed, [202]);
  assert.equal(existsSync(lockDir), false);
});

test("preflight sweep refuses a reused tracked child PID instead of killing broadly", () => {
  const root = mkdtempSync(path.join(os.tmpdir(), "rightsuite-heavy-preflight-reused-"));
  const lockDir = path.join(root, "slot");
  mkdirSync(lockDir);
  writeFileSync(path.join(lockDir, "owner.json"), JSON.stringify({ pid: 101, ownerStartedAtMs: 100, token: "stale" }));
  writeFileSync(path.join(lockDir, "child.json"), JSON.stringify({ childPid: 202, childStartedAtMs: 200 }));
  assert.throws(() => sweepTrackedHeavyWorkOrphan({
    root,
    alive: (pid) => pid === 101 || pid === 202,
    startedAt: (pid) => pid === 101 ? 5_000 : pid === 202 ? 4_000 : null,
    terminate: () => assert.fail("reused child PID must not be terminated"),
    log: () => {},
  }), /identity changed/);
});

test("preflight sweep refuses a tracked child without a verifiable start identity", () => {
  const root = mkdtempSync(path.join(os.tmpdir(), "rightsuite-heavy-preflight-unverified-"));
  const lockDir = path.join(root, "slot");
  mkdirSync(lockDir);
  writeFileSync(path.join(lockDir, "owner.json"), JSON.stringify({ pid: 101, ownerStartedAtMs: 100, token: "stale" }));
  writeFileSync(path.join(lockDir, "child.json"), JSON.stringify({ childPid: 202, childStartedAtMs: 200 }));
  assert.throws(() => sweepTrackedHeavyWorkOrphan({
    root,
    alive: (pid) => pid === 101 || pid === 202,
    startedAt: (pid) => pid === 101 ? 5_000 : null,
    terminate: () => assert.fail("unverified child PID must not be terminated"),
    log: () => {},
  }), /identity changed/);
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

test("slot records wait attribution: holder identity, slot ms and resource blockers", () => {
  const root = mkdtempSync(path.join(os.tmpdir(), "rightsuite-heavy-waits-"));
  const quiet = () => {};
  const good = { freePercent: 50, swapUsedBytes: 0, swapTotalBytes: 0, thermalLimited: false };
  const first = acquireHeavyWorkSlot({ root, pid: 101, argv: ["node", "right-release", "build"], alive: (pid) => pid === 101, snapshot: () => good, log: quiet });
  assert.equal(first.waits.slotWaitMs >= 0, true);
  assert.deepEqual(first.waits.slotBlockers, []);
  let snapshots = 0;
  const second = acquireHeavyWorkSlot({
    root, pid: 202, waitMs: 5000, pollMs: 1, alive: (pid) => pid === 101 || pid === 202,
    pause: () => { first.release(); },
    snapshot: () => (snapshots++ === 0 ? { ...good, freePercent: 1 } : good), log: quiet,
  });
  assert.equal(second.waits.slotBlockers[0].pid, 101);
  assert.match(second.waits.slotBlockers[0].argv, /right-release build/);
  assert.equal(second.waits.resourceBlockers.length > 0, true);
  const summary = summarizePreBrokerWaits({ heavySlot: second, suiteSlot: { waits: { waitMs: 7, blockers: ["suite build slot active pid 9"] } } });
  assert.equal(summary.layers.length, 2);
  assert.match(summary.summary, /heavy-work-slot=.*blocked-by pid 101/);
  assert.match(summary.summary, /suite-build-slot=7ms blocked-by suite build slot active pid 9/);
  assert.equal(summarizePreBrokerWaits({ heavyOwnerInherited: true }).layers[0].skipped, "slot owned by parent heavy-command");
  second.release();
});

test("slot decision: a broker host's run holds no slot but has local steps; a direct host's run holds it; an inherited slot is never taken twice (rightkit#1367 step 2)", () => {
  const broker = heavySlotDecision({ env: {}, brokerHost: true });
  assert.deepEqual([broker.take, broker.localSteps, broker.inherited], [false, true, false]);
  assert.match(broker.skipReason, /broker host: build-launch makes the build steps broker jobs.*held only around the release's own local steps/);
  const direct = heavySlotDecision({ env: {}, brokerHost: false });
  assert.deepEqual([direct.take, direct.localSteps, direct.skipReason], [true, false, null]);
  for (const brokerHost of [true, false]) {
    const inherited = heavySlotDecision({ env: { RIGHTSUITE_HEAVY_WORK_OWNER: "1" }, brokerHost });
    assert.deepEqual([inherited.take, inherited.localSteps, inherited.inherited, inherited.skipReason], [false, false, true, null], `brokerHost ${brokerHost}`);
  }
  const summary = summarizePreBrokerWaits({ heavySkipped: broker.skipReason });
  assert.equal(summary.layers[0].skipped, broker.skipReason);
  assert.match(summary.summary, /heavy-work-slot=0ms \(broker host/);
});

test("acquireRunHeavySlot: a broker-host run never creates the slot directory; a direct-host run (a Swift build included) takes it and releases it", () => {
  const good = { freePercent: 50, swapUsedBytes: 0, swapTotalBytes: 0, thermalLimited: false };
  const slotOptions = (root) => ({ root, snapshot: () => good, log: () => {} });
  const brokerRoot = mkdtempSync(path.join(os.tmpdir(), "rightsuite-heavy-run-broker-"));
  const broker = acquireRunHeavySlot({ env: {}, brokerHost: true, ...slotOptions(brokerRoot) });
  assert.equal(broker.slot, null);
  assert.equal(existsSync(path.join(brokerRoot, "slot")), false, "no slot for a run whose builds are the broker's");
  const directRoot = mkdtempSync(path.join(os.tmpdir(), "rightsuite-heavy-run-direct-"));
  const direct = acquireRunHeavySlot({ env: {}, brokerHost: false, ...slotOptions(directRoot) });
  assert.ok(direct.slot);
  assert.equal(existsSync(path.join(directRoot, "slot")), true, "a direct host's run holds the slot");
  direct.slot.release();
  assert.equal(existsSync(path.join(directRoot, "slot")), false);
  const inheritedRoot = mkdtempSync(path.join(os.tmpdir(), "rightsuite-heavy-run-inherited-"));
  assert.equal(acquireRunHeavySlot({ env: { RIGHTSUITE_HEAVY_WORK_OWNER: "1" }, brokerHost: false, ...slotOptions(inheritedRoot) }).slot, null);
  assert.equal(existsSync(path.join(inheritedRoot, "slot")), false);
});

test("withLocalHeavySlot: on a broker host the slot is held around just the local step; elsewhere the step runs as it is", async () => {
  const good = { freePercent: 50, swapUsedBytes: 0, swapTotalBytes: 0, thermalLimited: false };
  const root = mkdtempSync(path.join(os.tmpdir(), "rightsuite-heavy-local-"));
  const slotDir = path.join(root, "slot");
  const options = { root, snapshot: () => good, log: () => {} };
  const told = [];
  const seen = [];
  const out = await withLocalHeavySlot(heavySlotDecision({ env: {}, brokerHost: true }), async (slot) => { seen.push(existsSync(slotDir)); assert.ok(slot); return "installed"; }, { ...options, onSlot: (slot) => told.push(slot === null ? "end" : "start") });
  assert.equal(out, "installed");
  assert.deepEqual(seen, [true], "held while the local step runs");
  assert.deepEqual(told, ["start", "end"]);
  assert.equal(existsSync(slotDir), false, "released when the step ends, also by a throw");
  await assert.rejects(withLocalHeavySlot(heavySlotDecision({ env: {}, brokerHost: true }), async () => { throw new Error("install failed"); }, options), /install failed/);
  assert.equal(existsSync(slotDir), false);
  for (const decision of [heavySlotDecision({ env: {}, brokerHost: false }), heavySlotDecision({ env: { RIGHTSUITE_HEAVY_WORK_OWNER: "1" }, brokerHost: true })]) {
    let sawSlot = null;
    await withLocalHeavySlot(decision, async (slot) => { sawSlot = slot ?? (existsSync(slotDir) ? "dir" : null); }, options);
    assert.equal(sawSlot, null, "no extra slot for a run that holds or inherits one");
  }
});

test("runHeavyCommand still takes the slot around its command, and a parent that owns one takes none (unchanged by rightkit#1367 step 2)", async () => {
  const root = mkdtempSync(path.join(os.tmpdir(), "rightsuite-heavy-runheavy-"));
  const slotDir = path.join(root, "slot");
  const saved = { root: process.env.RIGHTSUITE_HEAVY_WORK_ROOT, mem: process.env.BUILD_GUARD_MIN_MEMORY_FREE_PERCENT, cpu: process.env.BUILD_GUARD_MAX_CPU_LOAD_PERCENT };
  process.env.RIGHTSUITE_HEAVY_WORK_ROOT = root; process.env.BUILD_GUARD_MIN_MEMORY_FREE_PERCENT = "0"; process.env.BUILD_GUARD_MAX_CPU_LOAD_PERCENT = "100";
  try {
    // The child exits 0 when the slot directory exists while it runs, 7 when it does not.
    const probe = ["--", process.execPath, "-e", "process.exit(require('node:fs').existsSync(process.argv[1]) ? 0 : 7)", slotDir];
    const { RIGHTSUITE_HEAVY_WORK_OWNER: _owner, ...clean } = process.env;
    assert.equal(await runHeavyCommand(probe, { env: clean }), 0, "the command ran while this process held the slot");
    assert.equal(existsSync(slotDir), false, "and released it");
    assert.equal(await runHeavyCommand(probe, { env: { ...clean, RIGHTSUITE_HEAVY_WORK_OWNER: "1" } }), 7, "a parent that owns a slot: none is taken");
    assert.equal(existsSync(slotDir), false);
  } finally {
    for (const [key, name] of [["root", "RIGHTSUITE_HEAVY_WORK_ROOT"], ["mem", "BUILD_GUARD_MIN_MEMORY_FREE_PERCENT"], ["cpu", "BUILD_GUARD_MAX_CPU_LOAD_PERCENT"]]) { if (saved[key] === undefined) delete process.env[name]; else process.env[name] = saved[key]; }
  }
});
