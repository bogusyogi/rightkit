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
