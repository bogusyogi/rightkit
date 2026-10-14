// CLI-level regression coverage for right-git.mjs. cli/right-git.mjs is a
// true entrypoint (it dispatches on process.argv at module-load time), not a
// library module, so its argument parsing can only be exercised honestly by
// spawning it as a real subprocess with real argv — the same way the QA
// report reproduced F7.

import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "node:child_process";
import { SAMPLE_MANIFEST } from "./test-support.mjs";

const packageRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)));
const cliPath = path.join(packageRoot, "cli", "right-git.mjs");

function runCli(args, cwd) {
  return spawnSync(process.execPath, [cliPath, ...args], { cwd, encoding: "utf8" });
}

// F7 regression: `right-git drift --all <root>` is the documented syntax
// (see printHelp: "drift --all [workspace-root]"). The old parseFlags
// treated `--all` like any other "--flag value" pair, so it consumed the
// root path as --all's *value* instead of leaving it positional; runDrift
// then fell back to `path.resolve(positional[0] || ".")`, silently scanning
// the current working directory instead of the supplied root. Only the
// undocumented `--all=true <root>` form worked. Prove the fix by running the
// CLI from a cwd that has no managed repos, with the real fixture root
// passed as a plain trailing argument, and asserting the fixture repo is the
// one actually discovered.
test("F7: `right-git drift --all <root>` (space-separated) scopes the scan to the given root, not process.cwd()", () => {
  const fixtureRoot = fs.mkdtempSync(path.join(os.tmpdir(), "right-git-cli-root-"));
  const repoDir = path.join(fixtureRoot, "managed-repo");
  fs.mkdirSync(repoDir, { recursive: true });
  fs.writeFileSync(path.join(repoDir, ".rightgit.json"), `${JSON.stringify(SAMPLE_MANIFEST, null, 2)}\n`, "utf8");
  fs.writeFileSync(path.join(repoDir, "package.json"), JSON.stringify({ name: "widgets" }), "utf8");

  // Deliberately not fixtureRoot and carries no managed repo of its own —
  // if the bug is present, the CLI scans this directory instead and the
  // fixture repo is never mentioned in the output.
  const emptyCwd = fs.mkdtempSync(path.join(os.tmpdir(), "right-git-cli-cwd-"));

  const result = runCli(["drift", "--all", fixtureRoot], emptyCwd);

  const output = `${result.stdout}${result.stderr}`;
  assert.ok(
    output.includes(repoDir),
    `expected 'drift --all ${fixtureRoot}' (run from an unrelated cwd) to discover ${repoDir}; got stdout=${JSON.stringify(result.stdout)} stderr=${JSON.stringify(result.stderr)}`,
  );
});

// Non-regression: the workaround documented by the QA report, `--all=true`
// followed by the root, must keep working exactly as before.
test("`right-git drift --all=true <root>` still scopes the scan to the given root", () => {
  const fixtureRoot = fs.mkdtempSync(path.join(os.tmpdir(), "right-git-cli-root-eq-"));
  const repoDir = path.join(fixtureRoot, "managed-repo");
  fs.mkdirSync(repoDir, { recursive: true });
  fs.writeFileSync(path.join(repoDir, ".rightgit.json"), `${JSON.stringify(SAMPLE_MANIFEST, null, 2)}\n`, "utf8");
  fs.writeFileSync(path.join(repoDir, "package.json"), JSON.stringify({ name: "widgets" }), "utf8");

  const emptyCwd = fs.mkdtempSync(path.join(os.tmpdir(), "right-git-cli-cwd-eq-"));
  const result = runCli(["drift", `--all=true`, fixtureRoot], emptyCwd);

  const output = `${result.stdout}${result.stderr}`;
  assert.ok(output.includes(repoDir), `expected discovery of ${repoDir}; got stdout=${JSON.stringify(result.stdout)} stderr=${JSON.stringify(result.stderr)}`);
});
