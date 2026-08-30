import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdtempSync, mkdirSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";

import { assertPrimaryReleaseCheckout, dirtyBuildInputs } from "./release-invocation.mjs";

function git(cwd, ...args) {
  return execFileSync("git", args, { cwd, encoding: "utf8", stdio: ["ignore", "pipe", "pipe"] }).trim();
}

function makeRepo() {
  const root = mkdtempSync(path.join(os.tmpdir(), "right-release-invocation-"));
  git(root, "init", "--initial-branch", "main");
  git(root, "config", "user.email", "release-test@example.com");
  git(root, "config", "user.name", "Release Test");
  git(root, "remote", "add", "origin", "https://github.com/Orthic-Labs/legion.git");
  mkdirSync(path.join(root, "src"));
  mkdirSync(path.join(root, "tests"));
  mkdirSync(path.join(root, "docs"));
  writeFileSync(path.join(root, "package.json"), "{}\n");
  writeFileSync(path.join(root, "src", "app.js"), "export const app = true;\n");
  writeFileSync(path.join(root, "tests", "app.test.js"), "// test\n");
  writeFileSync(path.join(root, "docs", "notes.md"), "notes\n");
  git(root, "add", ".");
  git(root, "commit", "-m", "initial");
  return root;
}

const buildInputs = {
  include: ["package.json", "src/**"],
  exclude: ["**/*.test.*", "**/tests/**"],
  json: {
    "package.json": [["dependencies"], ["scripts", "build"]],
  },
};

test("rejects release invocation from a linked Git worktree", () => {
  const primary = makeRepo();
  const linked = path.join(path.dirname(primary), `${path.basename(primary)}-linked`);
  git(primary, "worktree", "add", "--detach", linked);

  assert.throws(
    () => assertPrimaryReleaseCheckout(linked),
    /must be invoked from the primary Git worktree/i,
  );
});

test("allows release invocation from the primary Git checkout", () => {
  const primary = makeRepo();

  assert.doesNotThrow(() => assertPrimaryReleaseCheckout(primary));
});

test("rejects a detached HEAD even in the primary Git checkout", () => {
  const primary = makeRepo();
  git(primary, "checkout", "--detach");

  assert.throws(
    () => assertPrimaryReleaseCheckout(primary),
    /detached HEAD is forbidden/i,
  );
});

function tagEnv(root, tag) {
  return {
    ...process.env,
    GITHUB_ACTIONS: "true",
    GITHUB_EVENT_NAME: "push",
    GITHUB_REF_TYPE: "tag",
    GITHUB_REF: `refs/tags/${tag}`,
    GITHUB_SHA: git(root, "rev-parse", "HEAD"),
    GITHUB_REPOSITORY: "Orthic-Labs/legion",
  };
}

test("allows detached GitHub Actions lightweight and annotated v-tag checkouts only", () => {
  for (const annotated of [false, true]) {
    const root = makeRepo();
    const tag = annotated ? "v1.0.1" : "v1.0.0";
    git(root, "tag", ...(annotated ? ["-a", tag, "-m", tag] : [tag]));
    git(root, "checkout", "--detach", tag);
    assert.doesNotThrow(() => assertPrimaryReleaseCheckout(root, tagEnv(root, tag)));
  }
});

for (const [label, mutate] of [
  ["SHA mismatch", (env) => ({ ...env, GITHUB_SHA: "b".repeat(40) })],
  ["branch ref", (env) => ({ ...env, GITHUB_REF_TYPE: "branch", GITHUB_REF: "refs/heads/main" })],
  ["pull request", (env) => ({ ...env, GITHUB_EVENT_NAME: "pull_request" })],
  ["manual event", (env) => ({ ...env, GITHUB_EVENT_NAME: "workflow_dispatch" })],
  ["non-v tag", (env) => ({ ...env, GITHUB_REF: "refs/tags/release-1.0.0" })],
  ["repository mismatch", (env) => ({ ...env, GITHUB_REPOSITORY: "Orthic-Labs/other" })],
]) {
  test(`rejects detached GitHub Actions checkout with ${label}`, () => {
    const root = makeRepo();
    git(root, "tag", "v1.0.0");
    git(root, "checkout", "--detach", "v1.0.0");
    assert.throws(() => assertPrimaryReleaseCheckout(root, mutate(tagEnv(root, "v1.0.0"))), /detached HEAD is forbidden/i);
  });
}

test("rejects a tag that does not resolve to detached HEAD", () => {
  const root = makeRepo();
  git(root, "tag", "v1.0.0");
  writeFileSync(path.join(root, "src", "app.js"), "export const app = false;\n");
  git(root, "commit", "-am", "next");
  git(root, "checkout", "--detach", "HEAD");
  assert.throws(() => assertPrimaryReleaseCheckout(root, tagEnv(root, "v1.0.0")), /detached HEAD is forbidden/i);
});

test("reports a dirty tracked file declared as a build input", () => {
  const primary = makeRepo();
  writeFileSync(path.join(primary, "src", "app.js"), "export const app = false;\n");

  assert.deepEqual(dirtyBuildInputs(primary, buildInputs), ["src/app.js"]);
});

test("reports an untracked file declared as a build input", () => {
  const primary = makeRepo();
  writeFileSync(path.join(primary, "src", "new-runtime.js"), "export {};\n");

  assert.deepEqual(dirtyBuildInputs(primary, buildInputs), ["src/new-runtime.js"]);
});

test("ignores dirty files outside the packaged app input set", () => {
  const primary = makeRepo();
  writeFileSync(path.join(primary, "docs", "notes.md"), "changed notes\n");
  writeFileSync(path.join(primary, "tests", "app.test.js"), "// changed test\n");
  writeFileSync(path.join(primary, "scratch.txt"), "evaluation output\n");

  assert.deepEqual(dirtyBuildInputs(primary, buildInputs), []);
});

test("required release metadata cannot be hidden by build-input exclusions", () => {
  const primary = makeRepo();
  writeFileSync(path.join(primary, "package.json"), "{\"version\":\"2.0.0\"}\n");

  assert.deepEqual(dirtyBuildInputs(primary, {
    ...buildInputs,
    exclude: [...buildInputs.exclude, "package.json"],
    required: ["package.json"],
  }), ["package.json"]);
  assert.deepEqual(dirtyBuildInputs(primary, {
    include: ["package.json"],
    required: ["package.json"],
  }), ["package.json"]);
});

test("checks only build-relevant fields in mixed-purpose JSON manifests", () => {
  const primary = makeRepo();
  writeFileSync(path.join(primary, "package.json"), `${JSON.stringify({ scripts: { bakeoff: "node eval.mjs" } })}\n`);
  assert.deepEqual(dirtyBuildInputs(primary, buildInputs), []);

  writeFileSync(path.join(primary, "package.json"), `${JSON.stringify({ dependencies: { tauri: "2.0.0" } })}\n`);
  assert.deepEqual(dirtyBuildInputs(primary, buildInputs), ["package.json"]);
});

test("does not report a locally dirty file whose bytes already equal the release commit", () => {
  const primary = makeRepo();
  const localHead = git(primary, "rev-parse", "HEAD");
  writeFileSync(path.join(primary, "src", "app.js"), "export const app = false;\n");
  git(primary, "add", "src/app.js");
  git(primary, "commit", "-m", "release change");
  const releaseCommit = git(primary, "rev-parse", "HEAD");
  git(primary, "checkout", localHead);
  writeFileSync(path.join(primary, "src", "app.js"), "export const app = false;\n");

  assert.deepEqual(dirtyBuildInputs(primary, buildInputs, releaseCommit), []);
});
