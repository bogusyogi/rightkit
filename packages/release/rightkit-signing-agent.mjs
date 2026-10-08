#!/usr/bin/env node
import { resolveManagedRoots } from './managed-roots.mjs';
import * as fs from "node:fs";
import net from "node:net";
import os from "node:os";
import path from "node:path";
import { randomBytes, timingSafeEqual, randomUUID } from "node:crypto";
import { fileURLToPath } from "node:url";
import { spawn, spawnSync } from "./background-process.mjs";
import { parseCodesignArgv } from "./codesign-argv.mjs";

export const AGENT_LABEL = "dev.rightkit.signing-agent";
export const MAX_FRAME = 1024 * 1024;
export const NOTARIZATION_TIMEOUT_MS = 1800000;
export const MAX_NOTARIZATION_TIMEOUT_MS = 7200000;
export class SigningAgentError extends Error {
  constructor(code, message) { super(message); this.name = "SigningAgentError"; this.code = code; }
}
const fail = (code, message) => { throw new SigningAgentError(code, message); };
export function signingAgentPaths(home = os.userInfo().homedir) {
  const directory = resolveManagedRoots({ platform: "darwin", home }).signingAgent;
  return { directory, socket: path.join(directory, "agent.sock"), token: path.join(directory, "token"),
    config: path.join(directory, "config.json"), runtime: path.join(directory, "runtime"),
    log: path.join(directory, "requests.jsonl"), plist: path.join(home, "Library/LaunchAgents", `${AGENT_LABEL}.plist`) };
}
export function defaultSigningRoots(home = os.userInfo().homedir, managedRoot = resolveManagedRoots({ home }).dataRoot, cargoTargetDirs = []) {
  return [...new Set(["/Volumes/D/claude", managedRoot, "/Volumes/D/rightkit-build-control",
    path.join(home, "Library/Caches/RightSuite/release"), "/tmp", "/private/tmp", os.tmpdir(), ...cargoTargetDirs])];
}
export const within = (root, file) => file === root || file.startsWith(`${root}${path.sep}`);
export function privatePath(file, mode, { io = fs, uid = process.getuid?.(), kind = "file" } = {}) {
  const info = io.lstatSync(file);
  if (info.uid !== uid || (info.mode & 0o777) !== mode || info.isSymbolicLink()
    || !(kind === "directory" ? info.isDirectory() : kind === "socket" ? info.isSocket() : info.isFile())) {
    fail("MAC_SIGNING_AGENT_UNSAFE_PATH", "Signing agent path has invalid ownership, type, or permissions");
  }
}
export function ensurePrivateDirectory(directory, { io = fs, uid = process.getuid?.() } = {}) {
  // Reject symlink ancestors before mkdir/chmod. Existing public ancestors are
  // fine only if no other uid can replace their children.
  const parts = path.resolve(directory).split(path.sep).filter(Boolean);
  let current = path.parse(directory).root;
  for (const part of parts) {
    current = path.join(current, part);
    if (!io.existsSync(current)) io.mkdirSync(current, { mode: 0o700 });
    const info = io.lstatSync(current);
    if (!info.isDirectory() || info.isSymbolicLink() || ![0, uid].includes(info.uid)
      || ((info.mode & 0o022) && !(info.mode & 0o1000))) fail("MAC_SIGNING_AGENT_UNSAFE_PATH", "Unsafe signing agent directory ancestor");
  }
  io.chmodSync(directory, 0o700);
  privatePath(directory, 0o700, { io, uid, kind: "directory" });
}
function shape(value, fields) {
  if (!value || typeof value !== "object" || Array.isArray(value) || Object.keys(value).some((key) => !fields.includes(key))) {
    fail("MAC_SIGNING_AGENT_PROTOCOL", "Unknown request fields or invalid object");
  }
}
function absolute(value) {
  if (typeof value !== "string" || !path.isAbsolute(value) || value.includes("\0")) fail("MAC_SIGNING_AGENT_PATH_DENIED", "Paths must be absolute");
  return path.resolve(value);
}
export function validateSigningRequest(request, { roots, stagingRoot, io = fs } = {}) {
  // Node's JS realpath normalizes .. before following links. Native realpath
  // follows the same path traversal as codesign & catches link/.. escapes.
  const realpath = io.realpathSync.native ?? io.realpathSync;
  const common = ["schema", "token", "type", "allowedRoots", "cwd", "timeoutMs"];
  if (request?.type === "health") {
    shape(request, ["schema", "token", "type"]);
    if (request.schema !== 1) fail("MAC_SIGNING_AGENT_PROTOCOL", "Unsupported protocol schema");
    return request;
  }
  const notary = ["notarize", "staple"].includes(request?.type);
  if (!["codesign", "codesign-argv", "updater", "notarize", "staple"].includes(request?.type)) fail("MAC_SIGNING_AGENT_PROTOCOL", "Unsupported signing request type");
  shape(request, [...common, ...(notary ? ["path", "profile", "staple", "app", "validateOnly"] : request.type === "codesign-argv" ? ["argv", "targets"] : [request.type === "codesign" ? "items" : "files"])]);
  if (request.schema !== 1 || !Number.isInteger(request.timeoutMs) || request.timeoutMs < 1 || request.timeoutMs > (notary ? MAX_NOTARIZATION_TIMEOUT_MS : 180000)) {
    fail("MAC_SIGNING_AGENT_PROTOCOL", "Invalid schema or signing deadline");
  }
  if (!Array.isArray(request.allowedRoots) || !request.allowedRoots.length || request.allowedRoots.length > 32) fail("MAC_SIGNING_AGENT_PATH_DENIED", "Allowed roots required");
  // Callers may narrow installed policy, never widen it. Check lexical & real
  // containment, so symlinks cannot turn a workspace path into another root.
  const installed = roots.map(absolute);
  const allowed = request.allowedRoots.map((root) => {
    const lexical = absolute(root);
    if (![...installed, stagingRoot].filter(Boolean).some((base) => within(base, lexical))) fail("MAC_SIGNING_AGENT_PATH_DENIED", "Caller root is outside installed signing policy");
    const canonical = realpath(root);
    if (![...installed, stagingRoot].filter(Boolean).some((base) => {
      try { return within(realpath(base), canonical); } catch { return false; }
    })) fail("MAC_SIGNING_AGENT_PATH_DENIED", "Allowed root escapes installed policy");
    return { lexical, canonical };
  });
  function checked(file, { directory = false, entitlements = false } = {}) {
    const lexical = absolute(file);
    let canonical;
    try { canonical = realpath(file); } catch { fail("MAC_SIGNING_AGENT_PATH_DENIED", "Signing path is unavailable"); }
    if (!allowed.some((root) => within(root.lexical, lexical) && within(root.canonical, canonical))
      || (!entitlements && stagingRoot && within(stagingRoot, canonical))) fail("MAC_SIGNING_AGENT_PATH_DENIED", "Signing path escapes allowed roots");
    const info = io.statSync(canonical);
    if (directory ? !info.isDirectory() : !info.isFile() && (!info.isDirectory() || entitlements)) fail("MAC_SIGNING_AGENT_PATH_DENIED", "Invalid signing path type");
    return canonical;
  }
  const cwd = checked(request.cwd, { directory: true });
  if (notary) {
    const file = checked(request.path);
    if (request.type === "staple") {
      if (request.profile !== undefined || request.app !== undefined || request.staple !== undefined
        || (request.validateOnly !== undefined && typeof request.validateOnly !== "boolean")) fail("MAC_SIGNING_AGENT_PROTOCOL", "Invalid stapling options");
      if (!/\.(dmg|pkg|app)$/i.test(file) || (/\.app$/i.test(file) ? !io.statSync(file).isDirectory() : !io.statSync(file).isFile())) fail("MAC_SIGNING_AGENT_PATH_DENIED", "Invalid staple target");
      return { ...request, cwd, path: file };
    }
    const profile = request.profile ?? "apple-dev-notary";
    if (typeof profile !== "string" || !/^[A-Za-z0-9][A-Za-z0-9_. -]{0,127}$/.test(profile)
      || request.staple !== true || request.validateOnly !== undefined) fail("MAC_SIGNING_AGENT_PROTOCOL", "Notarization requires a profile name & staple: true");
    if (!/\.(dmg|zip|pkg)$/i.test(file) || !io.statSync(file).isFile()) fail("MAC_SIGNING_AGENT_PATH_DENIED", "Notarization requires DMG, ZIP, or PKG");
    const app = request.app === undefined ? undefined : checked(request.app, { directory: true });
    if (app && !/\.app$/i.test(app)) fail("MAC_SIGNING_AGENT_PATH_DENIED", "Stapling app must be an app bundle");
    if (/\.zip$/i.test(file) && !app) fail("MAC_SIGNING_AGENT_PROTOCOL", "ZIP submission requires app bundle for stapling");
    return { ...request, cwd, path: file, profile, ...(app ? { app } : {}) };
  }
  if (request.type === "codesign-argv") {
    const parsed = parseCodesignArgv(request.argv, request.cwd);
    if (!Array.isArray(request.targets) || request.targets.length !== parsed.targets.length
      || request.targets.some((target, index) => target !== parsed.targets[index])) fail("MAC_SIGNING_AGENT_PROTOCOL", "Codesign target paths do not match argv");
    for (const target of parsed.targets) checked(target);
    for (const file of parsed.entitlements) checked(file, { entitlements: true });
    // Preserve argv & cwd spelling: relative arguments must resolve exactly as
    // they would for the original caller, including symlink/.. semantics.
    return { ...request };
  }
  if (request.type === "updater") {
    if (!Array.isArray(request.files) || !request.files.length || request.files.length > 4096) fail("MAC_SIGNING_AGENT_PROTOCOL", "Updater files required");
    const files = request.files.map((file) => {
      const canonical = checked(file);
      if (!io.statSync(canonical).isFile()) fail("MAC_SIGNING_AGENT_PATH_DENIED", "Updater artifact must be a file");
      // The signer creates <artifact>.sig; reject existing output symlinks too.
      if (io.existsSync(`${canonical}.sig`) || (() => { try { return io.lstatSync(`${canonical}.sig`).isSymbolicLink(); } catch { return false; } })()) {
        if (io.lstatSync(`${canonical}.sig`).isSymbolicLink()) fail("MAC_SIGNING_AGENT_PATH_DENIED", "Updater signature cannot be a symlink");
        checked(`${canonical}.sig`);
      }
      return canonical;
    });
    return { ...request, cwd, files };
  }
  if (!Array.isArray(request.items) || !request.items.length || request.items.length > 4096) fail("MAC_SIGNING_AGENT_PROTOCOL", "Codesign items required");
  const items = request.items.map((item) => {
    shape(item, ["path", "identity", "entitlements", "runtime", "timestamp", "force", "preserveEntitlements"]);
    if (typeof item.identity !== "string" || !item.identity.trim() || item.identity === "-" || item.identity.includes("\0") || item.identity.length > 512) fail("MAC_SIGNING_AGENT_PROTOCOL", "Real signing identity required");
    for (const field of ["runtime", "timestamp", "force", "preserveEntitlements"]) if (item[field] !== undefined && typeof item[field] !== "boolean") fail("MAC_SIGNING_AGENT_PROTOCOL", "Signing options must be boolean");
    return { ...item, path: checked(item.path), ...(item.entitlements !== undefined ? { entitlements: checked(item.entitlements, { entitlements: true }) } : {}) };
  });
  return { ...request, cwd, items };
}
export function authenticateSigningToken(candidate, token) {
  if (typeof candidate !== "string" || !/^[a-f0-9]{64}$/.test(candidate) || !timingSafeEqual(Buffer.from(candidate), Buffer.from(token))) {
    fail("MAC_SIGNING_AGENT_AUTH", "Signing agent authentication failed");
  }
}
export function executeSigningRequest(request, { run = spawnSync, home = os.userInfo().homedir,
  username = os.userInfo().username, pnpm, environmentPath, now = Date.now } = {}) {
  if (request.type === "health") return { schema: 1, ok: true, service: AGENT_LABEL, pid: process.pid, capabilities: ["codesign", "codesign-argv", "updater", "notarize", "staple"] };
  const deadline = now() + request.timeoutMs;
  const env = { HOME: home, PATH: environmentPath, TMPDIR: path.join(signingAgentPaths(home).directory, "tmp") };
  if (request.type === "codesign-argv") {
    const r = run("/usr/bin/codesign", request.argv, { cwd: request.cwd, env, encoding: "utf8", stdio: "pipe", shell: false,
      windowsHide: true, timeout: request.timeoutMs, killSignal: "SIGKILL", maxBuffer: MAX_FRAME });
    if (r.error || !Number.isInteger(r.status)) fail(r.error?.code === "ETIMEDOUT" ? "MAC_SIGNING_AGENT_TIMEOUT" : "MAC_SIGNING_AGENT_JOB_FAILED", "Codesign execution failed");
    const report = { schema: 1, ok: true, results: [{ status: r.status, stdout: String(r.stdout ?? ""), stderr: String(r.stderr ?? "") }] };
    if (Buffer.byteLength(`${JSON.stringify(report)}\n`) > MAX_FRAME) fail("MAC_SIGNING_AGENT_PROTOCOL", "Codesign output exceeds protocol limit");
    return report;
  }
  const results = [];
  const outputLimit = Math.min(4096, Math.floor(MAX_FRAME / (4 * (request.items?.length ?? request.files.length))));
  const invoke = (command, args, sensitive = false) => {
    const remaining = deadline - now();
    if (remaining <= 0) fail("MAC_SIGNING_AGENT_TIMEOUT", "Signing deadline expired");
    const r = run(command, args, { cwd: request.cwd, env, encoding: "utf8", stdio: "pipe", shell: false,
      timeout: remaining, killSignal: "SIGKILL", maxBuffer: MAX_FRAME });
    const result = { status: r.error ? 1 : r.status ?? 1, stdout: sensitive ? "" : String(r.stdout ?? "").slice(-outputLimit),
      stderr: sensitive ? "" : String(r.stderr ?? "").slice(-outputLimit) };
    if (result.status !== 0) fail("MAC_SIGNING_AGENT_JOB_FAILED", sensitive ? "Updater signing failed" : `Codesign failed (exit ${result.status}): ${result.stderr}`);
    return { r, result };
  };
  try {
    if (request.type === "updater") {
      const { r } = invoke("/usr/bin/security", ["find-generic-password", "-a", username, "-s", "rightsuite-updater-key", "-w"], true);
      const key = String(r.stdout ?? "").trim();
      if (!key) fail("MAC_SIGNING_UPDATER_KEY_UNAVAILABLE", "Shared updater key unavailable");
      env.TAURI_SIGNING_PRIVATE_KEY = key;
      env.TAURI_SIGNING_PRIVATE_KEY_PASSWORD = "";
      for (const file of request.files) results.push(invoke(pnpm, ["exec", "tauri", "signer", "sign", file], true).result);
    } else {
      // Preserve caller's inside-out ordering. No arbitrary argv reaches spawn.
      for (const item of request.items) {
        const args = [...(item.force ? ["--force"] : []), ...(item.runtime ? ["--options", "runtime"] : []),
          ...(item.timestamp ? ["--timestamp"] : []), "--sign", item.identity,
          ...(item.entitlements ? ["--entitlements", item.entitlements] : []),
          ...(item.preserveEntitlements ? ["--preserve-metadata=entitlements"] : []), item.path];
        results.push(invoke("/usr/bin/codesign", args).result);
      }
    }
    return { schema: 1, ok: true, results };
  } catch (error) {
    if (request.type === "updater") fail(error.code?.startsWith("MAC_SIGNING_") ? error.code : "MAC_SIGNING_AGENT_JOB_FAILED", "Updater signing failed inside signing agent");
    throw error;
  } finally { delete env.TAURI_SIGNING_PRIVATE_KEY; delete env.TAURI_SIGNING_PRIVATE_KEY_PASSWORD; }
}
// No notarytool/stapler output is forwarded verbatim: diagnostics can contain
// account details. Only typed status, submission UUID, counts & issue codes leave Aqua.
export async function executeNotarizationRequest(request, { launch = spawn, home = os.userInfo().homedir,
  environmentPath = "/usr/bin:/bin", now = Date.now, id = randomUUID(), progress = () => {}, signal } = {}) {
  const deadline = now() + request.timeoutMs;
  const env = { HOME: home, PATH: environmentPath, TMPDIR: path.join(signingAgentPaths(home).directory, "tmp") };
  const report = { schema: 1, ok: false, id, notarization: { id: null, status: "Unknown", logSummary: null }, steps: [], stapling: [] };
  const invoke = async (step, args, file) => {
    progress({ schema: 1, event: "status", id, step });
    const remaining = deadline - now();
    if (remaining <= 0 || signal?.aborted) throw new SigningAgentError("MAC_SIGNING_AGENT_TIMEOUT", "Notarization deadline expired");
    const result = await new Promise((resolve) => {
      let child, output = "", bytes = 0, timedOut = false, overflow = false, failed = false;
      let timer;
      const kill = () => { try { child?.kill("SIGKILL"); } catch {} };
      const cancel = () => { timedOut = true; kill(); };
      const finish = (exitCode) => {
        clearTimeout(timer); signal?.removeEventListener("abort", cancel);
        resolve({ exitCode: Number.isInteger(exitCode) ? exitCode : null, output, timedOut, overflow, failed });
      };
      try {
        child = launch("/usr/bin/xcrun", args, { cwd: request.cwd, env, stdio: ["ignore", "pipe", "pipe"], shell: false });
        timer = setTimeout(cancel, remaining);
        signal?.addEventListener("abort", cancel, { once: true });
        child.stdout.on("data", (chunk) => {
          bytes += chunk.length;
          if (bytes > MAX_FRAME) { overflow = true; kill(); } else output += chunk.toString();
        });
        child.stderr.on("data", () => {}); // Drain, never disclose.
        child.once("error", () => { failed = true; finish(null); });
        child.once("close", finish);
      } catch { failed = true; finish(null); }
    });
    report.steps.push({ step, ...(file ? { file } : {}), exitCode: result.exitCode });
    if (result.timedOut) throw new SigningAgentError("MAC_SIGNING_AGENT_TIMEOUT", "Notarization deadline expired");
    if (result.failed || result.overflow) throw new SigningAgentError("MAC_SIGNING_AGENT_JOB_FAILED", `${step} execution failed`);
    return result;
  };
  const parse = (output) => { try { return JSON.parse(output); } catch { return {}; } };
  try {
    if (request.type === "notarize") {
      const submitted = await invoke("submit", ["notarytool", "submit", request.path, "--keychain-profile", request.profile ?? "apple-dev-notary", "--wait", "--output-format", "json"]);
      const result = parse(submitted.output);
      const submissionId = result.id ?? result.submissionId;
      report.notarization.id = typeof submissionId === "string" && /^[a-f0-9-]{36}$/i.test(submissionId) ? submissionId : null;
      report.notarization.status = ["Accepted", "Invalid", "In Progress", "Rejected"].includes(result.status) ? result.status : "Unknown";
      if (report.notarization.status === "Invalid" && report.notarization.id) {
        const logged = await invoke("log", ["notarytool", "log", report.notarization.id, "--keychain-profile", request.profile ?? "apple-dev-notary", "--output-format", "json"]);
        if (logged.exitCode === 0) {
          const log = parse(logged.output);
          const issues = Array.isArray(log.issues) ? log.issues : [];
          report.notarization.logSummary = { statusCode: Number.isInteger(log.statusCode) ? log.statusCode : null,
            errors: issues.filter((issue) => issue.severity === "error").length,
            warnings: issues.filter((issue) => issue.severity === "warning").length,
            issues: issues.slice(0, 100).map((issue) => ({ severity: ["error", "warning"].includes(issue.severity) ? issue.severity : "unknown",
              code: Number.isInteger(issue.code) ? issue.code : null })) };
        }
      }
      if (submitted.exitCode !== 0 || report.notarization.status !== "Accepted" || !report.notarization.id) {
        throw new SigningAgentError("MAC_SIGNING_NOTARIZATION_FAILED", `Notarization ${report.notarization.status}`);
      }
    }
    const targets = request.type === "staple" ? [request.path]
      : [...new Set([...( /\.(dmg|pkg)$/i.test(request.path) ? [request.path] : []), ...(request.app ? [request.app] : [])])];
    for (const file of targets) {
      let alreadyStapled = false;
      if (request.type === "staple" && !request.validateOnly) {
        const probe = await invoke("probe", ["stapler", "validate", file], file);
        alreadyStapled = probe.exitCode === 0;
      }
      if (!request.validateOnly && !alreadyStapled) {
        const stapled = await invoke("staple", ["stapler", "staple", file], file);
        if (stapled.exitCode !== 0) throw new SigningAgentError("MAC_SIGNING_STAPLE_FAILED", "Stapling failed");
      }
      const validated = await invoke("validate", ["stapler", "validate", file], file);
      if (validated.exitCode !== 0) throw new SigningAgentError("MAC_SIGNING_STAPLE_FAILED", "Staple validation failed");
      report.stapling.push({ file, alreadyStapled, validated: true });
    }
    report.ok = true;
  } catch (error) {
    report.code = error.code?.startsWith("MAC_SIGNING_") ? error.code : "MAC_SIGNING_AGENT_JOB_FAILED";
    report.message = report.code === "MAC_SIGNING_AGENT_TIMEOUT" ? "Notarization deadline expired" : "Notarization or stapling failed inside signing agent";
  }
  return report;
}

