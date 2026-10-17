import * as fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "./background-process.mjs";

import { requestSigningAgentSync, validateNotaryAgentReport } from "./signing-agent-client.mjs";
import { installSigningAgent, requireMacGuiSession } from "./signing-agent-install.mjs";
import { signingAgentPaths, defaultSigningRoots, ensurePrivateDirectory, privatePath, within, NOTARIZATION_TIMEOUT_MS, MAX_NOTARIZATION_TIMEOUT_MS, AGENT_IDENTIFIER_PATTERN, CODESIGN_IDENTIFIER_SCHEMA } from "./rightkit-signing-agent.mjs";
import { refuseManagedEnvSecrets } from "./managed-host-secrets.mjs";

export class MacSigningSessionError extends Error {
  constructor(code, message) {
    super(message);
    this.name = "MacSigningSessionError";
    this.code = code;
  }
}

const capture = { encoding: "utf8", stdio: "pipe", timeout: 5000, shell: false };
const interactionDenied = (r) => r.status === 36 || /errSecInteractionNotAllowed|User interaction is not allowed/i.test(`${r.stderr ?? ""}`);

// Metadata access alone does not prove private-key access. SSH always needs the
// GUI route; launchctl identifies other non-Aqua bootstrap/security sessions.
export function detectMacSigningSession({ env = process.env, run = spawnSync } = {}) {
  if (env.SSH_CONNECTION || env.SSH_TTY || env.SSH_CLIENT) return { guiRunner: true, reason: "SSH security session" };
  const manager = run("/bin/launchctl", ["managername"], capture);
  if (manager.error || manager.status !== 0 || !String(manager.stdout ?? "").trim()) {
    throw new MacSigningSessionError("MAC_SIGNING_SESSION_UNKNOWN", "Cannot determine launchctl security session before macOS signing");
  }
  const name = String(manager.stdout).trim();
  if (name !== "Aqua") return { guiRunner: true, reason: `launchctl manager ${name}` };
  const probe = run("/usr/bin/security", ["show-keychain-info", path.join(os.userInfo().homedir, "Library/Keychains/login.keychain-db")], capture);
  if (interactionDenied(probe)) return { guiRunner: true, reason: "login keychain interaction denied (exit 36)" };
  if (probe.error || probe.status !== 0) throw new MacSigningSessionError("MAC_SIGNING_KEYCHAIN_PROBE_FAILED", `Login keychain metadata probe failed (exit ${probe.status ?? "unavailable"})`);
  return { guiRunner: false, reason: "Aqua security session" };
}

