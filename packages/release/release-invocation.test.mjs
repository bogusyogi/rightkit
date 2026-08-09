import assert from "node:assert/strict";
import path from "node:path";
import test from "node:test";
import { resolvePrimaryWorktree } from "./release-invocation.mjs";

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
