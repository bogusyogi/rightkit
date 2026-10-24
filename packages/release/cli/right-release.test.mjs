// right-release runs the app check (../app-check.mjs) in-process before any command that compiles, packages, signs, ships or inspects a release starts a
// child. These tests run the real CLI as a process with a spy loaded in front of it: every child spawn is recorded and replaced by a
// no-op, and ../development.mjs (imported, not spawned) by a stub that records its arguments. Not run by `pnpm test` (its glob is
// the package root only); run it with: node --import ./test-support/broker-guard.mjs --test cli/right-release.test.mjs
import assert from "node:assert/strict";
import { execFileSync, spawnSync } from "../background-process.mjs";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath, pathToFileURL } from "node:url";
import { CHECKED_COMMANDS, addDays } from "../app-check.mjs";

const CLI = fileURLToPath(new URL("./right-release.mjs", import.meta.url));
const PACKAGE = path.resolve(path.dirname(CLI), "..");

// development.mjs is imported by the CLI (not spawned): a resolve hook swaps it for a stub that records the arguments main gets.
const DEVELOPMENT_STUB = 'import { appendFileSync } from "node:fs"; export function main(args) { appendFileSync(process.env.RK_CLI_SPY_LOG, JSON.stringify({ development: args }) + "\\n"); }';
// background-process.mjs is the one module every release child is launched through (the hidden-launch policy test enforces it), so the
// spy stands in for its spawn there: the stub re-exports the real module (loaded under a query string, which the hook does not match)
// and replaces spawn with a recorder that starts a no-op child instead of the command.
const REAL_BACKGROUND = pathToFileURL(fileURLToPath(new URL("../background-process.mjs", import.meta.url))).href;
const BACKGROUND_STUB = `export * from ${JSON.stringify(REAL_BACKGROUND + "?real")};
import { spawn as realSpawn } from ${JSON.stringify(REAL_BACKGROUND + "?real")};
import { appendFileSync } from "node:fs";
export function spawn(command, args, options) {
  appendFileSync(process.env.RK_CLI_SPY_LOG, JSON.stringify({ spawn: [command, ...(Array.isArray(args) ? args : [])], cwd: options?.cwd }) + "\\n");
  return realSpawn(process.execPath, ["-e", "process.exit(0)"], { stdio: "ignore" });
}`;
const PRELOAD = `
import { registerHooks } from "node:module";
registerHooks({
  resolve(specifier, context, nextResolve) {
    if (/(^|\\/)development\\.mjs$/.test(specifier)) return { url: "data:text/javascript," + encodeURIComponent(${JSON.stringify(DEVELOPMENT_STUB)}), shortCircuit: true };
    if (/(^|\\/)background-process\\.mjs$/.test(specifier)) return { url: "data:text/javascript," + encodeURIComponent(${JSON.stringify(BACKGROUND_STUB)}), shortCircuit: true };
    return nextResolve(specifier, context);
  },
});
`;

const tmpDir = (t, prefix) => {
  const dir = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), prefix)));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  return dir;
};
const git = (cwd, ...args) => execFileSync("git", args, { cwd, encoding: "utf8", stdio: ["ignore", "pipe", "pipe"] });

/** An app repository: green by default, red (a package.json script that runs right-release --skip-checks, G17) when asked. */
function app(t, { red = false } = {}) {
  const repo = tmpDir(t, "rk-cli-app-");
  git(repo, "init", "-q");
  for (const [k, v] of [["user.email", "t@example.test"], ["user.name", "t"]]) git(repo, "config", k, v);
  fs.mkdirSync(path.join(repo, "sub"));
  fs.writeFileSync(path.join(repo, "sub", "right-release.config.mjs"), "export default {};\n");
  fs.writeFileSync(path.join(repo, "sub", "switches.json"), "{}\n"); // G20: a release config needs its switches inventory
  fs.writeFileSync(path.join(repo, "package.json"), JSON.stringify({ scripts: { ship: red ? "right-release build --skip-checks" : "right-release build" } }));
  git(repo, "add", "-A");
  git(repo, "commit", "-q", "-m", "app");
  return repo;
}
const makeRed = (repo) => { fs.writeFileSync(path.join(repo, "package.json"), JSON.stringify({ scripts: { ship: "right-release build --skip-checks" } })); };
const makeGreen = (repo) => { fs.writeFileSync(path.join(repo, "package.json"), JSON.stringify({ scripts: { ship: "right-release build" } })); };

