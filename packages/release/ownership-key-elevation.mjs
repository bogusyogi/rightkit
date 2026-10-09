// Launch helpers for the protected ownership trust update. These only change HOW the
// privileged step is authorized or started; the verified trust child itself is unchanged.
import { mkdtempSync, readFileSync, rmSync, writeFileSync, existsSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { randomBytes } from "node:crypto";
import { windowsPowerShellEnvironment } from "./background-process.mjs";

const POWERSHELL = "C:\\Windows\\System32\\WindowsPowerShell\\v1.0\\powershell.exe";
const SCHTASKS = "C:\\Windows\\System32\\schtasks.exe";
export const DESKTOP_DELEGATED_ENV = "RK_OWNERSHIP_DESKTOP_DELEGATED";
export const DEFAULT_ELEVATION_LABEL = "RightKit ownership trust update";

// ---- macOS: system Authorization Services prompt via osascript -----------------------
// `do shell script ... with administrator privileges` shows the native auth dialog; the
// password goes to Authorization Services only and never transits this process or a script.
export function hasTerminal(stdin = process.stdin, stdout = process.stdout) {
  return stdin?.isTTY === true && stdout?.isTTY === true;
}

/** Strict POSIX single-quote escaping: the result is one shell word that equals `value` exactly. */
export function shellQuote(value) {
  const text = String(value);
  if (text.includes("\0")) throw new Error("shell argument contains a NUL byte");
  return `'${text.replaceAll("'", "'\\''")}'`;
}

/** Escape text for an AppleScript double-quoted string literal. */
export function appleScriptString(value) {
  const text = String(value);
  if (/[\r\n\0]/.test(text)) throw new Error("AppleScript string contains a line break or NUL byte");
  return `"${text.replaceAll("\\", "\\\\").replaceAll('"', '\\"')}"`;
}

export function buildMacShellCommand(nodeExe, args) {
  return ["/usr/bin/env", "-u", "NODE_OPTIONS", "-u", "NODE_PATH", nodeExe, ...args].map(shellQuote).join(" ");
}

/** Choose the macOS launch. TTY keeps the unchanged sudo prompt; otherwise the native admin dialog via osascript. */
export function buildMacInvocation({ tty, nodeExe, args, label = DEFAULT_ELEVATION_LABEL, env = process.env }) {
  const baseEnv = { ...env, NODE_OPTIONS: "", NODE_PATH: "" };
  if (tty) return { mode: "tty", command: "/usr/bin/sudo", args: ["-k", nodeExe, ...args], options: { stdio: "inherit", env: baseEnv } };
  const script = `do shell script ${appleScriptString(buildMacShellCommand(nodeExe, args))} with administrator privileges with prompt ${appleScriptString(label)}`;
  return { mode: "dialog", command: "/usr/bin/osascript", args: ["-e", script], options: { stdio: ["ignore", "inherit", "inherit"], env: baseEnv } };
}

// ---- Windows: hand a non-interactive/elevated run to the logged-on desktop session -
export function isSshSession(env = process.env) {
  return Boolean(env.SSH_CONNECTION || env.SSH_CLIENT || env.SSH_TTY);
}

export function windowsProcessIsElevated(run) {
  const r = run(POWERSHELL, ["-NoProfile", "-NonInteractive", "-Command", "$p=New-Object Security.Principal.WindowsPrincipal([Security.Principal.WindowsIdentity]::GetCurrent()); if (!$p.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) { exit 9 }"], { env: windowsPowerShellEnvironment() });
  return r.status === 0;
}

export function needsDesktopDelegation({ hostPlatform = process.platform, env = process.env, run, elevatedCheck = windowsProcessIsElevated }) {
  if (hostPlatform !== "win32" || env[DESKTOP_DELEGATED_ENV] === "1") return false;
  return isSshSession(env) || elevatedCheck(run) === true;
}

const psq = (value) => `'${String(value).replaceAll("'", "''")}'`;

export function buildDesktopWrapper(command, logPath, codePath) {
  const invocation = command.map(psq).join(" ");
  return [
    "$ErrorActionPreference='Continue'",
    `$env:${DESKTOP_DELEGATED_ENV}='1'`,
    "$c=1",
    `try { & ${invocation} *>&1 | Tee-Object -FilePath ${psq(logPath)}; $c=$LASTEXITCODE } catch { $_ | Out-File -Append -FilePath ${psq(logPath)}; $c=1 }`,
    `Set-Content -LiteralPath ${psq(codePath)} -Value $c`,
    "exit $c",
    "",
  ].join("\r\n");
}

export function buildSchtasksCreate(taskName, wrapperPath, user) {
  return [SCHTASKS, ["/Create", "/TN", taskName, "/TR", `"${POWERSHELL}" -NoProfile -ExecutionPolicy Bypass -File "${wrapperPath}"`, "/SC", "ONCE", "/ST", "00:00", "/RU", user, "/IT", "/RL", "LIMITED", "/F"]];
}

const USER_RE = /^[^\\/:*?"<>|\s][^\\/:*?"<>|]*\\[^\\/:*?"<>|]+$/;

/**
 * Run `command` once in the logged-on user's interactive, non-elevated desktop session via a
 * one-shot scheduled task, wait for it, then clean up. Returns { status, output }.
 */
export function delegateToDesktop({ run, command, tmp = os.tmpdir(), sleep = (ms) => Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, ms), now = Date.now, timeoutMs = 10 * 60 * 1000, pollMs = 1000, startGraceMs = 15000, suffix = randomBytes(6).toString("hex") }) {
  const env = windowsPowerShellEnvironment();
  const who = run(POWERSHELL, ["-NoProfile", "-NonInteractive", "-Command", "(Get-CimInstance -ClassName Win32_ComputerSystem).UserName"], { env, encoding: "utf8" });
  const user = String(who.stdout ?? "").trim();
  if (who.status !== 0 || !user) throw new Error("no interactive user is logged on to this Windows machine; sign in at the desktop and rerun trust-sync");
  if (!USER_RE.test(user)) throw new Error("the logged-on desktop user name is not in a recognized DOMAIN\\user form");
  const dir = mkdtempSync(path.join(tmp, "rk-desktop-"));
  const logPath = path.join(dir, "output.log");
  const codePath = path.join(dir, "exit-code.txt");
  const wrapperPath = path.join(dir, "run.ps1");
  const taskName = `RightKitOwnershipTrustSync-${suffix}`;
  let created = false;
  try {
    writeFileSync(wrapperPath, buildDesktopWrapper(command, logPath, codePath), { flag: "wx", mode: 0o600 });
    const [createCmd, createArgs] = buildSchtasksCreate(taskName, wrapperPath, user);
    const c = run(createCmd, createArgs, { env, encoding: "utf8" });
    if (c.status !== 0) throw new Error(`could not create the desktop-session task: ${String(c.stderr || c.stdout || "").trim()}`);
    created = true;
    const s = run(SCHTASKS, ["/Run", "/TN", taskName], { env, encoding: "utf8" });
    if (s.status !== 0) throw new Error(`could not start the desktop-session task: ${String(s.stderr || s.stdout || "").trim()}`);
    const started = now();
    for (;;) {
      if (existsSync(codePath)) break;
      const elapsed = now() - started;
      if (elapsed > timeoutMs) throw new Error("timed out waiting for the desktop-session trust-sync (UAC prompt not answered?)");
      const q = run(SCHTASKS, ["/Query", "/TN", taskName, "/FO", "LIST", "/V"], { env, encoding: "utf8" });
      const running = /^\s*Status:\s*Running\s*$/im.test(String(q.stdout ?? ""));
      if (!running && elapsed > startGraceMs && !existsSync(codePath)) throw new Error("the desktop-session task ended without reporting a result; check that the logged-on user is the same account");
      sleep(pollMs);
    }
    const status = Number.parseInt(readFileSync(codePath, "utf8").replace(/^\uFEFF/, "").trim(), 10);
    const output = existsSync(logPath) ? readFileSync(logPath, "utf8") : "";
    return { status: Number.isInteger(status) ? status : 1, output, user };
  } finally {
    if (created) { try { run(SCHTASKS, ["/Delete", "/TN", taskName, "/F"], { env, encoding: "utf8" }); } catch {} }
    try { rmSync(dir, { recursive: true, force: true }); } catch {}
  }
}
