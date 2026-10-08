import assert from "node:assert/strict";
import { execFileSync } from "./background-process.mjs";
import { existsSync, mkdtempSync, mkdirSync, readFileSync, realpathSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

const helper = fileURLToPath(new URL("./mirror-root-artifact.mjs", import.meta.url));

test("mirrors a nested package artifact into managed storage", () => {
  const temp = realpathSync(mkdtempSync(path.join(os.tmpdir(), "right-release-mirror-")));
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
    const managed = path.join(temp, "managed", "apps", "desktop");
    mkdirSync(path.dirname(source), { recursive: true });
    writeFileSync(source, "signed-dmg-fixture");

    execFileSync(process.execPath, [helper, "--file", source, "--package-root", packageRoot], { encoding: "utf8", env: { ...process.env, RIGHT_RELEASE_ARTIFACT_DIR: managed } });

    assert.equal(readFileSync(path.join(managed, "App_1.0.0.dmg"), "utf8"), "signed-dmg-fixture");
    assert.equal(existsSync(path.join(packageRoot, "App_1.0.0.dmg")), false);
    assert.equal(existsSync(path.join(main, "apps", "desktop", "App_1.0.0.dmg")), false);
  } finally {
    rmSync(temp, { recursive: true, force: true });
  }
});

test("copies a main-worktree artifact into managed storage", () => {
  const temp = realpathSync(mkdtempSync(path.join(os.tmpdir(), "right-release-mirror-main-")));
  try {
    git(temp, "init", "app");
    const root = path.join(temp, "app");
    const source = path.join(root, "target", "App_2.0.0.dmg");
    const managed = path.join(temp, "managed", "app");
    mkdirSync(path.dirname(source), { recursive: true });
    writeFileSync(source, "main-worktree-dmg");

    execFileSync(process.execPath, [helper, "--file", source, "--package-root", root], { encoding: "utf8", env: { ...process.env, RIGHT_RELEASE_ARTIFACT_DIR: managed } });

    assert.equal(readFileSync(path.join(managed, "App_2.0.0.dmg"), "utf8"), "main-worktree-dmg");
    assert.equal(existsSync(path.join(root, "App_2.0.0.dmg")), false);
  } finally {
    rmSync(temp, { recursive: true, force: true });
  }
});

test("routes build-time mirror into managed artifact directory", () => {
  const temp = mkdtempSync(path.join("/private/tmp", "right-release-mirror-managed-"));
  try {
    git(temp, "init", "app");
    const root = path.join(temp, "app");
    const source = path.join(root, "target", "App_3.0.0.dmg");
    const managed = path.join(temp, "managed", "artifacts", "app", "3.0.0");
    mkdirSync(path.dirname(source), { recursive: true });
    writeFileSync(source, "managed-dmg");
    execFileSync(process.execPath, [helper, "--file", source, "--package-root", root], { encoding: "utf8", env: { ...process.env, RIGHT_RELEASE_ARTIFACT_DIR: managed } });
    assert.equal(readFileSync(path.join(managed, "App_3.0.0.dmg"), "utf8"), "managed-dmg");
    assert.equal(existsSync(path.join(root, "App_3.0.0.dmg")), false);
  } finally {
    rmSync(temp, { recursive: true, force: true });
  }
});

test("fails closed without managed output or when output is inside repository", () => {
  const temp = mkdtempSync(path.join("/private/tmp", "right-release-mirror-required-"));
  try {
    git(temp, "init", "app");
    const root = path.join(temp, "app");
    const source = path.join(root, "target", "App_4.0.0.dmg");
    mkdirSync(path.dirname(source), { recursive: true });
    writeFileSync(source, "guarded");
    assert.throws(() => execFileSync(process.execPath, [helper, "--file", source, "--package-root", root], { encoding: "utf8", env: { ...process.env, RIGHT_RELEASE_ARTIFACT_DIR: "" } }), /RIGHT_RELEASE_ARTIFACT_DIR is required/);
    assert.throws(() => execFileSync(process.execPath, [helper, "--file", source, "--package-root", root], { encoding: "utf8", env: { ...process.env, RIGHT_RELEASE_ARTIFACT_DIR: root } }), /outside repository/);
  } finally { rmSync(temp, { recursive: true, force: true }); }
});

test("rejects unsafe names and symlinked managed ancestors", () => {
  const temp = mkdtempSync(path.join("/private/tmp", "right-release-mirror-guards-"));
  try {
    git(temp, "init", "app");
    const root = path.join(temp, "app");
    const source = path.join(root, "target", "App_5.0.0.dmg");
    const managed = path.join(temp, "managed");
    const outside = path.join(temp, "outside");
    mkdirSync(path.dirname(source), { recursive: true });
    mkdirSync(outside, { recursive: true });
    writeFileSync(source, "guarded");
    assert.throws(() => execFileSync(process.execPath, [helper, "--file", source, "--package-root", root, "--name", "../escape.dmg"], { encoding: "utf8", env: { ...process.env, RIGHT_RELEASE_ARTIFACT_DIR: managed } }), /unsafe artifact name/);
    symlinkSync(outside, managed);
    assert.throws(() => execFileSync(process.execPath, [helper, "--file", source, "--package-root", root], { encoding: "utf8", env: { ...process.env, RIGHT_RELEASE_ARTIFACT_DIR: managed } }), /symlink/);
  } finally { rmSync(temp, { recursive: true, force: true }); }
});

function git(cwd, ...args) {
  return execFileSync("git", args, { cwd, encoding: "utf8", stdio: ["ignore", "pipe", "pipe"] });
}
