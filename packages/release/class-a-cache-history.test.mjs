import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { test } from "node:test";
import { openClassAHistory, openClassBHistory } from "./class-a-cache-history.mjs";

function location(t) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "class-a-history-"));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  return { storeDir: path.join(root, "cache"), historyRoot: path.join(root, "history") };
}

function fingerprint(id, input) {
  const key = createHash("sha256").update(id + input).digest("hex");
  return { id, key, failureKey: createHash("sha256").update(key + "fail").digest("hex"), inputs: input, toolVersion: "v1", environment: "env1" };
}

test("prior verified results independently determine warm, changed, reverted & missing-store forecasts", (t) => {
  const dirs = location(t);
  const cold = openClassAHistory(dirs);
  const first = fingerprint("license", "source-a");
  const changed = fingerprint("license", "source-b");
  const red = fingerprint("secret-scan", "source-a");
  assert.deepEqual(cold.forecast([first, red]).map((r) => r.expected), ["run", "run"]);
  cold.record({ fingerprints: [first, red], mismatches: [], retained: [{ id: first.id, key: first.key, status: "pass" }, { id: red.id, key: red.failureKey, status: "fail" }] });
  const warm = openClassAHistory(dirs);
  assert.deepEqual(warm.forecast([first, red]).map((r) => [r.expected, r.status]), [["hit", "pass"], ["hit", "fail"]]);
  assert.equal(warm.forecast([changed])[0].expected, "run");
  warm.record({ fingerprints: [changed], mismatches: [], retained: [{ id: changed.id, key: changed.key, status: "pass" }] });
  fs.rmdirSync(dirs.storeDir);
  assert.deepEqual(openClassAHistory(dirs).forecast([first, changed]).map((r) => r.expected), ["hit", "hit"]);
});

test("existing cache without history & corrupted history cannot declare a cold run", (t) => {
  const dirs = location(t);
  fs.mkdirSync(dirs.storeDir);
  assert.throws(() => openClassAHistory(dirs), /existing store/);
  fs.rmdirSync(dirs.storeDir);
  const history = openClassAHistory(dirs);
  fs.writeFileSync(history.file, JSON.stringify({ body: "{}", sha256: "invalid" }));
  assert.throws(() => openClassAHistory(dirs), /checksum/);
});

test("concurrent scoped runs retain both records & refusal cannot change history", (t) => {
  const dirs = location(t);
  const a = openClassAHistory(dirs);
  const b = openClassAHistory(dirs);
  for (const [history, name] of [[a, "one"], [b, "two"]]) {
    const row = fingerprint(name, "v1");
    history.record({ fingerprints: [row], retained: [{ id: name, status: "pass", key: row.key }], mismatches: [] });
  }
  assert.throws(() => b.record({ mismatches: ["unexpected miss"] }), /refused/);
  assert.deepEqual(openClassAHistory(dirs).forecast(["one", "two"].map((id) => fingerprint(id, "v1"))).map((r) => r.expected), ["hit", "hit"]);
});

test("first-use store seals independent Class A & B histories in one file", (t) => {
  const dirs = location(t);
  const a = openClassAHistory(dirs);
  const b = openClassBHistory(dirs);
  assert.equal(a.file, b.file);
  const row = fingerprint("group:shared", "source-a");
  a.record({ fingerprints: [row], retained: [{ id: row.id, key: row.key, status: "pass" }], mismatches: [] });
  assert.equal(openClassAHistory(dirs).forecast([row])[0].expected, "hit");
  assert.equal(b.forecast([row])[0].expected, "run");
  b.record({ fingerprints: [row], retained: [{ id: row.id, key: row.failureKey, status: "fail" }], mismatches: [] });
  assert.equal(openClassBHistory(dirs).forecast([row])[0].status, "fail");
  assert.equal(openClassAHistory(dirs).forecast([row])[0].status, "pass");
});

test("Class B first use also seals Class A & interleaved writers retain every namespace", (t) => {
  const dirs = location(t);
  const b = openClassBHistory(dirs);
  const a = openClassAHistory(dirs);
  const b2 = openClassBHistory(dirs);
  for (const [history, id] of [[b, "batch:one"], [a, "scan"], [b2, "batch:two"]]) {
    const row = fingerprint(id, "source-a");
    history.record({ fingerprints: [row], retained: [{ id, key: row.key, status: "pass" }], mismatches: [] });
  }
  assert.equal(openClassAHistory(dirs).forecast([fingerprint("scan", "source-a")])[0].expected, "hit");
  assert.deepEqual(openClassBHistory(dirs).forecast(["batch:one", "batch:two"].map((id) => fingerprint(id, "source-a"))).map((row) => row.expected), ["hit", "hit"]);
  assert.throws(() => b.record({ mismatches: ["unexpected hit"] }), /refused Class B/);
});

