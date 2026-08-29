import assert from "node:assert/strict";
import { mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { spawnSync } from "node:child_process";
import test from "node:test";
import { materializeHardeningEvidence } from "./hardening-evidence.mjs";

test("exact artifact-bound schema token allowance suppresses only declared false positive", () => {
  const root = mkdtempSync(join(tmpdir(), "rightrelease-hardening-"));
  const artifact = join(root, "membrane.exe");
  const source = join(root, "remediation.rs");
  const evidencePath = join(root, "evidence.json");
  writeFileSync(artifact, "enum=system_prompt");
  writeFileSync(source, 'Self::SystemPrompt => "system_prompt",\n');
  materializeHardeningEvidence({
    root,
    outputPath: evidencePath,
    allowances: [{
      file: artifact,
      rule: "system-prompt-marker",
      exact: "system_prompt",
      sourceEvidence: "remediation.rs:1",
      rationale: "serialized remediation enum value",
    }],
  });
  const scanner = join(import.meta.dirname, "hardeningscan.mjs");
  const allowed = spawnSync(process.execPath, [scanner, "--allow-evidence", evidencePath, artifact], { cwd: root, encoding: "utf8" });
  assert.equal(allowed.status, 0, allowed.stderr);
  const stale = JSON.parse(readFileSync(evidencePath, "utf8"));
  stale.allowances[0].artifactSha256 = "0".repeat(64);
  writeFileSync(evidencePath, JSON.stringify(stale));
  const rejected = spawnSync(process.execPath, [scanner, "--allow-evidence", evidencePath, artifact], { cwd: root, encoding: "utf8" });
  assert.equal(rejected.status, 1);
  assert.match(rejected.stderr, /system prompt marker leaked/);
});

test("allowance requires narrow known token and source evidence", () => {
  const root = mkdtempSync(join(tmpdir(), "rightrelease-hardening-"));
  const artifact = join(root, "x.exe");
  const source = join(root, "x.rs");
  writeFileSync(artifact, "system_prompt");
  writeFileSync(source, 'const TOKEN: &str = "system_prompt";\n');
  assert.throws(() => materializeHardeningEvidence({
    root,
    outputPath: join(root, "evidence.json"),
    allowances: [{ file: artifact, rule: "system-prompt-marker", exact: "SYSTEM_PROMPT", sourceEvidence: "x.rs:1", rationale: "legitimate enum value" }],
  }), /exact token/);
});

test("scanner rejects crafted unknown allowance", () => {
  const root = mkdtempSync(join(tmpdir(), "rightrelease-hardening-"));
  const artifact = join(root, "x.exe");
  const source = join(root, "x.rs");
  const evidence = join(root, "evidence.json");
  writeFileSync(artifact, "system_prompt");
  writeFileSync(source, 'const TOKEN: &str = "system_prompt";\n');
  const receipt = materializeHardeningEvidence({
    root,
    outputPath: evidence,
    allowances: [{ file: artifact, rule: "system-prompt-marker", exact: "system_prompt", sourceEvidence: "x.rs:1", rationale: "serialized enum value" }],
  });
  receipt.allowances[0].rule = "secret-env-name";
  writeFileSync(evidence, JSON.stringify(receipt));
  const scanner = join(import.meta.dirname, "hardeningscan.mjs");
  const rejected = spawnSync(process.execPath, [scanner, "--allow-evidence", evidence, artifact], { cwd: root, encoding: "utf8" });
  assert.equal(rejected.status, 1);
  assert.match(rejected.stderr, /invalid hardening allowance/);
});

test("release CLI forbids complete hardening bypass", () => {
  const release = spawnSync(process.execPath, [join(import.meta.dirname, "release.mjs"), "--skip-hardening"], { cwd: import.meta.dirname, encoding: "utf8" });
  assert.equal(release.status, 1);
  assert.match(release.stderr, /--skip-hardening is forbidden/);
});

test("hardeningscan without --allow-evidence keeps the first artifact path", () => {
  // Regression: with no --allow-evidence flag, allowIndex is -1 and a filter
  // on `index !== allowIndex + 1` dropped artifact index 0, so a lone
  // artifact arg failed as "at least one artifact path is required".
  const dir = mkdtempSync(join(tmpdir(), "hardeningscan-args-"));
  const artifact = join(dir, "artifact.txt");
  writeFileSync(artifact, "clean text artifact\n");
  const scan = spawnSync(process.execPath, [join(import.meta.dirname, "hardeningscan.mjs"), artifact], { cwd: dir, encoding: "utf8" });
  assert.equal(scan.status, 0, scan.stderr);
  assert.match(scan.stdout + scan.stderr, /hardeningscan: clean/);
});
