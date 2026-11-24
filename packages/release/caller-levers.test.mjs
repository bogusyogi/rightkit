// Step F of the Dell sequence (status file 11.7; settled with Gemini, 2026-11-24; rightkit#1514): what a caller of the gate may not choose.
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { PROTECTED_RECORD_REF, callerRefusal, genuineGithubRun, ignoredCallerVariable } from "./bin/rightkit-gate.mjs";

const here = path.dirname(fileURLToPath(import.meta.url));
const none = () => false; const broker = (p) => /rustright[\\/]runtime$/.test(p);
const gh = { GITHUB_ACTIONS: "true", GITHUB_RUN_ID: "1", RUNNER_OS: "Windows" };

test("F5, F6: the working folder and the gate's own self-call flags are refused from a caller, allowed to the gate itself", () => {
  for (const args of [["check", "--worktree"], ["--class-a", "--commit", "x"], ["--all", "--commit", "x"], ["check", "--snapshot", "D:\\s"]]) {
    assert.match(callerRefusal({ args, exists: none }), /refused|own call/);
    assert.equal(callerRefusal({ args, exists: none, selfCall: true }), null);
  }
  // the git pre-push route and ordinary stages are untouched
  assert.equal(callerRefusal({ args: [], exists: none }), null);
  assert.equal(callerRefusal({ args: ["--to", "compile", "--commit", "x"], exists: none }), null);
});
test("F3, F7: GITHUB_ACTIONS and --receipt-in only inside a genuine GitHub run, never on a broker host", () => {
  assert.equal(genuineGithubRun({ env: gh, platform: "win32", exists: none }), true);
  assert.equal(genuineGithubRun({ env: { GITHUB_ACTIONS: "true" }, platform: "win32", exists: none }), false, "one variable set by hand is not GitHub");
  assert.equal(genuineGithubRun({ env: gh, platform: "win32", exists: broker }), false, "a host with the managed broker is not a GitHub runner");
  assert.match(callerRefusal({ args: ["--to", "compile"], env: { GITHUB_ACTIONS: "true" }, exists: none }), /GITHUB_ACTIONS=true is refused/);
  assert.match(callerRefusal({ args: ["--to", "release", "--receipt-in", "r.json"], env: {}, exists: none }), /--receipt-in is refused/);
  assert.equal(callerRefusal({ args: ["--to", "release", "--receipt-in", "r.json"], env: gh, platform: "linux", exists: none }), null, "the publisher job keeps it");
});
test("F1 and step F: roots are dropped on Mac and Windows, the scratch root and two guards everywhere", () => {
  for (const n of ["RIGHTKIT_DATA_ROOT", "RUSTRIGHT_DATA_ROOT", "RIGHTKIT_MANAGED_ROOT", "RIGHTKIT_EXTERNAL_ROOT", "RUSTRIGHT_GATE_ROOT", "RUSTRIGHT_CONTROL_ROOT", "RUSTRIGHT_RUNTIME_ROOT"]) {
    assert.equal(ignoredCallerVariable(n, "win32"), true, n); assert.equal(ignoredCallerVariable(n, "darwin"), true, n);
    assert.equal(ignoredCallerVariable(n, "linux"), false, `${n} stays on Linux, where a declared broker host sets it`);
  }
  for (const n of ["RIGHTKIT_SCRATCH_ROOT", "RIGHTKIT_STAGE_SIDECAR_ALLOW_NESTED", "RUSTRIGHT_OPERATION_DEADLINE_MS"]) assert.equal(ignoredCallerVariable(n, "linux"), true, n);
  assert.equal(ignoredCallerVariable("RIGHTKIT_BUILD_BROKER_SOCKET", "win32"), false);
});
test("F2: exception records are read from the protected branch, never from the selected commit", () => {
  const gate = fs.readFileSync(path.join(here, "bin", "rightkit-gate.mjs"), "utf8");
  assert.equal(PROTECTED_RECORD_REF, "refs/remotes/origin/main");
  assert.doesNotMatch(gate, /git\("show", `\$\{pinned\.commit\}:\$\{(ALL_UNITS_EXCEPTIONS_FILE|BROKER_EXCEPTIONS_FILE)\}`\)/);
  assert.match(gate, /\$\{PROTECTED_RECORD_REF\}:\$\{BROKER_EXCEPTIONS_FILE\}/); assert.match(gate, /\$\{PROTECTED_RECORD_REF\}:\$\{ALL_UNITS_EXCEPTIONS_FILE\}/);
});
test("F4: a check that skipped executing scans exits 3, never 0", () => {
  const gate = fs.readFileSync(path.join(here, "bin", "rightkit-gate.mjs"), "utf8");
  assert.match(gate, /if \(notRun\.length\) \{ o\.out\(.*PARTIAL.*return 3; \}/);
});
