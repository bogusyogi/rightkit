import assert from "node:assert/strict";
import { cpSync, mkdirSync, mkdtempSync, symlinkSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { buildValidArgs, runBlockingCapabilityProbe, runConformance } from "./ax/conformance.mjs";

const HERE = path.dirname(fileURLToPath(import.meta.url));

function tempRepo() {
  return mkdtempSync(path.join(os.tmpdir(), "right-ax-conformance-"));
}

test("AX-4 records blocking per harness binding and fails closed without one", async () => {
  let calls = 0;
  const passing = await runBlockingCapabilityProbe({
    bindingId: "codex-windows",
    invoke: async () => { calls += 1; return { blocked: true, reason: "denied", interceptionOrder: ["arcane"] }; },
  });
  assert.equal(calls, 1);
  assert.equal(passing.verdict, "pass");
  assert.deepEqual(passing.observed.interceptionOrder, ["arcane"]);
  const missing = await runBlockingCapabilityProbe({ bindingId: "future-harness" });
  assert.equal(missing.verdict, "fail");
  assert.equal(missing.observed.blocked, false);
});

// F12 (invariant I5 containment): runConformance now refuses to spawn a
// server whose command/args resolve outside the repo it was pointed at, so
// every fixture server the test suite spawns must live INSIDE the repo
// under test — copied in here — and be referenced by a repo-relative
// command, exactly like a real product's `surfaces.mcp.serve` would.
function repoWithServer(fixtureName) {
  const repo = tempRepo();
  const artifactRoot = mkdtempSync(path.join(os.tmpdir(), "right-ax-artifact-"));
  cpSync(path.join(HERE, "fixtures", fixtureName), path.join(artifactRoot, fixtureName));
  // "node" (bare, PATH-resolved) rather than process.execPath: .rightax.json
  // serve commands are a single whitespace-split string (splitServeCommand),
  // and process.execPath can itself contain spaces (e.g. "C:\Program
  // Files\nodejs\node.exe" on Windows), which would corrupt that split.
  return { repo, artifactRoot, manifest: { schemaVersion: 2, surfaces: { mcp: { command: "node", args: [fixtureName], cwd: "${ARTIFACT_ROOT}" } }, ax: { scenariosDir: "scenarios", matrix: { adapters: [{ id: "a", command: "node", args: [] }, { id: "b", command: "node", args: [] }], trialsPerAgent: 5, limits: {} }, adversarial: { casesDir: "cases", gate: "hard" }, gates: {} } } };
}

test("buildValidArgs recursively synthesizes required nested schema values", () => {
  const tool = {
    inputSchema: {
      type: "object",
      required: ["caller", "mode", "fixed", "choice", "alternate", "attempts", "items"],
      properties: {
        caller: { type: "object", required: ["root", "repositoryId", "scopeId"], properties: { root: { type: "string", minLength: 2 }, repositoryId: { type: "string" }, scopeId: { type: "string" } } },
        mode: { enum: ["repo", "workspace"] },
        fixed: { const: "fixed-value" },
        choice: { oneOf: [{ type: "string", minLength: 3 }, { type: "integer", minimum: 5, maximum: 7 }] },
        alternate: { anyOf: [{ type: "boolean" }, { type: "string" }] },
        attempts: { type: "integer", minimum: 3, maximum: 7 },
        items: { type: "array", minItems: 2, items: { type: "object", required: ["name"], properties: { name: { type: "string", maxLength: 4 } } } },
      },
    },
  };
  assert.deepEqual(buildValidArgs(tool), {
    caller: { root: "conformance-probe", repositoryId: "conformance-probe", scopeId: "conformance-probe" },
    mode: "repo",
    fixed: "fixed-value",
    choice: "conformance-probe",
    alternate: true,
    attempts: 3,
    items: [{ name: "conf" }, { name: "conf" }],
  });
});

test("buildValidArgs prefers a deep-cloned declared conformance example", () => {
  const example = { caller: { root: "/repo", repositoryId: "repo", scopeId: "scope" } };
  const args = buildValidArgs({ inputSchema: { type: "object", required: ["caller"], properties: { caller: { type: "object" } } } }, example);
  assert.deepEqual(args, example);
  assert.notEqual(args, example);
  assert.notEqual(args.caller, example.caller);
  args.caller.root = "/changed";
  assert.equal(example.caller.root, "/repo");
});

test("runConformance uses a matching declared example for its valid-args probe", async () => {
  const { repo, manifest, artifactRoot } = repoWithServer("fake-mcp-server.mjs");
  manifest.ax = { conformanceExamples: { good_tool: { input: "example", unexpected: true } } };
  const result = await runConformance({ repoPath: repo, manifest, artifactRoot });
  const valid = result.rows.find((row) => row.tool === "good_tool").probes.find((probe) => probe.kind === "valid-args");
  assert.equal(valid.status, "fail");
  assert.equal(valid.code, "unknown_field");
});

test("conformance passes good_tool and fails bad_tool against the fixture MCP server", async () => {
  const { repo, manifest, artifactRoot } = repoWithServer("fake-mcp-server.mjs");
  const result = await runConformance({ repoPath: repo, manifest, artifactRoot });
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
  const { repo, manifest, artifactRoot } = repoWithServer("fake-mcp-server.mjs");
  const result = await runConformance({ repoPath: repo, manifest, artifactRoot });
  const good = result.rows.find((row) => row.tool === "good_tool");
  assert.equal(good.checks.structuredContentValidation.status, "pass", JSON.stringify(good.checks.structuredContentValidation.errors));
  const bad = result.rows.find((row) => row.tool === "bad_tool");
  assert.equal(bad.checks.structuredContentValidation.status, "skipped-pending");
});

test("zero raw stack traces leak from the fixture server", async () => {
  const { repo, manifest, artifactRoot } = repoWithServer("fake-mcp-server.mjs");
  const result = await runConformance({ repoPath: repo, manifest, artifactRoot });
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
  const { repo, manifest, artifactRoot } = repoWithServer("nonconforming-mcp-server.mjs");
  const result = await runConformance({ repoPath: repo, manifest, artifactRoot });
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
  const { repo, manifest, artifactRoot } = repoWithServer("nonconforming-mcp-server.mjs");
  const result = await runConformance({ repoPath: repo, manifest, artifactRoot });
  const row = result.rows.find((r) => r.tool === "lax_enforcement_tool");
  const unknownFieldProbe = row.probes.find((p) => p.kind === "unknown-field");
  assert.equal(unknownFieldProbe.status, "fail", JSON.stringify(unknownFieldProbe));
  assert.equal(row.status, "fail");
});

// F4 regression: a JSON-RPC error on tools/list must be a typed fatal, never
// silently collapsed into an empty tool list that then passes vacuously.
test("F4: a tools/list JSON-RPC error is a typed fatal, not zero tools with a passing verdict", async () => {
  const { repo, manifest, artifactRoot } = repoWithServer("tools-list-error-mcp-server.mjs");
  manifest.surfaces.mcp.args = ["tools-list-error-mcp-server.mjs", "error"];
  const result = await runConformance({ repoPath: repo, manifest, artifactRoot });
  assert.equal(result.fatal?.code, "tools_list_failed", JSON.stringify(result.fatal));
  assert.equal(result.rows.length, 0);
  assert.equal(result.verdict, "fail");
});

// F4 regression, second shape: zero tools with NO error is equally
// untestable and must not pass either.
test("F4: zero tools (no error) is a typed fatal, not a passing empty rowset", async () => {
  const { repo, manifest, artifactRoot } = repoWithServer("tools-list-error-mcp-server.mjs");
  manifest.surfaces.mcp.args = ["tools-list-error-mcp-server.mjs", "empty"];
  const result = await runConformance({ repoPath: repo, manifest, artifactRoot });
  assert.equal(result.fatal?.code, "no_tools", JSON.stringify(result.fatal));
  assert.equal(result.verdict, "fail");
});

// F9 regression: a credential-shaped string returned by the target process
// in an error message must never be copied verbatim into the conformance
// report.
test("F9: a credential-shaped string in a tool's error message is redacted from the report", async () => {
  const { repo, manifest, artifactRoot } = repoWithServer("nonconforming-mcp-server.mjs");
  const result = await runConformance({ repoPath: repo, manifest, artifactRoot });
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
  const artifactRoot = "artifact"; mkdirSync(path.join(repo, artifactRoot));
  const outsideServer = path.join(HERE, "fixtures", "fake-mcp-server.mjs"); // deliberately NOT copied into repo
  const manifest = { surfaces: { mcp: { command: "node", args: [outsideServer], cwd: "$ARTIFACT_ROOT" } } };
  const result = await runConformance({ repoPath: repo, manifest, artifactRoot });
  assert.equal(result.fatal?.code, "invalid_artifact", JSON.stringify(result.fatal));
  assert.equal(result.rows.length, 0);
  assert.equal(result.verdict, "fail");
});

test("F12: a relative serve command that resolves inside the repo is not flagged", async () => {
  const { repo, manifest, artifactRoot } = repoWithServer("fake-mcp-server.mjs");
  const result = await runConformance({ repoPath: repo, manifest, artifactRoot });
  assert.notEqual(result.fatal?.code, "invalid_artifact");
});

// F8 regression: lexical containment (resolve()+startsWith on the raw
// string) is blind to a directory symlink that lives INSIDE the repo but
// resolves OUTSIDE it — the lexical path never leaves the repo, but the real
// filesystem target does. This creates an actual directory symlink/junction
// in a temp dir and points a serve command through it; if this host cannot
// create filesystem symlinks (e.g. no Developer Mode / admin on Windows),
// the test skips rather than false-failing — but the containment code path
// (resolvesOutsideRoot in lib/path-safety.mjs) must be exercised whenever a
// symlink can actually be created.
test("F8: a serve command reached through a directory symlink resolving outside the repo is refused", async (t) => {
  const repo = tempRepo();
  const artifactRoot = "artifact"; mkdirSync(path.join(repo, artifactRoot));
  const outsideDir = mkdtempSync(path.join(os.tmpdir(), "right-ax-outside-"));
  cpSync(path.join(HERE, "fixtures", "fake-mcp-server.mjs"), path.join(outsideDir, "fake-mcp-server.mjs"));
  const linkPath = path.join(repo, artifactRoot, "inside-link");
  try {
    symlinkSync(outsideDir, linkPath, "junction");
  } catch (error) {
    t.skip(`cannot create a filesystem symlink/junction on this host: ${error?.message ?? error}`);
    return;
  }
  const manifest = { surfaces: { mcp: { command: "node", args: [`inside-link${path.sep}fake-mcp-server.mjs`], cwd: "$ARTIFACT_ROOT" } } };
  const result = await runConformance({ repoPath: repo, manifest, artifactRoot });
  assert.equal(result.fatal?.code, "invalid_artifact", JSON.stringify(result.fatal));
  assert.equal(result.rows.length, 0);
  assert.equal(result.verdict, "fail");
});

// F8 (round 3) regression: a BARE arg — no path separator, no "./" prefix,
// e.g. "server-link.mjs" directly at the repo root — was treated identically
// to a PATH-resolved executable name like "node" and skipped realpath
// containment entirely, even though it resolves to a real filesystem entry
// relative to the repo (unlike a genuine PATH lookup). This creates that
// exact shape: a bare-named FILE symlink sitting directly in the repo root
// whose target lives outside the repo. Skips gracefully if this host cannot
// create filesystem symlinks.
test("F8: a bare (no './' prefix, no separator) arg symlink resolving outside the repo is refused", async (t) => {
  const repo = tempRepo();
  const artifactRoot = "artifact"; mkdirSync(path.join(repo, artifactRoot));
  const outsideDir = mkdtempSync(path.join(os.tmpdir(), "right-ax-outside-"));
  const outsideFile = path.join(outsideDir, "fake-mcp-server.mjs");
  cpSync(path.join(HERE, "fixtures", "fake-mcp-server.mjs"), outsideFile);
  const bareLinkName = "server-link.mjs";
  const linkPath = path.join(repo, artifactRoot, bareLinkName);
  try {
    symlinkSync(outsideFile, linkPath, "file");
  } catch (error) {
    t.skip(`cannot create a filesystem symlink on this host: ${error?.message ?? error}`);
    return;
  }
  const manifest = { surfaces: { mcp: { command: "node", args: [bareLinkName], cwd: "$ARTIFACT_ROOT" } } };
  const result = await runConformance({ repoPath: repo, manifest, artifactRoot });
  assert.equal(result.fatal?.code, "invalid_artifact", JSON.stringify(result.fatal));
  assert.equal(result.rows.length, 0);
  assert.equal(result.verdict, "fail");
});
