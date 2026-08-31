import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { applyPlan, planSync, syncCommand } from "./sync.mjs";
import { makeTempRepo, PUBLIC_DEPS, PRIVATE_DEPS, SAMPLE_MANIFEST } from "./test-support.mjs";

test("sync refuses a private repo and writes nothing", async () => {
  const dir = makeTempRepo(SAMPLE_MANIFEST);
  const result = await syncCommand(dir, { deps: PRIVATE_DEPS });
  assert.equal(result.ok, false);
  assert.equal(result.stage, "visibility");
  assert.equal(fs.existsSync(path.join(dir, ".github", "workflows")), false);
});

test("sync writes one rendered file per lane for a public repo", async () => {
  const dir = makeTempRepo(SAMPLE_MANIFEST);
  const result = await syncCommand(dir, { deps: PUBLIC_DEPS });
  assert.equal(result.ok, true);
  for (const filename of ["ci.yml", "package-smoke.yml"]) {
    assert.ok(fs.existsSync(path.join(dir, ".github", "workflows", filename)), `${filename} was not written`);
  }
});

test("sync --dry-run plans changes without writing files", async () => {
  const dir = makeTempRepo(SAMPLE_MANIFEST);
  const result = await syncCommand(dir, { deps: PUBLIC_DEPS, dryRun: true });
  assert.equal(result.ok, true);
  assert.ok(result.changes.some((c) => c.action === "create"));
  assert.equal(fs.existsSync(path.join(dir, ".github", "workflows")), false);
});

test("sync is idempotent: a second run reports no changes", async () => {
  const dir = makeTempRepo(SAMPLE_MANIFEST);
  await syncCommand(dir, { deps: PUBLIC_DEPS });
  const second = await syncCommand(dir, { deps: PUBLIC_DEPS });
  assert.ok(second.changes.every((c) => c.action === "unchanged"));
});

test("sync removes a workflow file for a lane dropped from the manifest", async () => {
  const dir = makeTempRepo(SAMPLE_MANIFEST);
  await syncCommand(dir, { deps: PUBLIC_DEPS });
  fs.writeFileSync(path.join(dir, ".rightgit.json"), `${JSON.stringify({ ...SAMPLE_MANIFEST, lanes: ["ci"] }, null, 2)}\n`, "utf8");
  const result = await syncCommand(dir, { deps: PUBLIC_DEPS });
  assert.equal(result.ok, true);
  assert.equal(fs.existsSync(path.join(dir, ".github", "workflows", "package-smoke.yml")), false);
  assert.ok(fs.existsSync(path.join(dir, ".github", "workflows", "ci.yml")));
});

test("sync reports manifest errors instead of writing files", async () => {
  const dir = makeTempRepo(null);
  const result = await syncCommand(dir, { deps: PUBLIC_DEPS });
  assert.equal(result.ok, false);
  assert.equal(result.stage, "manifest");
});

