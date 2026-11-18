import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { test } from "node:test";
import { openClassAHistory } from "./class-a-cache-history.mjs";

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
