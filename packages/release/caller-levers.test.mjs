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
test("F4: a check that skipped executing scans says PARTIAL and never prints a full pass line; it exits 0 for its callers (rightkit#1695)", () => {
  const gate = fs.readFileSync(path.join(here, "bin", "rightkit-gate.mjs"), "utf8");
  assert.match(gate, /if \(notRun\.length\) o\.out\(.*PARTIAL: .*executing check\(s\) did not run/);
  assert.doesNotMatch(gate, /PARTIAL: \$\{notRun\.length\}[^\n]*return 3;/);
  // The content-only branch prints "class A content pass", never the full/scoped pass line publish admission requires.
  const branch = gate.slice(gate.indexOf("if (classAOnly) {\n      // `check` judged the content only"), gate.indexOf("if (!recordHistory()) return 1;\n    o.out(`${tag} pass ("));
  assert.ok(branch.length > 0); assert.doesNotMatch(branch, /class A \$\{plan\.scopeDigest \? "scoped" : "full"\} pass/);
});
test("a run from the command line has every default file function (0.6.68 lost readFile, writeFile, mkdir, chmod and readdir to a comment)", async () => {
  const fs2 = await import("node:fs"); const os2 = await import("node:os"); const path2 = await import("node:path");
  const { main } = await import("./bin/rightkit-gate.mjs");
  const dir = fs2.mkdtempSync(path2.join(os2.tmpdir(), "rk-units-")); const file = path2.join(dir, "units.json"); fs2.writeFileSync(file, JSON.stringify({ units: ["cargo:rightkit-time"] }));
  const errs = []; const spawn = (c, a) => ({ status: c === "git" && a[1] === "--show-toplevel" ? 0 : 1, stdout: c === "git" && a[1] === "--show-toplevel" ? `${dir}\n` : "", stderr: "stub" });
  await main(["--to", "check", "--units-file", file, "--commit", "0".repeat(40)], { cwd: dir, out: () => {}, err: (s) => errs.push(String(s)), spawn });
  assert.equal(errs.filter((e) => /is not a function/.test(e)).length, 0, errs.join("\n"));
  const src = fs2.readFileSync(new URL("./bin/rightkit-gate.mjs", import.meta.url), "utf8");
  assert.match(src, /spawn: defaultSpawn, exists: fs\.existsSync, readFile: \(f\) => fs\.readFileSync\(f, "utf8"\), writeFile: fs\.writeFileSync, mkdir: fs\.mkdirSync, chmod: fs\.chmodSync, readdir: fs\.readdirSync,/);
  fs2.rmSync(dir, { recursive: true, force: true });
});
