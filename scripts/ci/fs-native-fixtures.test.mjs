import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import { fixturePlan, selectPinnedArtifact, validateSource } from "./fs-native-fixtures.mjs";
import { parseYaml } from "./workflow-yaml.mjs";

const repository = "bogusyogi/rightkit";
test("native fixture paths use runner context only in step environments", () => {
  const workflow = parseYaml(fs.readFileSync(new URL("../../.github/workflows/qualify-fs.yml", import.meta.url), "utf8"));
  const job = workflow.jobs.native;
  assert.equal(job.env, undefined);
  for (const mode of ["setup", "qualify", "cleanup"]) {
    const step = job.steps.find(row => row.run === `node scripts/ci/fs-native-fixtures.mjs ${mode}`);
    assert.deepEqual(step.env, {
      FS_NATIVE_BUNDLE: "${{ runner.temp }}/fs-native-bundle",
      FS_NATIVE_OUT: "${{ runner.temp }}/fs-native-results",
    });
  }
  assert.equal(job.steps.find(row => row.run?.endsWith(" cleanup")).if, "${{ always() }}");
});
const source = () => ({ id: 123, repository: { full_name: repository }, head_repository: { full_name: repository }, path: ".github/workflows/publish.yml", head_branch: "main", event: "workflow_dispatch", status: "completed", conclusion: "failure", head_sha: "a".repeat(40), run_attempt: 1 });
const jobs = () => ({ total_count: 5, jobs: ["guard", "plan", "pack", "verify (xcode-27, macos)", "verify (windows-2025, windows)"].map(name => ({ name, conclusion: "success", run_id: 123, head_sha: "a".repeat(40) })) });
test("seal-only failure permits qualification after both successful native jobs", () => {
  assert.equal(validateSource(source(), jobs(), repository, "123").sourceSha, "a".repeat(40));
});
test("reject wrong source identity, incomplete jobs & failed native proof", () => {
  for (const patch of [{ head_branch: "topic" }, { path: ".github/workflows/ci.yml" }, { head_repository: { full_name: "foreign/rightkit" } }, { status: "in_progress" }]) assert.throws(() => validateSource({ ...source(), ...patch }, jobs(), repository, "123"));
  const failed = jobs(); failed.jobs[4].conclusion = "failure";
  assert.throws(() => validateSource(source(), failed, repository, "123"));
  assert.throws(() => validateSource(source(), { ...jobs(), total_count: 6 }, repository, "123"));
});
test("artifact requires exact run, source SHA, digest & unique name", () => {
  const context = validateSource(source(), jobs(), repository, "123");
  const artifact = { id: 1, name: "publish-packed", expired: false, digest: `sha256:${"b".repeat(64)}`, size_in_bytes: 100, workflow_run: { id: 123, head_sha: context.sourceSha } };
  assert.equal(selectPinnedArtifact([artifact], artifact.name, context), artifact);
  for (const patch of [{ digest: null }, { expired: true }, { workflow_run: { id: 124, head_sha: context.sourceSha } }]) assert.throws(() => selectPinnedArtifact([{ ...artifact, ...patch }], artifact.name, context));
  assert.throws(() => selectPinnedArtifact([artifact, artifact], artifact.name, context));
});
test("native plans require supported host & reject diskpart path injection", () => {
  assert.equal(fixturePlan("win32", "scratch").images.length, 2);
  assert.equal(fixturePlan("darwin", "scratch").fat, null);
  assert.throws(() => fixturePlan("linux", "scratch"));
  assert.throws(() => fixturePlan("win32", 'scratch"\nexit'));
});
