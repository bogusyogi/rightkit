import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync, mkdtempSync, writeFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { createHash } from "node:crypto";
import { APPROVED, RECIPES, recipeForRun, verifyTransfer, authenticatePublication, registryUrl, lockEvidence, validatePublication } from "./registry-readback.mjs";

test("workflow runner scratch context stays below job steps boundary", () => {
  const source = readFileSync(new URL("../../.github/workflows/registry-readback.yml", import.meta.url), "utf8");
  const jobs = source.slice(source.indexOf("\njobs:")).split(/^  [A-Za-z][\w-]*:[ \t\r]*$/m).slice(1);
  assert.equal(jobs.length, 2);
  for (const job of jobs) {
    const steps = job.indexOf("    steps:");
    assert.ok(steps >= 0, "job needs steps");
    assert.doesNotMatch(job.slice(0, steps), /\$\{\{\s*runner\./, "runner context is unavailable at job env scope");
  }
});

function runFixture() {
  return { id: APPROVED.runId, repository: { full_name: APPROVED.repository }, head_repository: { full_name: APPROVED.repository }, path: ".github/workflows/publish.yml", event: "workflow_dispatch", head_branch: "main", head_sha: APPROVED.commit, status: "completed", conclusion: "success", run_attempt: 1 };
}
const jobsFixture = () => ({ total_count: 1, jobs: [{ name: "upload", status: "completed", conclusion: "success", run_attempt: 1, steps: [{ name: "Upload to npm", conclusion: "success" }] }] });
test("accept pinned successful main publication; reject changed source, event, workflow, run, attempt & upload", () => {
  authenticatePublication(runFixture(), jobsFixture(), APPROVED.runId, APPROVED.repository);
  for (const change of [{ head_sha: "0".repeat(40) }, { event: "pull_request" }, { path: ".github/workflows/other.yml" }, { head_branch: "other" }, { run_attempt: 2 }, { conclusion: "failure" }, { head_repository: { full_name: "other/rightkit" } }]) {
    assert.throws(() => authenticatePublication({ ...runFixture(), ...change }, jobsFixture(), APPROVED.runId, APPROVED.repository));
  }
  for (const change of [{ conclusion: "skipped" }, { steps: [] }, { run_attempt: 2 }]) {
    const jobs = jobsFixture(); Object.assign(jobs.jobs[0], change);
    assert.throws(() => authenticatePublication(runFixture(), jobs, APPROVED.runId, APPROVED.repository));
  }
  assert.throws(() => authenticatePublication(runFixture(), jobsFixture(), "123", APPROVED.repository));
});
test("receipt bytes are pinned before trusting any publication fields", () => {
  assert.throws(() => validatePublication(Buffer.from('{}'), {}), /receipt hash mismatch/);
  assert.throws(() => validatePublication(Buffer.from('{}'), {}, RECIPES['license-0.3.0']), /receipt hash mismatch/);
  assert.throws(() => validatePublication(Buffer.from('{}'), {}, { ...APPROVED }), /unreviewed/);
});

test("only reviewed recipes select successful authenticated publication", () => {
  assert.deepEqual(Object.keys(RECIPES).sort(), ['license-0.3.0','react-shell-0.7.2']);
  for (const [id, approved] of Object.entries(RECIPES)) {
    assert.equal(recipeForRun(approved.runId).id, id);
    const run = {...runFixture(),id:approved.runId,head_sha:approved.commit};
    authenticatePublication(run,jobsFixture(),approved.runId,approved.repository);
    assert.throws(()=>authenticatePublication({...run,head_sha:APPROVED.commit === approved.commit ? '0'.repeat(40) : APPROVED.commit},jobsFixture(),approved.runId,approved.repository));
  }
  for (const input of ['39411004654','license-0.3.0','',undefined]) assert.throws(()=>recipeForRun(input));
});

test("bundle transfer requires exact files & rejects changed bytes", () => {
  const dir=mkdtempSync(path.join(tmpdir(),'rk-transfer-'));
  try {
    const names=['recipe.json','publication-run.json','publication-jobs.json','publication-receipt.json','manifest.json','registry-readback.mjs','consume.mjs'];
    const digests={};
    for(const name of names) { writeFileSync(path.join(dir,name),name); digests[name]=createHash('sha256').update(name).digest('hex'); }
    const seal=()=>writeFileSync(path.join(dir,'transfer-sha256.json'),JSON.stringify(digests));
    seal(); assert.deepEqual(verifyTransfer(dir),digests);
    writeFileSync(path.join(dir,'recipe.json'),'changed'); assert.throws(()=>verifyTransfer(dir),/transfer hash mismatch/);
    digests['../escape']='0'.repeat(64); seal(); assert.throws(()=>verifyTransfer(dir),/file set mismatch/);
  } finally { rmSync(dir,{recursive:true,force:true}); }
});
test("reject alternate registry, credentials, Git, path & workspace sources", () => {
  for (const url of ["https://registry.npmjs.org.evil.test/x", "http://registry.npmjs.org/x", "https://user@registry.npmjs.org/x", "file:../x", "git+https://github.com/a/b", "workspace:*"]) assert.throws(() => registryUrl(url));
});
test("lock must bind exact version, registry URL & sealed integrity; every dependency must use registry", () => {
  const integrity = `sha512-${Buffer.alloc(64).toString("base64")}`;
  const unit = { name: APPROVED.name, version: APPROVED.version, integrity };
  const metadata = { dist: { tarball: "https://registry.npmjs.org/@rightkit/react-shell/-/react-shell-0.7.2.tgz", integrity } };
  const fixture = () => ({ lockfileVersion: 3, packages: { "": { dependencies: { [unit.name]: unit.version } }, [`node_modules/${unit.name}`]: { version: unit.version, resolved: metadata.dist.tarball, integrity } } });
  assert.equal(lockEvidence(fixture(), unit, metadata).version, unit.version);
  for (const change of [{ version: "0.7.1" }, { resolved: "file:../local" }, { integrity: "sha512-wrong" }, { link: true }]) {
    const lock = fixture(); Object.assign(lock.packages[`node_modules/${unit.name}`], change);
    assert.throws(() => lockEvidence(lock, unit, metadata));
  }
  const lock = fixture(); lock.packages["node_modules/extra"] = { version: "1.0.0", resolved: "https://other.test/x", integrity };
  assert.throws(() => lockEvidence(lock, unit, metadata));
  const ranged = fixture(); ranged.packages[""].dependencies[unit.name] = `^${unit.version}`;
  assert.throws(() => lockEvidence(ranged, unit, metadata));
});
