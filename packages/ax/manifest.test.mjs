import assert from "node:assert/strict";
import { mkdtempSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { loadManifest } from "./manifest.mjs";

function tempRepo() {
  return mkdtempSync(path.join(os.tmpdir(), "right-ax-manifest-"));
}

test("a well-formed .rightax.json loads with no errors", () => {
  const repo = tempRepo();
  writeFileSync(path.join(repo, ".rightax.json"), JSON.stringify({
    schemaVersion: 2,
    surfaces: { mcp: { command: "node", args: ["scripts/server.mjs"], cwd: "$ARTIFACT_ROOT", tools: 6 } },
    plugin: { name: "widget", skills: ["widget"] },
    ax: { scenariosDir: "evals/ax/scenarios", conformanceExamples: { widget_lookup: { query: "example" } }, matrix: { adapters: [{ id: "a", model: "m-a", version: "1", command: "node", args: [] }, { id: "b", model: "m-b", version: "1", command: "node", args: [] }], trialsPerAgent: 5, limits: { timeoutMs: 1, maxToolCalls: 0, maxTokens: 0, maxCostUsd: 0 } }, adversarial: { casesDir: "cases", gate: "hard" }, gates: { static: "hard", conformance: "hard", behavioral: "report-only" }, claimBoundaryRequired: true },
  }));
  const { manifest, errors } = loadManifest(repo);
  assert.deepEqual(errors, []);
  assert.equal(manifest.schemaVersion, 2);
});

test("matrix adapters need distinct execution identities", () => {
  const repo = tempRepo();
  const matrix = { adapters: [{ id: "a", model: "same", version: "1", command: "node", args: [] }, { id: "b", model: "same", version: "1", command: "node", args: [] }], trialsPerAgent: 5, limits: { timeoutMs: 1, maxToolCalls: 0, maxTokens: 0, maxCostUsd: 0 } };
  writeFileSync(path.join(repo, ".rightax.json"), JSON.stringify({ schemaVersion: 2, surfaces: {}, ax: { scenariosDir: "s", gates: {}, matrix } }));
  assert.ok(loadManifest(repo).errors.some((error) => error.includes("duplicate execution identity")));
});

test("matrix is optional, but supplied adapters and limits are strict", () => {
  const repo = tempRepo(); const base = { schemaVersion: 2, surfaces: {}, ax: { scenariosDir: "s", gates: {} } };
  writeFileSync(path.join(repo, ".rightax.json"), JSON.stringify(base));
  assert.deepEqual(loadManifest(repo).errors, []);
  const matrix = { adapters: [{ id: "Alpha", model: "m", version: "1", command: "node", args: [] }, { id: " alpha ", model: "m", version: "1", command: "node", args: [] }], trialsPerAgent: 5, limits: { timeoutMs: 1, maxToolCalls: 0, maxTokens: 0, maxCostUsd: 0, extra: true } };
  writeFileSync(path.join(repo, ".rightax.json"), JSON.stringify({ ...base, ax: { ...base.ax, matrix } }));
  const { errors } = loadManifest(repo);
  assert.ok(errors.some((error) => error.includes("unexpected property extra")));
  matrix.limits = { timeoutMs: 1, maxToolCalls: 0, maxTokens: 0, maxCostUsd: 0 };
  const duplicate = loadManifest(repo); writeFileSync(path.join(repo, ".rightax.json"), JSON.stringify({ ...base, ax: { ...base.ax, matrix } }));
  assert.ok(loadManifest(repo).errors.some((error) => error.includes("duplicate normalized adapter id")));
  void duplicate;
});

test("conformanceExamples accepts only non-empty tool names mapped to JSON objects", () => {
  const repo = tempRepo();
  writeFileSync(path.join(repo, ".rightax.json"), JSON.stringify({ schemaVersion: 1, surfaces: {}, ax: { gates: {}, conformanceExamples: { widget_lookup: { query: "example" }, "": { query: "invalid" }, bad_array: [], bad_string: "value", bad_null: null } } }));
  const { manifest, errors } = loadManifest(repo);
  assert.equal(manifest, null);
  assert.ok(errors.some((error) => /conformanceExamples/.test(error)), JSON.stringify(errors));
});

test("missing .rightax.json is reported, not thrown", () => {
  const repo = tempRepo();
  const { manifest, errors } = loadManifest(repo);
  assert.equal(manifest, null);
  assert.ok(errors[0].includes("not found"));
});

test("missing schemaVersion is rejected", () => {
  const repo = tempRepo();
  writeFileSync(path.join(repo, ".rightax.json"), JSON.stringify({ surfaces: {}, ax: { gates: {} } }));
  const { manifest, errors } = loadManifest(repo);
  assert.equal(manifest, null);
  assert.ok(errors.some((e) => /schemaVersion/.test(e)));
});

test("an invalid gate severity value is rejected", () => {
  const repo = tempRepo();
  writeFileSync(path.join(repo, ".rightax.json"), JSON.stringify({ schemaVersion: 1, surfaces: {}, ax: { gates: { static: "soft" } } }));
  const { manifest, errors } = loadManifest(repo);
  assert.equal(manifest, null);
  assert.ok(errors.some((e) => /value not in enum/.test(e)));
});

test("an unknown top-level manifest field is rejected (schema is strict)", () => {
  const repo = tempRepo();
  writeFileSync(path.join(repo, ".rightax.json"), JSON.stringify({ schemaVersion: 1, surfaces: {}, ax: { gates: {} }, notAField: true }));
  const { manifest, errors } = loadManifest(repo);
  assert.equal(manifest, null);
  assert.ok(errors.some((e) => /unexpected property notAField/.test(e)));
});

test("malformed JSON is reported without throwing", () => {
  const repo = tempRepo();
  writeFileSync(path.join(repo, ".rightax.json"), "{ not json");
  const { manifest, errors } = loadManifest(repo);
  assert.equal(manifest, null);
  assert.ok(errors[0].includes("not valid JSON"));
});
