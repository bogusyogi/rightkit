import assert from "node:assert/strict";
import { chmodSync, existsSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, statSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { spawnSync } from "./background-process.mjs";
import { HOOK_MARKER, installPreCommitHook, main, renderPreCommitHook } from "./ownership-hooks.mjs";

function git(cwd, args) {
  const result = spawnSync("git", ["-C", cwd, ...args], { encoding: "utf8", windowsHide: true, env: { ...process.env, GIT_TERMINAL_PROMPT: "0" } });
  assert.equal(result.status, 0, `git ${args.join(" ")}: ${result.stderr}`);
  return String(result.stdout);
}

// A fixture app with a pinned @rightkit/release. The gate stub records its arguments and exits with GATE_EXIT.
function appFixture(t, { pinned = true, gateExit = 0 } = {}) {
  const dir = realpathSync(mkdtempSync(path.join(os.tmpdir(), "rk-hooks-")));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  git(dir, ["init", "-q", "."]);
  git(dir, ["config", "user.email", "fixture@example.invalid"]);
  git(dir, ["config", "user.name", "fixture"]);
  writeFileSync(path.join(dir, "README.md"), "fixture\n");
  if (pinned) {
    const cli = path.join(dir, "node_modules", "@rightkit", "release", "cli");
    mkdirSync(cli, { recursive: true });
    writeFileSync(path.join(cli, "right-release.mjs"), `console.log("GATE " + process.argv.slice(2).join(" ")); process.exit(${gateExit});\n`);
  }
  return dir;
}

test("install writes an executable pre-commit that runs the pinned gate, and reinstall is a no-op", (t) => {
  const app = appFixture(t);
  const first = installPreCommitHook(app);
  assert.equal(first.status, "installed");
  const hook = path.join(app, ".git", "hooks", "pre-commit");
  assert.match(readFileSync(hook, "utf8"), new RegExp(HOOK_MARKER));
  assert.ok(statSync(hook).mode & 0o100, "hook must be executable");
  assert.equal(installPreCommitHook(app).status, "unchanged");
});

test("install refuses when @rightkit/release is not pinned in the repository", (t) => {
  const app = appFixture(t, { pinned: false });
  assert.throws(() => installPreCommitHook(app), /@rightkit\/release is not installed/);
  assert.equal(existsSync(path.join(app, ".git", "hooks", "pre-commit")), false);
});

test("a foreign pre-commit is preserved as a chained hook and runs first", (t) => {
  const app = appFixture(t);
  const hook = path.join(app, ".git", "hooks", "pre-commit");
  writeFileSync(hook, "#!/bin/sh\necho FOREIGN >> \"$(git rev-parse --show-toplevel)/foreign.log\"\nexit 0\n", { mode: 0o755 });
  chmodSync(hook, 0o755);
  const result = installPreCommitHook(app);
  assert.equal(result.status, "chained");
  const foreign = path.join(app, ".git", "hooks", "pre-commit.rightkit-foreign");
  assert.match(readFileSync(foreign, "utf8"), /FOREIGN/);
  assert.equal(installPreCommitHook(app).status, "unchanged");
  writeFileSync(path.join(app, "README.md"), "changed\n");
  git(app, ["add", "README.md"]);
  git(app, ["commit", "-qm", "change"]);
  assert.match(readFileSync(path.join(app, "foreign.log"), "utf8"), /FOREIGN/);
});

test("a foreign hook that fails blocks the commit before the gate runs", (t) => {
  const app = appFixture(t);
  const hook = path.join(app, ".git", "hooks", "pre-commit");
  writeFileSync(hook, "#!/bin/sh\nexit 3\n", { mode: 0o755 });
  installPreCommitHook(app);
  writeFileSync(path.join(app, "README.md"), "changed\n");
  git(app, ["add", "README.md"]);
  const result = spawnSync("git", ["-C", app, "commit", "-qm", "blocked"], { encoding: "utf8", windowsHide: true });
  assert.notEqual(result.status, 0);
  assert.doesNotMatch(String(result.stdout) + String(result.stderr), /GATE/);
});

test("git commit runs the installed gate, and a failing gate stops the commit", (t) => {
  const app = appFixture(t, { gateExit: 1 });
  installPreCommitHook(app);
  writeFileSync(path.join(app, "README.md"), "changed\n");
  git(app, ["add", "README.md"]);
  const result = spawnSync("git", ["-C", app, "commit", "-qm", "gated"], { encoding: "utf8", windowsHide: true });
  assert.notEqual(result.status, 0);
  // git forwards hook output to its stderr.
  assert.match(String(result.stdout) + String(result.stderr), /GATE ownership --staged/);
});

test("hook template fails with an install hint when the pinned package disappears", (t) => {
  const app = appFixture(t);
  installPreCommitHook(app);
  rmSync(path.join(app, "node_modules"), { recursive: true, force: true });
  writeFileSync(path.join(app, "README.md"), "changed\n");
  git(app, ["add", "README.md"]);
  const result = spawnSync("git", ["-C", app, "commit", "-qm", "missing"], { encoding: "utf8", windowsHide: true });
  assert.notEqual(result.status, 0);
  assert.match(String(result.stderr), /@rightkit\/release is not installed/);
});

test("the rendered template names the advisory scope and the --staged entry", () => {
  const body = renderPreCommitHook();
  assert.match(body, /ownership --staged/);
  assert.match(body, /--no-verify bypasses this gate/);
  assert.doesNotMatch(body, /no-verify-proof/);
});

test("hooks CLI reports usage and install results", (t) => {
  const app = appFixture(t);
  const printed = [];
  assert.equal(main(["install", "--app", app], { print: (line) => printed.push(line) }), 0);
  assert.match(printed.join("\n"), /pre-commit ownership gate installed/);
  assert.equal(main([], { print: () => {} }), 2);
});
