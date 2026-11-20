// Two more questions a gate process may remember (probe-memo.mjs), and the one it need not ask at all from a sealed install (broker-pins.mjs).
import assert from "node:assert/strict";
import path from "node:path";
import { test } from "node:test";
import { batchInputKey, pureGrep } from "./probe-memo.mjs";
import { readGateRevisionPipeline } from "./broker-pins.mjs";

const SHA = "3a017e99e2f69ab3c8cae9ca03a285b8de8cb5bf";
const C = ["-c", "safe.directory=D:/x", "-c", "core.fsmonitor=false"];

test("a grep of one full commit id is fixed by its arguments; a grep of a name, of the working tree, or with other options is not", () => {
  assert.equal(pureGrep("git", ["grep", "-z", "-l", "-I", "-e", "needle", SHA, "--"]), true);
  assert.equal(pureGrep("C:\\Program Files\\Git\\cmd\\git.exe", [...C, "grep", "-z", "-l", "-I", "-e", "needle", SHA, "--"]), true);
  assert.equal(pureGrep("git", ["grep", "-e", "needle", SHA, "--"]), true);
  for (const args of [
    ["grep", "-z", "-l", "-I", "-e", "needle", "HEAD", "--"], // a name moves
    ["grep", "-z", "-l", "-I", "-e", "needle", "--"], // the working tree
    ["grep", "-z", "-l", "-I", "-e", "needle", SHA], // no end of options
    ["grep", "-z", "-l", "-I", "-e", "needle", SHA, "--", "src"], // a path limit after it
    ["grep", "--untracked", "-e", "needle", SHA, "--"], // a long option
    ["grep", "-e", "needle", "-l", SHA, "--"], // the pattern is not last
    ["grep", "-l", SHA, "--"], // no pattern
    ["log", "-e", "needle", SHA, "--"],
  ]) assert.equal(pureGrep("git", args), false, args.join(" "));
  assert.equal(pureGrep("rg", ["grep", "-e", "needle", SHA, "--"]), false);
  assert.equal(pureGrep("git", ["grep", "-e", 7, SHA, "--"]), false);
});

test("cat-file --batch is keyed by its input only when every line of it is a full object id", () => {
  const input = `${SHA}\n${"a".repeat(40)}\n`;
  const key = batchInputKey("git", ["cat-file", "--batch"], { input });
  assert.match(key, /^[0-9a-f]{64}$/);
  assert.equal(batchInputKey("git.exe", [...C, "cat-file", "--batch"], { input: Buffer.from(input) }), key, "the same objects asked the same way are one question");
  assert.notEqual(batchInputKey("git", ["cat-file", "--batch"], { input: `${"a".repeat(40)}\n${SHA}\n` }), key, "another order is another output");
  for (const [args, options] of [
    [["cat-file", "--batch"], { input: "HEAD\n" }], // a name moves
    [["cat-file", "--batch"], { input: `${SHA}:path\n` }],
    [["cat-file", "--batch"], { input: "" }],
    [["cat-file", "--batch"], {}], // no input: nothing to key
    [["cat-file", "--batch-check"], { input }],
    [["cat-file", "--batch", "--follow-symlinks"], { input }],
    [["cat-file", "-p", SHA], { input }],
  ]) assert.equal(batchInputKey("git", args, options), null, `${args.join(" ")} ${JSON.stringify(options).slice(0, 40)}`);
  assert.equal(batchInputKey("hg", ["cat-file", "--batch"], { input }), null);
});

test("a gate installed outside any repository starts no git to learn its own revision; a gate in a checkout asks", () => {
  const sealed = path.join(path.sep, "managed", "trusted-gates", "abc", "node_modules", "@rightkit", "release", "bin", "rightkit-gate.mjs");
  const started = [];
  const spawn = (cmd, args) => { started.push([cmd, ...args].join(" ")); return { status: 128, stdout: "", stderr: "not a repository" }; };
  // No `.git` from the gate's folder up to the root, spelled or real.
  assert.equal(readGateRevisionPipeline({ gateFile: sealed, checkout: path.join(path.sep, "work"), spawn, exists: () => false, realpath: (p) => p, env: {} }), null);
  assert.deepEqual(started, []);
  // Reached through a link: the real folder is inside a checkout, so git is asked.
  const real = path.join(path.sep, "src", "rightkit", "packages", "release", "bin");
  readGateRevisionPipeline({ gateFile: sealed, checkout: path.join(path.sep, "work"), spawn, exists: (p) => p === path.join(path.sep, "src", "rightkit", ".git"), realpath: (p) => (p === path.dirname(sealed) ? real : p), env: {} });
  assert.equal(started.length, 1); assert.match(started[0], /rev-parse --show-toplevel$/);
  // A GIT_DIR in the environment can make any folder a repository: git is asked.
  started.length = 0;
  readGateRevisionPipeline({ gateFile: sealed, checkout: path.join(path.sep, "work"), spawn, exists: () => false, realpath: (p) => p, env: { GIT_DIR: "/somewhere" } });
  assert.equal(started.length, 1);
});
