import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { assemblePortableCore, CLIENT_PROJECTION_KINDS, validatePortableCore } from "./plugin/portable-core.mjs";

function fixture() {
  const root = mkdtempSync(join(tmpdir(), "rightax-portable-"));
  const source = join(root, "source");
  mkdirSync(join(source, "audit"), { recursive: true });
  writeFileSync(join(source, "audit", "SKILL.md"), "# Audit\n");
  writeFileSync(join(root, "plugin.json"), JSON.stringify({ name: "legion", version: "0.1.0" }));
  return { root, source, output: join(root, "output") };
}

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