let preload;
function runCli(t, cwd, args, { spy = true } = {}) {
  if (!preload) {
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), "rk-cli-spy-"));
    process.on("exit", () => fs.rmSync(dir, { recursive: true, force: true }));
    preload = path.join(dir, "spy.mjs");
    fs.writeFileSync(preload, PRELOAD);
  }
  const log = path.join(tmpDir(t, "rk-cli-log-"), "calls.log");
  const result = spawnSync(process.execPath, [...(spy ? ["--import", pathToFileURL(preload).href] : []), CLI, ...args], { cwd, encoding: "utf8", env: { ...process.env, RK_CLI_SPY_LOG: log } });
  const calls = fs.existsSync(log) ? fs.readFileSync(log, "utf8").split("\n").filter(Boolean).map((l) => JSON.parse(l)) : [];
  return { status: result.status, stdout: result.stdout, stderr: result.stderr, spawns: calls.filter((c) => c.spawn).map((c) => c.spawn), developments: calls.filter((c) => c.development).map((c) => c.development), calls };
}
const script = (name) => path.join(PACKAGE, name);

// Every command the check guards, with how it is invoked and what it starts when the check is green. A new entry in CHECKED_COMMANDS
// fails the coverage test below until it has a row here.
const GUARDED = [
  ["release", ["release", "--platform", "win"], { spawn: ["build-release.mjs", "--platform", "win"] }],
  ["build", ["build", "--platform", "win"], { spawn: ["build-release.mjs", "--platform", "win"] }],
  ["package", ["package", "--platform", "win"], { spawn: ["build-release.mjs", "--platform", "win"] }],
  ["dev", ["dev", "--platform", "win", "--dry-run"], { development: ["--platform", "win", "--dry-run"] }],
  ["qa", ["qa", "--stage", "source", "--platform", "win"], { spawn: ["release-qa-command.mjs", "--stage", "source", "--platform", "win"] }],
  ["upload", ["upload", "--release", "r1", "--platform", "win", "--tier", "patch"], { spawn: ["upload-release.mjs", "--release", "r1", "--platform", "win", "--tier", "patch"] }],
  ["publish", ["publish", "--release", "r1", "--platform", "win", "--tier", "patch"], { spawn: ["upload-release.mjs", "--release", "r1", "--platform", "win", "--tier", "patch"] }],
  ["publish-update", ["publish-update", "--release", "r1", "--platform", "win", "--tier", "patch"], { spawn: ["publish-update.mjs", "--release", "r1", "--platform", "win", "--tier", "patch"] }],
  ["github", ["github", "--release", "r1", "--tier", "patch", "--platform", "win", "--dry-run"], { spawn: ["github-release.mjs", "--release", "r1", "--tier", "patch", "--platform", "win", "--dry-run"] }],
  ["doctor", ["doctor", "--platform", "win"], { spawn: ["release.mjs", "--doctor", "--platform", "win"] }],
  ["sign-windows", ["sign-windows", "--platform", "win"], { spawn: ["sign-windows.mjs", "--platform", "win"] }],
  ["sign-updater", ["sign-updater", "--platform", "win"], { spawn: ["sign-updater.mjs", "--platform", "win"] }],
  ["create-mac-updater", ["create-mac-updater", "--platform", "mac"], { spawn: ["create-mac-updater.mjs", "--platform", "mac"] }],
  // The compile, package and signing commands the first version of the check left out (docs/PIPELINE-ARCHITECTURE.md P1).
  ["test", ["test", "--runner", "vitest", "--", "run"], { spawn: ["test-command.mjs", "--runner", "vitest", "--", "run"] }],
  ["web-build", ["web-build", "--dry-run"], { spawn: ["web-build.mjs", "--dry-run"] }],
  ["swift-package", ["swift-package", "mac-app", "--config", "swift-package.json"], { spawn: ["swift-package-command.mjs", "mac-app", "--config", "swift-package.json"] }],
  ["recipe-build", ["recipe-build", "--plan-file", "plan.json"], { spawn: ["recipe-build.mjs", "--plan-file", "plan.json"] }],
  ["stage-sidecar", ["stage-sidecar", "--config", "right-release.config.mjs"], { spawn: ["stage-sidecar-command.mjs", "--config", "right-release.config.mjs"] }],
  ["sign-candidate", ["sign-candidate", "--platform", "win", "--artifact", "a", "--candidate-receipt", "r.json", "--out", "b.json"], { spawn: ["sign-candidate.mjs", "--platform", "win", "--artifact", "a", "--candidate-receipt", "r.json", "--out", "b.json"] }],
  // The two the decision "deterministic first, no escape hatch" added: a signing step over a CI dev artifact (the tree on disk) and the
  // add-on build (it runs target.build.cmd over HEAD).
  ["sign-dev-artifact", ["sign-dev-artifact", "--artifact", "a", "--manifest", "sign.json"], { spawn: ["sign-dev-artifact.mjs", "--artifact", "a", "--manifest", "sign.json"] }],
  ["addon build", ["addon", "build", "--config", "sub/right-release.config.mjs", "--platform", "win"], { spawn: ["addon-command.mjs", "build", "--config", "sub/right-release.config.mjs", "--platform", "win"] }],
];

