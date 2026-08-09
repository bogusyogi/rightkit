import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { syncCommand } from "./sync.mjs";
import { driftCommand, findManagedRepos } from "./drift.mjs";
import { makeTempRepo, PUBLIC_DEPS, PRIVATE_DEPS, SAMPLE_MANIFEST } from "./test-support.mjs";

test("drift reports no drift right after a clean sync", async () => {
  const dir = makeTempRepo(SAMPLE_MANIFEST);
  await syncCommand(dir, { deps: PUBLIC_DEPS });
  const result = await driftCommand(dir, PUBLIC_DEPS);
  assert.equal(result.ok, true);
  assert.equal(result.driftDetected, false);
  assert.ok(result.findings.every((f) => f.status === "match"));
});

test("drift detects a hand-mutated managed file", async () => {
  const dir = makeTempRepo(SAMPLE_MANIFEST);
  await syncCommand(dir, { deps: PUBLIC_DEPS });
  const ciPath = path.join(dir, ".github", "workflows", "ci.yml");
  fs.appendFileSync(ciPath, "\n# hand edit\n");

  const result = await driftCommand(dir, PUBLIC_DEPS);
  assert.equal(result.ok, true);
  assert.equal(result.driftDetected, true);
  const ciFinding = result.findings.find((f) => f.filename === "ci.yml");
  assert.equal(ciFinding.status, "mismatch");
});

test("drift flags driftDetected when a lane's file is missing", async () => {
  const dir = makeTempRepo(SAMPLE_MANIFEST);
  await syncCommand(dir, { deps: PUBLIC_DEPS });
  fs.rmSync(path.join(dir, ".github", "workflows", "ci.yml"));

  const result = await driftCommand(dir, PUBLIC_DEPS);
  assert.equal(result.driftDetected, true);
  assert.ok(result.findings.some((f) => f.filename === "ci.yml" && f.status === "missing"));
});

test("drift flags a public->private visibility flip even with no file drift", async () => {
  const dir = makeTempRepo(SAMPLE_MANIFEST);
  await syncCommand(dir, { deps: PUBLIC_DEPS });
  const result = await driftCommand(dir, PRIVATE_DEPS);
  assert.equal(result.driftDetected, true);
  assert.equal(result.visibility.allowed, false);
});

test("findManagedRepos finds every directory carrying a .rightgit.json under a root", () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "right-git-root-"));
  const repoA = makeTempRepo(SAMPLE_MANIFEST);
  const repoB = makeTempRepo(SAMPLE_MANIFEST);
  fs.cpSync(repoA, path.join(root, "repo-a"), { recursive: true });
  fs.cpSync(repoB, path.join(root, "repo-b"), { recursive: true });
  fs.mkdirSync(path.join(root, "not-managed"));

  const found = findManagedRepos(root).map((p) => path.basename(p)).sort();
  assert.deepEqual(found, ["repo-a", "repo-b"]);
});

// F7 regression: a repo whose .rightgit.json was deleted (or never
// committed) but whose .github/workflows/ still carries MANAGED_MARKER
// files is an "orphan" — right-git put those workflows there, but the
// manifest-only scan in the old findManagedRepos could never see it, so
// `drift --all` silently skipped exactly the repos most likely to be
// drifted (missing manifest is itself drift).
test("findManagedRepos finds an orphan repo: managed workflow files present, .rightgit.json missing", () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "right-git-orphan-root-"));
  const dir = makeTempRepo(SAMPLE_MANIFEST);
  fs.cpSync(dir, path.join(root, "orphan-repo"), { recursive: true });

  const orphanDir = path.join(root, "orphan-repo");
  // A synced install would leave workflow files; simulate that state
  // directly, then remove the manifest to reproduce "manifest lost, workflow
  // files remain".
  fs.mkdirSync(path.join(orphanDir, ".github", "workflows"), { recursive: true });
  fs.writeFileSync(
    path.join(orphanDir, ".github", "workflows", "ci.yml"),
    "# Managed by right-git — do not hand-edit. Run `right-git sync` to update.\nname: ci\n",
    "utf8",
  );
  fs.rmSync(path.join(orphanDir, ".rightgit.json"));

  const found = findManagedRepos(root).map((p) => path.basename(p)).sort();
  assert.deepEqual(found, ["orphan-repo"]);
});

test("driftCommand reports an orphan repo as drift instead of a hard failure", async () => {
  const dir = makeTempRepo(SAMPLE_MANIFEST);
  await syncCommand(dir, { deps: PUBLIC_DEPS });
  fs.rmSync(path.join(dir, ".rightgit.json"));

  const result = await driftCommand(dir, PUBLIC_DEPS);
  assert.equal(result.ok, true);
  assert.equal(result.driftDetected, true);
  assert.ok(result.findings.some((f) => f.status === "orphan" && f.filename === "ci.yml"));
});
