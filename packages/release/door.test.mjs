import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { spawnSync } from "./background-process.mjs";
import { doorRefusal } from "./door.mjs";
import { RUN_ENV, endRun, startRun } from "./gate-stages.mjs";

function repo(t) {
  const dir = fs.realpathSync.native(fs.mkdtempSync(path.join(os.tmpdir(), "rk-door-")));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  const git = (...a) => { const r = spawnSync("git", a, { cwd: dir, encoding: "utf8" }); assert.equal(r.status, 0, r.stderr); return r.stdout.trim(); };
  git("init", "-q"); git("-c", "user.email=t@example.test", "-c", "user.name=t", "-c", "commit.gpgsign=false", "commit", "-q", "--allow-empty", "-m", "one");
  return { dir, git, commonDir: path.resolve(dir, git("rev-parse", "--git-common-dir")), head: git("rev-parse", "HEAD") };
}

test("a build step launched by a live door run for HEAD proceeds", (t) => {
  const r = repo(t);
  const run = startRun({ commonDir: r.commonDir, commit: r.head, tree: "t", stage: "package" });
  t.after(() => endRun(run));
  assert.equal(doorRefusal({ cwd: r.dir, env: run.env, command: "scripts/release/mac-payload.mjs" }), null);
});

test("a build step run by hand, by an ended run, or for another commit is refused", (t) => {
  const r = repo(t);
  assert.match(doorRefusal({ cwd: r.dir, env: {}, command: "scripts/release/mac-payload.mjs" }), /scripts\/release\/mac-payload\.mjs runs only through the one door/);
  const other = startRun({ commonDir: r.commonDir, commit: "0".repeat(40), tree: "t", stage: "package" });
  t.after(() => endRun(other));
  assert.match(doorRefusal({ cwd: r.dir, env: other.env }), /the door run is for 000000000000/);
  const ended = startRun({ commonDir: r.commonDir, commit: r.head, tree: "t", stage: "package" });
  t.after(() => endRun(ended));
  assert.match(doorRefusal({ cwd: r.dir, env: ended.env, alive: () => false }), /has ended/);
  assert.match(doorRefusal({ cwd: r.dir, env: { [RUN_ENV]: "f".repeat(32) } }), /names no live door run/);
});

test("outside a git checkout a build step is refused", (t) => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "rk-door-nogit-"));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  assert.match(doorRefusal({ cwd: dir, env: {}, command: "step" }), /step runs only in a git checkout/);
});
