import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { test } from "node:test";
import { fileScanCache, runScans } from "./deterministic-scans.mjs";
import { openClassBHistory } from "./class-a-cache-history.mjs";

// Two Class B rows (Node subprocesses stand in for cargo), a real file cache and a real Class B history.
function fixture(t) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "rk-classb-per-row-"));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  fs.writeFileSync(path.join(root, "package.json"), '{"license":"MIT"}');
  const scans = ["alpha", "beta"].map((name) => {
    const args = ["--eval", "0", "--", "-p", name];
    return { id: `check:${name}`, class: "B", root: "content", cwd: ".", tool: "node", args, command: `${process.execPath} ${args.join(" ")}` };
  });
  const historyOptions = { historyRoot: path.join(root, "history"), storeDir: path.join(root, "cache") };
  openClassBHistory(historyOptions);
  const disk = fileScanCache(historyOptions.storeDir);
  const cache = { ...disk, digest: (scan) => scan.id };
  const plan = { roots: { contentRoot: root, liveRoot: root }, scans };
  const run = (history, extra = {}) => runScans(plan, { classes: ["B"], classAEarlier: true, cache, classBCacheForecast: history.forecast, ...extra });
  return { run, openHistory: () => openClassBHistory(historyOptions) };
}

test("a run stopped after its rows were cached, with no end-of-run record, leaves the next run divergent (the 2026-11-25 bug)", async (t) => {
  const f = fixture(t);
  const first = await f.run(f.openHistory());
  assert.equal(first.ok, true);
  // Stopped: the end-of-run history.record never ran. The cache holds both results; the history knows neither.
  const second = await f.run(f.openHistory());
  assert.ok(second.classBCacheDecision.mismatches.length > 0, "the forecast says run, the lookup finds the entry");
});

test("recording each stored row as it is stored lets the next run hit cleanly even though the stopped run never reached its end-of-run record", async (t) => {
  const f = fixture(t);
  const history = f.openHistory();
  const first = await f.run(history, { onClassBStored: ({ decision, entry }) => history.record({ fingerprints: decision.fingerprints, retained: [entry], mismatches: [] }) });
  assert.equal(first.ok, true);
  // No end-of-run record (the run was stopped).
  const second = await f.run(f.openHistory());
  assert.deepEqual(second.classBCacheDecision.mismatches, []);
  assert.deepEqual(second.classBCacheDecision.dispatchedRows, []);
});
