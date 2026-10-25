import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, symlinkSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { buildDiskReport, measureScratchUnits, reportDue, scratchOwner } from "./disk-report.mjs";

const T0 = Date.parse("2026-10-23T03:40:00Z");
const DAY = 86400_000;

// A `checkDisk` result as far as the report reads it: one agent-volume floor, the outermost writers and the unmanaged remainder.
function stubCheck({ used, writers, unmanaged, total = 10_000, uncovered = [], over = {} }) {
  const floor = { id: "d", label: "D:", path: "/Volumes/D", freeBytes: total - used, totalBytes: total, thresholdBytes: 2500, warn: total - used < 2500, hardFreeBytes: 2000, hardWarn: false, agentVolume: true };
  const rows = writers.map(({ id, category, bytes }) => ({ id, category: category ?? id, action: "report", matches: 1, bytes, capBytes: over[id] ?? 1 << 40, over: over[id] !== undefined && bytes > over[id] }));
  return {
    schema: 1, platform: "mac", floor, floors: [floor], writers: rows,
    unmanaged: [{ floor: "d", usedBytes: used, writerBytes: used - unmanaged, unmanagedBytes: unmanaged, writers: writers.map(({ id, category, bytes }) => ({ id, category: category ?? id, bytes })), uncoveredTopLevel: uncovered.map((name) => ({ name, path: `/Volumes/D/${name}`, bytes: 1, symlink: false })) }],
  };
}

const policy = { scratchStaleDays: 1, holdingDays: 1 };
const report = (args) => buildDiskReport({ policy, host: "h", moved: 0, purged: 0, ...args });

test("two reports: the category deltas add up to the change in used bytes, the unmanaged remainder included", () => {
  const first = report({ check: stubCheck({ used: 1000, writers: [{ id: "a", bytes: 400 }, { id: "b", bytes: 300 }], unmanaged: 300 }), previous: null, now: T0 });
  assert.equal(first.delta, null);
  assert.deepEqual(first.categories, { a: 400, b: 300, unmanaged: 300 });
  const second = report({ check: stubCheck({ used: 1300, writers: [{ id: "a", bytes: 600 }, { id: "b", bytes: 300 }], unmanaged: 400 }), previous: first, now: T0 + DAY });
  assert.deepEqual(second.delta, { hours: 24, usedBytes: 300, categories: { a: 200, b: 0, unmanaged: 100 } });
  assert.equal(Object.values(second.delta.categories).reduce((a, b) => a + b, 0), second.delta.usedBytes);
  // a category that disappears or appears still balances
  const third = report({ check: stubCheck({ used: 1200, writers: [{ id: "a", bytes: 600 }, { id: "c", bytes: 100 }], unmanaged: 500 }), previous: second, now: T0 + 2 * DAY });
  assert.equal(Object.values(third.delta.categories).reduce((a, b) => a + b, 0), third.delta.usedBytes);
  assert.equal(third.delta.usedBytes, -100);
});

test("writers sharing a category add up under it, and the report falls back to the raw writers when the volume has no unmanaged entry", () => {
  const r = report({ check: stubCheck({ used: 500, writers: [{ id: "agent-tmp", category: "agent-cache", bytes: 100 }, { id: "agent-cache", category: "agent-cache", bytes: 150 }], unmanaged: 250 }), now: T0 });
  assert.deepEqual(r.categories, { "agent-cache": 250, unmanaged: 250 });
  const bare = stubCheck({ used: 500, writers: [{ id: "x", bytes: 5 }], unmanaged: 0 });
  delete bare.unmanaged;
  assert.deepEqual(report({ check: bare, now: T0 }).categories, { x: 5, unmanaged: 0 });
});

