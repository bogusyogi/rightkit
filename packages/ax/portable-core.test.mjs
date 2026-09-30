import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { assemblePortableCore, CLIENT_PROJECTION_KINDS, validatePortableCore, validateResourceClosure } from "./plugin/portable-core.mjs";

function fixture() {
  const root = mkdtempSync(join(tmpdir(), "rightax-portable-"));
  const source = join(root, "source");
  mkdirSync(join(source, "audit"), { recursive: true });
  writeFileSync(join(source, "audit", "SKILL.md"), "# Audit\n");
  writeFileSync(join(root, "plugin.json"), JSON.stringify({ name: "legion", version: "0.1.0" }));
  return { root, source, output: join(root, "output") };
}

test("ships the hooks manifest when one is declared", () => {
  // A core without hooks leaves every declared hook event unregistered, which
  // reads to a user as the guard being absent rather than degraded.
  const f = fixture();
  const hooks = join(f.root, "hooks.json");
  writeFileSync(hooks, JSON.stringify({ description: "guard", hooks: { SessionStart: [] } }));
  const contract = assemblePortableCore({
    outputDir: f.output,
    pluginManifestPath: join(f.root, "plugin.json"),
    hooksManifestPath: hooks,
    skills: [{ id: "audit", visibility: "public", sourceRoot: f.source, sourceDir: join(f.source, "audit") }],
    clientProjections: Object.fromEntries(Object.entries(CLIENT_PROJECTION_KINDS).map(([id, declaration]) => [id, declaration])),
  });
  assert.ok(contract.publicFiles.includes("hooks/hooks.json"));
  assert.deepEqual(validatePortableCore(f.output).errors, [], "a core carrying hooks validates");
});

test("ships declared agents alongside skills", () => {
  // A core that carries only skills leaves agent-only roles unreachable: that
  // is how Legion's sage, alchemist and covenant-seat never reached any client
  // while oracle worked, because oracle is also a skill.
  const f = fixture();
  mkdirSync(join(f.source, "agents"), { recursive: true });
  writeFileSync(join(f.source, "agents", "sage.md"), "# Sage" + String.fromCharCode(10));
  const contract = assemblePortableCore({
    outputDir: f.output,
    pluginManifestPath: join(f.root, "plugin.json"),
    skills: [{ id: "audit", visibility: "public", sourceRoot: f.source, sourceDir: join(f.source, "audit") }],
    agents: [{ name: "sage", sourceRoot: f.source, sourceFile: join(f.source, "agents", "sage.md") }],
    clientProjections: Object.fromEntries(Object.entries(CLIENT_PROJECTION_KINDS).map(([id, declaration]) => [id, declaration])),
  });
  assert.deepEqual(contract.publicAgents, ["sage"]);
  assert.ok(contract.publicFiles.includes("agents/sage.md"));
  assert.equal(readFileSync(join(f.output, "agents", "sage.md"), "utf8"), "# Sage" + String.fromCharCode(10));
  assert.deepEqual(validatePortableCore(f.output).errors, [], "an assembled core with agents validates");
});

test("assembles declared public skills and exact client boundaries", () => {
  const f = fixture();
  const contract = assemblePortableCore({
    outputDir: f.output,
    pluginManifestPath: join(f.root, "plugin.json"),
    skills: [{ id: "audit", visibility: "public", sourceRoot: f.source, sourceDir: join(f.source, "audit") }],
    clientProjections: Object.fromEntries(Object.entries(CLIENT_PROJECTION_KINDS).map(([id, declaration]) => [id, declaration])),
  });
  assert.deepEqual(contract.publicSkills, ["audit"]);
  assert.equal(contract.clientProjections.pi.executableRegistration, false);
  assert.deepEqual(validatePortableCore(f.output).errors, []);
});

test("rejects private or personal skills", () => {
  const f = fixture();
  for (const declaration of [{ visibility: "private" }, { visibility: "public", personal: true }]) {
    assert.throws(() => assemblePortableCore({
      outputDir: f.output,
      pluginManifestPath: join(f.root, "plugin.json"),
      skills: [{ id: "audit", sourceRoot: f.source, sourceDir: join(f.source, "audit"), ...declaration }],
      clientProjections: {},
    }), /rejects/);
  }
});

