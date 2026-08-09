import assert from "node:assert/strict";
import { cpSync, mkdtempSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { runConformance } from "./ax/conformance.mjs";

const HERE = path.dirname(fileURLToPath(import.meta.url));

function tempRepo() {
  return mkdtempSync(path.join(os.tmpdir(), "right-ax-conformance-"));
}

// F12 (invariant I5 containment): runConformance now refuses to spawn a
// server whose command/args resolve outside the repo it was pointed at, so
// every fixture server the test suite spawns must live INSIDE the repo
// under test — copied in here — and be referenced by a repo-relative
// command, exactly like a real product's `surfaces.mcp.serve` would.
function repoWithServer(fixtureName) {
  const repo = tempRepo();
  cpSync(path.join(HERE, "fixtures", fixtureName), path.join(repo, fixtureName));
  // "node" (bare, PATH-resolved) rather than process.execPath: .rightax.json
  // serve commands are a single whitespace-split string (splitServeCommand),
  // and process.execPath can itself contain spaces (e.g. "C:\Program
  // Files\nodejs\node.exe" on Windows), which would corrupt that split.
  return { repo, manifest: { surfaces: { mcp: { serve: `node ${fixtureName}` } } } };
}

test("conformance passes good_tool and fails bad_tool against the fixture MCP server", async () => {
  const { repo, manifest } = repoWithServer("fake-mcp-server.mjs");
  const result = await runConformance({ repoPath: repo, manifest });
  assert.equal(result.fatal, null, JSON.stringify(result.fatal));
  assert.equal(result.initialize.status, "pass");
  const good = result.rows.find((row) => row.tool === "good_tool");
  const bad = result.rows.find((row) => row.tool === "bad_tool");
  assert.equal(good.status, "pass", JSON.stringify(good.reasons));
  assert.equal(bad.status, "fail");
  assert.ok(bad.reasons.some((reason) => reason.includes("missing required arguments")));
  assert.equal(result.verdict, "fail");
});

test("structuredContent is validated against the advertised outputSchema", async () => {
  const { repo, manifest } = repoWithServer("fake-mcp-server.mjs");
  const result = await runConformance({ repoPath: repo, manifest });
  const good = result.rows.find((row) => row.tool === "good_tool");
  assert.equal(good.checks.structuredContentValidation.status, "pass", JSON.stringify(good.checks.structuredContentValidation.errors));
  const bad = result.rows.find((row) => row.tool === "bad_tool");
  assert.equal(bad.checks.structuredContentValidation.status, "skipped-pending");
});

test("zero raw stack traces leak from the fixture server", async () => {
  const { repo, manifest } = repoWithServer("fake-mcp-server.mjs");
  const result = await runConformance({ repoPath: repo, manifest });
  assert.deepEqual(result.rawStackTraces, []);
});

test("a missing surfaces.mcp.serve command is a typed fatal, not a crash", async () => {
  const repo = tempRepo();
  const result = await runConformance({ repoPath: repo, manifest: {} });
  assert.equal(result.fatal.code, "no_mcp_surface");
  assert.equal(result.verdict, "fail");
});

// F3 regression: a tool that rejects EVERY call, including one built to
// satisfy its own advertised required-argument schema, must fail
// conformance. Before the fix, evaluateProbe() treated any typed error on
// valid-args as a pass ("the server responded with a stable error code"),
// so a completely unusable tool passed.
test("F3: a tool that rejects every valid call fails conformance (not a vacuous pass)", async () => {
  const { repo, manifest } = repoWithServer("nonconforming-mcp-server.mjs");
  const result = await runConformance({ repoPath: repo, manifest });
  const row = result.rows.find((r) => r.tool === "reject_all_tool");
  const validArgsProbe = row.probes.find((p) => p.kind === "valid-args");
  assert.equal(validArgsProbe.status, "fail", JSON.stringify(validArgsProbe));
  assert.equal(row.status, "fail");
  assert.equal(result.verdict, "fail");
});

// F3 regression: a tool that declares additionalProperties:false but then
// accepts a call carrying an unknown field is not enforcing the schema it
// advertises. Before the fix, any "success" response to the unknown-field
// probe was unconditionally a pass.
test("F3: a tool declaring additionalProperties:false but accepting unknown fields fails conformance", async () => {
  const { repo, manifest } = repoWithServer("nonconforming-mcp-server.mjs");
  const result = await runConformance({ repoPath: repo, manifest });
  const row = result.rows.find((r) => r.tool === "lax_enforcement_tool");
  const unknownFieldProbe = row.probes.find((p) => p.kind === "unknown-field");
  assert.equal(unknownFieldProbe.status, "fail", JSON.stringify(unknownFieldProbe));
  assert.equal(row.status, "fail");
});

// F4 regression: a JSON-RPC error on tools/list must be a typed fatal, never
// silently collapsed into an empty tool list that then passes vacuously.
test("F4: a tools/list JSON-RPC error is a typed fatal, not zero tools with a passing verdict", async () => {
  const { repo, manifest } = repoWithServer("tools-list-error-mcp-server.mjs");
  manifest.surfaces.mcp.serve = "node tools-list-error-mcp-server.mjs error";
  const result = await runConformance({ repoPath: repo, manifest });
  assert.equal(result.fatal?.code, "tools_list_failed", JSON.stringify(result.fatal));
  assert.equal(result.rows.length, 0);
  assert.equal(result.verdict, "fail");
});

// F4 regression, second shape: zero tools with NO error is equally
// untestable and must not pass either.
test("F4: zero tools (no error) is a typed fatal, not a passing empty rowset", async () => {
  const { repo, manifest } = repoWithServer("tools-list-error-mcp-server.mjs");
  manifest.surfaces.mcp.serve = "node tools-list-error-mcp-server.mjs empty";
  const result = await runConformance({ repoPath: repo, manifest });
  assert.equal(result.fatal?.code, "no_tools", JSON.stringify(result.fatal));
  assert.equal(result.verdict, "fail");
});

// F9 regression: a credential-shaped string returned by the target process
// in an error message must never be copied verbatim into the conformance
// report.
test("F9: a credential-shaped string in a tool's error message is redacted from the report", async () => {
  const { repo, manifest } = repoWithServer("nonconforming-mcp-server.mjs");
  const result = await runConformance({ repoPath: repo, manifest });
  const serialized = JSON.stringify(result);
  assert.ok(!serialized.includes("sk-test-EXAMPLE123"), "credential leaked into conformance report");
  assert.ok(serialized.includes("[REDACTED]"));
});

// F12 regression (invariant I5, containment): a manifest command/arg that
// resolves outside the repo must be refused, not silently spawned — the
// exact shape of the QA repro (a manifest pointing at a scratch file
// entirely outside the audited repo).
test("F12: a serve command pointing outside the repo is refused as a typed fatal", async () => {
  const repo = tempRepo();
  const outsideServer = path.join(HERE, "fixtures", "fake-mcp-server.mjs"); // deliberately NOT copied into repo
  const manifest = { surfaces: { mcp: { serve: `node ${outsideServer}` } } };
  const result = await runConformance({ repoPath: repo, manifest });
  assert.equal(result.fatal?.code, "artifact_outside_repo", JSON.stringify(result.fatal));
  assert.equal(result.rows.length, 0);
  assert.equal(result.verdict, "fail");
});

test("F12: a relative serve command that resolves inside the repo is not flagged", async () => {
  const { repo, manifest } = repoWithServer("fake-mcp-server.mjs");
  const result = await runConformance({ repoPath: repo, manifest });
  assert.notEqual(result.fatal?.code, "artifact_outside_repo");
});
