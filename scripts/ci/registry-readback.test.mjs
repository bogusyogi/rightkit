import test from "node:test";
import assert from "node:assert/strict";
import { APPROVED, authenticatePublication, registryUrl, lockEvidence, validatePublication } from "./registry-readback.mjs";

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
