import test from "node:test";
import assert from "node:assert/strict";
import path from "node:path";
import { pnpmPinOf, pnpmPinProblem, PNPM_PIN_REFUSAL } from "./bin/rightkit-gate.mjs";
import { compileLaunch } from "./class-a-launch-guard.mjs";

// Windows-shaped fixture (rightkit#1329, HeardRight 9d22cd7c on the Dell): global pnpm 11.24.0, packageManager pnpm@12.16.1.
const WIN_ROOT = "D:\\snap\\hr";
const pkg = (pm) => JSON.stringify({ name: "x", ...(pm ? { packageManager: pm } : {}) });
const fixture = (pins, onPath, calls = []) => ({
  root: WIN_ROOT,
  files: Object.keys(pins),
  readFile: (f) => pins[path.relative(WIN_ROOT, f).split(path.sep).join("/")] ?? pkg(null),
  spawn: (cmd, args, opts) => { calls.push({ cmd, args, cwd: opts?.cwd }); return onPath === null ? { status: 1, stdout: "", stderr: "boom" } : { status: 0, stdout: `${onPath}\r\n`, stderr: "" }; },
  neutralCwd: "C:\\Users\\x\\AppData\\Local\\Temp",
});

test("pnpmPinOf reads the exact pnpm pin, with or without an integrity suffix, and nothing else", () => {
  assert.equal(pnpmPinOf(pkg("pnpm@12.16.1+sha512.abc")), "12.16.1");
  assert.equal(pnpmPinOf(pkg("pnpm@12.16.1")), "12.16.1");
  assert.equal(pnpmPinOf(pkg("yarn@4.0.0")), null);
  assert.equal(pnpmPinOf(pkg(null)), null);
  assert.equal(pnpmPinOf("{not json"), null);
});

test("a PATH pnpm older than the packageManager pin fails typed, naming both versions, the file and the fix, before any class A row", () => {
  const calls = [];
  const p = pnpmPinProblem(fixture({ "package.json": pkg("pnpm@12.16.1+sha512.abc") }, "11.24.0", calls));
  assert.equal(p.code, PNPM_PIN_REFUSAL);
  assert.match(p.message, /RK_PNPM_VERSION_MISMATCH/);
  assert.match(p.message, /pnpm on PATH is 11\.24\.0/);
  assert.match(p.message, /package\.json pins packageManager pnpm@12\.16\.1/);
  assert.match(p.message, /pnpm self-update 12\.16\.1/);
  assert.equal(calls.length, 1);
  assert.equal(calls[0].cwd, "C:\\Users\\x\\AppData\\Local\\Temp", "the probe runs outside the repository, so pnpm cannot switch versions while answering");
});

test("a matching PATH pnpm, no pnpm pin, or any nested mismatch", () => {
  assert.equal(pnpmPinProblem(fixture({ "package.json": pkg("pnpm@12.16.1") }, "12.16.1")), null);
  const calls = [];
  assert.equal(pnpmPinProblem(fixture({ "package.json": pkg(null) }, "11.24.0", calls)), null);
  assert.equal(calls.length, 0, "no pin: pnpm is not probed");
  const nested = pnpmPinProblem(fixture({ "package.json": pkg("pnpm@12.16.1"), "apps/a/package.json": pkg("pnpm@11.24.0") }, "12.16.1"));
  assert.match(nested.message, /apps\/a\/package\.json pins packageManager pnpm@11\.24\.0/);
});

test("an unreadable PATH pnpm fails typed, not as a launch refusal", () => {
  const p = pnpmPinProblem(fixture({ "package.json": pkg("pnpm@12.16.1") }, null));
  assert.equal(p.code, PNPM_PIN_REFUSAL);
  assert.match(p.message, /cannot read the version of the pnpm on PATH/);
});

test("the launch guard still refuses pnpm's own cmd relaunch: the early check admits nothing", () => {
  const why = compileLaunch("cmd", ["/d", "/s", "/c", "\"C:\\Users\\x\\pnpm\\.tools\\pnpm\\12.16.1\\bin\\pnpm.cmd ^\"exec^\" ^\"tsc^\"\""]);
  assert.match(String(why), /cmd caret escape/);
});
