// `right-release update`: runs the @rightkit/outdated updater that @rightkit/release pins (plan item 2.10, gap C4-17).
//
// pnpm links only a project's direct dependencies' bins, so `pnpm exec right-update` does not exist in an app that depends on
// @rightkit/release alone. The refusal for a stale pin therefore names `pnpm exec right-release update run`; these tests build the
// installed layouts (npm-flat and pnpm virtual store) around the real CLI and prove the updater's bin is found and run from there.
import assert from "node:assert/strict";
import { cpSync, mkdirSync, mkdtempSync, realpathSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { spawnSync } from "./background-process.mjs";
import {
  OUTDATED_BIN,
  OUTDATED_PACKAGE,
  OutdatedUnavailableError,
  UPDATE_COMMAND,
  findOutdatedPackage,
  resolveOutdatedBin,
} from "./outdated-command.mjs";

const here = path.dirname(fileURLToPath(import.meta.url));
const tempDir = (t) => {
  const dir = mkdtempSync(path.join(os.tmpdir(), "rk-outdated-"));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  return dir;
};
const write = (file, text) => { mkdirSync(path.dirname(file), { recursive: true }); writeFileSync(file, text); };

// The files of this package the CLI needs to run `update`: the CLI, its three small imports and the command module.
const CLI_FILES = ["cli/right-release.mjs", "execution-policy.mjs", "host-upload-refusal.mjs", "background-process.mjs", "outdated-command.mjs", "app-check.mjs", "legion-semantic-contracts.mjs", "consumer-contracts.mjs", "consumer-contracts.json", "ownership-class-rules.mjs", "source-closure-rules.mjs", "shell-contract.mjs", "suite-trust-roots.mjs", "suite-api.generated.mjs", "consumer-input-contracts.mjs", "consumer-source-contracts.mjs", "workflow-yaml.mjs"];
function installRelease(dir) {
  for (const file of CLI_FILES) { mkdirSync(path.dirname(path.join(dir, file)), { recursive: true }); cpSync(path.join(here, file), path.join(dir, file)); }
  write(path.join(dir, "package.json"), JSON.stringify({ name: "@rightkit/release", version: "9.9.9", type: "module", bin: { "right-release": "cli/right-release.mjs" } }));
}
// A stand-in updater: prints its argv, cwd and the marker, and exits with the code its first argument asks for.
const FAKE_BIN = `#!/usr/bin/env node
const args = process.argv.slice(2);
if (args[0] === "--version") { process.stdout.write("7.7.7\\n"); process.exit(0); }
process.stdout.write(JSON.stringify({ args, cwd: process.cwd() }) + "\\n");
process.exit(args[0] === "fail" ? 3 : 0);
`;
function installOutdated(dir, { bin = { [OUTDATED_BIN]: "dist/bin.js" }, name = OUTDATED_PACKAGE, file = "dist/bin.js" } = {}) {
  write(path.join(dir, "package.json"), JSON.stringify({ name, version: "7.7.7", type: "module", bin, exports: { ".": { types: "./dist/index.d.ts", import: "./dist/index.js" } } }));
  if (file) write(path.join(dir, file), FAKE_BIN);
  write(path.join(dir, "dist", "index.js"), "export {};\n");
}
const runCli = (releaseDir, args, cwd) => spawnSync(process.execPath, [path.join(releaseDir, "cli", "right-release.mjs"), ...args], { cwd, encoding: "utf8", windowsHide: true, timeout: 60_000 });

test("UPDATE_COMMAND is what the stale-pin refusal tells an app to run", () => {
  assert.equal(UPDATE_COMMAND, "pnpm exec right-release update run");
});

test("npm-flat layout: the updater installed beside @rightkit/release is found and run with the caller's arguments and cwd", (t) => {
  // The lookup answers real paths; macOS puts the temporary directory behind the /var -> /private/var symlink.
  const app = realpathSync(tempDir(t));
  installRelease(path.join(app, "node_modules", "@rightkit", "release"));
  installOutdated(path.join(app, "node_modules", "@rightkit", "outdated"));
  const releaseDir = path.join(app, "node_modules", "@rightkit", "release");
  assert.equal(findOutdatedPackage(releaseDir), path.join(app, "node_modules", "@rightkit", "outdated"));
  assert.equal(resolveOutdatedBin(releaseDir), path.join(app, "node_modules", "@rightkit", "outdated", "dist", "bin.js"));
  const ok = runCli(releaseDir, ["update", "run", "--repo", "r with space", "--dry-run"], app);
  assert.equal(ok.status, 0, ok.stderr);
  assert.deepEqual(JSON.parse(ok.stdout), { args: ["run", "--repo", "r with space", "--dry-run"], cwd: realpathSync(app) });
  const version = runCli(releaseDir, ["update", "--version"], app);
  assert.equal(version.stdout.trim(), "7.7.7");
  // The updater's exit status is the command's exit status.
  const failed = runCli(releaseDir, ["update", "fail"], app);
  assert.equal(failed.status, 3);
});

test("pnpm layout: the updater beside the virtual-store copy of @rightkit/release is found through the app's symlink", (t) => {
  const app = tempDir(t);
  const store = path.join(app, "node_modules", ".pnpm");
  const releaseReal = path.join(store, "@rightkit+release@9.9.9", "node_modules", "@rightkit", "release");
  const outdatedReal = path.join(store, "@rightkit+outdated@7.7.7", "node_modules", "@rightkit", "outdated");
  installRelease(releaseReal);
  installOutdated(outdatedReal);
  try {
    symlinkSync(outdatedReal, path.join(store, "@rightkit+release@9.9.9", "node_modules", "@rightkit", "outdated"), "junction");
    mkdirSync(path.join(app, "node_modules", "@rightkit"), { recursive: true });
    symlinkSync(releaseReal, path.join(app, "node_modules", "@rightkit", "release"), "junction");
  } catch (error) {
    t.skip(`cannot create directory links here (${error.code})`);
    return;
  }
  // The app has only @rightkit/release among its direct dependencies: no @rightkit/outdated and no right-update bin at its root.
  const viaApp = path.join(app, "node_modules", "@rightkit", "release");
  const result = runCli(viaApp, ["update", "check", "--json"], app);
  assert.equal(result.status, 0, result.stderr);
  assert.deepEqual(JSON.parse(result.stdout).args, ["check", "--json"]);
});

test("a missing updater is a typed, actionable refusal: exit 2, never a silent success", (t) => {
  const app = tempDir(t);
  const releaseDir = path.join(app, "node_modules", "@rightkit", "release");
  installRelease(releaseDir);
  assert.equal(findOutdatedPackage(releaseDir), null);
  assert.throws(() => resolveOutdatedBin(releaseDir), (error) => error instanceof OutdatedUnavailableError && error.code === "OUTDATED_UNAVAILABLE" && /not installed for @rightkit\/release/.test(error.message));
  const result = runCli(releaseDir, ["update", "run"], app);
  assert.equal(result.status, 2);
  assert.match(result.stderr, /right-release update: @rightkit\/outdated is not installed for @rightkit\/release; reinstall @rightkit\/release/);
  assert.equal(result.stdout, "");
});

test("an installed package that is not the updater, has no right-update bin, or lists a missing or escaping file is refused", (t) => {
  const refused = (setup) => {
    const app = tempDir(t);
    const releaseDir = path.join(app, "node_modules", "@rightkit", "release");
    installRelease(releaseDir);
    setup(path.join(app, "node_modules", "@rightkit", "outdated"), app);
    return () => resolveOutdatedBin(releaseDir);
  };
  assert.throws(refused((dir) => installOutdated(dir, { name: "@rightkit/other" })), /is not an @rightkit\/outdated that provides right-update/);
  assert.throws(refused((dir) => installOutdated(dir, { bin: { "other-bin": "dist/bin.js" } })), /provides right-update/);
  assert.throws(refused((dir) => installOutdated(dir, { file: null })), /lists right-update at dist\/bin\.js, which is not installed/);
  assert.throws(refused((dir, app) => { installOutdated(dir, { bin: { [OUTDATED_BIN]: "../../../evil.mjs" }, file: null }); write(path.join(app, "evil.mjs"), "// outside the package\n"); }), /not installed/);
  assert.equal(refused((dir) => installOutdated(dir, { bin: "dist/bin.js" }))().endsWith(path.join("dist", "bin.js")), true, "a string bin is the updater's bin");
});

test("the CLI help lists the update command", () => {
  const help = spawnSync(process.execPath, [path.join(here, "cli", "right-release.mjs"), "--help"], { encoding: "utf8", windowsHide: true, timeout: 60_000 });
  assert.equal(help.status, 0);
  assert.match(help.stdout, /^ {2}update check\|apply\|gate\|land\|discard\|run\|schedule \[--repo <path>\]$/m);
  assert.match(help.stdout, /pinned @rightkit\/outdated updater \(right-update\)/);
});
