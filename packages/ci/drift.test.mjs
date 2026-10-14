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

// Regenerate approval check (drift warns; it is advisory and never changes driftDetected).
// The stub answers the visibility lookup, `git remote get-url origin`, and `gh api`; any other
// command throws, so a test cannot reach the real gh or network.
const REGENERATE_MANIFEST = { ...SAMPLE_MANIFEST, lanes: ["ci", "regenerate"], regenerate: { script: "regenerate" } };
const PERMISSIONS_PATH = "repos/acme/widgets/actions/permissions/workflow";

function workflowSettingsDeps({ approve = true, origin = "git@github.com:acme/widgets.git", gitFails = false, apiError = null, apiStdout = null } = {}) {
  const deps = {
    calls: [],
    execFile: async (cmd, args) => {
      deps.calls.push([cmd, ...args].join(" "));
      if (cmd === "gh" && args[0] === "repo") return { stdout: JSON.stringify({ visibility: "PUBLIC" }) };
      if (cmd === "git" && args[0] === "remote") {
        if (gitFails) throw new Error("fatal: No such remote 'origin'");
        return { stdout: `${origin}\n` };
      }
      if (cmd === "gh" && args[0] === "api") {
        if (apiError) throw apiError;
        return { stdout: apiStdout ?? JSON.stringify({ default_workflow_permissions: "read", can_approve_pull_request_reviews: approve }) };
      }
      throw new Error(`unexpected exec: ${cmd}`);
    },
  };
  return deps;
}

test("drift warns when the regenerate lane's pull-request setting is off, without reporting drift", async () => {
  const dir = makeTempRepo(REGENERATE_MANIFEST);
  await syncCommand(dir, { deps: PUBLIC_DEPS });
  const deps = workflowSettingsDeps({ approve: false });
  const result = await driftCommand(dir, deps);
  assert.equal(result.ok, true);
  assert.equal(result.driftDetected, false);
  assert.equal(result.warnings.length, 1);
  assert.match(result.warnings[0], /Allow GitHub Actions to create and approve pull requests/);
  assert.deepEqual(result.notes, []);
  assert.ok(deps.calls.includes(`gh api ${PERMISSIONS_PATH}`), `expected gh api call; got ${deps.calls.join(" | ")}`);
});

test("drift is silent when the regenerate lane's pull-request setting is on", async () => {
  const dir = makeTempRepo(REGENERATE_MANIFEST);
  await syncCommand(dir, { deps: PUBLIC_DEPS });
  const result = await driftCommand(dir, workflowSettingsDeps({ approve: true }));
  assert.equal(result.driftDetected, false);
  assert.deepEqual(result.warnings, []);
  assert.deepEqual(result.notes, []);
});

test("drift skips the approval check with a note when gh is not installed", async () => {
  const dir = makeTempRepo(REGENERATE_MANIFEST);
  await syncCommand(dir, { deps: PUBLIC_DEPS });
  const missing = Object.assign(new Error("spawn gh ENOENT"), { code: "ENOENT" });
  const result = await driftCommand(dir, workflowSettingsDeps({ apiError: missing }));
  assert.equal(result.driftDetected, false);
  assert.deepEqual(result.warnings, []);
  assert.equal(result.notes.length, 1);
  assert.match(result.notes[0], /skipped the Actions pull-request approval check \(gh is not installed\)/);
});

test("drift notes an unreadable or incomplete permissions response instead of guessing", async () => {
  const dir = makeTempRepo(REGENERATE_MANIFEST);
  await syncCommand(dir, { deps: PUBLIC_DEPS });
  const notJson = await driftCommand(dir, workflowSettingsDeps({ apiStdout: "<html>403</html>" }));
  assert.deepEqual(notJson.warnings, []);
  assert.match(notJson.notes[0], /gh api returned no JSON/);

  const incomplete = await driftCommand(dir, workflowSettingsDeps({ apiStdout: JSON.stringify({ default_workflow_permissions: "read" }) }));
  assert.deepEqual(incomplete.warnings, []);
  assert.match(incomplete.notes[0], /no can_approve_pull_request_reviews/);

  const forbidden = await driftCommand(dir, workflowSettingsDeps({ apiError: new Error("HTTP 403: Resource not accessible") }));
  assert.deepEqual(forbidden.warnings, []);
  assert.match(forbidden.notes[0], /gh api failed: HTTP 403/);
});

test("drift skips the approval check for a non-GitHub origin and never calls gh api", async () => {
  const dir = makeTempRepo(REGENERATE_MANIFEST);
  await syncCommand(dir, { deps: PUBLIC_DEPS });
  const deps = workflowSettingsDeps({ origin: "https://gitlab.com/acme/widgets.git" });
  const result = await driftCommand(dir, deps);
  assert.deepEqual(result.warnings, []);
  assert.match(result.notes[0], /origin is not a github\.com repository/);
  assert.equal(deps.calls.some((call) => call.startsWith("gh api")), false);
});

test("drift notes a missing origin remote and does not guess the repository", async () => {
  const dir = makeTempRepo(REGENERATE_MANIFEST);
  await syncCommand(dir, { deps: PUBLIC_DEPS });
  const deps = workflowSettingsDeps({ gitFails: true });
  const result = await driftCommand(dir, deps);
  assert.deepEqual(result.warnings, []);
  assert.match(result.notes[0], /no origin remote/);
  assert.equal(deps.calls.some((call) => call.startsWith("gh api")), false);
});

test("drift never asks Actions settings for a manifest without the regenerate lane", async () => {
  const dir = makeTempRepo(SAMPLE_MANIFEST);
  await syncCommand(dir, { deps: PUBLIC_DEPS });
  const deps = workflowSettingsDeps({ approve: false });
  const result = await driftCommand(dir, deps);
  assert.deepEqual(result.warnings, []);
  assert.deepEqual(result.notes, []);
  assert.equal(deps.calls.some((call) => call.startsWith("gh api")), false);
});