export function managedSigningLog(file, event, { io = fs } = {}) {
  if (io.existsSync(file)) {
    privatePath(file, 0o600, { io });
    if (io.statSync(file).size > 5 * 1024 * 1024) {
      const previous = `${file}.1`;
      if (io.existsSync(previous)) { privatePath(previous, 0o600, { io }); io.unlinkSync(previous); }
      io.renameSync(file, previous);
    }
  }
  // Only service-authored metadata is logged, never request/token/key/argv/output.
  io.appendFileSync(file, `${JSON.stringify({ at: new Date().toISOString(), ...event })}\n`, { mode: 0o600 });
}
export async function startSigningAgent({ paths = signingAgentPaths(), config, io = fs, run = spawnSync, launch = spawn, heartbeatMs = 10000,
  log = (event) => managedSigningLog(paths.log, event), createServer = net.createServer } = {}) {
  privatePath(paths.directory, 0o700, { io, kind: "directory" });
  privatePath(paths.config, 0o600, { io });
  config ??= JSON.parse(io.readFileSync(paths.config, "utf8"));
  const token = randomBytes(32).toString("hex"); // Rotate on every start, including every boot.
  for (const file of [paths.socket, paths.token]) {
    try { io.lstatSync(file); } catch (error) { if (error.code === "ENOENT") continue; throw error; }
    privatePath(file, 0o600, { io, kind: file === paths.socket ? "socket" : "file" });
    io.unlinkSync(file);
  }
  io.writeFileSync(paths.token, token, { mode: 0o600, flag: "wx" });
  let queue = Promise.resolve();
  let pending = 0;
  const server = createServer((socket) => {
    socket.on("error", () => {});
    socket.setTimeout(200000, () => socket.destroy());
    let input = Buffer.alloc(0), submitted = false;
    socket.on("data", (chunk) => {
      if (submitted) { log({ event: "request.rejected", code: "MAC_SIGNING_AGENT_PROTOCOL" }); socket.destroy(); return; }
      input = Buffer.concat([input, chunk]);
      if (input.length > MAX_FRAME) { log({ event: "request.rejected", code: "MAC_SIGNING_AGENT_PROTOCOL" }); socket.destroy(); return; }
      const end = input.indexOf(10);
      if (end < 0) return;
      submitted = true;
      const id = randomUUID();
      let request;
      const respond = (report) => { if (!socket.destroyed) socket.end(`${JSON.stringify(report)}\n`); };
      try {
        if (end !== input.length - 1) fail("MAC_SIGNING_AGENT_PROTOCOL", "One request per connection required");
        request = JSON.parse(input.subarray(0, end).toString("utf8"));
        authenticateSigningToken(request?.token, token);
        request = validateSigningRequest(request, { roots: config.roots, stagingRoot: path.join(paths.directory, "requests"), io });
        if (pending >= 32) fail("MAC_SIGNING_AGENT_BUSY", "Signing queue is full");
      } catch (error) {
        const code = error.code?.startsWith("MAC_SIGNING_") ? error.code : "MAC_SIGNING_AGENT_PROTOCOL";
        log({ event: "request.rejected", id, code });
        respond({ schema: 1, ok: false, code, message: "Signing request rejected" });
        return;
      }
      const receivedAt = Date.now();
      const notary = ["notarize", "staple"].includes(request.type);
      const controller = new AbortController();
      const progress = (frame) => { if (!socket.destroyed) socket.write(`${JSON.stringify(frame)}\n`); };
      const heartbeat = notary ? setInterval(() => progress({ schema: 1, event: "heartbeat", id }), heartbeatMs) : null;
      if (notary) socket.on("close", () => { clearInterval(heartbeat); controller.abort(); });
      pending++;
      log({ event: "request.accepted", id, type: request.type, count: notary ? 1 : request.targets?.length ?? request.items?.length ?? request.files?.length ?? 0 });
      queue = queue.then(async () => {
        if (socket.destroyed) { log({ event: "request.cancelled", id, type: request.type }); return; }
        log({ event: "request.started", id, type: request.type });
        try {
          if (request.type !== "health") {
            request.timeoutMs -= Date.now() - receivedAt;
            if (request.timeoutMs <= 0) fail("MAC_SIGNING_AGENT_TIMEOUT", "Signing deadline expired in queue");
            // Revalidate immediately before effects, not merely on admission.
            request = validateSigningRequest(request, { roots: config.roots, stagingRoot: path.join(paths.directory, "requests"), io });
          }
          const report = notary
            ? await executeNotarizationRequest(request, { launch, home: config.home, environmentPath: config.environmentPath, id, progress, signal: controller.signal })
            : executeSigningRequest(request, { run, home: config.home, username: config.username, pnpm: config.pnpm, environmentPath: config.environmentPath });
          respond(report);
          log({ event: report.ok ? "request.completed" : "request.failed", id, type: request.type,
            ...(report.code ? { code: report.code } : {}),
            ...(notary ? { status: report.notarization.status, steps: report.steps.map(({ step, exitCode }) => ({ step, exitCode })) } : {}),
            ...(request.type === "codesign-argv" ? { status: report.results[0].status } : {}) });
        } catch (error) {
          const code = error.code?.startsWith("MAC_SIGNING_") ? error.code : "MAC_SIGNING_AGENT_JOB_FAILED";
          respond({ schema: 1, ok: false, ...(notary ? { id } : {}), code, message: request.type === "updater" ? "Updater signing failed" : notary ? "Notarization job failed" : error.message });
          log({ event: "request.failed", id, type: request.type, code });
        }
      }).finally(() => { clearInterval(heartbeat); pending--; }).catch(() => { socket.destroy(); });
    });
  });
  await new Promise((resolve, reject) => { server.once("error", reject); server.listen(paths.socket, resolve); });
  io.chmodSync(paths.socket, 0o600);
  log({ event: "service.started", pid: process.pid });
  server.on("error", () => { log({ event: "service.failed", code: "MAC_SIGNING_AGENT_SOCKET" }); });
  return server;
}
const realOrSelf = (file) => { try { return fs.realpathSync(file); } catch { return path.resolve(file); } };
if (process.argv[1] && realOrSelf(process.argv[1]) === realOrSelf(fileURLToPath(import.meta.url))) {
  process.umask(0o077);
  startSigningAgent().then((server) => {
    const stop = () => { managedSigningLog(signingAgentPaths().log, { event: "service.stopping" }); server.close(() => process.exit(0)); setTimeout(() => process.exit(0), 2000).unref(); };
    process.on("SIGTERM", stop); process.on("SIGINT", stop);
  }).catch((error) => {
    const event = { event: "service.failed", code: error.code ?? "MAC_SIGNING_AGENT_START_FAILED" };
    try { managedSigningLog(signingAgentPaths().log, event); } catch {}
    console.error(JSON.stringify(event)); process.exitCode = 1;
  });
}
