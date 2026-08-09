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
    schemaVersion: 1,
    surfaces: { mcp: { serve: "node scripts/server.mjs", tools: 6 } },
    plugin: { name: "widget", skills: ["widget"] },
    ax: { scenariosDir: "evals/ax/scenarios", conformanceExamples: { widget_lookup: { query: "example" } }, gates: { static: "hard", conformance: "hard", behavioral: "report-only" }, claimBoundaryRequired: true },
  }));
  const { manifest, errors } = loadManifest(repo);
  assert.deepEqual(errors, []);
  assert.equal(manifest.schemaVersion, 1);
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
