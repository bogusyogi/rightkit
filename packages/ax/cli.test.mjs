import assert from "node:assert/strict";
import { cpSync, existsSync, mkdirSync, mkdtempSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";
import test from "node:test";
import { fileURLToPath } from "node:url";
import packageJson from "./package.json" with { type: "json" };

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

test("ownership-review prepare emits bounded packet without starting a provider", () => {
  const repo = mkdtempSync(path.join(os.tmpdir(), "right-ax-review-"));
  const git = (...args) => {
    const result = spawnSync("git", args, { cwd: repo, encoding: "utf8", windowsHide: true });
    assert.equal(result.status, 0, result.stderr);
    return result.stdout.trim();
  };
  git("init", "-q");
  git("config", "user.email", "test@example.invalid");
  git("config", "user.name", "AX test");
  const catalog = path.join(repo, "rightkit/packages/release/rightkit-ownership.json");
  mkdirSync(path.dirname(catalog), { recursive: true });
  writeFileSync(catalog, JSON.stringify({ schema: 1, capabilities: [] }));
  const source = path.join(repo, "src/example.mjs");
  mkdirSync(path.dirname(source), { recursive: true });
  writeFileSync(source, "export const value = 1;\n");
  git("add", "."); git("commit", "-qm", "base"); const base = git("rev-parse", "HEAD");
  writeFileSync(source, "export const value = 2;\n");
  git("add", "."); git("commit", "-qm", "head"); const head = git("rev-parse", "HEAD");
  const result = run(["ownership-review", "prepare", repo, "--base", base, "--head", head]);
  assert.equal(result.status, 0, result.stderr);
  const packet = JSON.parse(result.stdout);
  assert.equal(packet.state, "prepared");
  assert.equal(packet.providerState.status, "not_started");
  assert.equal(packet.advisoryOnly, true);
});

test("--version prints a version string", () => {
  const result = run(["--version"]);
  assert.equal(result.status, 0);
  assert.equal(result.stdout.trim(), `right-ax ${packageJson.version}`);
});

test("help advertises current matrix and artifact surface only", () => {
  const result = run(["--help"]);
  assert.match(result.stdout, /adversarial/);
  assert.match(result.stdout, /artifact-root/);
  assert.match(result.stdout, /--matrix/);
  assert.doesNotMatch(result.stdout, /--driver|--agents/);
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

test("plugin init creates a missing target directory instead of failing", () => {
  const parent = mkdtempSync(path.join(os.tmpdir(), "right-ax-cli-"));
  const missing = path.join(parent, "missing", "nested-repo");
  const result = run(["plugin", "init", missing]);
  assert.equal(result.status, 0, result.stderr);
  assert.match(result.stdout, /right-ax plugin init: wrote \d+ file/);
  assert.equal(existsSync(path.join(missing, "plugin.json")), true);
});

test("ax report --format sarif against the fixture server emits SARIF on stdout", () => {
  const repo = mkdtempSync(path.join(os.tmpdir(), "right-ax-cli-"));
  // F12 (invariant I5 containment): runConformance refuses a serve command
  // that resolves outside the repo, so the fixture server must live inside
  // the repo under test, referenced by a repo-relative command.
  const artifact = path.join(repo, "artifact");
  mkdirSync(artifact);
  cpSync(SERVER_PATH, path.join(artifact, "fake-mcp-server.mjs"));
  writeFileSync(path.join(repo, ".rightax.json"), JSON.stringify({ schemaVersion: 2, surfaces: { mcp: { command: "node", args: ["fake-mcp-server.mjs"], cwd: "${ARTIFACT_ROOT}" } }, ax: { scenariosDir: "evals/ax/scenarios", gates: { static: "report-only", conformance: "report-only" } } }));
  const result = run(["ax", "report", repo, "--artifact-root", artifact, "--format", "sarif"]);
  const sarif = JSON.parse(result.stdout);
  assert.equal(sarif.version, "2.1.0");
});

// F11 regression: `right-ax ax behavioral` must exit non-zero when the run
// genuinely fails (every trial fails), not exit 0 because the harness
// itself didn't crash. Uses --driver external with no --agents wiring, so
// every trial comes back as "produced no trial for scenario X" — a real,
// total failure, not a harness break.
test("removed behavioral driver flags fail with usage", () => {
  const repo = mkdtempSync(path.join(os.tmpdir(), "right-ax-cli-"));
  const scenariosDir = path.join(repo, "evals", "ax", "scenarios");
  mkdirSync(scenariosDir, { recursive: true });
  writeFileSync(path.join(scenariosDir, "one.yaml"), JSON.stringify({ scenarioId: "one", expected: { requiredClaims: ["done"] } }));
  writeFileSync(path.join(repo, ".rightax.json"), JSON.stringify({ schemaVersion: 2, surfaces: {}, ax: { scenariosDir: "evals/ax/scenarios", gates: { behavioral: "report-only" } } }));
  const result = run(["ax", "behavioral", repo, "--driver", "external"]);
  assert.equal(result.status, 2);
  assert.match(result.stderr, /uses manifest adapters/);
});

test("static without MCP and report without a live gate need no artifact", () => {
  const repo = mkdtempSync(path.join(os.tmpdir(), "right-ax-cli-"));
  mkdirSync(path.join(repo, "evals", "ax", "scenarios"), { recursive: true });
  writeFileSync(path.join(repo, ".rightax.json"), JSON.stringify({ schemaVersion: 2, surfaces: {}, ax: { scenariosDir: "evals/ax/scenarios", gates: { static: "report-only" } } }));
  const staticResult = run(["ax", "static", repo]);
  assert.notEqual(staticResult.status, 2);
  const reportResult = run(["ax", "report", repo]);
  assert.notEqual(reportResult.status, 2);
  assert.doesNotThrow(() => JSON.parse(reportResult.stdout));
});

test("MCP gates require an artifact and conformance requires MCP", () => {
  const repo = mkdtempSync(path.join(os.tmpdir(), "right-ax-cli-"));
  mkdirSync(path.join(repo, "evals", "ax", "scenarios"), { recursive: true });
  writeFileSync(path.join(repo, ".rightax.json"), JSON.stringify({ schemaVersion: 2, surfaces: { mcp: { command: "node", args: ["server.mjs"], cwd: "${ARTIFACT_ROOT}" } }, ax: { scenariosDir: "evals/ax/scenarios", gates: { static: "hard", conformance: "hard" } } }));
  assert.equal(run(["ax", "static", repo]).status, 2);
  assert.equal(run(["ax", "conformance", repo]).status, 2);
  writeFileSync(path.join(repo, ".rightax.json"), JSON.stringify({ schemaVersion: 2, surfaces: {}, ax: { scenariosDir: "evals/ax/scenarios", gates: { conformance: "hard" } } }));
  assert.equal(run(["ax", "conformance", repo, "--artifact-root", "artifact"]).status, 2);
});

test("adopt parses one serve program and repeatable argv tokens", () => {
  const repo = mkdtempSync(path.join(os.tmpdir(), "right-ax-cli-"));
  mkdirSync(path.join(repo, "evals", "ax", "scenarios"), { recursive: true });
  const result = run(["adopt", repo, "--dry-run", "--serve", "node", "--serve-arg", "scripts/{server}.mjs", "--serve-arg", "--stdio"]);
  const parsed = JSON.parse(result.stdout);
  assert.deepEqual(parsed.manifest.surfaces.mcp, { command: "node", args: ["${ARTIFACT_ROOT}/scripts/{server}.mjs", "--stdio"], cwd: "${ARTIFACT_ROOT}" });
  assert.equal(run(["adopt", repo, "--dry-run", "--serve-arg", "server.mjs"]).status, 2);
});