test("top growers are the 10 largest positive deltas, with the owner prefix of a scratch run", () => {
  const base = stubCheck({ used: 1000, writers: [{ id: "a", bytes: 400 }], unmanaged: 600 });
  const names = ["legion-1", "legion-2", "legion-3", "legion-4", "legion-5", "legion-6", "rk-1", "rk-2", "rk-3", "rk-4", "rk-5", "rk-6"];
  const before = report({ check: base, now: T0, scratchUnits: names.map((name) => ({ name, bytes: 10 })) });
  const after = report({ check: base, now: T0 + DAY, previous: before, scratchUnits: names.map((name, i) => ({ name, bytes: 10 + (i === 4 ? 0 : (i + 1) * 100) })) });
  assert.equal(after.growers.length, 10);
  // the largest deltas first: rk-6 (index 11) down to legion-3; legion-5 (no growth) and the two smallest do not make it
  assert.deepEqual(after.growers.map((g) => g.id), ["scratch/rk-6", "scratch/rk-5", "scratch/rk-4", "scratch/rk-3", "scratch/rk-2", "scratch/rk-1", "scratch/legion-6", "scratch/legion-4", "scratch/legion-3", "scratch/legion-2"]);
  assert.deepEqual(after.growers[0], { id: "scratch/rk-6", bytes: 1210, deltaBytes: 1200, owner: "rk" });
  assert.equal(after.growers.at(-1).owner, "legion");
  assert.equal(after.growers.every((g) => g.deltaBytes > 0), true);
  assert.deepEqual(after.scratchByOwner, { legion: 1660, rk: 5760 });
  assert.equal(scratchOwner("pulse-run-9"), "pulse");
  assert.equal(scratchOwner("plain"), "plain");
});

test("a unit that is new since the last report counts its whole size as growth; the first report has no growers", () => {
  const check = stubCheck({ used: 1000, writers: [{ id: "a", bytes: 400 }], unmanaged: 600 });
  const first = report({ check, now: T0, scratchUnits: [] });
  assert.deepEqual(first.growers, []);
  const second = report({ check, now: T0 + DAY, previous: first, scratchUnits: [{ name: "hr-new", bytes: 700 }] });
  assert.deepEqual(second.growers, [{ id: "scratch/hr-new", bytes: 700, deltaBytes: 700, owner: "hr" }]);
});

test("units keep the 50 largest so the next report can diff", () => {
  const check = stubCheck({ used: 1000, writers: [{ id: "a", bytes: 400 }], unmanaged: 600 });
  const scratchUnits = Array.from({ length: 80 }, (_, i) => ({ name: `rk-${i}`, bytes: i + 1 }));
  const r = report({ check, now: T0, scratchUnits });
  assert.equal(Object.keys(r.units).length, 50);
  assert.equal(r.units.a, 400);
  assert.equal(r.units["scratch/rk-79"], 80);
  assert.equal(r.units["scratch/rk-0"], undefined);
});

test("steady state is the daily growth times (scratch idle + holding + 1) days", () => {
  const check = (used) => stubCheck({ used, writers: [{ id: "a", bytes: used - 100 }], unmanaged: 100 });
  const first = report({ check: check(1000), now: T0 });
  assert.deepEqual(first.steadyState, { growthBytesPerDay: 0, daysParked: 3, estimateBytes: 0 });
  const second = report({ check: check(1300), previous: first, now: T0 + DAY });
  assert.deepEqual(second.steadyState, { growthBytesPerDay: 300, daysParked: 3, estimateBytes: 900 });
  const slow = buildDiskReport({ check: check(1300), previous: first, now: T0 + DAY, policy: { scratchStaleDays: 3, holdingDays: 3 }, host: "h" });
  assert.deepEqual(slow.steadyState, { growthBytesPerDay: 300, daysParked: 7, estimateBytes: 2100 });
  // a shrinking volume has no growth, and a short interval is not extrapolated past an hour
  assert.equal(report({ check: check(900), previous: first, now: T0 + DAY }).steadyState.growthBytesPerDay, 0);
  assert.equal(report({ check: check(1100), previous: first, now: T0 + 1800_000 }).steadyState.growthBytesPerDay, 100 * 24);
});

test("budgetsOver lists the writers over their cap and the scratch owners over their prefix cap", () => {
  const check = stubCheck({ used: 1000, writers: [{ id: "a", bytes: 400 }, { id: "b", bytes: 300 }], unmanaged: 300, over: { a: 100, b: 1000 } });
  const r = buildDiskReport({ check, now: T0, policy: { ...policy, scratchPrefixCaps: { rk: 50, legion: 1000 } }, host: "h", scratchUnits: [{ name: "rk-a", bytes: 30 }, { name: "rk-b", bytes: 30 }, { name: "legion-a", bytes: 30 }] });
  assert.deepEqual(r.budgetsOver, [{ id: "a", bytes: 400, capBytes: 100 }, { id: "scratch:rk", bytes: 60, capBytes: 50 }]);
});

