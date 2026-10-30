// GB-08 (BR11/B31, GATE R-D): every production RustRight subprocess starts the installed runtime's .exe on Windows with the exact argv array,
// no shell and no visible console; a PATH that lists the `rustright.cmd` shim first never decides what starts; an executable that cannot be
// resolved fails before anything starts (so nothing is marked submitted).
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { AGENT_SHIM_MARKER, BROKER_CLIENT_UNRESOLVED, brokerClientInvocation, locateBrokerClient } from "./broker-client.mjs";
import { managedRuntimeClient } from "./managed-roots.mjs";
import { readBrokerReceipt } from "./unit-gate.mjs";
import { applyRightkitAdoption, launchOf } from "./rightkit-drift.mjs";
import { execFileSync } from "./background-process.mjs";
import { mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";

const RUNTIME_EXE = "D:\\.rustright\\runtime\\rustright.exe";
const SHIM_EXE = "E:\\stale\\rustright.exe";
const CMD = "C:\\Users\\a\\agent-bin\\rustright.cmd";
const SHIM = `@echo off\r\nrem ${AGENT_SHIM_MARKER}\r\nsetlocal\r\nset "RUSTRIGHT_MANAGED_AGENT_SHIM="\r\nset "RIGHTKIT_MANAGED_AGENT_SHIM="\r\n"${SHIM_EXE}" %*\r\nexit /b %ERRORLEVEL%\r\n`;
const env = { RIGHTKIT_CONTROL_ROOT: "D:\\rightkit-control" };
const exists = (p) => p === RUNTIME_EXE;
const installed = (e, platform) => managedRuntimeClient(e, platform, exists);
const wherePathCmdFirst = (file) => (file === "where" ? { status: 0, stdout: `${CMD}\r\n`, stderr: "" } : null);

test("[GB-08] the installed runtime .exe wins over a PATH that lists the .cmd shim first; PATH is consulted only without an installed runtime", () => {
  assert.equal(managedRuntimeClient(env, "win32", exists), RUNTIME_EXE, "the runtime manifest location (<drive>\\.rustright\\runtime) is found");
  let probed = 0;
  const probe = (file, args) => { probed += 1; return wherePathCmdFirst(file, args); };
  assert.equal(locateBrokerClient({ platform: "win32", env, probe, installed }), RUNTIME_EXE);
  assert.equal(probed, 0, "PATH was not even asked");
  assert.equal(locateBrokerClient({ platform: "win32", env, probe, installed: () => undefined }), CMD, "no installed runtime: PATH, as before");
  const inv = brokerClientInvocation("rustright", ["status", "id-1"], { cwd: "C:\\w", env }, { platform: "win32", probe, installed });
  assert.equal(inv.command, RUNTIME_EXE);
  assert.deepEqual(inv.args, ["status", "id-1"], "exact argv array");
});

test("[GB-08] unit receipt read starts the installed .exe with the exact argv, shell false, hidden window; a missing client is typed and starts nothing", () => {
  const calls = [];
  const signed = JSON.stringify({ receipt: { id: "job-7", provenance: { payload: "p", signature: "s" } } });
  const run = (file, args, options) => { calls.push({ file, args, options }); return file === "where" ? wherePathCmdFirst(file) : { status: 0, stdout: signed, stderr: "" }; };
  const got = readBrokerReceipt("job-7", { run, platform: "win32", env, installed, readFile: () => SHIM });
  assert.equal(got.id, "job-7");
  assert.deepEqual(calls.map((c) => c.file), [RUNTIME_EXE], "only the installed exe was started; no `where`, no .cmd");
  assert.deepEqual(calls[0].args, ["status", "job-7"]);
  assert.equal(calls[0].options.shell, false);
  assert.equal(calls[0].options.windowsHide, true);
  // A PATH shim of a foreign shape and no installed runtime: typed refusal, nothing started.
  const started = [];
  const run2 = (file, args) => { started.push(file); return file === "where" ? wherePathCmdFirst(file) : { status: 0, stdout: signed, stderr: "" }; };
  assert.throws(() => readBrokerReceipt("job-7", { run: run2, platform: "win32", env, installed: () => undefined, readFile: () => "@echo off\r\nrustright.exe %*\r\n" }), /rustright status failed/);
  assert.deepEqual(started, ["where"], "the unresolvable shim was never started");
});

test("[GB-08] adoption refresh (rightkit-drift, the SessionStart hook's copy too) starts rustright.exe with shell false on Windows and refuses an unresolvable client before starting it", async (t) => {
  const root = await mkdtemp(path.join(os.tmpdir(), "rk-gb08-"));
  t.after(() => rm(root, { recursive: true, force: true }));
  await mkdir(path.join(root, "src-tauri"), { recursive: true });
  await writeFile(path.join(root, "src-tauri", "Cargo.toml"), '[dependencies]\nrightkit-db = "=0.1.0"\n');
  execFileSync("git", ["-C", root, "init", "-q"], { stdio: "ignore" }); execFileSync("git", ["-C", root, "add", "."], { stdio: "ignore" });
  const versions = { npm: {}, stagedNpm: {}, cargo: { "rightkit-db": "0.2.1" }, stagedCargo: { "rightkit-db": "0.2.1" } };
  const calls = [];
  const spawn = (file, args, options) => { calls.push({ file, args, options }); return file === "where" ? { status: 0, stdout: `${CMD}\r\n`, stderr: "" } : { status: 0, stdout: "", stderr: "" }; };
  applyRightkitAdoption(root, versions, { includeBreaking: true, runOptions: { platform: "win32", spawn, readFile: () => SHIM } });
  const started = calls.filter((c) => c.file !== "where");
  assert.equal(started.length, 1);
  assert.equal(started[0].file, SHIM_EXE, "the shim's target .exe, never rustright.cmd or the bare name");
  assert.deepEqual(started[0].args, ["cargo", "metadata", "--format-version", "1", "--manifest-path", path.join(root, "src-tauri", "Cargo.toml")]);
  assert.equal(started[0].options.shell, false);
  assert.equal(started[0].options.windowsHide, true);
  assert.deepEqual(launchOf("pnpm", ["install"], { platform: "win32" }), { command: "pnpm", args: ["install"], shell: true }, "a non-RustRight command keeps its launch");
  assert.deepEqual(launchOf("rustright", ["x"], { platform: "linux" }), { command: "rustright", args: ["x"], shell: false });
  assert.throws(() => launchOf("rustright", ["x"], { platform: "win32", spawn: () => ({ status: 0, stdout: `${CMD}\r\n` }), readFile: () => "@echo off\r\nrustright.exe %*\r\n" }), /RK_BROKER_CLIENT_UNRESOLVED/);
  assert.equal(BROKER_CLIENT_UNRESOLVED, "RK_BROKER_CLIENT_UNRESOLVED");
  const policyCopy = fs.readFileSync(new URL("../build-control-policy/src/rightkit-drift.mjs", import.meta.url), "utf8");
  assert.equal(policyCopy, fs.readFileSync(new URL("./rightkit-drift.mjs", import.meta.url), "utf8"), "the hook's byte copy carries the same launcher");
});