// Parse legacy callers' signing argv into a closed, typed protocol. The agent
// reconstructs argv itself & never accepts executable names or environment.
export function codesignAgentItems(commands) {
  if (!Array.isArray(commands) || !commands.length) throw new TypeError("Codesign commands required");
  return commands.map((args) => {
    if (!Array.isArray(args) || args.some((arg) => typeof arg !== "string")) throw new TypeError("Invalid codesign command");
    const item = {};
    for (let i = 0; i < args.length - 1; i++) {
      const option = args[i];
      if (option === "--force" && item.force === undefined) item.force = true;
      else if (option === "--timestamp" && item.timestamp === undefined) item.timestamp = true;
      else if (option === "--options" && args[i + 1] === "runtime" && item.runtime === undefined) { item.runtime = true; i++; }
      else if (option === "--sign" && item.identity === undefined) item.identity = args[++i];
      else if (option === "--identifier" && item.identifier === undefined) item.identifier = args[++i];
      else if (option === "--entitlements" && item.entitlements === undefined) item.entitlements = args[++i];
      else if (option === "--preserve-metadata=entitlements" && item.preserveEntitlements === undefined) item.preserveEntitlements = true;
      else throw new TypeError(`Unsupported agent codesign option: ${option}`);
    }
    item.path = args.at(-1);
    if (item.identifier !== undefined && !AGENT_IDENTIFIER_PATTERN.test(item.identifier)) throw new TypeError("Agent signing requires a reverse-DNS codesign identifier");
    if (!item.identity || item.identity === "-" || !path.isAbsolute(item.path ?? "")
      || (item.entitlements !== undefined && !path.isAbsolute(item.entitlements))) throw new TypeError("Agent signing requires real identity & absolute paths");
    return item;
  });
}
export function runMacGuiSigning({ kind, commands, files, cwd = process.cwd(), env = process.env,
  run = spawnSync, io = fs, timeoutMs = 180000, home = os.userInfo().homedir,
  uid = process.getuid?.(), username = os.userInfo().username, allowedRoots,
  request = requestSigningAgentSync, install = installSigningAgent } = {}) {
  if (!Number.isInteger(timeoutMs) || timeoutMs < 1 || timeoutMs > 180000) throw new TypeError("Signing timeout must be 1..180000ms");
  if (!["codesign", "updater"].includes(kind)) throw new TypeError("Only typed codesign & updater requests supported");
  const paths = signingAgentPaths(home);
  let roots = allowedRoots ?? defaultSigningRoots(home, env.RIGHTKIT_MANAGED_ROOT);
  if (!allowedRoots && io.existsSync(paths.config)) {
    privatePath(paths.config, 0o600, { io, uid });
    roots = JSON.parse(io.readFileSync(paths.config, "utf8")).roots;
  }
  roots = roots.filter((root) => io.existsSync(root));
  const plan = { schema: 1, type: kind, allowedRoots: roots, cwd: path.resolve(cwd), timeoutMs };
  let staging;
  try {
    if (kind === "codesign") {
      plan.items = codesignAgentItems(commands);
      // Only a request that carries a typed identifier uses schema 2; every other codesign request stays schema 1.
      if (plan.items.some((item) => item.identifier !== undefined)) plan.schema = CODESIGN_IDENTIFIER_SCHEMA;
      // Development signing generates entitlements in TMPDIR. Copy only those
      // plist bytes into managed private staging, never broaden signing roots.
      for (const item of plan.items) {
        if (!item.entitlements || roots.some((root) => within(root, item.entitlements) && within(io.realpathSync(root), io.realpathSync(item.entitlements)))) continue;
        if (!staging) {
          ensurePrivateDirectory(paths.directory, { io, uid });
          ensurePrivateDirectory(path.join(paths.directory, "requests"), { io, uid });
          staging = io.mkdtempSync(path.join(paths.directory, "requests/request-"));
          io.chmodSync(staging, 0o700);
          plan.allowedRoots = [...roots, staging];
        }
        const target = path.join(staging, `${plan.items.indexOf(item)}.plist`);
        io.writeFileSync(target, io.readFileSync(item.entitlements), { mode: 0o600, flag: "wx" });
        item.entitlements = target;
      }
    } else {
      if (!Array.isArray(files) || !files.length || files.some((file) => typeof file !== "string" || !path.isAbsolute(file))) throw new TypeError("Updater artifacts must be absolute paths");
      plan.files = files;
    }
    const clientOptions = { home, timeoutMs: timeoutMs + 5000 };
    let report;
    try { report = request(plan, clientOptions); }
    catch (error) {
      let missing = error.code === "MAC_SIGNING_AGENT_UNAVAILABLE";
      // An installed agent that predates typed identifiers refuses schema 2: upgrade it once, never loop on a rejected request.
      if (!missing && plan.schema === CODESIGN_IDENTIFIER_SCHEMA && error.code === "MAC_SIGNING_AGENT_PROTOCOL") {
        const health = request({ schema: 1, type: "health" }, { home, timeoutMs: 1500 });
        missing = !health.capabilities?.includes("codesign-identifier");
      }
      if (!missing) throw error;
      requireMacGuiSession({ run, uid });
      install({ home, username, uid, run, io, managedRoot: env.RIGHTKIT_MANAGED_ROOT });
      report = request(plan, clientOptions); // One install attempt, one retry.
    }
    if (report.schema !== 1 || report.ok !== true || !Array.isArray(report.results)
      || report.results.length !== (plan.items?.length ?? plan.files.length)
      || report.results.some((r) => r.status !== 0 || typeof r.stdout !== "string" || typeof r.stderr !== "string")
      || (kind === "updater" && report.results.some((r) => r.stdout || r.stderr))) {
      throw new MacSigningSessionError("MAC_SIGNING_AGENT_PROTOCOL", "Signing agent returned invalid results");
    }
    return report.results;
  } finally { if (staging) io.rmSync(staging, { recursive: true, force: true }); }
}

export function runMacCodesignCommands(commands, options = {}) {
  const { cwd = process.cwd(), env = process.env, run = spawnSync } = options;
  const session = detectMacSigningSession({ env, run });
  if (session.guiRunner) return runMacGuiSigning({ ...options, kind: "codesign", commands });
  const results = [];
  for (const args of commands) {
    const r = run("codesign", args, { cwd, env, encoding: "utf8", stdio: ["inherit", "pipe", "pipe"], shell: false });
    results.push(r);
    if (r.error || r.status !== 0) break;
  }
  return results;
}

