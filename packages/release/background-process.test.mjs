import assert from "node:assert/strict";
import childProcess from "node:child_process";
import { mkdtempSync, readdirSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { promisify } from "node:util";
import test from "node:test";
import * as background from "./background-process.mjs";

test("release sources & tests cannot bypass hidden child launch policy", () => {
  const violations = [];
  function visit(directory) {
    for (const entry of readdirSync(directory, { withFileTypes: true })) {
      if (entry.name === "node_modules" || entry.name.startsWith(".")) continue;
      const file = path.join(directory, entry.name);
      if (entry.isDirectory()) visit(file);
      else if (entry.name.endsWith(".mjs") && !["background-process.mjs", "background-process.test.mjs"].includes(entry.name)) {
        if (/["'](?:node:)?child_process["']/.test(readFileSync(file, "utf8"))) violations.push(path.relative(import.meta.dirname, file));
      }
    }
  }
  visit(import.meta.dirname);
  assert.deepEqual(violations, [], "Use background-process.mjs for every release subprocess");
});

for (const name of ["spawn", "spawnSync", "execFileSync", "execFile"]) {
  test(`${name} hides children even with explicit false & preserves launch options`, (t) => {
    const sentinel = {};
    const mocked = t.mock.method(childProcess, name, () => sentinel);
    const options = { windowsHide: false, cwd: "fixture", env: { TEST: "1" }, timeout: 321, encoding: "utf8", shell: true, stdio: "pipe" };
    const args = ["space argument", "", "ü·中"];
    const callback = () => {};
    assert.equal(background[name]("fixture", args, options, callback), sentinel);
    const call = mocked.mock.calls[0].arguments;
    assert.equal(call[0], "fixture");
    assert.equal(call[1], args);
    assert.deepEqual(call[2], { ...options, windowsHide: true });
    if (name === "execFile") assert.equal(call[3], callback);
    assert.equal(options.windowsHide, false, "caller options must not be mutated");
    background[name]("fixture", options, callback);
    assert.deepEqual(mocked.mock.calls[1].arguments.slice(1, 3), [[], { ...options, windowsHide: true }]);
    background[name]("fixture");
    assert.deepEqual(mocked.mock.calls[2].arguments.slice(1, 3), [[], { windowsHide: true }]);
    background[name]("fixture", undefined, options);
    assert.deepEqual(mocked.mock.calls[3].arguments.slice(1, 3), [[], { ...options, windowsHide: true }]);
  });
}

test("execFile preserves callback-only overload", (t) => {
  const mocked = t.mock.method(childProcess, "execFile", () => ({}));
  const callback = () => {};
  background.execFile("fixture", callback);
  assert.deepEqual(mocked.mock.calls[0].arguments, ["fixture", [], { windowsHide: true }, callback]);
});

test("Windows PowerShell environment removes PSModulePath case-insensitively without mutating inputs", () => {
  const base = { Path: "base", pSmOdUlEpAtH: "legacy", RK_TRUST_DIR: "old", Keep: "yes" };
  const overrides = { PATH: "override", RK_TRUST_DIR: "new", Extra: "value" };
  assert.deepEqual(background.windowsPowerShellEnvironment(overrides, base), {
    Path: "base", PATH: "override", RK_TRUST_DIR: "new", Keep: "yes", Extra: "value",
  });
  assert.deepEqual(base, { Path: "base", pSmOdUlEpAtH: "legacy", RK_TRUST_DIR: "old", Keep: "yes" });
  assert.deepEqual(overrides, { PATH: "override", RK_TRUST_DIR: "new", Extra: "value" });
});

test("real subprocesses retain Unicode output, exit codes & promisified result", async () => {
  const code = "process.stdout.write('ü·中'); process.stderr.write('diagnostic')";
  const result = background.spawnSync(process.execPath, ["-e", code], { encoding: "utf8" });
  assert.equal(result.status, 0);
  assert.equal(result.stdout, "ü·中");
  assert.equal(result.stderr, "diagnostic");
  assert.equal(background.spawnSync(process.execPath, ["-e", "process.exit(7)"]).status, 7);
  const pending = promisify(background.execFile)(process.execPath, ["-e", code], { encoding: "utf8" });
  assert.ok(pending.child.pid);
  assert.deepEqual(await pending, { stdout: "ü·中", stderr: "diagnostic" });
});

test("Windows child console is invisible through sync & async launchers", { skip: process.platform !== "win32" }, async () => {
  const script = `Add-Type -TypeDefinition 'using System; using System.Runtime.InteropServices; public class ConsoleProbe { [DllImport("kernel32.dll")] public static extern IntPtr GetConsoleWindow(); [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr hwnd); }'; if ([ConsoleProbe]::IsWindowVisible([ConsoleProbe]::GetConsoleWindow())) { exit 9 }; Write-Output 'hidden'`;
  const args = ["-NoLogo", "-NoProfile", "-NonInteractive", "-EncodedCommand", Buffer.from(script, "utf16le").toString("base64")];
  assert.match(background.execFileSync("powershell.exe", args, { encoding: "utf8" }), /hidden/);
  assert.equal(background.spawnSync("powershell.exe", args).status, 0);
  const child = background.spawn("powershell.exe", args, { stdio: "pipe" });
  await new Promise((resolve, reject) => {
    child.once("error", reject);
    child.once("close", (code) => { try { assert.equal(code, 0); resolve(); } catch (error) { reject(error); } });
  });
  assert.match((await promisify(background.execFile)("powershell.exe", args)).stdout, /hidden/);
});

test("Windows PowerShell Get-Acl works through sanitized launch", { skip: process.platform !== "win32" }, () => {
  const directory = mkdtempSync(path.join(tmpdir(), "rk-acl-module-path-"));
  try {
    const script = "$ErrorActionPreference='Stop'; Import-Module Microsoft.PowerShell.Security; $a=Get-Acl -LiteralPath $env:RK_ACL_PROBE_TARGET; $a.GetOwner([Security.Principal.SecurityIdentifier]).Value";
    const args = ["-NoProfile", "-NonInteractive", "-Command", script];
    const sanitized = background.spawnSync("C:\\Windows\\System32\\WindowsPowerShell\\v1.0\\powershell.exe", args, {
      encoding: "utf8", windowsHide: true,
      env: background.windowsPowerShellEnvironment({ RK_ACL_PROBE_TARGET: directory }),
    });
    assert.equal(sanitized.status, 0, sanitized.stderr);
    assert.match(sanitized.stdout, /^S-1-\d+(?:-\d+)+\s*$/);
  } finally { rmSync(directory, { recursive: true, force: true }); }
});
