// Launch helpers for the protected ownership trust update. These only change HOW the
// privileged step is authorized or started; the verified trust child itself is unchanged.
import { mkdtempSync, readFileSync, readSync, rmSync, writeFileSync, existsSync } from "node:fs";
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

export function buildDesktopWrapper(command, logPath, codePath, cwd) {
  const invocation = command.map(psq).join(" ");
  const log = (expr) => `${expr} | Out-File -Append -Encoding utf8 -FilePath ${psq(logPath)}`;
  return [
    "$ErrorActionPreference='Continue'",
    `$env:${DESKTOP_DELEGATED_ENV}='1'`,
    "$c=1",
    ...(cwd ? [`try { Set-Location -LiteralPath ${psq(cwd)} -ErrorAction Stop } catch { ${log("$_")}; Set-Content -LiteralPath ${psq(codePath)} -Value 1; exit 1 }`] : []),
    log(`"desktop task cwd: $((Get-Location).Path)"`),
    log(`"desktop task argv: " + ${psq(invocation)}`),
    `try { & ${invocation} *>&1 | ForEach-Object { "$_" } | Out-File -Append -Encoding utf8 -FilePath ${psq(logPath)}; $c=$LASTEXITCODE } catch { ${log("$_")}; $c=1 }`,
    `Set-Content -LiteralPath ${psq(codePath)} -Value $c`,
    "exit $c",
    "",
  ].join("\r\n");
}

/** Decode a relayed task log: UTF-16LE (BOM or NUL-interleaved) or UTF-8, never mojibake. */
export function decodeLog(buffer) {
  const b = Buffer.isBuffer(buffer) ? buffer : Buffer.from(buffer);
  if (b.length >= 2 && b[0] === 0xff && b[1] === 0xfe) return b.subarray(2).toString("utf16le");
  if (b.length >= 2 && b[0] === 0xfe && b[1] === 0xff) return Buffer.from(b.subarray(2)).swap16().toString("utf16le");
  const sample = b.subarray(0, 512);
  let nuls = 0; for (const x of sample) if (x === 0) nuls += 1;
  if (sample.length >= 2 && nuls * 4 >= sample.length) return b.toString("utf16le");
  return b.toString("utf8").replace(/^\uFEFF/, "");
}

export function buildSchtasksCreate(taskName, wrapperPath, user) {
  return [SCHTASKS, ["/Create", "/TN", taskName, "/TR", `"${POWERSHELL}" -NoProfile -ExecutionPolicy Bypass -File "${wrapperPath}"`, "/SC", "ONCE", "/ST", "00:00", "/RU", user, "/IT", "/RL", "LIMITED", "/F"]];
}

const USER_RE = /^[^\\/:*?"<>|\s][^\\/:*?"<>|]*\\[^\\/:*?"<>|]+$/;

/**
 * Run `command` once in the logged-on user's interactive, non-elevated desktop session via a
 * one-shot scheduled task, wait for it, then clean up. Returns { status, output }.
 */
export function delegateToDesktop({ run, command, tmp = os.tmpdir(), sleep = (ms) => Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, ms), now = Date.now, cwd = process.cwd(), timeoutMs = 10 * 60 * 1000, pollMs = 1000, startGraceMs = 15000, suffix = randomBytes(6).toString("hex") }) {
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
    writeFileSync(wrapperPath, buildDesktopWrapper(command, logPath, codePath, path.resolve(cwd)), { flag: "wx", mode: 0o600 });
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
    const output = existsSync(logPath) ? decodeLog(readFileSync(logPath)) : "";
    return { status: Number.isInteger(status) ? status : 1, output, user };
  } finally {
    if (created) { try { run(SCHTASKS, ["/End", "/TN", taskName], { env, encoding: "utf8" }); } catch {} }
    if (created) { try { run(SCHTASKS, ["/Delete", "/TN", taskName, "/F"], { env, encoding: "utf8" }); } catch {} }
    try { rmSync(dir, { recursive: true, force: true }); } catch {}
  }
}