test("every command the check guards has a case below", () => {
  assert.deepEqual(GUARDED.map(([name]) => name).sort(), [...CHECKED_COMMANDS.keys()].sort());
});

for (const [name, args, expected] of GUARDED) {
  test(`${name}: a red check starts nothing and exits 1 with the findings; a green one runs the command exactly as before`, (t) => {
    const red = runCli(t, app(t, { red: true }), args);
    assert.equal(red.status, 1, red.stdout + red.stderr);
    assert.deepEqual(red.calls, [], "no child was spawned and nothing was imported to run");
    assert.match(red.stderr, /^FAIL skip-checks \(G17\): package\.json:1 script "ship" runs right-release with --skip-checks$/m);
    assert.match(red.stderr, /^     fix: drop --skip-checks/m);
    assert.match(red.stderr, new RegExp(`^right-release ${name}: the app check found problems in .*; nothing was started\\.`, "m"));
    assert.equal(red.stdout, "", "stdout stays clean for the commands that print JSON");
    const green = runCli(t, app(t), args);
    assert.equal(green.status, 0, green.stdout + green.stderr);
    assert.equal(green.stderr, "", "a green check prints nothing");
    if (expected.spawn) {
      const [file, ...rest] = expected.spawn;
      assert.deepEqual(green.spawns, [[process.execPath, script(file), ...rest]]);
    } else {
      assert.deepEqual(green.spawns, []);
      assert.deepEqual(green.developments, [expected.development], "development.mjs main was called with the arguments after dev");
    }
  });
}

test("direct flags run build, so they are checked too; build --web is the build", (t) => {
  const direct = ["--platform", "win", "--tier", "patch"];
  const red = runCli(t, app(t, { red: true }), direct);
  assert.equal(red.status, 1);
  assert.deepEqual(red.calls, []);
  assert.match(red.stderr, /^right-release build: the app check found problems/m);
  assert.deepEqual(runCli(t, app(t), direct).spawns, [[process.execPath, script("build-release.mjs"), ...direct]]);
  const web = runCli(t, app(t, { red: true }), ["build", "--web", "--dry-run"]);
  assert.equal(web.status, 1);
  assert.deepEqual(web.calls, []);
  assert.deepEqual(runCli(t, app(t), ["build", "--web", "--dry-run"]).spawns, [[process.execPath, script("web-build.mjs"), "--dry-run"]]);
});

