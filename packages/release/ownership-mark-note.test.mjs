import test from "node:test"; import assert from "node:assert"; import fs from "node:fs"; import os from "node:os"; import path from "node:path";
import { markBrokerSubmit } from "./ownership-mark.mjs";
test("sent records pid and note per sub-handle; repeat keeps the first", () => {
  const d = fs.mkdtempSync(path.join(os.tmpdir(), "mark-")); const f = path.join(d, "owner.json");
  fs.writeFileSync(f, JSON.stringify({ handle: "h1" }));
  markBrokerSubmit(f, "h1", "h1#a", { note: "cargo:x:clippy" });
  const r = markBrokerSubmit(f, "h1", "h1#b");
  assert.deepStrictEqual(r.broker.handles, ["h1#a", "h1#b"]);
  assert.strictEqual(r.broker.sent["h1#a"].note, "cargo:x:clippy");
  assert.strictEqual(r.broker.sent["h1#a"].pid, process.pid);
  assert.match(r.broker.sent["h1#b"].note, /\S/);
  assert.strictEqual(markBrokerSubmit(f, "h1", "h1#a", { note: "other" }).broker.sent["h1#a"].note, "cargo:x:clippy");
});
