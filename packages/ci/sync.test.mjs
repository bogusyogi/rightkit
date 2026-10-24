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
    packageManager: "pnpm@11.25.0",
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
  const dir = makeTempRepo(manifest, { packageJson: { name: "membrane", devDependencies: { "@rightkit/ci": "0.2.13" } } });
  const missing = await syncCommand(dir, { deps: PUBLIC_DEPS });
  assert.equal(missing.ok, false);
  assert.equal(missing.stage, "dependency");
  assert.match(missing.errors[0], /direct devDependency @rightkit\/release/);
  assert.equal(fs.existsSync(path.join(dir, ".github", "workflows")), false);

  fs.writeFileSync(path.join(dir, "package.json"), JSON.stringify({ name: "membrane", devDependencies: { "@rightkit/ci": "0.2.13", "@rightkit/release": "0.6.5" } }), "utf8");
  const present = await syncCommand(dir, { deps: PUBLIC_DEPS });
  assert.equal(present.ok, true);
  assert.ok(fs.existsSync(path.join(dir, ".github", "workflows", "release-candidate.yml")));
});

// The check job of every CHECK_JOB_LANES lane runs `rightkit-gate check` from the repository's own @rightkit/release, which first
// shipped it in 0.6.5: an exact pin below that renders a workflow whose first job cannot run, so sync refuses it before writing.
const withRelease = (spec, field = "devDependencies") => ({ name: "widgets", [field]: { "@rightkit/release": spec } });

test("sync refuses an exact @rightkit/release pin below 0.6.5 for a lane that renders the check job, and writes nothing", async () => {
  for (const [spec, field] of [["0.6.4", "devDependencies"], ["0.2.84", "devDependencies"], ["0.5.99", "dependencies"], ["0.0.1", "devDependencies"]]) {
    const dir = makeTempRepo(SAMPLE_MANIFEST, { packageJson: withRelease(spec, field) });
    const result = await syncCommand(dir, { deps: PUBLIC_DEPS });
    assert.equal(result.ok, false, spec);
    assert.equal(result.stage, "dependency", spec);
    assert.equal(result.errors.length, 1);
    assert.match(result.errors[0], new RegExp(`^lanes ci, package-smoke render the RightKit check job, which runs rightkit-gate check from the repository's @rightkit/release \\(0\\.6\\.5 is the first version that ships it\\); @rightkit/release is pinned to ${spec.replaceAll(".", "\\.")}, which has no rightkit-gate check \\(pin 0\\.6\\.5 or later\\)$`), spec);
    assert.equal(fs.existsSync(path.join(dir, ".github", "workflows")), false, `${spec}: nothing is written`);
  }
  // planSync (what install uses) refuses the same way
  const plan = await planSync(makeTempRepo(SAMPLE_MANIFEST, { packageJson: withRelease("0.6.4") }), PUBLIC_DEPS);
  assert.deepEqual([plan.ok, plan.stage, plan.changes], [false, "dependency", []]);
});

test("sync accepts an exact @rightkit/release pin of 0.6.5 or later, compared as numbers, without a warning", async () => {
  for (const spec of ["0.6.5", "0.6.10", "0.10.0", "1.0.0"]) {
    const dir = makeTempRepo(SAMPLE_MANIFEST, { packageJson: withRelease(spec) });
    const result = await syncCommand(dir, { deps: PUBLIC_DEPS });
    assert.equal(result.ok, true, spec);
    assert.deepEqual(result.warnings, [], spec);
    assert.ok(fs.existsSync(path.join(dir, ".github", "workflows", "ci.yml")), spec);
  }
});

test("sync warns, and still syncs, for a range pin, a non-registry spec or no @rightkit/release dependency", async () => {
  const cases = [
    [withRelease("^0.6.5"), /@rightkit\/release is "\^0\.6\.5", not an exact version, so it cannot be checked to be 0\.6\.5 or later \(pin an exact version\)$/],
    [withRelease("~0.5.0"), /is "~0\.5\.0", not an exact version/],
    [withRelease("workspace:*"), /is "workspace:\*", not an exact version/],
    [{ name: "widgets" }, /package\.json declares no @rightkit\/release dependency, so the job has no gate to run$/],
    [{ name: "widgets", devDependencies: { "@rightkit/ci": "0.3.0" } }, /declares no @rightkit\/release dependency/],
  ];
  for (const [packageJson, message] of cases) {
    const dir = makeTempRepo(SAMPLE_MANIFEST, { packageJson });
    const result = await syncCommand(dir, { deps: PUBLIC_DEPS });
    assert.equal(result.ok, true, JSON.stringify(packageJson));
    assert.equal(result.warnings.length, 1, JSON.stringify(packageJson));
    assert.match(result.warnings[0], /^lanes ci, package-smoke render the RightKit check job/);
    assert.match(result.warnings[0], message);
    assert.ok(fs.existsSync(path.join(dir, ".github", "workflows", "ci.yml")), "the warning does not stop the sync");
  }
  // an unreadable package.json is a warning too; the dry run reports it without writing
  const dir = makeTempRepo(SAMPLE_MANIFEST);
  fs.writeFileSync(path.join(dir, "package.json"), "{ not json", "utf8");
  const dry = await syncCommand(dir, { deps: PUBLIC_DEPS, dryRun: true });
  assert.equal(dry.ok, true);
  assert.match(dry.warnings[0], /package\.json is not readable here, so the pin cannot be checked$/);
  // a single lane reads singular
  const single = makeTempRepo({ ...SAMPLE_MANIFEST, lanes: ["ci"] }, { packageJson: withRelease("^0.6.5") });
  assert.match((await planSync(single, PUBLIC_DEPS)).warnings[0], /^lane ci renders the RightKit check job/);
});

test("sync asks nothing of @rightkit/release when no lane renders a check job", async () => {
  const dir = makeTempRepo({ ...SAMPLE_MANIFEST, lanes: ["regenerate"], regenerate: { script: "regenerate:files" } }, { packageJson: withRelease("0.2.84") });
  const result = await syncCommand(dir, { deps: PUBLIC_DEPS, dryRun: true });
  assert.equal(result.ok, true, JSON.stringify(result.errors));
  assert.deepEqual(result.warnings, []);
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
