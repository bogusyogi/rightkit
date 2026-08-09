import assert from "node:assert/strict";
import { cpSync, mkdirSync, mkdtempSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { runStaticChecks } from "./ax/static.mjs";

const HERE = path.dirname(fileURLToPath(import.meta.url));

function tempRepo() {
  return mkdtempSync(path.join(os.tmpdir(), "right-ax-static-"));
}

const GOOD_DESCRIPTION_A = "Use this when you need widget A. Do not use it for widget B.";
const GOOD_DESCRIPTION_B = "Use this when you need widget B. Do not use it for widget A.";

function goodTools() {
  const outputSchema = { type: "object", properties: { status: { type: "string" }, truncated: { type: "boolean" }, continuationCursor: { type: ["string", "null"] }, claimBoundary: { type: "object", properties: { status: {}, cleanClaimAllowed: {}, safeClaims: {}, prohibitedClaims: {}, gaps: {} }, required: ["status", "cleanClaimAllowed", "safeClaims", "prohibitedClaims", "gaps"] } } };
  return [
    { name: "widget_a", description: GOOD_DESCRIPTION_A, inputSchema: { type: "object", additionalProperties: false, properties: { query: { type: "string" } } }, outputSchema, annotations: { readOnlyHint: true } },
    { name: "widget_b", description: GOOD_DESCRIPTION_B, inputSchema: { type: "object", additionalProperties: false, properties: { query: { type: "string" } } }, outputSchema, annotations: { readOnlyHint: true } },
  ];
}

function badTools() {
  return [
    { name: "widget_a", description: "Does a thing.", inputSchema: { type: "object", additionalProperties: true, properties: { repoRoot: { type: "string" } } } },
    { name: "widget_b", description: "Does a thing.", inputSchema: { type: "object", additionalProperties: true, properties: { repoRoot: { type: "string" } } } },
  ];
}

function writeGoodRepoFixtures(repo) {
  writeFileSync(path.join(repo, "plugin.json"), JSON.stringify({ $schema: "https://agent-plugins.org/schemas/1.0.0/plugin.schema.json", name: "widget" }));
  mkdirSync(path.join(repo, "mcp"), { recursive: true });
  // One fixture file satisfies rules 6 (code+retryable+remediation), 9
  // (effects module naming both tools + effect keys), and 11 (redact).
  writeFileSync(path.join(repo, "mcp", "effects.mjs"), `
    export const TOOL_EFFECTS = { widget_a: { reads: [], writes: [], executesProjectCode: false, network: "none", installsSoftware: false, destructive: false, idempotent: true, approval: "not_required" }, widget_b: {} };
    export function failureEnvelope(code, message) { return { error: { code, message, retryable: false, remediation: "retry", redact: true } }; }
  `);
  mkdirSync(path.join(repo, "skills", "widget"), { recursive: true });
  writeFileSync(path.join(repo, "skills", "widget", "SKILL.md"), "# widget\n\nUses `widget_a` and `widget_b`. See `mcp/effects.mjs`.\n");
}

function writeBadRepoFixtures(repo) {
  mkdirSync(path.join(repo, "skills", "widget"), { recursive: true });
  writeFileSync(path.join(repo, "skills", "widget", "SKILL.md"), "# widget\n\nUses `widget_a` and `widget_ghost` (phantom). See `docs/missing-file.md`.\n");
}

test("all 14 rules pass on a well-formed fixture", async () => {
  const repo = tempRepo();
  writeGoodRepoFixtures(repo);
  const manifest = { ax: { claimBoundaryRequired: true } };
  const result = await runStaticChecks({ repoPath: repo, manifest, tools: goodTools() });
  const failing = result.rules.filter((rule) => rule.status === "fail");
  assert.deepEqual(failing, [], JSON.stringify(failing, null, 2));
  assert.equal(result.verdict, "pass");
});

test("rule 1 fails on semantic-duplicate tool descriptions", async () => {
  const repo = tempRepo();
  writeGoodRepoFixtures(repo);
  const tools = goodTools();
  tools[1].description = tools[0].description;
  const result = await runStaticChecks({ repoPath: repo, manifest: {}, tools });
  assert.ok(result.rules.some((r) => r.ruleId === "static-1-tool-set" && r.status === "fail"));
});

test("rule 2 fails when a tool description lacks when-to-use / when-NOT-to-use", async () => {
  const repo = tempRepo();
  writeGoodRepoFixtures(repo);
  const tools = goodTools();
  tools[0].description = "Does widget A things.";
  const result = await runStaticChecks({ repoPath: repo, manifest: {}, tools });
  assert.ok(result.rules.some((r) => r.ruleId === "static-2-wire-description" && r.status === "fail" && r.evidence.includes("widget_a")));
});

test("rule 3 fails when inputSchema does not set additionalProperties:false", async () => {
  const repo = tempRepo();
  writeGoodRepoFixtures(repo);
  const tools = goodTools();
  tools[0].inputSchema.additionalProperties = true;
  const result = await runStaticChecks({ repoPath: repo, manifest: {}, tools });
  assert.ok(result.rules.some((r) => r.ruleId === "static-3-strict-input-schema" && r.status === "fail"));
});

test("rule 4 fails when no outputSchema is advertised", async () => {
  const repo = tempRepo();
  writeGoodRepoFixtures(repo);
  const tools = goodTools();
  delete tools[0].outputSchema;
  const result = await runStaticChecks({ repoPath: repo, manifest: {}, tools });
  assert.ok(result.rules.some((r) => r.ruleId === "static-4-output-schema" && r.status === "fail"));
});

test("rule 5 fails when claimBoundary is required but missing from outputSchema", async () => {
  const repo = tempRepo();
  writeGoodRepoFixtures(repo);
  const tools = goodTools();
  delete tools[0].outputSchema.properties.claimBoundary;
  const result = await runStaticChecks({ repoPath: repo, manifest: { ax: { claimBoundaryRequired: true } }, tools });
  assert.ok(result.rules.some((r) => r.ruleId === "static-5-claim-boundary" && r.status === "fail" && r.evidence.includes("widget_a")));
});

test("rule 5 is not-applicable when claimBoundaryRequired is false", async () => {
  const repo = tempRepo();
  const result = await runStaticChecks({ repoPath: repo, manifest: { ax: { claimBoundaryRequired: false } }, tools: badTools() });
  assert.ok(result.rules.some((r) => r.ruleId === "static-5-claim-boundary" && r.status === "not-applicable"));
});

test("rule 6 fails when no source file declares a typed code+retryable+remediation envelope", async () => {
  const repo = tempRepo();
  writeBadRepoFixtures(repo);
  const result = await runStaticChecks({ repoPath: repo, manifest: {}, tools: goodTools() });
  assert.ok(result.rules.some((r) => r.ruleId === "static-6-typed-errors" && r.status === "fail"));
});

test("rule 7 reports (non-fatal) when no tool declares truncated+cursor fields", async () => {
  const repo = tempRepo();
  const tools = goodTools();
  delete tools[0].outputSchema.properties.truncated;
  delete tools[1].outputSchema.properties.truncated;
  const result = await runStaticChecks({ repoPath: repo, manifest: {}, tools });
  assert.ok(result.rules.some((r) => r.ruleId === "static-7-bounded-output" && r.status === "report"));
});

test("rule 8 fails on an unconstrained path-like input property", async () => {
  const repo = tempRepo();
  const result = await runStaticChecks({ repoPath: repo, manifest: {}, tools: badTools() });
  assert.ok(result.rules.some((r) => r.ruleId === "static-8-opaque-handles" && r.status === "fail" && r.evidence.includes("repoRoot")));
});

test("rule 9 fails with no annotations and no effects module", async () => {
  const repo = tempRepo();
  writeBadRepoFixtures(repo);
  const result = await runStaticChecks({ repoPath: repo, manifest: {}, tools: badTools() });
  assert.ok(result.rules.some((r) => r.ruleId === "static-9-effect-declarations" && r.status === "fail"));
});

test("rule 11 reports (non-fatal) when no redaction helper is found", async () => {
  const repo = tempRepo();
  writeBadRepoFixtures(repo);
  const result = await runStaticChecks({ repoPath: repo, manifest: {}, tools: goodTools() });
  assert.ok(result.rules.some((r) => r.ruleId === "static-11-no-secrets" && r.status === "report"));
});

test("rule 12 fails when SKILL.md references a phantom tool sharing a real prefix", async () => {
  const repo = tempRepo();
  writeBadRepoFixtures(repo);
  const result = await runStaticChecks({ repoPath: repo, manifest: {}, tools: goodTools() });
  assert.ok(result.rules.some((r) => r.ruleId === "static-12-no-phantom-capabilities" && r.status === "fail" && r.evidence.includes("widget_ghost")));
});

test("rule 13 fails when plugin.json is missing", async () => {
  const repo = tempRepo();
  const result = await runStaticChecks({ repoPath: repo, manifest: {}, tools: goodTools() });
  assert.ok(result.rules.some((r) => r.ruleId === "static-13-plugin-json-present" && r.status === "fail"));
});

test("rule 14 fails when SKILL.md references a nonexistent file", async () => {
  const repo = tempRepo();
  writeBadRepoFixtures(repo);
  const result = await runStaticChecks({ repoPath: repo, manifest: {}, tools: goodTools() });
  assert.ok(result.rules.some((r) => r.ruleId === "static-14-skill-references" && r.status === "fail" && r.evidence.includes("missing-file.md")));
});

// F14 regression: a well-formed, semantically explicit description using
// common paraphrases ("Best for X. Avoid for Y.") must not false-FAIL just
// because it doesn't match the original narrow phrase list.
test("F14: rule 2 accepts common paraphrases of when-to-use / when-not-to-use", async () => {
  const repo = tempRepo();
  writeGoodRepoFixtures(repo);
  const tools = goodTools();
  tools[0].description = "Best for demo probes. Avoid for production work.";
  const result = await runStaticChecks({ repoPath: repo, manifest: {}, tools });
  assert.ok(result.rules.some((r) => r.ruleId === "static-2-wire-description" && r.evidence.includes("widget_a") && r.status === "pass"), JSON.stringify(result.rules.filter((r) => r.ruleId === "static-2-wire-description")));
});

// F14 regression: a skills/<name>/SKILL.md's own relative reference (e.g.
// `./references/guide.md`) must resolve against that SKILL.md's own
// directory, not the package root — the original code always resolved
// against repoPath, so a genuinely shipped file false-FAILed.
test("F14: rule 14 resolves a SKILL.md-relative reference against the SKILL.md's own directory", async () => {
  const repo = tempRepo();
  mkdirSync(path.join(repo, "skills", "demo", "references"), { recursive: true });
  writeFileSync(path.join(repo, "skills", "demo", "references", "guide.md"), "# guide\n");
  writeFileSync(path.join(repo, "skills", "demo", "SKILL.md"), "# demo\n\nSee `./references/guide.md` for details.\n");
  const result = await runStaticChecks({ repoPath: repo, manifest: {}, tools: [] });
  assert.ok(result.rules.some((r) => r.ruleId === "static-14-skill-references" && r.status === "pass"), JSON.stringify(result.rules.filter((r) => r.ruleId === "static-14-skill-references")));
});

// F10 regressions: rules 5, 6, 9, and 11 must never report "pass" from
// evidence that cannot actually prove the property (a computed-but-unused
// shape flag for rule 5; a lexical grep for rules 6/9/11 that matches
// comment text unrelated to any real implementation).
test("F10: rule 5 fails when claimBoundary is present but its shape is wrong (shapeOk=false), not pass", async () => {
  const repo = tempRepo();
  writeGoodRepoFixtures(repo);
  const tools = goodTools();
  // Present claimBoundary object, but missing one required sub-key (gaps)
  // from both `properties` and `required` — shapeOk must be false.
  delete tools[0].outputSchema.properties.claimBoundary.properties.gaps;
  tools[0].outputSchema.properties.claimBoundary.required = tools[0].outputSchema.properties.claimBoundary.required.filter((key) => key !== "gaps");
  const result = await runStaticChecks({ repoPath: repo, manifest: { ax: { claimBoundaryRequired: true } }, tools });
  const rule5 = result.rules.find((r) => r.ruleId === "static-5-claim-boundary" && r.evidence.includes("widget_a"));
  assert.equal(rule5.status, "fail", JSON.stringify(rule5));
  assert.match(rule5.evidence, /shapeOk=false/);
});

test("F10: rule 6 reports unverifiable (not pass) when the only evidence is a lexical comment match", async () => {
  const repo = tempRepo();
  mkdirSync(path.join(repo, "skills", "widget"), { recursive: true });
  writeFileSync(path.join(repo, "skills", "widget", "SKILL.md"), "# widget\n");
  // Mirrors the QA repro: a comment mentioning all three tokens, with no
  // real typed-error implementation anywhere.
  writeFileSync(path.join(repo, "fake-source.mjs"), "// code retryable remediation\nexport const noop = 1;\n");
  const result = await runStaticChecks({ repoPath: repo, manifest: {}, tools: [] });
  const rule6 = result.rules.find((r) => r.ruleId === "static-6-typed-errors");
  assert.equal(rule6.status, "unverifiable", JSON.stringify(rule6));
});

test("F10: rule 9 reports unverifiable (not pass) when the only evidence is a lexical effects-module comment", async () => {
  const repo = tempRepo();
  mkdirSync(path.join(repo, "mcp"), { recursive: true });
  // Comment mentions the tool name and an effect key, but declares nothing.
  writeFileSync(path.join(repo, "mcp", "effects.mjs"), "// demo_tool reads\nexport const noop = 1;\n");
  const tools = [{ name: "demo_tool", description: "Use this when needed. Do not use it otherwise.", inputSchema: { type: "object", additionalProperties: false, properties: {} } }];
  const result = await runStaticChecks({ repoPath: repo, manifest: {}, tools });
  const rule9 = result.rules.find((r) => r.ruleId === "static-9-effect-declarations");
  assert.equal(rule9.status, "unverifiable", JSON.stringify(rule9));
});

test("F10: rule 11 reports unverifiable (not pass) when the only evidence is the word \"redact\" in a comment", async () => {
  const repo = tempRepo();
  writeFileSync(path.join(repo, "fake-source.mjs"), "// redact secrets eventually\nexport const noop = 1;\n");
  const result = await runStaticChecks({ repoPath: repo, manifest: {}, tools: [] });
  const rule11 = result.rules.find((r) => r.ruleId === "static-11-no-secrets");
  assert.equal(rule11.status, "unverifiable", JSON.stringify(rule11));
});

test("static gate can fetch the live tools/list payload from a real MCP server (no pre-supplied tools)", async () => {
  const repo = tempRepo();
  const artifactRoot = mkdtempSync(path.join(os.tmpdir(), "right-ax-static-artifact-"));
  cpSync(path.join(HERE, "fixtures", "fake-mcp-server.mjs"), path.join(artifactRoot, "fake-mcp-server.mjs"));
  const manifest = { surfaces: { mcp: { command: "node", args: ["fake-mcp-server.mjs"], cwd: "${ARTIFACT_ROOT}" } }, ax: {} };
  const result = await runStaticChecks({ repoPath: repo, manifest, artifactRoot });
  assert.equal(result.toolCount, 2);
  assert.ok(result.rules.some((r) => r.ruleId === "static-2-wire-description" && r.evidence.includes("good_tool") && r.status === "pass"));
  assert.ok(result.rules.some((r) => r.ruleId === "static-2-wire-description" && r.evidence.includes("bad_tool") && r.status === "fail"));
});