// ---- Informed consent: show WHAT is about to be installed before any password/UAC prompt ----
export const CONFIRM_TITLE = "RightKit ownership trust-sync";

/** Escape free text for an AppleScript string expression; newlines become `return` concatenations. */
export function appleScriptText(value) {
  const text = String(value).replaceAll("\0", "").replaceAll("\r\n", "\n").replaceAll("\r", "\n");
  return text.split("\n").map((line) => `"${line.replaceAll("\\", "\\\\").replaceAll('"', '\\"')}"`).join(" & return & ");
}

/** Cap a summary for a dialog; the dropped tail is reported as "...and N more". */
export function capSummary(lines, { maxLines = 16, maxChars = 1600 } = {}) {
  const out = [];
  let used = 0;
  for (let i = 0; i < lines.length; i += 1) {
    const line = lines[i].length > 300 ? `${lines[i].slice(0, 299)}\u2026` : lines[i];
    if (out.length >= maxLines - 1 || used + line.length > maxChars) { out.push(`\u2026and ${lines.length - i} more`); return out.join("\n"); }
    out.push(line); used += line.length + 1;
  }
  return out.join("\n");
}

export function buildMacConfirmScript(text) {
  return `display dialog ${appleScriptText(text)} with title ${appleScriptText(CONFIRM_TITLE)} buttons {"Cancel","Approve\u2026"} default button "Approve\u2026" cancel button "Cancel" with icon caution`;
}

export function buildWindowsConfirmScript(text) {
  const lines = String(text).replaceAll("\0", "").split(/\r?\n/).map(psq).join(", ");
  // user32 MessageBoxW with no owner: OKCANCEL | ICONWARNING | SETFOREGROUND | TOPMOST so it is drawn in front.
  return [
    "Add-Type -Namespace RK -Name Dlg -MemberDefinition '[DllImport(\"user32.dll\", CharSet=CharSet.Unicode)] public static extern int MessageBoxW(System.IntPtr h, string t, string c, uint f);'",
    `$r=[RK.Dlg]::MessageBoxW([IntPtr]::Zero, ([string]::Join([Environment]::NewLine, @(${lines}))), ${psq(CONFIRM_TITLE)}, 0x50031)`,
    "if ($r -eq 1) { exit 0 } else { exit 1 }",
  ].join("; ");
}

function readLineSync() {
  const buf = Buffer.alloc(1); let line = "";
  for (;;) { let n = 0; try { n = readSync(0, buf, 0, 1, null); } catch { return line; } if (n === 0 || buf[0] === 10) return line; line += String.fromCharCode(buf[0]); }
}

/**
 * Ask the human to approve the summarized change. Prints the summary, then: TTY -> type `yes`;
 * macOS GUI -> osascript dialog; Windows GUI -> MessageBox. Throws "cancelled by user" unless approved.
 */
export function confirmTrustChange({ lines, run, interactiveRun, print = console.log, hostPlatform = process.platform, tty = hasTerminal(), readLine = readLineSync }) {
  const text = capSummary(lines);
  print(lines.join("\n"));
  const cancelled = () => new Error("cancelled by user");
  if (tty) {
    print("Type 'yes' to approve this trust update (anything else cancels):");
    if (String(readLine() ?? "").trim() !== "yes") throw cancelled();
    return;
  }
  if (hostPlatform === "darwin") {
    const r = run("/usr/bin/osascript", ["-e", buildMacConfirmScript(text)], { encoding: "utf8" });
    if (r.status !== 0) throw cancelled();
    return;
  }
  if (hostPlatform === "win32") {
    const encoded = Buffer.from(buildWindowsConfirmScript(text), "utf16le").toString("base64");
    // Must be an explicitly SHOWN process: a hidden STARTUPINFO would hide the dialog's first window.
    const r = (interactiveRun ?? run)(POWERSHELL, ["-NoProfile", "-Sta", "-EncodedCommand", encoded], { env: windowsPowerShellEnvironment(), encoding: "utf8", windowsHide: false });
    if (r.status !== 0) throw cancelled();
    return;
  }
  throw new Error("no way to ask for approval on this platform; run from a terminal");
}
