import assert from "node:assert/strict";
import { execFileSync } from "./background-process.mjs";
import { mkdtempSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { assertPrimaryReleaseCheckout, resolvePrimaryWorktree } from "./release-invocation.mjs";

test("resolves a submodule primary worktree from core.worktree", () => {
  assert.equal(resolvePrimaryWorktree({
    repoRoot: path.resolve("/suite/membrane"),
    worktrees: "worktree /suite/.git/modules/membrane\nHEAD abc\n",
    gitDir: "/suite/.git/modules/membrane",
    coreWorktree: "../../../membrane",
  }), path.resolve("/suite/membrane"));
});

test("preserves an ordinary primary worktree", () => {
  assert.equal(resolvePrimaryWorktree({
    repoRoot: "/suite/app",
    worktrees: "worktree /suite/app\nHEAD abc\n",
    gitDir: "/suite/app/.git",
    coreWorktree: "",
  }), "/suite/app");
});

// Under the tag-last release chain, no stage checks out a tag: every stage
// checks out one admitted commit SHA off the default branch, so HEAD is always
// detached at the exact commit the dispatch was admitted against. These
// exercise `isVerifiedGithubActionsAdmittedDispatch` (only reachable through
// the exported `assertPrimaryReleaseCheckout`), built entirely from throwaway
// local Git repos — no network.
function git(cwd, ...args) {
  return execFileSync("git", args, { cwd, encoding: "utf8", stdio: ["ignore", "pipe", "pipe"] }).trim();
}

// A bare "origin" plus a local clone-equivalent, wired the same way a checkout
// action wires them: a real `origin` remote with a populated remote-tracking
// ref for the dispatched branch, exactly what the ancestor proof needs.
function admittedDispatchRepo() {
  const bareDir = mkdtempSync(path.join(os.tmpdir(), "right-release-admit-bare-"));
  git(bareDir, "init", "--bare", "--initial-branch", "main");

  const repoRoot = mkdtempSync(path.join(os.tmpdir(), "right-release-admit-repo-"));
  git(repoRoot, "init", "--initial-branch", "main");
  git(repoRoot, "config", "user.email", "release-invocation-test@example.com");
  git(repoRoot, "config", "user.name", "Release Invocation Test");
  writeFileSync(path.join(repoRoot, "file.txt"), "one\n");
  git(repoRoot, "add", ".");
  git(repoRoot, "commit", "-m", "commit one");
  writeFileSync(path.join(repoRoot, "file.txt"), "two\n");
  git(repoRoot, "add", ".");
  git(repoRoot, "commit", "-m", "commit two");
  const admittedSha = git(repoRoot, "rev-parse", "HEAD");

  git(repoRoot, "remote", "add", "origin", bareDir);
  git(repoRoot, "push", "origin", "main");
  // Belt-and-suspenders: some Git versions only update the remote-tracking ref
  // implicitly on push. Make it explicit so the test never depends on that.
  git(repoRoot, "fetch", "origin", "main:refs/remotes/origin/main");
  // The exemption's own repository check only inspects GITHUB_REPOSITORY against
  // the origin URL's github.com path — it never touches the network again after
  // this point, so the bare path can be swapped for a syntactically valid GitHub
  // URL without breaking anything the test still needs from `origin`.
  git(repoRoot, "remote", "set-url", "origin", "https://github.com/acme/widget.git");

  git(repoRoot, "checkout", "--detach", admittedSha);

  return { repoRoot, admittedSha };
}

function admittedDispatchEnv({ repoRoot, admittedSha, ...overrides }) {
  return {
    GITHUB_ACTIONS: "true",
    GITHUB_EVENT_NAME: "workflow_dispatch",
    GITHUB_REF_TYPE: "branch",
    GITHUB_REF: "refs/heads/main",
    GITHUB_SHA: admittedSha,
    GITHUB_REPOSITORY: "acme/widget",
    RIGHT_GIT_SOURCE_REVISION: admittedSha,
    ...overrides,
  };
}

test("accepts a well-formed admitted dispatch checkout with a detached HEAD", () => {
  const { repoRoot, admittedSha } = admittedDispatchRepo();
  assert.doesNotThrow(() => assertPrimaryReleaseCheckout(repoRoot, admittedDispatchEnv({ repoRoot, admittedSha })));
});

test("rejects an admitted dispatch checkout with the wrong event name", () => {
  const { repoRoot, admittedSha } = admittedDispatchRepo();
  assert.throws(
    () => assertPrimaryReleaseCheckout(repoRoot, admittedDispatchEnv({ repoRoot, admittedSha, GITHUB_EVENT_NAME: "push" })),
    /detached HEAD is forbidden/,
  );
});

test("rejects an admitted dispatch checkout on a tag-type ref", () => {
  const { repoRoot, admittedSha } = admittedDispatchRepo();
  assert.throws(
    () => assertPrimaryReleaseCheckout(repoRoot, admittedDispatchEnv({
      repoRoot,
      admittedSha,
      GITHUB_REF_TYPE: "tag",
      GITHUB_REF: "refs/tags/v1.0.0",
    })),
    /detached HEAD is forbidden/,
  );
});

test("rejects an admitted dispatch checkout whose HEAD does not equal the admitted source revision", () => {
  const { repoRoot, admittedSha } = admittedDispatchRepo();
  const firstCommitSha = git(repoRoot, "rev-parse", "HEAD~1");
  assert.throws(
    () => assertPrimaryReleaseCheckout(repoRoot, admittedDispatchEnv({ repoRoot, admittedSha, RIGHT_GIT_SOURCE_REVISION: firstCommitSha })),
    /detached HEAD is forbidden/,
  );
});

test("rejects an admitted dispatch checkout against a mismatched repository", () => {
  const { repoRoot, admittedSha } = admittedDispatchRepo();
  assert.throws(
    () => assertPrimaryReleaseCheckout(repoRoot, admittedDispatchEnv({ repoRoot, admittedSha, GITHUB_REPOSITORY: "someone-else/other" })),
    /detached HEAD is forbidden/,
  );
});

test("rejects an admitted dispatch checkout whose HEAD is not an ancestor of the dispatched remote branch", () => {
  const { repoRoot } = admittedDispatchRepo();
  // A commit made after the push, never reachable from origin/main.
  writeFileSync(path.join(repoRoot, "file.txt"), "orphaned\n");
  git(repoRoot, "add", ".");
  git(repoRoot, "commit", "-m", "unpushed orphaned commit");
  const orphanedSha = git(repoRoot, "rev-parse", "HEAD");
  git(repoRoot, "checkout", "--detach", orphanedSha);
  assert.throws(
    () => assertPrimaryReleaseCheckout(repoRoot, admittedDispatchEnv({ repoRoot, admittedSha: orphanedSha, RIGHT_GIT_SOURCE_REVISION: orphanedSha })),
    /detached HEAD is forbidden/,
  );
});

test("rejects an admitted dispatch checkout when the dispatched branch's remote-tracking ref is absent", () => {
  const { repoRoot, admittedSha } = admittedDispatchRepo();
  git(repoRoot, "update-ref", "-d", "refs/remotes/origin/main");
  assert.throws(
    () => assertPrimaryReleaseCheckout(repoRoot, admittedDispatchEnv({ repoRoot, admittedSha })),
    /detached HEAD is forbidden/,
  );
});