test("rejects missing or overridden client boundaries", () => {
  const f = fixture();
  const exact = Object.fromEntries(Object.entries(CLIENT_PROJECTION_KINDS).map(([id, declaration]) => [id, declaration]));
  assert.throws(() => assemblePortableCore({
    outputDir: f.output,
    pluginManifestPath: join(f.root, "plugin.json"),
    skills: [{ id: "audit", visibility: "public", sourceRoot: f.source, sourceDir: join(f.source, "audit") }],
    clientProjections: { ...exact, claude: { ...exact.claude, portableCore: true } },
  }), /invalid claude/);
  const { pi, ...missingPi } = exact;
  assert.throws(() => assemblePortableCore({
    outputDir: f.output,
    pluginManifestPath: join(f.root, "plugin.json"),
    skills: [{ id: "audit", visibility: "public", sourceRoot: f.source, sourceDir: join(f.source, "audit") }],
    clientProjections: missingPi,
  }), /missing required client projection: pi/);
});

test("rejects secret paths and personal workspace markers inside public skill", () => {
  for (const [name, content] of [[".env", "TOKEN=x"], ["notes.md", "D:\\Claude\\private"]]) {
    const f = fixture();
    writeFileSync(join(f.source, "audit", name), content);
    assert.throws(() => assemblePortableCore({
      outputDir: f.output,
      pluginManifestPath: join(f.root, "plugin.json"),
      skills: [{ id: "audit", visibility: "public", sourceRoot: f.source, sourceDir: join(f.source, "audit") }],
      clientProjections: CLIENT_PROJECTION_KINDS,
    }), /rejects/);
  }
});

test("records fidelity boundaries & rejects broken resource closure", () => {
  const f = fixture();
  writeFileSync(join(f.source, "audit", "SKILL.md"), "# Audit\n\n[missing](references/missing.md)\n");
  const exact = Object.fromEntries(Object.entries(CLIENT_PROJECTION_KINDS).map(([id, declaration]) => [id, declaration]));
  assert.equal(exact.pi.fidelity, "skills-only");
  assert.equal(exact.pi.executableRegistration, false);
  assert.throws(() => assemblePortableCore({
    outputDir: f.output,
    pluginManifestPath: join(f.root, "plugin.json"),
    skills: [{ id: "audit", visibility: "public", sourceRoot: f.source, sourceDir: join(f.source, "audit") }],
    clientProjections: exact,
  }), /missing resource/);
});

test("rescans complete output and rejects undeclared files", () => {
  const f = fixture();
  const exact = Object.fromEntries(Object.entries(CLIENT_PROJECTION_KINDS).map(([id, declaration]) => [id, declaration]));
  assemblePortableCore({
    outputDir: f.output,
    pluginManifestPath: join(f.root, "plugin.json"),
    skills: [{ id: "audit", visibility: "public", sourceRoot: f.source, sourceDir: join(f.source, "audit") }],
    clientProjections: exact,
  });
  writeFileSync(join(f.output, "private.md"), "not allowed");
  const result = validatePortableCore(f.output);
  assert.equal(result.valid, false);
  assert.match(result.errors.join("\n"), /undeclared output file/);
  assert.equal(validateResourceClosure(f.output, new Set(["plugin.json", "skills/audit/SKILL.md", "rightax-portable-core.json"])).length, 0);
});

test("rejects secret-bearing manifest fields", () => {
  const f = fixture();
  writeFileSync(join(f.root, "plugin.json"), JSON.stringify({ name: "legion", version: "0.1.0", token: "do-not-ship" }));
  assert.throws(() => assemblePortableCore({
    outputDir: f.output,
    pluginManifestPath: join(f.root, "plugin.json"),
    skills: [{ id: "audit", visibility: "public", sourceRoot: f.source, sourceDir: join(f.source, "audit") }],
    clientProjections: CLIENT_PROJECTION_KINDS,
  }), /secret-bearing/);
});
