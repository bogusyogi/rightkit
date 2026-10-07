import assert from "node:assert/strict";
import { cpSync, existsSync, mkdirSync, mkdtempSync, readdirSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "./background-process.mjs";
import test from "node:test";
import { fileURLToPath } from "node:url";

const releaseRoot = path.dirname(fileURLToPath(import.meta.url));
const repositoryRoot = path.resolve(releaseRoot, "..", "..", "..", "..");

test("production ownership CLI fails closed with typed JSON when protected trust is unavailable", () => {
  const command = spawnSync(process.execPath, [path.join(releaseRoot, "ownership-command.mjs"), "--app", repositoryRoot, "--json"], { encoding: "utf8", windowsHide: true });
  assert.equal(command.status, 1);
  const output = JSON.parse(command.stdout);
  assert.equal(output.ok, false);
  assert.equal(output.error.type, "protected-evidence");
  assert.match(output.error.message, /protected ownership trust location was not verified|protected ownership trust registry is missing/);
});

test("published ownership command runs without development sibling packages & emits typed fail-closed JSON", () => {
  const temp = mkdtempSync(path.join(os.tmpdir(), "right-release-ownership-pack-"));
  try {
    const packageRoot = path.join(temp, "package");
    mkdirSync(packageRoot);
    // Mirror package.json's published root globs & cli directory without any
    // development sibling packages or workspace links.
    for (const entry of readdirSync(releaseRoot, { withFileTypes: true })) {
      const source = path.join(releaseRoot, entry.name);
      const destination = path.join(packageRoot, entry.name);
      if (entry.isDirectory() && entry.name === "cli") cpSync(source, destination, { recursive: true });
      else if (entry.isFile() && /\.(?:mjs|json|sh|py)$/.test(entry.name)
        && !entry.name.endsWith(".test.mjs") && entry.name !== "standalone-clone-evidence.json") {
        cpSync(source, destination);
      }
    }
    assert.ok(existsSync(path.join(packageRoot, "ownership-command.mjs")));
    assert.equal(existsSync(path.join(packageRoot, "build-control-clients")), false);
    const packageModules = path.join(packageRoot, "node_modules");
    mkdirSync(packageModules);
    cpSync(realpathSync(path.join(releaseRoot, "node_modules", "smol-toml")), path.join(packageModules, "smol-toml"), { recursive: true });

    const app = path.join(temp, "app");
    mkdirSync(app);
    for (const args of [["init", "-q"], ["config", "user.email", "test@example.invalid"], ["config", "user.name", "test"]]) {
      const git = spawnSync("git", ["-C", app, ...args], { encoding: "utf8", windowsHide: true });
      assert.equal(git.status, 0, git.stderr);
    }
    writeFileSync(path.join(app, "source.txt"), "test\n");
    for (const args of [["add", "."], ["commit", "-qm", "fixture"]]) {
      const git = spawnSync("git", ["-C", app, ...args], { encoding: "utf8", windowsHide: true });
      assert.equal(git.status, 0, git.stderr);
    }

    const command = spawnSync(process.execPath, [path.join(packageRoot, "ownership-command.mjs"), "--app", app, "--json"], { encoding: "utf8", windowsHide: true });
    assert.equal(command.status, 1, command.stderr);
    const output = JSON.parse(command.stdout);
    assert.equal(output.ok, false);
    assert.equal(output.error.type, "protected-evidence");
    assert.equal(output.rows.length, 0);
    assert.equal(output.violations.length, 0);
    assert.match(output.error.message, /protected ownership trust location was not verified|protected ownership trust registry is missing|git remote get-url origin failed/);
  } finally {
    rmSync(temp, { recursive: true, force: true });
  }
});
