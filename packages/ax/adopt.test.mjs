import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { adoptRepo } from "./adopt.mjs";
import { loadManifest } from "./manifest.mjs";

const repo = () => mkdtempSync(join(tmpdir(), "right-ax-adopt-"));
const scenarios = (path) => mkdirSync(join(path, "evals", "ax", "scenarios"), { recursive: true });

test("adopt writes a schema-v2 manifest with artifact-rooted inferred entrypoint", () => {
  const path = repo();
  scenarios(path);
  mkdirSync(join(path, "scripts"));
  writeFileSync(join(path, "scripts", "cortex-mcp.mjs"), "");

  const result = adoptRepo(path);
  assert.equal(result.ok, true);
  assert.deepEqual(result.manifest.surfaces.mcp, { command: "node", args: ["${ARTIFACT_ROOT}/scripts/cortex-mcp.mjs"], cwd: "${ARTIFACT_ROOT}" });
  assert.equal(result.manifest.ax.matrix, undefined);
  assert.equal(result.manifest.ax.adversarial, undefined);
  assert.equal(loadManifest(path).errors.length, 0);
  assert.equal(readFileSync(join(path, ".rightax.json"), "utf8").includes("$ARTIFACT_ROOT"), false);
});

test("adopt tokenizes only relative path-like explicit arguments", () => {
  const path = repo();
  scenarios(path);

  const result = adoptRepo(path, { write: false, serve: { command: "node", args: ["scripts\\server.mjs", "--stdio", "production"] } });
  assert.deepEqual(result.manifest.surfaces.mcp, { command: "node", args: ["${ARTIFACT_ROOT}/scripts/server.mjs", "--stdio", "production"], cwd: "${ARTIFACT_ROOT}" });
});

test("adopt refuses an absolute explicit serve argument", () => {
  const path = repo();
  scenarios(path);

  const result = adoptRepo(path, { write: false, serve: { command: "node", args: ["C:\\unsafe\\server.mjs"] } });
  assert.equal(result.ok, false);
  assert.deepEqual(result.findings.map((finding) => finding.code), ["unsafe_serve_arg"]);
  assert.deepEqual(result.manifest.surfaces, {});
});
