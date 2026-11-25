import assert from "node:assert/strict";
import test from "node:test";
import { buildFromAcceptanceRun, claimsOfRealBuild } from "./acceptance-result.mjs";

const C = "a".repeat(40); const F = "f".repeat(40);
const contractLines = Array.from({ length: 13 }, (_, i) => ({ id: `NN${String(i + 1).padStart(2, "0")}`, text: `Line ${i + 1}.` }));
const run = (over = {}) => ({
  schema: "rightkit.acceptance-run/1", casesSha256: "c".repeat(64), judgeFiles: { "acceptance-run.mjs": "d".repeat(64) },
  realBuild: { repository: "/repo", commit: C },
  verdict: { lines: contractLines.map((l) => ({ id: l.id, verdict: "pass", why: "" })), realBuild: [{ id: "NN01", verdict: "pass" }, { id: "NN11", verdict: "pass" }], passed: 13, of: 13, pass: true },
  ...over,
});
const launch = { runId: "r1", kind: "compile", fingerprint: { platform: "win32" }, gate: { path: `D:/m/trusted-gates/${"e".repeat(64)}/node_modules/@rightkit/release/bin/rightkit-gate.mjs` }, startedAt: "s", endedAt: "e" };

test("an acceptance run becomes the admission record: the contract's thirteen sentences, the real build's commit and host, the run's judge files", () => {
  const r = buildFromAcceptanceRun({ run: run(), launch, contractLines, contractText: "contract", tree: "t".repeat(40), claims: [{ id: "j1", sha256: "1".repeat(64) }] });
  assert.equal(r.schema, "rightkit.acceptance-result/1"); assert.equal(r.commit, C); assert.equal(r.host, "dell"); assert.equal(r.pass, true);
  assert.deepEqual(r.lines.map((l) => l.text), contractLines.map((l) => l.text)); assert.ok(r.lines.every((l) => l.verdict === "pass"));
  assert.equal(r.gate.sealedSha256, "e".repeat(64)); assert.deepEqual(r.judgeFiles, { "acceptance-run.mjs": "d".repeat(64) }); assert.deepEqual(r.jobs, [{ requestId: "j1", claimSha256: "1".repeat(64) }]);
});

test("the record does not pass when the real build failed or did not run, or when a line is missing", () => {
  assert.equal(buildFromAcceptanceRun({ run: run({ verdict: { ...run().verdict, realBuild: [{ id: "NN01", verdict: "fail" }] } }), launch, contractLines, contractText: "c", tree: "t", claims: [] }).pass, false);
  assert.equal(buildFromAcceptanceRun({ run: run({ verdict: { ...run().verdict, realBuild: [] } }), launch, contractLines, contractText: "c", tree: "t", claims: [] }).pass, false);
  const missing = buildFromAcceptanceRun({ run: run({ verdict: { ...run().verdict, lines: run().verdict.lines.slice(0, 12) } }), launch, contractLines, contractText: "c", tree: "t", claims: [] });
  assert.equal(missing.lines.find((l) => l.id === "NN13").verdict, "not run");
});

test("only the real build's claims are taken: receipts new to its run and signed for its commit", () => {
  const files = {
    "old.json": { provenance: { payload: { jobId: "old", gate: { commit: C } } }, publication: { x: 1 } },
    "real.json": { provenance: { payload: { jobId: "real", gate: { commit: C } } }, publication: { x: 2 } },
    "fixture.json": { provenance: { payload: { jobId: "fix", gate: { commit: F } } }, publication: { x: 3 } },
    "noclaim.json": { provenance: { payload: { jobId: "none", gate: { commit: C } } } },
  };
  const claims = claimsOfRealBuild({ receiptsDir: "/r", before: new Set(["old.json"]), commit: C, list: () => Object.keys(files), read: (p) => JSON.stringify(files[p.split(/[\\/]/).pop()]) });
  assert.deepEqual(claims.map((c) => c.id), ["real"]);
});
