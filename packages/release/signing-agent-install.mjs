import * as fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "./background-process.mjs";
import { AGENT_LABEL, signingAgentPaths, defaultSigningRoots, ensurePrivateDirectory, privatePath, SigningAgentError } from "./rightkit-signing-agent.mjs";
import { requestSigningAgentSync } from "./signing-agent-client.mjs";

const capture = { encoding: "utf8", stdio: "pipe", shell: false, timeout: 10000 };
export function requireMacGuiSession({ run = spawnSync, uid = process.getuid?.() } = {}) {
  const session = run("/bin/launchctl", ["print", `gui/${uid}`], capture);
  if (session.error || session.status !== 0) throw new SigningAgentError("MAC_SIGNING_NO_GUI_SESSION", `No logged-in Aqua session for uid ${uid}`);
}
const xml = (value) => String(value).replaceAll("&", "&amp;").replaceAll("<", "&lt;").replaceAll(">", "&gt;").replaceAll('"', "&quot;").replaceAll("'", "&apos;");
export function signingAgentPlist({ paths, node = process.execPath, home }) {
  return `<?xml version="1.0" encoding="UTF-8"?>\n<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">\n<plist version="1.0"><dict>
<key>Label</key><string>${AGENT_LABEL}</string>
<key>ProgramArguments</key><array><string>${xml(node)}</string><string>${xml(path.join(paths.runtime, "rightkit-signing-agent.mjs"))}</string></array>
<key>LimitLoadToSessionType</key><string>Aqua</string>
<key>RunAtLoad</key><true/><key>KeepAlive</key><true/>
<key>ProcessType</key><string>Background</string>
<key>ThrottleInterval</key><integer>10</integer>
<key>Umask</key><integer>63</integer>
<key>EnvironmentVariables</key><dict><key>HOME</key><string>${xml(home)}</string></dict>
<key>StandardOutPath</key><string>/dev/null</string>
<key>StandardErrorPath</key><string>/dev/null</string>
</dict></plist>\n`;
}
export function installSigningAgent({ home = os.userInfo().homedir, username = os.userInfo().username,
  uid = process.getuid?.(), platform = process.platform, run = spawnSync, io = fs, node = process.execPath,
  managedRoot = "/Volumes/D/.rightkit-managed", health = requestSigningAgentSync,
  now = Date.now, sleep = (ms) => Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, ms) } = {}) {
  if (platform !== "darwin") throw new SigningAgentError("MAC_SIGNING_AGENT_PLATFORM", "Signing agent requires macOS");
  requireMacGuiSession({ run, uid });
  const paths = signingAgentPaths(home);
  ensurePrivateDirectory(paths.directory, { io, uid });
  ensurePrivateDirectory(paths.runtime, { io, uid });
  ensurePrivateDirectory(path.join(paths.directory, "requests"), { io, uid });
  ensurePrivateDirectory(path.join(paths.directory, "tmp"), { io, uid });
  if (!path.isAbsolute(managedRoot) || ["/", home, "/Volumes", "/Volumes/D"].includes(path.resolve(managedRoot))) throw new SigningAgentError("MAC_SIGNING_AGENT_PATH_DENIED", "Managed signing root must be a dedicated absolute directory");
  const found = run("/usr/bin/which", ["pnpm"], capture);
  const pnpm = String(found.stdout ?? "").trim();
  if (found.error || found.status !== 0 || !path.isAbsolute(pnpm) || pnpm.includes("\n")) throw new SigningAgentError("MAC_SIGNING_AGENT_INSTALL_FAILED", "Provisioned pnpm executable is unavailable");
  // Resident runtime is self-contained; no package/checkout imports after install.
  for (const name of ["rightkit-signing-agent.mjs", "background-process.mjs"]) {
    const target = path.join(paths.runtime, name);
    if (io.existsSync(target)) privatePath(target, 0o600, { io, uid });
    io.writeFileSync(target, io.readFileSync(new URL(`./${name}`, import.meta.url)), { mode: 0o600 });
  }
  if (io.existsSync(paths.config)) privatePath(paths.config, 0o600, { io, uid });
  io.writeFileSync(paths.config, JSON.stringify({ schema: 1, home, username, pnpm,
    roots: defaultSigningRoots(home, path.resolve(managedRoot)),
    environmentPath: [...new Set([path.dirname(node), path.dirname(pnpm), "/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/bin", "/usr/sbin", "/sbin"])].join(":") }), { mode: 0o600 });
  ensurePrivateDirectory(path.dirname(paths.plist), { io, uid });
  if (io.existsSync(paths.plist)) privatePath(paths.plist, 0o600, { io, uid });
  io.writeFileSync(paths.plist, signingAgentPlist({ paths, node, home }), { mode: 0o600 });
  const loaded = run("/bin/launchctl", ["print", `gui/${uid}/${AGENT_LABEL}`], capture);
  if (!loaded.error && loaded.status === 0) {
    const stopped = run("/bin/launchctl", ["bootout", `gui/${uid}/${AGENT_LABEL}`], capture);
    if (stopped.error || stopped.status !== 0) throw new SigningAgentError("MAC_SIGNING_AGENT_INSTALL_FAILED", "Could not stop existing signing agent");
  }
  const boot = run("/bin/launchctl", ["bootstrap", `gui/${uid}`, paths.plist], capture);
  if (boot.error || boot.status !== 0) {
    requireMacGuiSession({ run, uid });
    throw new SigningAgentError("MAC_SIGNING_AGENT_INSTALL_FAILED", `Signing agent bootstrap failed (exit ${boot.status ?? "unavailable"})`);
  }
  const deadline = now() + 10000;
  do {
    try {
      const report = health({ schema: 1, type: "health" }, { home, timeoutMs: 1000 });
      return { schema: 1, installed: true, running: true, pid: report.pid, socket: paths.socket, plist: paths.plist };
    } catch (error) {
      if (!["MAC_SIGNING_AGENT_UNAVAILABLE", "MAC_SIGNING_AGENT_CONNECTION", "MAC_SIGNING_AGENT_TIMEOUT"].includes(error.code)) throw error;
      sleep(100);
    }
  } while (now() < deadline);
  throw new SigningAgentError("MAC_SIGNING_AGENT_INSTALL_FAILED", "Signing agent bootstrapped but health did not become ready");
}
export function signingAgentStatus({ home = os.userInfo().homedir, uid = process.getuid?.(), run = spawnSync,
  io = fs, health = requestSigningAgentSync } = {}) {
  const paths = signingAgentPaths(home);
  const installed = io.existsSync(paths.plist);
  let guiSession = false;
  try { requireMacGuiSession({ run, uid }); guiSession = true; } catch {}
  try {
    const report = health({ schema: 1, type: "health" }, { home, timeoutMs: 1500 });
    return { schema: 1, installed, guiSession, running: true, pid: report.pid, socket: paths.socket };
  } catch (error) { return { schema: 1, installed, guiSession, running: false, code: error.code, socket: paths.socket }; }
}
export function uninstallSigningAgent({ home = os.userInfo().homedir, uid = process.getuid?.(), platform = process.platform, run = spawnSync, io = fs } = {}) {
  if (platform !== "darwin") throw new SigningAgentError("MAC_SIGNING_AGENT_PLATFORM", "Signing agent requires macOS");
  const paths = signingAgentPaths(home);
  const loaded = run("/bin/launchctl", ["print", `gui/${uid}/${AGENT_LABEL}`], capture);
  if (!loaded.error && loaded.status === 0) {
    const result = run("/bin/launchctl", ["bootout", `gui/${uid}/${AGENT_LABEL}`], capture);
    if (result.error || result.status !== 0) throw new SigningAgentError("MAC_SIGNING_AGENT_UNINSTALL_FAILED", "Could not stop signing agent");
  }
  if (io.existsSync(paths.plist)) { privatePath(paths.plist, 0o600, { io, uid }); io.unlinkSync(paths.plist); }
  if (io.existsSync(paths.directory)) { privatePath(paths.directory, 0o700, { io, uid, kind: "directory" }); io.rmSync(paths.directory, { recursive: true, force: true }); }
  return { schema: 1, installed: false, running: false };
}
