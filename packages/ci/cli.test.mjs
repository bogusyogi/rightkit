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

// A sync or install warning (an @rightkit/release pin the rendered check job cannot be proved to have) is printed by the CLI, to
// stderr, and does not change the exit code; an exact pin below 0.6.5 is an error at stage 'dependency' with exit 1. The visibility
// gate asks `gh repo view --json visibility`. A preload answers exactly that call with PUBLIC inside the CLI process and passes
// every other launch through: the same stand-in on every platform (a `#!/bin/sh` script on PATH cannot be launched by execFile on
// Windows, which only finds real executables).
const PUBLIC_GH = `data:text/javascript,${encodeURIComponent([
  'import cp from "node:child_process"; import { syncBuiltinESMExports } from "node:module"; import { promisify } from "node:util";',
  "const visibility = (file, args) => file === 'gh' && Array.isArray(args) && args.join(' ') === 'repo view --json visibility';",
  "const original = cp.execFile; const originalAsync = original[promisify.custom];",
  "const stub = function execFile(file, args, ...rest) { if (!visibility(file, args)) return original.call(this, file, args, ...rest); const cb = rest.find((x) => typeof x === 'function'); queueMicrotask(() => cb?.(null, '{\"visibility\":\"PUBLIC\"}\\n', '')); return { pid: 0 }; };",
  "stub[promisify.custom] = (file, args, ...rest) => (visibility(file, args) ? Promise.resolve({ stdout: '{\"visibility\":\"PUBLIC\"}\\n', stderr: '' }) : originalAsync(file, args, ...rest));",
  "cp.execFile = stub; syncBuiltinESMExports();",
].join("\n"))}`;
const runCliWithGh = (args, cwd) => spawnSync(process.execPath, ["--import", PUBLIC_GH, cliPath, ...args], { cwd, encoding: "utf8" });
const checkJobRepo = (packageJson, manifest = SAMPLE_MANIFEST) => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "right-git-cli-check-"));
  if (manifest) fs.writeFileSync(path.join(dir, ".rightgit.json"), `${JSON.stringify(manifest, null, 2)}\n`, "utf8");
  fs.writeFileSync(path.join(dir, "package.json"), JSON.stringify(packageJson), "utf8");
  return dir;
};

test("without the visibility stand-in, and with no gh on PATH, sync fails closed at the visibility gate (the stand-in is what makes the fixture public)", () => {
  const result = spawnSync(process.execPath, [cliPath, "sync", checkJobRepo({ name: "widgets", devDependencies: { "@rightkit/release": "0.6.5" } }), "--dry-run"], { cwd: os.tmpdir(), encoding: "utf8", env: { ...process.env, PATH: path.dirname(process.execPath) } });
  assert.notEqual(result.status, 0, `${result.stdout}${result.stderr}`);
  assert.match(`${result.stdout}${result.stderr}`, /visibility/i);
});

test("`right-git sync` prints the @rightkit/release warning for a range pin or no dependency, and exits 0", () => {
  for (const [packageJson, message] of [
    [{ name: "widgets", devDependencies: { "@rightkit/release": "^0.6.5" } }, /^ {2}warn: lanes ci, package-smoke render the RightKit check job.*is "\^0\.6\.5", not an exact version/m],
    [{ name: "widgets" }, /^ {2}warn: lanes ci, package-smoke render the RightKit check job.*declares no @rightkit\/release dependency/m],
  ]) {
    const result = runCliWithGh(["sync", checkJobRepo(packageJson), "--dry-run"], os.tmpdir());
    assert.equal(result.status, 0, `${result.stdout}${result.stderr}`);
    assert.match(result.stderr, message);
    assert.match(result.stdout, /^right-git sync: \d+ file\(s\) would change\.$/m);
  }
});

test("`right-git sync` refuses an exact @rightkit/release pin below 0.6.5 at stage 'dependency', exit 1, and `install` warns like `sync`", () => {
  const refused = runCliWithGh(["sync", checkJobRepo({ name: "widgets", devDependencies: { "@rightkit/release": "0.6.4" } })], os.tmpdir());
  assert.equal(refused.status, 1, `${refused.stdout}${refused.stderr}`);
  assert.match(refused.stderr, /^right-git sync: failed at stage 'dependency'$/m);
  assert.match(refused.stderr, /@rightkit\/release is pinned to 0\.6\.4, which has no rightkit-gate check \(pin 0\.6\.5 or later\)/);
  assert.doesNotMatch(refused.stderr, /warn:/);
  const installed = runCliWithGh(["install", checkJobRepo({ name: "widgets" }, null), "--lanes", "ci"], os.tmpdir());
  assert.equal(installed.status, 0, `${installed.stdout}${installed.stderr}`);
  assert.match(installed.stderr, /^ {2}warn: lane ci renders the RightKit check job.*declares no @rightkit\/release dependency/m);
  assert.match(installed.stdout, /^right-git install: wrote /m);
  const clean = runCliWithGh(["sync", checkJobRepo({ name: "widgets", devDependencies: { "@rightkit/release": "0.6.5" } }), "--dry-run"], os.tmpdir());
  assert.equal(clean.status, 0);
  assert.doesNotMatch(clean.stderr, /warn:/);
});

// process.exit() used to end the CLI with stdout still queued: when the reader of a pipe had not caught up, `right-git sync --dry-run | tee log`
// lost the end of the diff and the `would change` line (about one run in ten here, which made the warning test above flaky). A preload that
// delays every stdout write makes the queue deterministic: the output must still arrive whole, and the exit code must be the command's.
test("the CLI puts out everything it wrote before it exits, even when stdout is slow to take it", () => {
  const slowStdout = `data:text/javascript,${encodeURIComponent(
    "const write = process.stdout.write.bind(process.stdout);"
    + "process.stdout.write = (chunk, ...rest) => { setTimeout(() => write(chunk, ...rest), 40); return true; };",
  )}`;
  const run = (args) => spawnSync(process.execPath, ["--import", slowStdout, "--import", PUBLIC_GH, cliPath, ...args], { cwd: os.tmpdir(), encoding: "utf8" });
  const synced = run(["sync", checkJobRepo({ name: "widgets", devDependencies: { "@rightkit/release": "0.6.5" } }), "--dry-run"]);
  assert.equal(synced.status, 0, synced.stderr);
  assert.match(synced.stdout, /^CREATE ci\.yml$/m, "the first thing it printed");
  assert.match(synced.stdout, /^right-git sync: \d+ file\(s\) would change\.$/m, "and the last");
  const refused = run(["sync", checkJobRepo({ name: "widgets", devDependencies: { "@rightkit/release": "0.6.4" } })]);
  assert.equal(refused.status, 1, refused.stderr);
  assert.match(refused.stderr, /^right-git sync: failed at stage 'dependency'$/m);
  const unknown = run(["frobnicate"]);
  assert.equal(unknown.status, 2);
  assert.match(unknown.stderr, /^right-git: unknown command: frobnicate$/m);
  assert.match(unknown.stdout, /right-git/, "the help it prints for an unknown command");
  assert.equal(run(["--version"]).stdout.trim(), JSON.parse(fs.readFileSync(path.join(packageRoot, "package.json"), "utf8")).version);
});