test("there is no flag that skips the check", (t) => {
  const repo = app(t, { red: true });
  for (const flag of ["--no-check", "--skip-check", "--skip-checks", "--check=false", "--force"]) {
    const result = runCli(t, repo, ["build", "--platform", "win", flag]);
    assert.equal(result.status, 1, flag);
    assert.deepEqual(result.calls, [], flag);
  }
  assert.equal(runCli(t, repo, ["build", "--help"]).status, 1, "--help is not a way past it either");
});

test("a baseline entry cannot excuse what the build refuses: switches-inventory fails build and doctor, and still passes right-release check", (t) => {
  const repo = app(t);
  fs.rmSync(path.join(repo, "sub", "switches.json"));
  fs.mkdirSync(path.join(repo, ".rightkit"));
  // The CLI is a child process, so `now` cannot be injected: the entry is added today (UTC) and ends 30 days later, valid on any day it is run.
  const added = new Date().toISOString().slice(0, 10);
  fs.writeFileSync(path.join(repo, ".rightkit", "app-check-baseline.json"), JSON.stringify({ schema: 1, known: [{ id: "switches-inventory|sub/right-release.config.mjs|sub", issue: "app#9", added, sunset: addDays(added, 30) }] }));
  git(repo, "add", "-A");
  git(repo, "commit", "-q", "-m", "baseline the missing inventory");
  const check = runCli(t, repo, ["check"]);
  assert.equal(check.status, 0, check.stdout + check.stderr);
  assert.match(check.stdout, /^KNOWN switches-inventory: /m);
  for (const args of [["build", "--platform", "win"], ["doctor", "--platform", "win"]]) {
    const result = runCli(t, repo, args);
    assert.equal(result.status, 1, args[0]);
    assert.deepEqual(result.calls, [], args[0]);
    assert.match(result.stderr, /^FAIL switches-inventory \(G20\): .*\(the baseline lists it, but the build refuses it\)$/m);
  }
});

test("build, upload, packaging, addon build and the signing commands check HEAD; dev, doctor, test, stage-sidecar and sign-dev-artifact check what a compile would see", (t) => {
  const worktree = (name) => CHECKED_COMMANDS.get(name) === "worktree";
  assert.deepEqual(GUARDED.map(([name]) => name).filter(worktree).sort(), ["dev", "doctor", "sign-dev-artifact", "stage-sidecar", "test"]);
  // HEAD is red, the tree on disk is fixed and uncommitted: the commands that read HEAD refuse, the others go on.
  const headRed = app(t, { red: true });
  makeGreen(headRed);
  for (const [name, args] of GUARDED) {
    const result = runCli(t, headRed, args);
    if (worktree(name)) assert.equal(result.status, 0, `${name}: ${result.stderr}`);
    else { assert.equal(result.status, 1, `${name} must check HEAD`); assert.deepEqual(result.calls, [], name); }
  }
  // The other way round: HEAD is green, the tree on disk has an uncommitted red edit.
  const treeRed = app(t);
  makeRed(treeRed);
  for (const [name, args] of GUARDED) {
    const result = runCli(t, treeRed, args);
    if (worktree(name)) { assert.equal(result.status, 1, `${name} must check the worktree`); assert.deepEqual(result.calls, [], name); }
    else assert.equal(result.status, 0, `${name}: ${result.stderr}`);
  }
});

