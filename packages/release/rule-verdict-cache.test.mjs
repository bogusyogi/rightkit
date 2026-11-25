// GC-A27 NN06: a row whose inputs did not change is never run again, and nothing is written for it. A passing rule verdict that is already stored is not
// written back (found by acceptance case 2 on gate 0.6.63, 2026-11-24: the same commit again rewrote one rule-verdicts file; rightkit#1514).
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { test } from "node:test";
import { ruleVerdictCache } from "./bin/rightkit-gate.mjs";

test("a stored passing rule verdict is read, and passing it again writes nothing", () => {
  const common = fs.mkdtempSync(path.join(os.tmpdir(), "rk-rule-verdict-"));
  const spawn = () => ({ status: 0, stdout: `${common}\n`, stderr: "" });
  const cache = ruleVerdictCache({ checkout: common, spawn });
  const key = cache.key({ rule: "app-check", tree: "t".repeat(40), args: [] });
  const file = path.join(common, "rightkit-gate-cache", "rule-verdicts", `${key}.json`);
  assert.equal(cache.passed(key), null);
  cache.pass(key);
  assert.equal(Boolean(cache.passed(key)), true);
  const long = new Date("2020-01-01T00:00:00Z"); fs.utimesSync(file, long, long); const before = fs.readFileSync(file, "utf8");
  cache.pass(key);
  assert.equal(fs.readFileSync(file, "utf8"), before, "the stored verdict is not rewritten");
  assert.equal(fs.statSync(file).mtimeMs, long.getTime(), "the file is not touched");
  assert.deepEqual(fs.readdirSync(path.dirname(file)), [`${key}.json`], "no temporary file is left");
  // a verdict that cannot be read as a pass is written
  fs.writeFileSync(file, "{"); cache.pass(key); assert.equal(Boolean(cache.passed(key)), true);
  // rightkit#1695: the app rules' pass keeps its rule count; a pass stored without one is not reused where the count is needed, and is rewritten with it.
  assert.equal(cache.passed(key, { needRules: true }), null);
  cache.pass(key, 52); assert.equal(cache.passed(key, { needRules: true })?.rules, 52);
  const kept = fs.readFileSync(file, "utf8"); cache.pass(key, 52); assert.equal(fs.readFileSync(file, "utf8"), kept, "a pass with its count is not rewritten");
  fs.rmSync(common, { recursive: true, force: true });
});
