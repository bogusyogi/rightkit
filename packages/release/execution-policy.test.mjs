import test from "node:test";
import assert from "node:assert/strict";
import { mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { GITHUB_ACTIONS_ONLY_ERROR, assertReleaseExecutionAllowed, releaseExecutionPolicy } from "./execution-policy.mjs";

test("public release builds are GitHub Actions only", async () => {
  const root = await mkdtemp(path.join(tmpdir(), "right-release-policy-"));
  try {
    await mkdir(path.join(root, ".git"));
    await writeFile(path.join(root, ".git", "config"), '[remote "origin"]\n\turl = git@github.com:Orthic-Labs/example.git\n');
    await writeFile(path.join(root, ".rightgit.json"), '{"compile":"github-actions-only"}\n');
    assert.equal(releaseExecutionPolicy(root).compile, "github-actions-only");
    assert.throws(() => assertReleaseExecutionAllowed({ cwd: root, environment: {}, command: "right-release build" }), (error) => error.code === GITHUB_ACTIONS_ONLY_ERROR);
    assert.doesNotThrow(() => assertReleaseExecutionAllowed({ cwd: root, environment: { GITHUB_ACTIONS: "true" }, command: "right-release build" }));
  } finally { await rm(root, { recursive: true, force: true }); }
});
