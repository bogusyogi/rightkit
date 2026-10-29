// `right-release hooks`: the advisory pre-commit ownership hook is retired (docs/GATE-CONTRACT.md: commit runs no local hook check). The
// module removes the hook an earlier release wrote and writes none. item9-hooks.test.mjs holds the whole-repository cases.
import assert from "node:assert/strict";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { spawnSync } from "./background-process.mjs";
import * as hooks from "./ownership-hooks.mjs";
import { HOOK_MARKER, main, retirePreCommitHook } from "./ownership-hooks.mjs";

const FIXTURE = JSON.parse(readFileSync(new URL("./fixtures/retired-hooks.json", import.meta.url), "utf8"));

function git(cwd, args) {
  const result = spawnSync("git", ["-C", cwd, ...args], { encoding: "utf8", windowsHide: true, env: { ...process.env, GIT_TERMINAL_PROMPT: "0" } });
  assert.equal(result.status, 0, `git ${args.join(" ")}: ${result.stderr}`);
  return String(result.stdout);
}

function fixtureApp(t) {
  const dir = realpathSync(mkdtempSync(path.join(os.tmpdir(), "rk-hooks-")));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  git(dir, ["init", "-q", "."]);
  git(dir, ["config", "user.email", "fixture@example.invalid"]);
  git(dir, ["config", "user.name", "fixture"]);
  writeFileSync(path.join(dir, "README.md"), "fixture\n");
  return dir;
}
const hookFile = (app, name) => path.join(app, ".git", "hooks", name);

test("the module writes no hook: the generators are gone and the marker is the one it removes by", () => {
  assert.equal(hooks.renderPreCommitHook, undefined);
  assert.equal(hooks.installPreCommitHook, undefined);
  assert.equal(HOOK_MARKER, "rightkit-ownership-precommit v1");
  assert.ok(FIXTURE.ownershipHook.includes(HOOK_MARKER));
});

test("retirement removes the ownership hook an earlier release wrote, and a second run finds nothing", (t) => {
  const app = fixtureApp(t);
  mkdirSync(path.join(app, ".git", "hooks"), { recursive: true });
  writeFileSync(hookFile(app, "pre-commit"), FIXTURE.ownershipHook, { mode: 0o755 });
  const first = retirePreCommitHook(app);
  assert.equal(first.status, "retired");
  assert.equal(existsSync(hookFile(app, "pre-commit")), false);
  assert.equal(retirePreCommitHook(app).status, "absent");
});

test("the foreign pre-commit the ownership step preserved is restored, byte for byte, and runs on commit", (t) => {
  const app = fixtureApp(t);
  const foreign = "#!/bin/sh\necho FOREIGN >> \"$(git rev-parse --show-toplevel)/foreign.log\"\nexit 0\n";
  mkdirSync(path.join(app, ".git", "hooks"), { recursive: true });
  writeFileSync(hookFile(app, "pre-commit"), FIXTURE.ownershipHook, { mode: 0o755 });
  writeFileSync(hookFile(app, "pre-commit.rightkit-foreign"), foreign, { mode: 0o755 });
  assert.equal(retirePreCommitHook(app).status, "restored");
  assert.equal(readFileSync(hookFile(app, "pre-commit"), "utf8"), foreign);
  assert.equal(existsSync(hookFile(app, "pre-commit.rightkit-foreign")), false);
  git(app, ["add", "README.md"]);
  git(app, ["commit", "-qm", "change"]);
  assert.match(readFileSync(path.join(app, "foreign.log"), "utf8"), /FOREIGN/);
});

test("a foreign pre-commit is never touched, and the chained name alone is not a reason to move anything", (t) => {
  const app = fixtureApp(t);
  mkdirSync(path.join(app, ".git", "hooks"), { recursive: true });
  const mine = "#!/bin/sh\nexit 0\n";
  writeFileSync(hookFile(app, "pre-commit"), mine, { mode: 0o755 });
  writeFileSync(hookFile(app, "pre-commit.rightkit-foreign"), "#!/bin/sh\necho kept\n", { mode: 0o755 });
  assert.equal(retirePreCommitHook(app).status, "absent");
  assert.equal(readFileSync(hookFile(app, "pre-commit"), "utf8"), mine);
  assert.equal(readFileSync(hookFile(app, "pre-commit.rightkit-foreign"), "utf8"), "#!/bin/sh\necho kept\n");
});

test("hooks install retires and prints so; any other form prints the usage and exits 2", (t) => {
  const app = fixtureApp(t);
  mkdirSync(path.join(app, ".git", "hooks"), { recursive: true });
  writeFileSync(hookFile(app, "pre-commit"), FIXTURE.ownershipHook, { mode: 0o755 });
  const lines = [];
  assert.equal(main(["install", "--app", app], { print: (line) => lines.push(line) }), 0);
  assert.match(lines.join("\n"), /pre-commit ownership gate retired \(retired\)/);
  assert.match(lines.join("\n"), /ownership --staged` is still available by hand/);
  assert.equal(existsSync(hookFile(app, "pre-commit")), false);
  const usage = [];
  assert.equal(main([], { print: (line) => usage.push(line) }), 2);
  assert.equal(main(["--help"], { print: () => {} }), 0);
  assert.match(usage.join("\n"), /retires the ownership pre-commit hook; no hook is written/);
});
