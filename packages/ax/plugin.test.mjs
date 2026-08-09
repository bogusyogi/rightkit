import assert from "node:assert/strict";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { scaffoldPlugin } from "./plugin/scaffold.mjs";
import { validateRepoLayout } from "./plugin/validate.mjs";

function tempRepo() {
  return mkdtempSync(path.join(os.tmpdir(), "right-ax-plugin-"));
}

test("scaffold -> validate round trip is valid with no findings", () => {
  const repo = tempRepo();
  writeFileSync(path.join(repo, "package.json"), JSON.stringify({ name: "widget-tool", version: "1.0.0", description: "A widget tool." }));
  const { written } = scaffoldPlugin(repo, { mcpCommand: "node server.mjs" });
  assert.equal(written.length, 3);
  for (const file of written) assert.ok(existsSync(file), file);
  const { valid, findings } = validateRepoLayout(repo);
  assert.equal(valid, true, JSON.stringify(findings));
  assert.deepEqual(findings, []);
});

test("plugin.json rejects a name with consecutive dashes or dots", () => {
  const repo = tempRepo();
  writeFileSync(path.join(repo, "plugin.json"), JSON.stringify({ $schema: "https://agent-plugins.org/schemas/1.0.0/plugin.schema.json", name: "bad--name" }));
  const { valid, findings } = validateRepoLayout(repo);
  assert.equal(valid, false);
  assert.ok(findings.some((f) => f.ruleId === "plugin-json-schema" && f.severity === "error" && /pattern mismatch/.test(f.evidence)));
});

test("plugin.json missing $schema is a fatal schema violation", () => {
  const repo = tempRepo();
  writeFileSync(path.join(repo, "plugin.json"), JSON.stringify({ name: "ok-name" }));
  const { valid, findings } = validateRepoLayout(repo);
  assert.equal(valid, false);
  assert.ok(findings.some((f) => f.ruleId === "plugin-json-schema" && /missing required property \$schema/.test(f.evidence)));
});

test("unknown top-level plugin.json field is reported, not fatal", () => {
  const repo = tempRepo();
  writeFileSync(path.join(repo, "plugin.json"), JSON.stringify({ $schema: "https://agent-plugins.org/schemas/1.0.0/plugin.schema.json", name: "ok-name", totallyUnknownField: true }));
  const { valid, findings } = validateRepoLayout(repo);
  assert.equal(valid, true);
  assert.ok(findings.some((f) => f.severity === "info" && /unexpected property totallyUnknownField/.test(f.evidence)));
});

test("mcp.json path escaping the plugin root via ${PLUGIN_ROOT} is fatal", () => {
  const repo = tempRepo();
  writeFileSync(path.join(repo, "plugin.json"), JSON.stringify({ $schema: "https://agent-plugins.org/schemas/1.0.0/plugin.schema.json", name: "ok-name" }));
  writeFileSync(path.join(repo, "mcp.json"), JSON.stringify({
    $schema: "https://agent-plugins.org/schemas/1.0.0/mcp.schema.json",
    mcpServers: { main: { type: "stdio", command: "node", cwd: "${PLUGIN_ROOT}/../../etc" } },
  }));
  const { valid, findings } = validateRepoLayout(repo);
  assert.equal(valid, false);
  assert.ok(findings.some((f) => f.ruleId === "plugin-path-containment"));
});

test("mcp.json with a contained ${PLUGIN_DATA} path is valid", () => {
  const repo = tempRepo();
  writeFileSync(path.join(repo, "plugin.json"), JSON.stringify({ $schema: "https://agent-plugins.org/schemas/1.0.0/plugin.schema.json", name: "ok-name" }));
  writeFileSync(path.join(repo, "mcp.json"), JSON.stringify({
    $schema: "https://agent-plugins.org/schemas/1.0.0/mcp.schema.json",
    mcpServers: { main: { type: "stdio", command: "node", args: ["server.mjs"], env: { DATA_DIR: "${PLUGIN_DATA}/cache" } } },
  }));
  const { valid, findings } = validateRepoLayout(repo);
  assert.equal(valid, true, JSON.stringify(findings));
});

// F8 regressions: the original validateMcpPathSafety only ever examined
// `cwd`, plus `args`/`env` entries that happened to contain a
// ${PLUGIN_ROOT}/${PLUGIN_DATA} placeholder — it never looked at `command`
// at all, and skipped args/env entirely when no placeholder was present. A
// plain absolute, UNC, drive-relative, or parent-escaping path (with no
// placeholder) sailed through unexamined in every one of those fields. Each
// case below mirrors one of the QA repro's named fixtures.
function mcpManifestWithServer(server) {
  return { $schema: "https://agent-plugins.org/schemas/1.0.0/mcp.schema.json", mcpServers: { main: { type: "stdio", command: "node", ...server } } };
}

