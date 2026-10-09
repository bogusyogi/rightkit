// Human-presence gate for the Windows protected trust update. It runs INSIDE the elevated updater, so it
// cannot be skipped by invoking the updater directly from an already-elevated (e.g. SSH) process: such a
// process has no interactive desktop and no way to answer a password prompt for the signed-in user.
//
// Why not UAC alone: UAC is not a human gate when the machine elevates silently (ConsentPromptBehaviorAdmin=0),
// and an elevated process can launch further elevated processes with no prompt at all. Verifying the signed-in
// user's password via the system credential dialog needs a secret and a person at the screen, and needs no new
// infrastructure. Approval is affirmative-only: the script prints a per-run token on the single success path
// and the caller accepts nothing else (kill, timeout, cancel, error and empty output all mean "no").
import { randomBytes } from "node:crypto";
import { spawnSync, windowsPowerShellEnvironment } from "./background-process.mjs";

const POWERSHELL = "C:\\Windows\\System32\\WindowsPowerShell\\v1.0\\powershell.exe";
export const PRESENCE_TITLE = "RightKit ownership trust-sync - confirm it is you";
export const PRESENCE_APPROVE_PREFIX = "RK_PRESENCE_VERIFIED:";
export const PRESENCE_TIMEOUT_MS = 5 * 60 * 1000;

const psq = (value) => `'${String(value).replaceAll("'", "''")}'`;

const CSHARP = `
using System; using System.Runtime.InteropServices; using System.Text;
public static class RkPresence {
  [StructLayout(LayoutKind.Sequential, CharSet=CharSet.Unicode)] public struct CREDUI_INFO { public int cbSize; public IntPtr hwndParent; public string pszMessageText; public string pszCaptionText; public IntPtr hbmBanner; }
  [DllImport("credui.dll", CharSet=CharSet.Unicode)] static extern int CredUIPromptForWindowsCredentials(ref CREDUI_INFO ui, int authError, ref uint authPackage, IntPtr inBuf, uint inSize, out IntPtr outBuf, out uint outSize, ref bool save, int flags);
  [DllImport("credui.dll", CharSet=CharSet.Unicode)] static extern bool CredUnPackAuthenticationBufferW(int flags, IntPtr buf, uint size, StringBuilder user, ref int userLen, StringBuilder domain, ref int domLen, StringBuilder pass, ref int passLen);
  [DllImport("advapi32.dll", SetLastError=true, CharSet=CharSet.Unicode)] static extern bool LogonUserW(string user, string domain, string pass, int type, int provider, out IntPtr token);
  [DllImport("kernel32.dll")] static extern bool CloseHandle(IntPtr h);
  [DllImport("ole32.dll")] static extern void CoTaskMemFree(IntPtr p);
  // Returns the verified "DOMAIN\\\\user" or null. Three attempts; Cancel ends immediately.
  public static string Verify(string message, string caption) {
    int authError = 0;
    for (int attempt = 0; attempt < 3; attempt++) {
      CREDUI_INFO ui = new CREDUI_INFO(); ui.cbSize = Marshal.SizeOf(typeof(CREDUI_INFO)); ui.pszMessageText = message; ui.pszCaptionText = caption;
      uint pkg = 0; IntPtr outBuf; uint outSize; bool save = false;
      int rc = CredUIPromptForWindowsCredentials(ref ui, authError, ref pkg, IntPtr.Zero, 0, out outBuf, out outSize, ref save, 0x200);
      if (rc != 0) return null;
      StringBuilder u = new StringBuilder(513), d = new StringBuilder(257), p = new StringBuilder(513);
      int ul = u.Capacity, dl = d.Capacity, pl = p.Capacity;
      bool ok = CredUnPackAuthenticationBufferW(0, outBuf, outSize, u, ref ul, d, ref dl, p, ref pl);
      byte[] zero = new byte[outSize]; Marshal.Copy(zero, 0, outBuf, (int)outSize); CoTaskMemFree(outBuf);
      if (!ok) return null;
      IntPtr token;
      string dom = d.Length > 0 ? d.ToString() : ".";
      bool logon = LogonUserW(u.ToString(), dom, p.ToString(), 2, 0, out token);
      p.Clear();
      if (logon) { CloseHandle(token); return (d.Length > 0 ? d.ToString() + "\\\\" : "") + u.ToString(); }
      authError = 1326;
    }
    return null;
  }
}`;