test("the report's field names are frozen: Pulse's Storage card reads them", () => {
  const check = stubCheck({ used: 1000, writers: [{ id: "a", bytes: 400 }], unmanaged: 600, uncovered: ["zzz"] });
  const first = report({ check, now: T0, moved: 11, purged: 22 });
  const r = report({ check, previous: first, now: T0 + DAY, moved: 11, purged: 22, scratchUnits: [{ name: "rk-x", bytes: 5 }] });
  assert.deepEqual(Object.keys(r), ["ts", "event", "schema", "platform", "host", "freeBytes", "usedBytes", "totalBytes", "floors", "categories", "delta", "units", "growers", "budgetsOver", "unmanagedBytes", "uncoveredTopLevel", "scratchByOwner", "steadyState", "movedBytes", "purgedBytes"]);
  assert.deepEqual([r.event, r.schema, r.platform, r.host, r.ts], ["disk.report", 1, "mac", "h", "2026-10-24T03:40:00.000Z"]);
  assert.deepEqual([r.freeBytes, r.usedBytes, r.totalBytes], [9000, 1000, 10000]);
  assert.deepEqual(r.floors, [{ id: "d", freeBytes: 9000, softBytes: 2500, hardBytes: 2000, warn: false }]);
  assert.deepEqual(Object.keys(r.delta), ["hours", "usedBytes", "categories"]);
  assert.deepEqual(Object.keys(r.steadyState), ["growthBytesPerDay", "daysParked", "estimateBytes"]);
  assert.deepEqual([r.unmanagedBytes, r.uncoveredTopLevel, r.movedBytes, r.purgedBytes], [600, ["zzz"], 11, 22]);
  assert.deepEqual(r.scratchByOwner, { rk: 5 });
  assert.deepEqual(Object.keys(r.growers[0] ?? { id: 0, bytes: 0, deltaBytes: 0, owner: 0 }), ["id", "bytes", "deltaBytes", "owner"]);
});

test("a floor that cannot be read reports null sizes instead of failing the report", () => {
  const check = stubCheck({ used: 1000, writers: [{ id: "a", bytes: 400 }], unmanaged: 600 });
  check.floors = [{ id: "d", label: "D:", path: "/Volumes/D", absent: true, error: "ENOENT", warn: true, agentVolume: true }];
  check.floor = check.floors[0];
  check.unmanaged = [];
  const r = report({ check, now: T0 });
  assert.deepEqual([r.freeBytes, r.usedBytes, r.totalBytes], [null, null, null]);
  assert.deepEqual(r.floors, [{ id: "d", freeBytes: null, softBytes: null, hardBytes: null, warn: true }]);
});

test("reportDue: one report per UTC day", () => {
  assert.equal(reportDue({ lastReportTs: undefined, now: T0 }), true);
  assert.equal(reportDue({ lastReportTs: "garbage", now: T0 }), true);
  assert.equal(reportDue({ lastReportTs: "2026-10-23T00:00:00Z", now: Date.parse("2026-10-23T23:59:59Z") }), false);
  assert.equal(reportDue({ lastReportTs: "2026-10-23T23:59:59Z", now: Date.parse("2026-10-24T00:00:00Z") }), true);
});

test("measureScratchUnits sizes each child of each scratch root once and skips symlinks", () => {
  const base = mkdtempSync(path.join(os.tmpdir(), "rk-report-"));
  const root = path.join(base, "scratch");
  mkdirSync(path.join(root, "rk-a"), { recursive: true });
  mkdirSync(path.join(root, "legion-b"));
  writeFileSync(path.join(root, "loose.txt"), "x");
  symlinkSync(base, path.join(root, "link"));
  const sized = [];
  const units = measureScratchUnits({ policy: { scratchRoots: { mac: [root, path.join(base, "absent")] } }, platform: "darwin", size: (p) => { sized.push(p); return 7; } });
  assert.deepEqual(units.map((u) => [u.name, u.bytes]).sort(), [["legion-b", 7], ["loose.txt", 7], ["rk-a", 7]]);
  assert.equal(sized.length, 3);
});