export function runMacGuiNotarization({ kind = "notarize", file, profile = "apple-dev-notary", app, validateOnly,
  cwd = process.cwd(), env = process.env, run = spawnSync, io = fs, home = os.userInfo().homedir,
  uid = process.getuid?.(), username = os.userInfo().username, allowedRoots, timeoutMs = NOTARIZATION_TIMEOUT_MS,
  request = requestSigningAgentSync, install = installSigningAgent } = {}) {
  if (!["notarize", "staple"].includes(kind) || !Number.isInteger(timeoutMs) || timeoutMs < 1 || timeoutMs > MAX_NOTARIZATION_TIMEOUT_MS) throw new TypeError("Invalid notarization request or timeout");
  if (kind === "notarize" && (typeof profile !== "string" || !/^[A-Za-z0-9][A-Za-z0-9_. -]{0,127}$/.test(profile))) throw new TypeError("Notarization requires a profile name only");
  requireMacGuiSession({ run, uid });
  const paths = signingAgentPaths(home);
  let roots = allowedRoots ?? defaultSigningRoots(home, env.RIGHTKIT_MANAGED_ROOT);
  if (!allowedRoots && io.existsSync(paths.config)) {
    privatePath(paths.config, 0o600, { io, uid });
    roots = JSON.parse(io.readFileSync(paths.config, "utf8")).roots;
  }
  const plan = { schema: 1, type: kind, path: path.resolve(file), cwd: path.resolve(cwd),
    allowedRoots: roots.filter((root) => io.existsSync(root)), timeoutMs,
    ...(kind === "notarize" ? { profile, staple: true, ...(app ? { app: path.resolve(app) } : {}) } : { validateOnly: validateOnly === true }) };
  const options = { home, timeoutMs: timeoutMs + 5000 };
  let report;
  try { report = request(plan, options); }
  catch (error) {
    let missing = error.code === "MAC_SIGNING_AGENT_UNAVAILABLE";
    // Upgrade earlier installed agents once; never turn a rejected path or
    // malformed request into an install/retry loop.
    if (error.code === "MAC_SIGNING_AGENT_PROTOCOL") {
      const health = request({ schema: 1, type: "health" }, { home, timeoutMs: 1500 });
      missing = !health.capabilities?.includes(kind);
    }
    if (!missing) throw error;
    install({ home, username, uid, run, io, managedRoot: env.RIGHTKIT_MANAGED_ROOT });
    report = request(plan, options);
  }
  validateNotaryAgentReport(report);
  const targets = kind === "staple" ? [plan.path] : [...new Set([...( /\.(dmg|pkg)$/i.test(plan.path) ? [plan.path] : []), ...(plan.app ? [plan.app] : [])])];
  const canonical = (value) => (io.realpathSync.native ?? io.realpathSync)(value);
  if (!report.ok || (kind === "notarize" && (report.notarization.status !== "Accepted" || !report.notarization.id
    || !report.steps.some((step) => step.step === "submit" && step.exitCode === 0)))
    || report.stapling.length !== targets.length || targets.some((target) => !report.stapling.some((item) => item.file === canonical(target)
      && report.steps.some((step) => step.step === "validate" && step.file === item.file && step.exitCode === 0)
      && (kind !== "notarize" || report.steps.some((step) => step.step === "staple" && step.file === item.file && step.exitCode === 0))))
    || report.steps.some((step) => step.step !== "probe" && step.exitCode !== 0)) {
    throw new MacSigningSessionError("MAC_SIGNING_AGENT_PROTOCOL", "Signing agent returned incomplete notarization or stapling evidence");
  }
  return report;
}

export function signMacUpdaterFiles(files, options = {}) {
  const { cwd = process.cwd(), env = process.env, run = spawnSync, username = os.userInfo().username } = options;
  // Inherited updater key material must not bypass the Keychain or GUI signing agent on managed hosts.
  refuseManagedEnvSecrets(env, ["TAURI_SIGNING_PRIVATE_KEY", "TAURI_SIGNING_PRIVATE_KEY_PASSWORD"]);
  const commands = files.map((file) => ["exec", "tauri", "signer", "sign", file]);
  const signingEnv = { ...env, TAURI_SIGNING_PRIVATE_KEY_PASSWORD: env.TAURI_SIGNING_PRIVATE_KEY_PASSWORD ?? "" };
  if (env.SSH_CONNECTION || env.SSH_TTY || env.SSH_CLIENT || (!signingEnv.TAURI_SIGNING_PRIVATE_KEY && detectMacSigningSession({ env, run }).guiRunner)) {
    return runMacGuiSigning({ ...options, kind: "updater", files });
  }
  if (!signingEnv.TAURI_SIGNING_PRIVATE_KEY) {
    const key = run("security", ["find-generic-password", "-a", username, "-s", "rightsuite-updater-key", "-w"], capture);
    if (key.error || key.status !== 0 || !String(key.stdout ?? "").trim()) {
      throw new MacSigningSessionError("MAC_SIGNING_UPDATER_KEY_UNAVAILABLE", "Shared updater key unavailable in Keychain service rightsuite-updater-key");
    }
    signingEnv.TAURI_SIGNING_PRIVATE_KEY = String(key.stdout).trim();
  }
  const results = [];
  for (const args of commands) {
    const r = run("pnpm", args, { cwd, env: signingEnv, encoding: "utf8", stdio: "pipe", shell: false });
    if (r.error || r.status !== 0) throw new MacSigningSessionError("MAC_SIGNING_UPDATER_FAILED", `Updater signing failed (exit ${r.status ?? "unavailable"})`);
    results.push({ status: 0, stdout: "", stderr: "" });
  }
  return results;
}
