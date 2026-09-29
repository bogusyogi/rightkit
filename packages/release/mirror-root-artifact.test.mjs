import assert from "node:assert/strict";
import { execFileSync } from "./background-process.mjs";
import { mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

const helper = fileURLToPath(new URL("./mirror-root-artifact.mjs", import.meta.url));

test("mirrors a nested package artifact from a linked worktree into both package roots", () => {
  const temp = mkdtempSync(path.join(os.tmpdir(), "right-release-mirror-"));
  const main = path.join(temp, "suite");
  const worktree = path.join(temp, "release-worktree");
  try {
    mkdirSync(path.join(main, "apps", "desktop"), { recursive: true });
    git(temp, "init", "suite");
    git(main, "config", "user.email", "test@example.com");
    git(main, "config", "user.name", "Right Release Test");
    writeFileSync(path.join(main, "tracked"), "fixture\n");
    git(main, "add", "tracked");
    git(main, "commit", "-m", "fixture");
    git(main, "worktree", "add", worktree, "-b", "release-test");

    const packageRoot = path.join(worktree, "apps", "desktop");
    const source = path.join(packageRoot, "target", "App_1.0.0.dmg");
    mkdirSync(path.dirname(source), { recursive: true });
    writeFileSync(source, "signed-dmg-fixture");

    execFileSync(process.execPath, [helper, "--file", source, "--package-root", packageRoot], { encoding: "utf8" });

    assert.equal(readFileSync(path.join(packageRoot, "App_1.0.0.dmg"), "utf8"), "signed-dmg-fixture");
    assert.equal(readFileSync(path.join(main, "apps", "desktop", "App_1.0.0.dmg"), "utf8"), "signed-dmg-fixture");
  } finally {
    rmSync(temp, { recursive: true, force: true });
  }
});

test("copies a main-worktree artifact into its package root", () => {
  const temp = mkdtempSync(path.join(os.tmpdir(), "right-release-mirror-main-"));
  try {
    git(temp, "init", "app");
    const root = path.join(temp, "app");
    const source = path.join(root, "target", "App_2.0.0.dmg");
    mkdirSync(path.dirname(source), { recursive: true });
    writeFileSync(source, "main-worktree-dmg");

    execFileSync(process.execPath, [helper, "--file", source, "--package-root", root], { encoding: "utf8" });

    assert.equal(readFileSync(path.join(root, "App_2.0.0.dmg"), "utf8"), "main-worktree-dmg");
  } finally {
    rmSync(temp, { recursive: true, force: true });
  }
});

function git(cwd, ...args) {
  return execFileSync("git", args, { cwd, encoding: "utf8", stdio: ["ignore", "pipe", "pipe"] });
}