test("F8: a drive-relative cwd (e.g. C:foo, no leading slash) is flagged", () => {
  const repo = tempRepo();
  writeFileSync(path.join(repo, "plugin.json"), JSON.stringify({ $schema: "https://agent-plugins.org/schemas/1.0.0/plugin.schema.json", name: "ok-name" }));
  writeFileSync(path.join(repo, "mcp.json"), JSON.stringify(mcpManifestWithServer({ cwd: "C:escape-from-root" })));
  const { valid, findings } = validateRepoLayout(repo);
  assert.equal(valid, false);
  assert.ok(findings.some((f) => f.ruleId === "plugin-path-containment" && /drive-relative/.test(f.evidence)));
});

test("F8: a parent-traversal arg with no placeholder is flagged", () => {
  const repo = tempRepo();
  writeFileSync(path.join(repo, "plugin.json"), JSON.stringify({ $schema: "https://agent-plugins.org/schemas/1.0.0/plugin.schema.json", name: "ok-name" }));
  writeFileSync(path.join(repo, "mcp.json"), JSON.stringify(mcpManifestWithServer({ args: ["../../outside/server.mjs"] })));
  const { valid, findings } = validateRepoLayout(repo);
  assert.equal(valid, false);
  assert.ok(findings.some((f) => f.ruleId === "plugin-path-containment" && /traversal/.test(f.evidence)));
});

test("F8: an absolute arg with no placeholder is flagged", () => {
  const repo = tempRepo();
  writeFileSync(path.join(repo, "plugin.json"), JSON.stringify({ $schema: "https://agent-plugins.org/schemas/1.0.0/plugin.schema.json", name: "ok-name" }));
  writeFileSync(path.join(repo, "mcp.json"), JSON.stringify(mcpManifestWithServer({ args: ["/etc/passwd"] })));
  const { valid, findings } = validateRepoLayout(repo);
  assert.equal(valid, false);
  assert.ok(findings.some((f) => f.ruleId === "plugin-path-containment" && /absolute path/.test(f.evidence)));
});

test("F8: a UNC arg is flagged", () => {
  const repo = tempRepo();
  writeFileSync(path.join(repo, "plugin.json"), JSON.stringify({ $schema: "https://agent-plugins.org/schemas/1.0.0/plugin.schema.json", name: "ok-name" }));
  writeFileSync(path.join(repo, "mcp.json"), JSON.stringify(mcpManifestWithServer({ args: ["\\\\evil-host\\share\\server.mjs"] })));
  const { valid, findings } = validateRepoLayout(repo);
  assert.equal(valid, false);
  assert.ok(findings.some((f) => f.ruleId === "plugin-path-containment" && /UNC path/.test(f.evidence)));
});

test("F8: a parent-traversal command (not just args) is flagged", () => {
  const repo = tempRepo();
  writeFileSync(path.join(repo, "plugin.json"), JSON.stringify({ $schema: "https://agent-plugins.org/schemas/1.0.0/plugin.schema.json", name: "ok-name" }));
  writeFileSync(path.join(repo, "mcp.json"), JSON.stringify({ $schema: "https://agent-plugins.org/schemas/1.0.0/mcp.schema.json", mcpServers: { main: { type: "stdio", command: "../../outside/node" } } }));
  const { valid, findings } = validateRepoLayout(repo);
  assert.equal(valid, false);
  assert.ok(findings.some((f) => f.ruleId === "plugin-path-containment" && f.evidence.includes(".command")));
});

test("F8: a bare executable name (e.g. \"node\") in command/args is never flagged", () => {
  const repo = tempRepo();
  writeFileSync(path.join(repo, "plugin.json"), JSON.stringify({ $schema: "https://agent-plugins.org/schemas/1.0.0/plugin.schema.json", name: "ok-name" }));
  writeFileSync(path.join(repo, "mcp.json"), JSON.stringify(mcpManifestWithServer({ args: ["server.mjs", "--flag"] })));
  const { valid, findings } = validateRepoLayout(repo);
  assert.equal(valid, true, JSON.stringify(findings));
});

test("a skills/<name>/ directory missing SKILL.md is a fatal finding", () => {
  const repo = tempRepo();
  writeFileSync(path.join(repo, "plugin.json"), JSON.stringify({ $schema: "https://agent-plugins.org/schemas/1.0.0/plugin.schema.json", name: "ok-name" }));
  mkdirSync(path.join(repo, "skills", "empty"), { recursive: true });
  const { valid, findings } = validateRepoLayout(repo);
  assert.equal(valid, false);
  assert.ok(findings.some((f) => f.ruleId === "plugin-skill-file"));
});
