// GC-A27 NN08: nothing is started twice in one run. Three read-only commands the gate asked two to four times a run (acceptance cases on 0.6.63,
// 2026-11-24; rightkit#1514) are answered from memory the second time: same command, arguments, folder and environment.
import assert from "node:assert/strict";
import { test } from "node:test";
import { isRememberedRead, rememberReads } from "./deterministic-scans.mjs";

const SHA = "a".repeat(40);
test("only reads whose answer cannot change in a run are remembered", () => {
  assert.equal(isRememberedRead("git", ["show", `${SHA}:package.json`]), true);
  assert.equal(isRememberedRead("C:\\Program Files\\Git\\cmd\\git.exe", ["-c", "core.fsmonitor=false", "rev-parse", "--show-toplevel"]), true);
  assert.equal(isRememberedRead("pnpm.cmd", ["--version"]), true);
  // a moving name, a write, or anything else is always asked again
  assert.equal(isRememberedRead("git", ["show", "HEAD:package.json"]), false);
  assert.equal(isRememberedRead("git", ["rev-parse", "HEAD"]), false);
  assert.equal(isRememberedRead("git", ["status", "--porcelain"]), false);
  assert.equal(isRememberedRead("pnpm", ["install"]), false);
  assert.equal(isRememberedRead("cargo", ["--version"]), false);
});
test("the same read in the same folder is started once; another folder, another environment or a failure is started again", () => {
  const calls = []; let fail = false;
  const spawn = (c, a, o) => { calls.push([c, a.join(" "), o.cwd ?? ""]); return fail ? { status: 1, stdout: "", stderr: "no" } : { status: 0, stdout: `out ${calls.length}`, stderr: "" }; };
  const run = rememberReads(spawn, { store: new Map() });
  const first = run("git", ["show", `${SHA}:package.json`], { cwd: "D:\\repo" });
  assert.equal(run("git", ["show", `${SHA}:package.json`], { cwd: "D:\\repo" }).stdout, first.stdout);
  assert.equal(calls.length, 1, "the second identical read starts nothing");
  run("git", ["show", `${SHA}:package.json`], { cwd: "D:\\other" }); assert.equal(calls.length, 2, "another folder is another read");
  run("pnpm", ["--version"], { cwd: "D:\\repo" }); run("pnpm", ["--version"], { cwd: "D:\\repo" }); assert.equal(calls.length, 3);
  run("pnpm", ["--version"], { cwd: "D:\\repo", env: { PATH: "x" } }); assert.equal(calls.length, 4, "another environment is another read");
  run("git", ["status"], { cwd: "D:\\repo" }); run("git", ["status"], { cwd: "D:\\repo" }); assert.equal(calls.length, 6, "anything else always runs");
  fail = true; run("git", ["rev-parse", "--show-toplevel"], { cwd: "D:\\x" }); run("git", ["rev-parse", "--show-toplevel"], { cwd: "D:\\x" });
  assert.equal(calls.length, 8, "a failed read is not remembered");
});