test("legacy Class A history stays readable without inventing Class B evidence", (t) => {
  const dirs = location(t);
  const a = openClassAHistory(dirs);
  const row = fingerprint("legacy", "source-a");
  a.record({ fingerprints: [row], retained: [{ id: row.id, key: row.key, status: "pass" }], mismatches: [] });
  const document = JSON.parse(JSON.parse(fs.readFileSync(a.file, "utf8")).body);
  const body = JSON.stringify({ schema: 1, namespace: document.namespace, rows: document.stages.A });
  fs.writeFileSync(a.file, JSON.stringify({ body, sha256: createHash("sha256").update(body).digest("hex") }));
  const legacy = fs.readFileSync(a.file, "utf8");
  assert.equal(openClassAHistory(dirs).forecast([row])[0].expected, "hit");
  assert.throws(() => openClassBHistory(dirs), /Class B cache history missing.*legacy store/);
  assert.equal(fs.readFileSync(a.file, "utf8"), legacy);
  const changed = fingerprint("legacy", "source-b");
  openClassAHistory(dirs).record({ fingerprints: [changed], retained: [{ id: changed.id, key: changed.key, status: "pass" }], mismatches: [] });
  assert.equal(JSON.parse(JSON.parse(fs.readFileSync(a.file, "utf8")).body).schema, 1);
  assert.throws(() => openClassBHistory(dirs), /legacy store/);
});

test("Class B refuses lost history on populated or empty existing stores", (t) => {
  const dirs = location(t);
  fs.mkdirSync(dirs.storeDir);
  assert.throws(() => openClassBHistory(dirs), /Class B cache history missing.*existing store/);
  fs.writeFileSync(path.join(dirs.storeDir, "live-result.json"), JSON.stringify({ status: "pass" }));
  assert.throws(() => openClassBHistory(dirs), /existing store/);
});

test("Class B handles uncacheable rows, groups, changed & reverted keys", (t) => {
  const dirs = location(t);
  const cold = openClassBHistory(dirs);
  assert.deepEqual(cold.forecast([{ id: "test:uncacheable", uncacheableReason: "runtime test" }]), [{ id: "test:uncacheable", expected: "uncacheable", reason: "runtime test" }]);
  const first = fingerprint("check:batch:members", "member-a");
  const changed = fingerprint(first.id, "member-b");
  cold.record({ fingerprints: [first], retained: [{ id: first.id, key: first.key, status: "pass" }], mismatches: [] });
  const warm = openClassBHistory(dirs);
  assert.equal(warm.forecast([changed])[0].expected, "run");
  warm.record({ fingerprints: [changed], retained: [{ id: changed.id, key: changed.failureKey, status: "fail" }], mismatches: [] });
  assert.deepEqual(openClassBHistory(dirs).forecast([first, changed]).map((row) => [row.expected, row.status]), [["hit", "pass"], ["hit", "fail"]]);
  assert.throws(() => openClassBHistory(dirs).forecast([{ ...changed, key: fingerprint("unexplained", "key").key, failureKey: undefined }]), /Cause unknown/);
});

test("Class B identifies invocation changes & rejects unexplained group keys", (t) => {
  const dirs = location(t);
  const first = { ...fingerprint("check:member", "same-inputs"), invocationId: "batch:a,b" };
  const changed = { ...first, key: fingerprint("batch:a,c", "same-inputs").key, invocationId: "batch:a,c" };
  const cold = openClassBHistory(dirs);
  cold.record({ fingerprints: [first], retained: [{ id: first.id, key: first.key, status: "pass" }], mismatches: [] });
  const warm = openClassBHistory(dirs);
  const forecast = warm.forecast([changed])[0];
  assert.equal(forecast.expected, "run");
  assert.deepEqual(forecast.changes.invocation, { from: first.invocationId, to: changed.invocationId });
  assert.throws(() => warm.forecast([{ ...changed, invocationId: first.invocationId }]), /Cause unknown/);
  assert.throws(() => warm.record({ fingerprints: [changed], retained: [{ id: changed.id, key: first.key, status: "pass" }], mismatches: [] }), /stored evidence does not match frozen identity/);
});
