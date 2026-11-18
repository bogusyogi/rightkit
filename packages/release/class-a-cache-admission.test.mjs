import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { test } from "node:test";
import { fileScanCache, runScans } from "./deterministic-scans.mjs";

function fixture(t, { hasLicense = true } = {}) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "rk-class-a-admit-"));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  const content = path.join(dir, "tree");
  const store = path.join(dir, "cache");
  fs.mkdirSync(content);
  fs.writeFileSync(path.join(content, "package.json"), hasLicense ? '{"license":"MIT"}' : "{}");
  fs.writeFileSync(path.join(content, "notes.txt"), "original\n");
  const trackedFiles = ["package.json", "notes.txt"];
  const plan = {
    roots: { contentRoot: content, liveRoot: content },
    scans: [
      { id: "license", class: "A", command: "internal:license", internal: "license", files: ["package.json"] },
      { id: "secret-scan", class: "A", command: "internal:secret-scan", internal: "secret-scan", dirs: ["."] },
    ],
  };
  const reads = [];
  const disk = fileScanCache(store);
  const cache = {
    ...disk,
    digest(scan) {
      const hash = createHash("sha256");
      for (const f of scan.files ?? trackedFiles) hash.update(f).update("\0").update(fs.readFileSync(path.join(content, f))).update("\0");
      return hash.digest("hex");
    },
    readOutcome(key) { reads.push(key); return disk.readOutcome(key); },
  };
  const run = (classACacheForecast) => runScans(plan, { classes: ["A"], cache, classACacheForecast, trackedFiles });
  const edit = () => fs.appendFileSync(path.join(content, "notes.txt"), "edit\n");
  const cacheFile = (key) => path.join(store, `${key}.json`);
  return { edit, cacheFile, reads, cache, run };
}

const cold = (rows) => rows.map(({ id }) => ({ id, expected: "run" }));
const warm = (evidence) => evidence.map(({ id, key, status }) => ({ id, expected: "hit", key, status }));

test("Class A freezes row keys before lookup & admits cold, warm and changed rows", async (t) => {
  const f = fixture(t);
  const first = await f.run((rows) => {
    assert.deepEqual(f.reads, []);
    assert.deepEqual(rows.map(({ id }) => id), ["license", "secret-scan"]);
    assert.ok(rows.every(({ key, environment, toolVersion }) => key && environment && toolVersion));
    return cold(rows);
  });
  assert.equal(first.ok, true);
  assert.deepEqual(first.classACacheDecision.dispatchedRows, ["license", "secret-scan"]);
  const retained = first.classACacheDecision.retained;
  const second = await f.run(() => warm(retained));
  assert.equal(second.ok, true);
  assert.deepEqual(second.cacheHits, ["license", "secret-scan"]);
  assert.deepEqual(second.classACacheDecision.dispatchedRows, []);
  f.edit();
  const third = await f.run(() => [warm(retained).find(({ id }) => id === "license"), { id: "secret-scan", expected: "run" }]);
  assert.equal(third.ok, true);
  assert.deepEqual(third.cacheHits, ["license"]);
  assert.deepEqual(third.classACacheDecision.dispatchedRows, ["secret-scan"]);
});

test("SC-1 reports both swapped row identities before dispatch although totals agree", async (t) => {
  const f = fixture(t);
  await f.run(cold);
  f.edit();
  const result = await f.run(() => [{ id: "license", expected: "run" }, { id: "secret-scan", expected: "hit" }]);
  assert.equal(result.ok, false);
  assert.deepEqual(result.classACacheDecision.mismatches.map(({ id, expected, actual }) => [id, expected, actual]), [
    ["license", "run", "hit"], ["secret-scan", "hit", "run"],
  ]);
  assert.deepEqual(result.classACacheDecision.dispatchedRows, []);
});

test("S1 refuses an entry removed after forecast or after its initial lookup", async (t) => {
  const f = fixture(t);
  const seed = await f.run(cold);
  const retained = seed.classACacheDecision.retained;
  const licenseKey = retained.find(({ id }) => id === "license").key;
  const licenseFile = f.cacheFile(licenseKey);
  const license = fs.readFileSync(licenseFile);
  const lost = await f.run(() => { fs.unlinkSync(licenseFile); return warm(retained); });
  assert.equal(lost.ok, false);
  assert.deepEqual(lost.classACacheDecision.mismatches.map(({ id }) => id), ["license"]);
  assert.deepEqual(lost.classACacheDecision.dispatchedRows, []);
  fs.writeFileSync(licenseFile, license);
  const secretKey = retained.find(({ id }) => id === "secret-scan").key;
  const read = f.cache.readOutcome;
  f.cache.readOutcome = (key) => {
    if (key === secretKey && fs.existsSync(licenseFile)) fs.unlinkSync(licenseFile);
    return read(key);
  };
  const late = await f.run(() => warm(retained));
  assert.equal(late.ok, false);
  assert.deepEqual(late.classACacheDecision.mismatches.map(({ id, reason }) => [id, reason]), [["license", "cache entry changed between lookup and dispatch"]]);
  assert.deepEqual(late.classACacheDecision.dispatchedRows, []);
});

test("an admitted deterministic failure stays red without redispatch", async (t) => {
  const f = fixture(t, { hasLicense: false });
  const seed = await f.run(cold);
  assert.equal(seed.ok, false);
  const retained = seed.classACacheDecision.retained;
  assert.equal(retained.find(({ id }) => id === "license").status, "fail");
  const reused = await f.run(() => warm(retained));
  assert.equal(reused.ok, false);
  assert.equal(reused.scans.find(({ id }) => id === "license").failureCached, true);
  assert.deepEqual(reused.classACacheDecision.mismatches, []);
  assert.deepEqual(reused.classACacheDecision.dispatchedRows, []);
});