test("recipe-build checks the repository of --app-root, from any working directory; the flags of the other new commands make no difference", (t) => {
  const repo = app(t, { red: true });
  const elsewhere = tmpDir(t, "rk-cli-elsewhere-");
  const red = runCli(t, elsewhere, ["recipe-build", "--plan-file", "plan.json", "--app-root", repo]);
  assert.equal(red.status, 1);
  assert.deepEqual(red.calls, []);
  assert.match(red.stderr, /^FAIL skip-checks \(G17\)/m);
  assert.match(red.stderr, /^right-release recipe-build: the app check found problems in /m);
  makeGreen(repo);
  git(repo, "commit", "-q", "-am", "green");
  const green = runCli(t, elsewhere, [`recipe-build`, "--app-root", repo, "--plan-file", "plan.json"]);
  assert.equal(green.status, 0, green.stderr);
  assert.deepEqual(green.spawns, [[process.execPath, script("recipe-build.mjs"), "--app-root", repo, "--plan-file", "plan.json"]]);
  // --check, --list, --dry-run and --help only change what the command does after the check; none of them is a way past it
  makeRed(repo);
  git(repo, "commit", "-q", "-am", "red again");
  for (const args of [["stage-sidecar", "--check"], ["test", "--list"], ["web-build", "--dry-run"], ["swift-package", "msix", "--dry-run", "--config", "swift-package.json"], ["sign-candidate", "--dry-run"], ["recipe-build", "--help"]]) {
    const result = runCli(t, repo, args);
    assert.equal(result.status, 1, args.join(" "));
    assert.deepEqual(result.calls, [], args.join(" "));
  }
});

test("--config picks the repository: the git top-level of the config's folder, from any working directory", (t) => {
  const repo = app(t, { red: true });
  const elsewhere = tmpDir(t, "rk-cli-elsewhere-");
  const config = path.join(repo, "sub", "right-release.config.mjs");
  for (const args of [["doctor", "--config", config], ["doctor", `--config=${config}`]]) {
    const red = runCli(t, elsewhere, args);
    assert.equal(red.status, 1, args.join(" "));
    assert.match(red.stderr, /FAIL skip-checks \(G17\)/);
    assert.deepEqual(red.calls, []);
  }
  // A relative --config resolves against the working directory.
  const relative = runCli(t, repo, ["doctor", "--config", "sub/right-release.config.mjs"]);
  assert.equal(relative.status, 1);
  makeGreen(repo);
  git(repo, "commit", "-q", "-am", "green");
  const green = runCli(t, elsewhere, ["doctor", "--config", config]);
  assert.equal(green.status, 0, green.stderr);
  assert.deepEqual(green.spawns, [[process.execPath, script("release.mjs"), "--doctor", "--config", config]]);
});

test("a guarded command outside any git repository exits 1 and starts nothing: the check cannot run", (t) => {
  const outside = tmpDir(t, "rk-cli-nogit-");
  const result = runCli(t, outside, ["build", "--platform", "win"]);
  assert.equal(result.status, 1);
  assert.deepEqual(result.calls, []);
  assert.match(result.stderr, /^right-release build: the app check needs a git repository and .* is not inside one; nothing was started$/m);
});

test("commands the check does not guard run on a red repository, and so do the report forms of guarded ones", (t) => {
  const repo = app(t, { red: true });
  const cases = [
    [["cache", "status"], ["cache-command.mjs", "status"]],
    [["deps", "--check"], ["deps.mjs", "--check"]],
    [["qa", "audit"], ["release-qa-command.mjs", "audit"]],
    // dmg takes --app/--out paths, compiles nothing and has no repository (app-check.mjs UNCHECKED_COMMANDS, proved in app-check.test.mjs)
    [["dmg", "build", "--app", "x.app", "--out", "x.dmg"], ["dmg-build.mjs", "build", "--app", "x.app", "--out", "x.dmg"]],
    // the other forms of addon compile nothing: only `addon build` is guarded
    [["addon", "doctor", "--config", "sub/right-release.config.mjs", "--platform", "win"], ["addon-command.mjs", "doctor", "--config", "sub/right-release.config.mjs", "--platform", "win"]],
    [["addon", "upload", "--config", "sub/right-release.config.mjs", "--platform", "win", "--tier", "patch", "--dry-run"], ["addon-command.mjs", "upload", "--config", "sub/right-release.config.mjs", "--platform", "win", "--tier", "patch", "--dry-run"]],
    [["addon", "adopt", "--lock", "addon.lock.json", "--output", "out", "--platform", "win"], ["addon-command.mjs", "adopt", "--lock", "addon.lock.json", "--output", "out", "--platform", "win"]],
    [["publish", "cargo", "--crate", "rightkit-x", "--dry-run"], ["publish-cargo.mjs", "--crate", "rightkit-x", "--dry-run"]],
    [["--version"], ["release.mjs", "--version"]],
  ];
  for (const [args, [file, ...rest]] of cases) {
    const result = runCli(t, repo, args);
    assert.equal(result.status, 0, `${args.join(" ")}: ${result.stderr}`);
    assert.deepEqual(result.spawns, [[process.execPath, script(file), ...rest]], args.join(" "));
  }
  const suite = runCli(t, repo, ["doctor", "--all"]);
  assert.equal(suite.status, 0, suite.stderr);
  assert.deepEqual(suite.spawns, [[process.execPath, "--test", script("right-suite-contract.test.mjs")]], "doctor --all is the suite contract test, not an app");
});