/** PowerShell run in the interactive desktop session. Prints the per-run token only after a verified password for the signed-in user. */
export function buildPresenceScript(summary, nonce) {
  return [
    "$ErrorActionPreference='Stop'",
    "try {",
    // Not interactive (session 0, SSH, service) => no human can be present: refuse before any prompt.
    "if (-not [Environment]::UserInteractive) { exit 3 }",
    "if ([System.Diagnostics.Process]::GetCurrentProcess().SessionId -eq 0) { exit 3 }",
    "$console=(Get-CimInstance -ClassName Win32_ComputerSystem).UserName; if (-not $console) { exit 3 }",
    `Add-Type -TypeDefinition @'${CSHARP}\n'@`,
    `$who=[RkPresence]::Verify(${psq(String(summary).replaceAll("\0", ""))}, ${psq(PRESENCE_TITLE)})`,
    "if (-not $who) { exit 1 }",
    // The verified account must be the signed-in console user, not just any account whose password is known.
    "$a=(New-Object Security.Principal.NTAccount($who)).Translate([Security.Principal.SecurityIdentifier]).Value",
    "$b=(New-Object Security.Principal.NTAccount($console)).Translate([Security.Principal.SecurityIdentifier]).Value",
    "if ($a -ne $b) { exit 1 }",
    `[Console]::Out.WriteLine(${psq(`${PRESENCE_APPROVE_PREFIX}${nonce}`)}); exit 0`,
    "} catch { exit 2 }",
  ].join("\n");
}

/** Compact, bounded description of the change for the credential dialog. */
export function presenceSummary({ existing, next }) {
  const keys = (t) => t?.keyCheckpoint?.sequence ?? "none";
  const digest = (t) => String(t?.minimumCatalogDigest ?? "none").slice(0, 8);
  return [
    "RightKit will update the protected ownership trust on this computer.",
    `Catalog: ${digest(existing)} -> ${digest(next)}`,
    `Key log sequence: ${keys(existing)} -> ${keys(next)}`,
    `Repositories: ${existing?.repositories?.length ?? 0} -> ${next?.repositories?.length ?? 0}`,
    "Enter your Windows password to approve. Cancel to refuse.",
  ].join("\n").slice(0, 900);
}

/**
 * Throws unless a human verified their Windows password on this machine's desktop for exactly this run.
 * Affirmative-only: exit 0 AND the exact per-run token on stdout; kill, timeout, spawn error and anything else deny.
 */
export function requireWindowsPresence({ summary, run = spawnSync, nonce = randomBytes(16).toString("hex"), timeoutMs = PRESENCE_TIMEOUT_MS } = {}) {
  const script = buildPresenceScript(summary, nonce);
  const r = run(POWERSHELL, ["-NoProfile", "-Sta", "-EncodedCommand", Buffer.from(script, "utf16le").toString("base64")], {
    encoding: "utf8", windowsHide: false, timeout: timeoutMs, killSignal: "SIGKILL", env: windowsPowerShellEnvironment(),
  });
  const approved = r && !r.error && r.status === 0 && String(r.stdout ?? "").split(/\r?\n/).includes(`${PRESENCE_APPROVE_PREFIX}${nonce}`);
  if (!approved) throw Object.assign(new Error("RK_OWNERSHIP_PRESENCE_DENIED: the signed-in user did not verify their Windows password at the desktop; trust was not changed"), { code: "RK_OWNERSHIP_PRESENCE_DENIED" });
  return true;
}
