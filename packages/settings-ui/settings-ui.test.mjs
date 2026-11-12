// Runs against the built `dist` (the `test` script builds first): the store and the shell adapter need no React.
import assert from "node:assert/strict";
import { test } from "node:test";
import { createSettingsStore, sharedSettingsStore } from "./dist/store.js";
import { EVENTS, METHODS, createShellBackends } from "./dist/shell-adapter.js";

const tick = () => new Promise((r) => setTimeout(r, 0));

function fakeBackend(initial, { failPatch = false } = {}) {
  let doc = { ...initial };
  let rev = 1;
  const b = {
    loads: 0,
    async load() {
      b.loads += 1;
      return { values: { ...doc }, revision: rev };
    },
    async patch(p) {
      if (failPatch) throw new Error("disk full");
      for (const [k, v] of Object.entries(p)) v === null ? delete doc[k] : (doc[k] = v);
      rev += 1;
      return { values: { ...doc }, revision: rev };
    },
  };
  return b;
}

test("one store per backend: one load, shared values, no backend.subscribe needed", async () => {
  const backend = fakeBackend({ a: 1 });
  const s1 = sharedSettingsStore(backend);
  const s2 = sharedSettingsStore(backend);
  assert.equal(s1, s2);
  await Promise.all([s1.refresh(), s2.refresh(), s1.refresh()]);
  assert.equal(backend.loads, 1);
  await s1.write({ a: 2 });
  assert.equal(s2.getState().values.a, 2);
  assert.notEqual(sharedSettingsStore(fakeBackend({})), s1);
});

test("a revision may be a string", async () => {
  const backend = { load: async () => ({ values: {}, revision: "18446744073709551615" }), patch: async (p) => ({ values: p, revision: "18446744073709551614" }) };
  const s = createSettingsStore(backend);
  await s.load();
  assert.equal(s.revision(), "18446744073709551615");
});

test("two queued writes to one key that both fail leave the stored value, and the queue draining reloads", async () => {
  const backend = fakeBackend({ k: "stored" }, { failPatch: true });
  const s = createSettingsStore(backend);
  await s.load();
  const loadsBefore = backend.loads;
  const [r1, r2] = await Promise.all([s.write({ k: "first" }), s.write({ k: "second" })]);
  assert.equal(r1.ok, false);
  assert.equal(r2.ok, false);
  assert.equal(s.getState().values.k, "stored");
  await tick();
  assert.ok(backend.loads > loadsBefore, "reloaded from the backend");
  assert.equal(s.getState().saving, false);
});

test("a failed write does not undo a later queued write that owns the key", async () => {
  const backend = fakeBackend({ k: "stored" });
  const realPatch = backend.patch;
  let n = 0;
  backend.patch = (p, r) => (n++ === 0 ? Promise.reject(new Error("no")) : realPatch(p, r));
  const s = createSettingsStore(backend);
  await s.load();
  const [r1, r2] = await Promise.all([s.write({ k: "first" }), s.write({ k: "second" })]);
  assert.equal(r1.ok, false);
  assert.equal(r2.ok, true);
  await tick();
  assert.equal(s.getState().values.k, "second");
});

function fakeTransport() {
  const calls = [];
  const listeners = new Set();
  let listens = 0;
  return {
    calls,
    get listens() {
      return listens;
    },
    emit: (keys) => listeners.forEach((l) => l({ keys })),
    invoke: async (command) => void calls.push(command),
    listen: async (event, handler) => {
      assert.equal(event, EVENTS.settingsChanged);
      listens += 1;
      listeners.add(handler);
      return () => listeners.delete(handler);
    },
  };
}

test("shell adapter: the host is unsubscribed only when the last handler leaves", async () => {
  const t = fakeTransport();
  const { settings } = createShellBackends(t);
  const seenA = [];
  const seenB = [];
  const [offA, offB] = await Promise.all([settings.subscribe((k) => seenA.push(k)), settings.subscribe((k) => seenB.push(k))]);
  assert.equal(t.listens, 1);
  assert.equal(t.calls.filter((c) => c === METHODS.settingsSubscribe).length, 1);
  t.emit(["x"]);
  assert.deepEqual([seenA, seenB], [[["x"]], [["x"]]]);
  offA();
  offA();
  await tick();
  assert.equal(t.calls.includes(METHODS.settingsUnsubscribe), false);
  t.emit(["y"]);
  assert.deepEqual(seenB, [["x"], ["y"]]);
  assert.equal(seenA.length, 1);
  offB();
  await tick();
  assert.equal(t.calls.filter((c) => c === METHODS.settingsUnsubscribe).length, 1);
  // A later subscriber connects afresh.
  await settings.subscribe(() => {});
  assert.equal(t.calls.filter((c) => c === METHODS.settingsSubscribe).length, 2);
});
