#!/usr/bin/env node
// `right-release session-run [--state-dir <dir>] [--timeout-ms <n>] -- <command> [args...]` (rightkit#816, #428): run one command in the signing user's
// interactive Windows logon session and return its exit code and output.
//
// Why: the Azure signer (sign-windows.mjs) needs the token cache of the logon session (sign-windows.mjs names it: AzureCliCredential finds a token only
// in the desktop session that signed in). A command started from an SSH shell, a service or an agent shell runs in another session and finds none. The
// way into the logon session without a prompt is a ONE-SHOT scheduled task registered for the current user with `/IT` (interactive only): it starts,
// runs the command hidden, and is deleted again. Nothing stays registered, no credential is passed or stored, and nothing runs with elevated rights
// (`/RL LIMITED`).
//
// The command and its arguments travel as JSON in the state folder and are read by a fixed PowerShell wrapper that calls the program with an argument
// array, so there is no shell quoting of a user string anywhere (no injection through an argument). The wrapper writes `exit.txt` last; the runner
// polls for it, copies the log to stdout, removes the task and the state folder, and exits with the command's code.
import { existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { pathToFileURL } from "node:url";
import { spawnSync } from "./background-process.mjs";

export class SessionRunError extends Error {
  constructor(message) { super(message); this.name = "SessionRunError"; this.code = "RK_SESSION_RUN"; }
}
const fail = (message) => { throw new SessionRunError(message); };

export const SESSION_WRAPPER = [
  "$ErrorActionPreference = 'Stop'",
  "$dir = Split-Path -Parent $MyInvocation.MyCommand.Path",
  "$job = Get-Content -Raw -LiteralPath (Join-Path $dir 'job.json') | ConvertFrom-Json",
  "$code = 1",
  "try {",
  "  $argList = @(); if ($job.args) { $argList = @($job.args) }",
  "  & $job.command @argList *> (Join-Path $dir 'output.log')",
  "  if ($null -ne $LASTEXITCODE) { $code = $LASTEXITCODE } else { $code = 0 }",
  "} catch { $_ | Out-String | Add-Content -LiteralPath (Join-Path $dir 'output.log'); $code = 1 }",
  "Set-Content -LiteralPath (Join-Path $dir 'exit.txt') -Value $code -NoNewline",
  "",
].join("\r\n");

export function parseSessionRunArgs(argv) {
  const opts = { stateDir: null, timeoutMs: 30 * 60 * 1000, command: [] };
  for (let i = 0; i < argv.length; i += 1) {
    const flag = argv[i];
    if (flag === "--") { opts.command = argv.slice(i + 1); break; }
    if (flag === "--state-dir" || flag === "--timeout-ms") {
      const value = argv[i + 1];
      if (value === undefined || value.startsWith("--")) fail(`${flag} needs a value`);
      i += 1;
      if (flag === "--state-dir") opts.stateDir = value;
      else { opts.timeoutMs = Number(value); if (!Number.isInteger(opts.timeoutMs) || opts.timeoutMs < 1000) fail("--timeout-ms must be an integer of at least 1000"); }
    } else fail(`unknown argument: ${flag} (put the command after --)`);
  }
  if (!opts.command.length || !opts.command[0]) fail("a command is required after --");
  return opts;
}

/** The one-shot task's `/TR` text: a hidden PowerShell running the fixed wrapper. The wrapper path never holds a user string. */
export function sessionTaskAction(wrapperPath) {
  if (/["\r\n]/.test(wrapperPath)) fail("the state folder path must not contain a quote or a line break");
  return `powershell.exe -NoProfile -NonInteractive -WindowStyle Hidden -ExecutionPolicy Bypass -File "${wrapperPath}"`;
}

export async function runInInteractiveSession(opts, deps = {}) {
  const { run = spawnSync, fs = { existsSync, mkdirSync, readFileSync, rmSync, writeFileSync }, platform = process.platform, env = process.env, user = os.userInfo().username, pid = process.pid, now = Date.now, sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms)), log = (line) => console.error(line), write = (text) => process.stdout.write(text) } = deps;
  if (platform !== "win32") fail("session-run is for Windows: it uses a one-shot scheduled task in the interactive logon session");
  const stateRoot = opts.stateDir ?? path.join(env.RIGHT_RELEASE_STATE_ROOT ?? "", "session-run");
  if (!path.isAbsolute(stateRoot)) fail("an absolute --state-dir (or RIGHT_RELEASE_STATE_ROOT) is required: the job and its output live there");
  const dir = path.join(stateRoot, `job-${pid}-${now()}`);
  const task = `RightKit-session-run-${pid}-${now()}`;
  fs.mkdirSync(dir, { recursive: true });
  const wrapper = path.join(dir, "run.ps1");
  fs.writeFileSync(wrapper, SESSION_WRAPPER);
  fs.writeFileSync(path.join(dir, "job.json"), JSON.stringify({ command: opts.command[0], args: opts.command.slice(1) }));
  const schtasks = (args) => run("schtasks.exe", args, { encoding: "utf8", windowsHide: true, shell: false });
  const must = (result, what) => { if (result?.error || result?.status !== 0) fail(`${what} failed (${result?.error?.message ?? `exit ${result?.status}`}): ${String(result?.stderr ?? result?.stdout ?? "").trim()}`); };
  let registered = false;
  try {
    must(schtasks(["/Create", "/TN", task, "/SC", "ONCE", "/ST", "23:59", "/RU", user, "/IT", "/RL", "LIMITED", "/F", "/TR", sessionTaskAction(wrapper)]), "registering the one-shot task");
    registered = true;
    must(schtasks(["/Run", "/TN", task]), "starting the one-shot task (is the user logged on to the desktop?)");
    const deadline = now() + opts.timeoutMs;
    while (!fs.existsSync(path.join(dir, "exit.txt"))) {
      if (now() > deadline) fail(`the command did not finish within ${Math.round(opts.timeoutMs / 1000)}s; the task ${task} is being removed`);
      await sleep(500);
    }
    const output = fs.existsSync(path.join(dir, "output.log")) ? String(fs.readFileSync(path.join(dir, "output.log"), "utf8")) : "";
    const code = Number.parseInt(String(fs.readFileSync(path.join(dir, "exit.txt"), "utf8")).trim(), 10);
    if (output) write(output.endsWith("\n") ? output : `${output}\n`);
    return { code: Number.isInteger(code) ? code : 1, task };
  } finally {
    if (registered) { try { schtasks(["/End", "/TN", task]); } catch { /* not running */ } try { schtasks(["/Delete", "/TN", task, "/F"]); } catch (error) { log(`session-run: could not delete task ${task} (${error.message})`); } }
    fs.rmSync(dir, { recursive: true, force: true });
  }
}

export async function main(argv = process.argv.slice(2)) {
  try {
    const result = await runInInteractiveSession(parseSessionRunArgs(argv));
    return result.code;
  } catch (error) {
    console.error(`right-release session-run: ${error.message}`);
    return error instanceof SessionRunError ? 125 : 1;
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) main().then((code) => { process.exitCode = code; });
