import { test } from "node:test";
import assert from "node:assert/strict";
import { BUILT_BINS_RECORD, builtBinsOfRun } from "./gate-stages.mjs";

// A target directory as a map file -> bytes, and compiledBinaries' answer for it.
const disk = (files) => (builtBins) => Object.entries(builtBins).flatMap(([dir, { profile, bins }]) => bins.map((bin) => { const f = `${dir}/${profile}/${bin}.exe`; return { dir, path: `${profile}/${bin}.exe`, name: `${bin}.exe`, size: f in files ? files[f].length : null, sha256: f in files ? `sha-of-${files[f]}` : null }; }));
const memory = () => { const m = new Map(); return { m, read: (k) => (m.has(k) ? { key: k, row: m.get(k) } : null), write: (k, row) => m.set(k, row) }; };
const units = { service: { bins: ["rustright-watchdog"], testLinksBins: true }, broker: { bins: ["rustright-broker"], testLinksBins: true }, lib: { bins: [], testLinksBins: true }, noTest: { bins: ["tool"], testLinksBins: false } };
const unitOf = (r) => units[String(r.id).split(":")[2]];
const keyOf = (r) => `key-${r.id}`;
const T = "x86_64-pc-windows-msvc/release";
const place = () => ({ dir: "D:/out", profile: T });

test("a unit answered from the cache still has its program in the receipt, while the program on disk is the one it linked", () => {
  const store = memory(); const files = { [`D:/out/${T}/rustright-watchdog.exe`]: "W1", [`D:/out/${T}/rustright-broker.exe`]: "B1" };
  // Run 1: both units run. Both programs are vouched for and their digests remembered.
  const first = builtBinsOfRun({ rows: [{ id: "test:rust:service", status: "pass" }, { id: "test:rust:broker", status: "pass" }], unitOf, placeOf: place, keyOf, store, binaries: disk(files) });
  assert.deepEqual(first, { "D:/out": { profile: T, bins: ["rustright-broker", "rustright-watchdog"] } });
  assert.equal(store.m.get("key-test:rust:service").schema, BUILT_BINS_RECORD);
  assert.deepEqual(store.m.get("key-test:rust:service").sha256, { "rustright-watchdog.exe": "sha-of-W1" });

  // Run 2 (the commit that could not be installed from): the service unit is answered from the cache, the broker's ran. Before: only the broker.
  const notes = [];
  const second = builtBinsOfRun({ rows: [{ id: "test:rust:service", status: "pass", cached: true }, { id: "test:rust:broker", status: "pass" }], unitOf, placeOf: place, keyOf, store, binaries: disk(files), note: (n) => notes.push(n) });
  assert.deepEqual(second, { "D:/out": { profile: T, bins: ["rustright-broker", "rustright-watchdog"] } });
  assert.deepEqual(notes, []);
});

test("a cached unit vouches for nothing when its program was built over, is gone, was never recorded, or the unit now has other programs", () => {
  const rows = [{ id: "test:rust:service", status: "pass", cached: true }];
  const seeded = () => { const store = memory(); builtBinsOfRun({ rows: [{ id: "test:rust:service", status: "pass" }], unitOf, placeOf: place, keyOf, store, binaries: disk({ [`D:/out/${T}/rustright-watchdog.exe`]: "W1" }) }); return store; };
  const run = (store, files, uo = unitOf) => { const notes = []; return { built: builtBinsOfRun({ rows, unitOf: uo, placeOf: () => { throw new Error("a cached row has no place of its own"); }, keyOf, store, binaries: disk(files), note: (n) => notes.push(n) }), notes }; };
  // Another commit built over it.
  let r = run(seeded(), { [`D:/out/${T}/rustright-watchdog.exe`]: "W2" }); assert.deepEqual(r.built, {}); assert.match(r.notes[0], /rustright-watchdog\.exe.*not the bytes it linked/);
  // Gone.
  r = run(seeded(), {}); assert.deepEqual(r.built, {}); assert.match(r.notes[0], /missing or is not the bytes/);
  // Never recorded (a pass stored by an older gate).
  r = run(memory(), { [`D:/out/${T}/rustright-watchdog.exe`]: "W1" }); assert.deepEqual(r.built, {}); assert.match(r.notes[0], /no record of the programs it linked/);
  // The unit has a program the record does not name.
  r = run(seeded(), { [`D:/out/${T}/rustright-watchdog.exe`]: "W1" }, () => ({ bins: ["rustright-watchdog", "extra"], testLinksBins: true })); assert.deepEqual(r.built, {}); assert.match(r.notes[0], /names other programs/);
  // A stored record of another shape is not trusted.
  const odd = memory(); odd.write("key-test:rust:service", { schema: "something/0", dir: "D:/out", profile: T, bins: ["rustright-watchdog"], sha256: { "rustright-watchdog.exe": "sha-of-W1" } });
  r = run(odd, { [`D:/out/${T}/rustright-watchdog.exe`]: "W1" }); assert.deepEqual(r.built, {});
});

test("what vouched for nothing before still does: a failed row, a row that is no test row, a unit without programs or without a linking test, a row with no place", () => {
  const store = memory(); const files = { [`D:/out/${T}/rustright-broker.exe`]: "B1", [`D:/out/${T}/tool.exe`]: "T1" };
  const built = builtBinsOfRun({ rows: [{ id: "test:rust:broker", status: "fail" }, { id: "clippy:rust:broker", status: "pass" }, { id: "test:rust:lib", status: "pass" }, { id: "test:rust:noTest", status: "pass" }], unitOf, placeOf: place, keyOf, store, binaries: disk(files) });
  assert.deepEqual(built, {}); assert.equal(store.m.size, 0);
  assert.deepEqual(builtBinsOfRun({ rows: [{ id: "test:rust:broker", status: "pass" }], unitOf, placeOf: () => ({ dir: "D:/out", profile: null }), keyOf, store, binaries: disk(files) }), {});
  // A run whose program is not on disk afterwards is vouched for as before (its digest reads null in the receipt) but nothing is remembered for later.
  assert.deepEqual(builtBinsOfRun({ rows: [{ id: "test:rust:service", status: "pass" }], unitOf, placeOf: place, keyOf, store, binaries: disk(files) }), { "D:/out": { profile: T, bins: ["rustright-watchdog"] } });
  assert.equal(store.m.size, 0);
  // Two rows that name one directory with different profiles vouch for nothing there.
  const two = builtBinsOfRun({ rows: [{ id: "test:rust:broker", status: "pass" }, { id: "test:rust:service", status: "pass" }], unitOf, placeOf: (r) => ({ dir: "D:/out", profile: r.id.endsWith("broker") ? T : "debug" }), keyOf: () => null, store, binaries: disk(files) });
  assert.deepEqual(two, { "D:/out": { profile: null, bins: [] } });
});
