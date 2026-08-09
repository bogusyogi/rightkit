import assert from "node:assert/strict";
import { cpSync, mkdirSync, mkdtempSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";
import test from "node:test";
import { fileURLToPath } from "node:url";

const HERE = path.dirname(fileURLToPath(import.meta.url));
const CLI = path.join(HERE, "cli", "right-ax.mjs");
const SERVER_PATH = path.join(HERE, "fixtures", "fake-mcp-server.mjs");

function run(args) {
  return spawnSync(process.execPath, [CLI, ...args], { encoding: "utf8" });
}

test("--help prints usage and exits 0", () => {
  const result = run(["--help"]);
  assert.equal(result.status, 0);
  assert.match(result.stdout, /right-ax <command> \[options\]/);
});

test("--version prints a version string", () => {
  const result = run(["--version"]);
  assert.equal(result.status, 0);
  assert.match(result.stdout, /right-ax 0\.1\.0/);
});

test("an unknown command exits non-zero with usage", () => {
  const result = run(["bogus"]);
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /unknown command/);
});

test("ax static exits non-zero when .rightax.json is missing", () => {
  const repo = mkdtempSync(path.join(os.tmpdir(), "right-ax-cli-"));
  const result = run(["ax", "static", repo]);
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /invalid or missing \.rightax\.json/);
});

test("ax report --format sarif against the fixture server emits SARIF on stdout", () => {
  const repo = mkdtempSync(path.join(os.tmpdir(), "right-ax-cli-"));
  // F12 (invariant I5 containment): runConformance refuses a serve command
  // that resolves outside the repo, so the fixture server must live inside
  // the repo under test, referenced by a repo-relative command.
  cpSync(SERVER_PATH, path.join(repo, "fake-mcp-server.mjs"));
  writeFileSync(path.join(repo, ".rightax.json"), JSON.stringify({ schemaVersion: 1, surfaces: { mcp: { serve: "node fake-mcp-server.mjs" } }, ax: { gates: { static: "report-only", conformance: "report-only", behavioral: "report-only" } } }));
  const result = run(["ax", "report", repo, "--format", "sarif"]);
  const sarif = JSON.parse(result.stdout);
  assert.equal(sarif.version, "2.1.0");
});

// F11 regression: `right-ax ax behavioral` must exit non-zero when the run
// genuinely fails (every trial fails), not exit 0 because the harness
// itself didn't crash. Uses --driver external with no --agents wiring, so
// every trial comes back as "produced no trial for scenario X" — a real,
// total failure, not a harness break.
test("ax behavioral exits non-zero on a total-failure run (not exit 0 because only .fatal was checked)", () => {
  const repo = mkdtempSync(path.join(os.tmpdir(), "right-ax-cli-"));
  const scenariosDir = path.join(repo, "evals", "ax", "scenarios");
  mkdirSync(scenariosDir, { recursive: true });
  writeFileSync(path.join(scenariosDir, "one.yaml"), JSON.stringify({ scenarioId: "one", expected: { requiredClaims: ["done"] } }));
  writeFileSync(path.join(repo, ".rightax.json"), JSON.stringify({ schemaVersion: 1, surfaces: {}, ax: { scenariosDir: "evals/ax/scenarios", gates: { static: "hard", conformance: "hard", behavioral: "report-only" } } }));
  const result = run(["ax", "behavioral", repo, "--driver", "external", "--k", "2"]);
  const parsed = JSON.parse(result.stdout);
  assert.equal(parsed.verdict, "fail");
  assert.notEqual(result.status, 0);
});