test("sync rejects a Membrane-like release chain without root @rightkit/release", async () => {
  const manifest = {
    schemaVersion: 1,
    compile: "github-actions-only",
    lanes: ["ci", "release-candidate"],
    matrix: { os: ["windows-2025", "macos-15"], node: ["22.23.2"] },
    packageManager: "pnpm@11.24.0",
    templateVersion: "1.5.0",
    profile: "rust-hybrid",
    gate: { path: "scripts/ci/run-ci.sh" },
    candidate: {
      buildScript: "release:candidate:build",
      checkScript: "release:candidate:check",
      admissionScript: "release:admit",
      stageSummaryScript: "release:stage-summary",
      artifactRoot: "membrane-release-candidate",
      os: ["windows-2025", "macos-15"],
      signWindows: true,
      signMacos: true,
      releaseChain: {
        evidenceVerificationScript: "release:verify-evidence",
        windowsFinalizeScript: "release:finalize:windows",
        macosFinalizeScript: "release:finalize:macos",
        installedQualificationScript: "release:qualify:installed",
        publishScript: "release:publish:qualified",
        qualificationRunnerVariable: "MEMBRANE_QUALIFICATION_RUNNER_LABEL",
      },
    },
  };
  const dir = makeTempRepo(manifest, { packageJson: { name: "membrane", devDependencies: { "@rightkit/git": "0.2.13" } } });
  const missing = await syncCommand(dir, { deps: PUBLIC_DEPS });
  assert.equal(missing.ok, false);
  assert.equal(missing.stage, "dependency");
  assert.match(missing.errors[0], /direct devDependency @rightkit\/release/);
  assert.equal(fs.existsSync(path.join(dir, ".github", "workflows")), false);

  fs.writeFileSync(path.join(dir, "package.json"), JSON.stringify({ name: "membrane", devDependencies: { "@rightkit/git": "0.2.13", "@rightkit/release": "0.2.84" } }), "utf8");
  const present = await syncCommand(dir, { deps: PUBLIC_DEPS });
  assert.equal(present.ok, true);
  assert.ok(fs.existsSync(path.join(dir, ".github", "workflows", "release-candidate.yml")));
});

test("sync refuses an unmanaged same-name workflow", async () => {
  const dir = makeTempRepo(SAMPLE_MANIFEST);
  const target = path.join(dir, ".github", "workflows", "ci.yml");
  fs.mkdirSync(path.dirname(target), { recursive: true });
  fs.writeFileSync(target, "name: handwritten\n", "utf8");
  const result = await syncCommand(dir, { deps: PUBLIC_DEPS });
  assert.equal(result.stage, "managed-conflict");
  assert.equal(fs.readFileSync(target, "utf8"), "name: handwritten\n");
});

test("apply preflights every planned byte before writing", async () => {
  const dir = makeTempRepo(SAMPLE_MANIFEST);
  const plan = await planSync(dir, PUBLIC_DEPS);
  const target = path.join(dir, ".github", "workflows", "ci.yml");
  fs.mkdirSync(path.dirname(target), { recursive: true });
  fs.writeFileSync(target, "raced\n", "utf8");
  const applied = applyPlan(dir, plan);
  assert.equal(applied.stage, "apply-conflict");
  assert.equal(fs.existsSync(path.join(dir, ".github", "workflows", "package-smoke.yml")), false);
});

// F15 end-to-end: a hostile `.rightgit.json` on disk — the real attack
// surface, not just the in-memory validateManifest() unit — must be refused
// at the manifest stage, before renderLanes() (and therefore render.mjs's
// unescaped `${TOKEN}` substitution) ever sees the payload. This is the
// concrete proof that "none survive into rendered output": nothing is
// rendered or written at all when the manifest is hostile. Covers a family
// of payloads across the fields that flow into the credential-bearing
// publish-package-managers lane.
const HOSTILE_MANIFESTS = [
  { tap: "acme/tap", formulaName: "${{ secrets.APPLE_CERTIFICATE }}" },
  { tap: "acme/tap", formulaName: "`whoami`" },
  { tap: "acme/tap", formulaName: "widgets\nrun: exit 1" },
  { tap: "acme/${{ secrets.X }}", formulaName: "widgets" },
];

for (const [i, homebrew] of HOSTILE_MANIFESTS.entries()) {
  test(`F15: sync refuses a hostile manifest and writes nothing (payload ${i})`, async () => {
    const dir = makeTempRepo({
      ...SAMPLE_MANIFEST,
      lanes: ["publish-package-managers"],
      publish: { homebrew, winget: { packageIdentifier: "Acme.Widgets" } },
    });
    const result = await syncCommand(dir, { deps: PUBLIC_DEPS });
    assert.equal(result.ok, false);
    assert.equal(result.stage, "manifest");
    assert.equal(fs.existsSync(path.join(dir, ".github", "workflows")), false, "no workflow file may be written for a hostile manifest");
  });
}
