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
  assert.deepEqual(second.delta, { hours: 24, usedBytes: 300, categories: { a: 200, b: 0, unmanaged: 100 }, overlapBytes: 0 });
  assert.equal(Object.values(second.delta.categories).reduce((a, b) => a + b, 0) - second.delta.overlapBytes, second.delta.usedBytes);
  // a category that disappears or appears still balances
  const third = report({ check: stubCheck({ used: 1200, writers: [{ id: "a", bytes: 600 }, { id: "c", bytes: 100 }], unmanaged: 500 }), previous: second, now: T0 + 2 * DAY });
  assert.equal(Object.values(third.delta.categories).reduce((a, b) => a + b, 0) - third.delta.overlapBytes, third.delta.usedBytes);
  assert.equal(third.delta.usedBytes, -100);
});

test("writers sharing a category add up under it, and the report falls back to the raw writers when the volume has no unmanaged entry", () => {
  const r = report({ check: stubCheck({ used: 500, writers: [{ id: "agent-tmp", category: "agent-cache", bytes: 100 }, { id: "agent-cache", category: "agent-cache", bytes: 150 }], unmanaged: 250 }), now: T0 });
  assert.deepEqual(r.categories, { "agent-cache": 250, unmanaged: 250 });
  const bare = stubCheck({ used: 500, writers: [{ id: "x", bytes: 5 }], unmanaged: 0 });
  delete bare.unmanaged;
  assert.deepEqual(report({ check: bare, now: T0 }).categories, { x: 5, unmanaged: 495 }); // the remainder is what the writers do not hold
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
  assert.deepEqual(Object.keys(r), ["ts", "event", "schema", "platform", "host", "freeBytes", "usedBytes", "totalBytes", "floors", "categories", "delta", "units", "growers", "budgetsOver", "unmanagedBytes", "overlapBytes", "uncoveredTopLevel", "scratchByOwner", "steadyState", "movedBytes", "purgedBytes"]);
  assert.deepEqual([r.event, r.schema, r.platform, r.host, r.ts], ["disk.report", 1, "mac", "h", "2026-10-24T03:40:00.000Z"]);
  assert.deepEqual([r.freeBytes, r.usedBytes, r.totalBytes], [9000, 1000, 10000]);
  assert.deepEqual(r.floors, [{ id: "d", freeBytes: 9000, softBytes: 2500, hardBytes: 2000, warn: false }]);
  assert.deepEqual(Object.keys(r.delta), ["hours", "usedBytes", "categories", "overlapBytes"]);
  assert.deepEqual(Object.keys(r.steadyState), ["growthBytesPerDay", "daysParked", "estimateBytes"]);
  assert.deepEqual([r.unmanagedBytes, r.overlapBytes, r.uncoveredTopLevel, r.movedBytes, r.purgedBytes], [600, 0, ["zzz"], 11, 22]);
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

const sum = (categories) => Object.values(categories).reduce((a, b) => a + b, 0);

test("writers that add up to more than the used bytes (APFS clones counted twice) report the overlap, and the identity holds both ways", () => {
  // du sums allocated bytes per writer: two writers can both count a cloned file, so 3100 of writer bytes sit on a 3000-byte volume
  const cloned = report({ check: stubCheck({ used: 3000, writers: [{ id: "claude", bytes: 2000 }, { id: "models", bytes: 1100 }], unmanaged: 0 }), now: T0 });
  assert.deepEqual(cloned.categories, { claude: 2000, models: 1100, unmanaged: 0 }); // nothing is scaled and the remainder is never negative
  assert.equal(cloned.overlapBytes, 100);
  assert.equal(cloned.unmanagedBytes, 0);
  assert.equal(sum(cloned.categories) - cloned.overlapBytes, cloned.usedBytes); // categories - overlap = used
  const plain = report({ check: stubCheck({ used: 3000, writers: [{ id: "claude", bytes: 2000 }, { id: "models", bytes: 800 }], unmanaged: 200 }), now: T0 });
  assert.equal(plain.overlapBytes, 0);
  assert.equal(sum(plain.categories) - plain.overlapBytes, plain.usedBytes);
  // the change between two reports balances in each direction: into an overlap, out of one, and across one
  const into = report({ check: stubCheck({ used: 3100, writers: [{ id: "claude", bytes: 2000 }, { id: "models", bytes: 1500 }], unmanaged: 0 }), previous: plain, now: T0 + DAY });
  assert.equal(into.overlapBytes, 400);
  assert.equal(into.delta.overlapBytes, 400);
  assert.equal(sum(into.delta.categories) - into.delta.overlapBytes, into.delta.usedBytes);
  assert.equal(into.delta.usedBytes, 100);
  const out = report({ check: stubCheck({ used: 3000, writers: [{ id: "claude", bytes: 2000 }, { id: "models", bytes: 800 }], unmanaged: 200 }), previous: into, now: T0 + 2 * DAY });
  assert.equal(out.delta.overlapBytes, -400);
  assert.equal(sum(out.delta.categories) - out.delta.overlapBytes, out.delta.usedBytes);
  assert.equal(out.delta.usedBytes, -100);
  const across = report({ check: stubCheck({ used: 3000, writers: [{ id: "claude", bytes: 2000 }, { id: "models", bytes: 1100 }], unmanaged: 0 }), previous: cloned, now: T0 + DAY });
  assert.equal(across.delta.overlapBytes, 0);
  assert.equal(sum(across.delta.categories) - across.delta.overlapBytes, across.delta.usedBytes);
  assert.equal(across.delta.usedBytes, 0);
});

// The reviewer's scenario: D: is not mounted on day 1 (only the internal drive can be read), mounted on day 2 and 3.
const internal = { id: "mac-internal", label: "Mac internal", path: "/System/Volumes/Data", totalBytes: 500e9, freeBytes: 100e9, thresholdBytes: 42e9, warn: false, holdingSource: true };
const dAbsent = { id: "mac-d", label: "Mac D:", path: "/Volumes/D", absent: true, warn: true, agentVolume: true, error: "ENOENT" };
const dOk = (used) => ({ id: "mac-d", label: "Mac D:", path: "/Volumes/D", totalBytes: 4000e9, freeBytes: 4000e9 - used, thresholdBytes: 268e9, warn: false, agentVolume: true });
const withFloors = (floors, unmanaged = []) => ({ platform: "mac", floors, floor: floors[0], writers: [], unmanaged, warnings: [] });
const dUnmanaged = (used) => [{ floor: "mac-d", usedBytes: used, writerBytes: used, unmanagedBytes: 0, writers: [{ id: "claude-workspace", category: "claude-workspace", bytes: used }], uncoveredTopLevel: [] }];

test("an unreadable agent volume is never replaced by another volume: sizes, categories and delta are null, and the next report has no delta either", () => {
  const day1 = report({ check: withFloors([internal, dAbsent]), now: T0 });
  assert.deepEqual([day1.freeBytes, day1.usedBytes, day1.totalBytes], [null, null, null]); // not the internal drive's 400 GB
  assert.equal(day1.categories, null);
  assert.equal(day1.unmanagedBytes, null);
  assert.equal(day1.overlapBytes, null);
  assert.equal(day1.delta, null);
  assert.deepEqual(day1.growers, []);
  assert.deepEqual(day1.steadyState, { growthBytesPerDay: 0, daysParked: 3, estimateBytes: 0 });
  assert.deepEqual(day1.uncoveredTopLevel, []);
  assert.deepEqual(day1.floors.map((f) => [f.id, f.freeBytes, f.warn]), [["mac-internal", 100e9, false], ["mac-d", null, true]]); // the floors keep telling the truth per volume
  // D: is back: the first readable report has a real size but nothing to compare it with (the previous one had no agent volume)
  const day2 = report({ check: withFloors([internal, dOk(3000e9)], dUnmanaged(3000e9)), previous: day1, now: T0 + DAY });
  assert.deepEqual([day2.freeBytes, day2.usedBytes, day2.totalBytes], [1000e9, 3000e9, 4000e9]);
  assert.deepEqual(day2.categories, { "claude-workspace": 3000e9, unmanaged: 0 });
  assert.equal(day2.delta, null); // without the fix: a delta of +3000 GB against the internal drive's 400 GB
  assert.deepEqual(day2.growers, []);
  assert.deepEqual(day2.steadyState, { growthBytesPerDay: 0, daysParked: 3, estimateBytes: 0 }); // and no growth rate of 2600 GB a day
  // the report after that compares two agent-volume reports again
  const day3 = report({ check: withFloors([internal, dOk(3100e9)], dUnmanaged(3100e9)), previous: day2, now: T0 + 2 * DAY });
  assert.equal(day3.delta.usedBytes, 100e9);
  assert.equal(sum(day3.delta.categories) - day3.delta.overlapBytes, day3.delta.usedBytes);
  // and an unreadable day in the middle breaks the chain on both sides
  const gap = report({ check: withFloors([internal, dAbsent]), previous: day3, now: T0 + 3 * DAY });
  assert.equal(gap.delta, null);
  assert.equal(report({ check: withFloors([internal, dOk(3100e9)], dUnmanaged(3100e9)), previous: gap, now: T0 + 4 * DAY }).delta, null);
  // a policy that flags no agent volume at all still reports its first readable floor
  const plain = { ...internal, holdingSource: undefined };
  const solo = report({ check: withFloors([plain, { ...plain, id: "other", freeBytes: 1 }]), now: T0 });
  assert.deepEqual([solo.usedBytes, solo.freeBytes], [400e9, 100e9]);
  assert.equal(solo.delta, null);
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