test("dmg build has no repository: it dispatches from a directory that is no git repository, which is why the check cannot guard it", (t) => {
  const outside = tmpDir(t, "rk-cli-nogit-");
  const args = ["dmg", "build", "--app", "x.app", "--out", "x.dmg", "--volname", "X"];
  const result = runCli(t, outside, args);
  assert.equal(result.status, 0, result.stderr);
  assert.deepEqual(result.spawns, [[process.execPath, script("dmg-build.mjs"), ...args.slice(1)]]);
  // the guarded commands, from the same directory, cannot run their check and start nothing
  for (const guarded of [["addon", "build", "--platform", "win"], ["sign-dev-artifact", "--artifact", "a", "--manifest", "m.json"]]) {
    const refused = runCli(t, outside, guarded);
    assert.equal(refused.status, 1, guarded.join(" "));
    assert.deepEqual(refused.calls, [], guarded.join(" "));
    assert.match(refused.stderr, new RegExp(`^right-release ${guarded[0] === "addon" ? "addon build" : guarded[0]}: the app check needs a git repository`, "m"));
  }
});

test("check: the same findings as rightkit-gate check, exit 1 red and 0 green, --list, and --config picks the repository", (t) => {
  const repo = app(t, { red: true });
  const red = runCli(t, repo, ["check", "--commit", "HEAD"]);
  assert.equal(red.status, 1);
  assert.match(red.stdout, /^FAIL skip-checks \(G17\): package\.json:1 /m);
  assert.match(red.stdout, /^check: 1 problem\(s\) in \w{12} \(12 rules, /m);
  assert.deepEqual(red.calls, [], "check spawns nothing");
  const gate = spawnSync(process.execPath, [path.join(PACKAGE, "bin", "rightkit-gate.mjs"), "check", "--commit", "HEAD"], { cwd: repo, encoding: "utf8" });
  assert.equal(gate.stdout.replace(/[\d.]+s/, "Ns"), red.stdout.replace(/[\d.]+s/, "Ns"), "right-release check and rightkit-gate check print the same");
  const fromElsewhere = runCli(t, tmpDir(t, "rk-cli-elsewhere-"), ["check", "--config", path.join(repo, "sub", "right-release.config.mjs")]);
  assert.equal(fromElsewhere.status, 1);
  assert.match(fromElsewhere.stdout, /FAIL skip-checks/);
  makeGreen(repo);
  assert.equal(runCli(t, repo, ["check", "--worktree"]).status, 0, "the green tree on disk passes --worktree");
  assert.equal(runCli(t, repo, ["check", "--commit", "HEAD"]).status, 1, "the committed HEAD is still red");
  const list = runCli(t, repo, ["check", "--list"]);
  assert.equal(list.status, 0);
  assert.match(list.stdout, /^qa-platform \(G13\)$/m);
  assert.equal(list.stdout.trim().split("\n").length, 12);
});

test("hooks install is an alias of rightkit-gate install-hook, run in the app; other hooks forms still reach the ownership module", (t) => {
  const repo = app(t);
  const aliased = runCli(t, tmpDir(t, "rk-cli-elsewhere-"), ["hooks", "install", "--app", repo]);
  assert.equal(aliased.status, 0, aliased.stderr);
  assert.deepEqual(aliased.spawns, [[process.execPath, script("bin/rightkit-gate.mjs"), "install-hook"]]);
  assert.equal(path.resolve(aliased.calls[0].cwd), repo, "in the app directory");
  const forced = runCli(t, repo, ["hooks", "install", "--force"]);
  assert.deepEqual(forced.spawns, [[process.execPath, script("bin/rightkit-gate.mjs"), "install-hook", "--force"]]);
  assert.equal(path.resolve(forced.calls[0].cwd), repo);
  const noApp = runCli(t, repo, ["hooks", "install", "--app"]);
  assert.equal(noApp.status, 2);
  assert.match(noApp.stderr, /hooks install: --app needs a directory/);
  assert.deepEqual(runCli(t, repo, ["hooks"]).spawns, [[process.execPath, script("ownership-hooks.mjs")]]);
  // For real, without the spy: the pre-push hook is written and core.hooksPath set.
  const real = runCli(t, repo, ["hooks", "install"], { spy: false });
  assert.equal(real.status, 0, real.stderr);
  assert.match(real.stdout, /wrote \.githooks\/pre-push/);
  assert.equal(git(repo, "config", "--get", "core.hooksPath").trim(), ".githooks");
  assert.match(fs.readFileSync(path.join(repo, ".githooks", "pre-push"), "utf8"), /rightkit_gate check --commit HEAD/);
});

test("the help text names the check, its baseline, the commands that run it, the hooks alias, and that nothing skips it", (t) => {
  const help = runCli(t, tmpDir(t, "rk-cli-help-"), ["--help"]);
  assert.equal(help.status, 0);
  assert.match(help.stdout, /^  check \[--repo <dir>\] \[--config <file>\]/m);
  assert.match(help.stdout, /there is no flag that skips it/);
  assert.match(help.stdout, /\.rightkit\/app-check-baseline\.json \(entries carry issue, added and sunset; the sunset is at most 90 days after added\)/);
  assert.match(help.stdout, /hooks install \[--app <dir>\] \[--force\] +Alias of rightkit-gate install-hook/);
  // every command the check guards is named where the help says which ones run it (release is the compatibility alias of build, with its own line)
  const guards = /The offline app check[\s\S]*?(?=\n {2}doctor )/.exec(help.stdout)?.[0] ?? "";
  assert.ok(guards.length > 200, "read the help for check");
  for (const name of [...CHECKED_COMMANDS.keys()].filter((n) => n !== "release")) assert.match(guards, new RegExp(`(^|[ ,])${name}[ ,.]`, "i"), `${name} is named in the help for check`);
  assert.match(help.stdout, /^ {2}release +Compatibility alias for build$/m);
  assert.deepEqual(help.calls, []);
});

test("the help text for doctor names --inspect and --strict as release.mjs defines them, and that --admission does nothing", (t) => {
  const help = runCli(t, tmpDir(t, "rk-cli-help-"), ["--help"]).stdout;
  const doctor = /^  doctor \[--platform mac\|win\][^\n]*\n(?:\s{20,}[^\n]*\n)+/m.exec(help)?.[0] ?? "";
  assert.match(doctor, /^  doctor \[--platform mac\|win\] \[--gates\] \[--inspect\] \[--strict\]$/m);
  assert.match(doctor, /--gates also runs the build's pre-build source gates/);
  assert.match(doctor, /--inspect lists admission findings \(newer RightKit versions\) as information instead of refusing them/);
  assert.match(doctor, /--strict makes a row that could not run on this host \(NOT RUN\) a blocking problem/);
  assert.match(doctor, /--admission is still accepted and does nothing \(admission is the default\); it cannot be combined with --inspect/);
  // what the help says is what release.mjs does: the flags exist there, and --inspect with --admission is a usage error
  const release = fs.readFileSync(path.join(PACKAGE, "release.mjs"), "utf8");
  for (const flag of ["--inspect", "--strict", "--admission"]) assert.ok(release.includes(`arg === "${flag}"`), `${flag} is a release.mjs flag`);
  assert.match(release, /--inspect and --admission contradict each other/);
});
