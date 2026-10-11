import { createHash } from "node:crypto";
import { accessSync, closeSync, existsSync, lstatSync, mkdirSync, mkdtempSync, openSync, readdirSync, readFileSync, realpathSync, renameSync, rmSync, unlinkSync, writeFileSync, writeSync } from "node:fs";
import path from "node:path";
import os from "node:os";
import { fileURLToPath } from "node:url";
import { spawnSync, spawnSyncInteractive, windowsPowerShellEnvironment } from "./background-process.mjs";
import { createKeyEvent, keyFingerprint, verifyKeyLog, verifyKeyRequest } from "./ownership-key-log.mjs";
import { canonicalRepositoryIdentity, DEFAULT_TRUST_PATH, loadTrustedExternal, resolveKeyLogTrust, __testOnly as approvalTestOnly } from "./ownership-approval.mjs";
import { readCommittedKeyAuthority } from "./ownership-key-authority.mjs";
import { buildMacInvocation, confirmTrustChange, delegateToDesktop, hasTerminal, needsDesktopDelegation } from "./ownership-key-elevation.mjs";
import { summarizeRequestFile } from "./ownership-trust-summary.mjs";

const here = path.dirname(fileURLToPath(import.meta.url));
const LOG = ".rightkit/ownership-keys.json";
const REQUEST_DIR = ".rightkit/key-requests";
const HEX = /^[a-f0-9]{64}$/;
const sha = (b) => createHash("sha256").update(b).digest("hex");
const canonical = (v) => v === null || typeof v !== "object" ? JSON.stringify(v) : Array.isArray(v) ? `[${v.map(canonical).join(",")}]` : `{${Object.keys(v).sort().map((k) => `${JSON.stringify(k)}:${canonical(v[k])}`).join(",")}}`;
const platform = process.platform === "win32" ? "windows" : process.platform === "darwin" ? "macos" : null;
const parse = (text, what) => { try { return JSON.parse(text); } catch { throw new Error(`${what} is invalid JSON`); } };
const json = (v) => `${JSON.stringify(v, null, 2)}\n`;
let commandTemp;
const temp = (name) => path.join(commandTemp ?? os.tmpdir(), `.rk-${name}-${process.pid}-${Date.now()}.json`);

function repoRoot(app, run) {
  const r = run("git", ["-C", app, "rev-parse", "--show-toplevel"]);
  if (r.status !== 0) throw new Error("--app must resolve to a Git repository");
  return path.resolve(String(r.stdout).trim());
}
function git(run, root, args) {
  const r = run("git", ["-C", root, ...args]);
  if (r.status !== 0) throw new Error(String(r.stderr || `git ${args[0]} failed`).trim());
  return String(r.stdout ?? "").trim();
}
function readLog(root) {
  const f = path.join(root, LOG);
  if (!existsSync(f)) return { schema: 1, events: [] };
  const st = lstatSync(f);
  if (!st.isFile() || st.isSymbolicLink()) throw new Error("ownership key log must be a regular file");
  return parse(readFileSync(f, "utf8"), "ownership key log");
}
function ensureCleanMetadata(root, run, rels) {
  for (const rel of rels) assertSafeMetadataParents(root, rel);
  // Limited to the metadata paths: an unscoped status of a large checkout overflows the child's output buffer.
  const result = run("git", ["-C", root, "status", "--porcelain=v1", "--untracked-files=all", "--", ...rels]);
  if (result.status !== 0) throw new Error("could not inspect repository metadata status");
  const status = String(result.stdout ?? "");
  for (const rel of rels) {
    if (status.split(/\r?\n/).some((line) => line.slice(3).replaceAll('"', "").endsWith(rel))) throw new Error(`refusing to overwrite dirty metadata: ${rel}`);
  }
}
function metadataIsDirty(root, run) {
  const r = run("git", ["-C", root, "status", "--porcelain=v1", "--untracked-files=all", "--", LOG, REQUEST_DIR]);
  if (r.status !== 0) throw new Error("could not inspect repository metadata status");
  return String(r.stdout ?? "").split(/\r?\n/).some((line) => {
    const file = line.slice(3).replaceAll('"', "");
    return file === LOG || file.startsWith(`${REQUEST_DIR}/`);
  });
}
function networkUnavailable(stderr) {
  return stderr?.error?.code === "ETIMEDOUT" || /could not resolve|unable to access|failed to connect|connection timed out|network is unreachable|temporary failure|connection refused|could not resolve host|timed out/i.test(String(stderr?.stderr ?? stderr ?? ""));
}
function resolveAuthority(root, run, injected) {
  let authority = injected;
  if (typeof authority === "function") authority = authority(root);
  if (!authority) {
    const origin = run("git", ["-C", root, "remote", "get-url", "origin"]);
    if (origin.status !== 0) throw new Error("ownership key authority requires a configured origin remote");
    authority = { repositoryIdentity: canonicalRepositoryIdentity(String(origin.stdout ?? "").trim()), checkoutPath: path.resolve(root) };
  }
  if (!authority || Object.keys(authority).sort().join("\0") !== ["checkoutPath", "repositoryIdentity"].sort().join("\0")) throw new Error("key authority must contain repositoryIdentity and checkoutPath");
  const repositoryIdentity = canonicalRepositoryIdentity(authority.repositoryIdentity);
  const checkoutPath = path.resolve(authority.checkoutPath);
  if (checkoutPath !== path.resolve(root)) throw new Error("key authority checkoutPath must match this repository checkout");
  return { repositoryIdentity, checkoutPath };
}
function sameAuthority(stored, current) {
  if (!stored || Object.keys(stored).sort().join("\0") !== ["checkoutPath", "repositoryIdentity"].sort().join("\0")
    || stored.repositoryIdentity !== current.repositoryIdentity || typeof stored.checkoutPath !== "string") return false;
  const storedPath = path.resolve(stored.checkoutPath);
  const currentPath = path.resolve(current.checkoutPath);
  return process.platform === "win32" ? storedPath.toLowerCase() === currentPath.toLowerCase() : storedPath === currentPath;
}
function runNetworkGit(run, args) {
  return run("git", args, {
    timeout: 30_000,
    env: { ...process.env, GIT_TERMINAL_PROMPT: "0", GIT_SSH_COMMAND: "ssh -o BatchMode=yes -o ConnectTimeout=30" },
  });
}
function syncBeforeCeremony(root, run) {
  if (metadataIsDirty(root, run)) throw new Error("ownership metadata has uncommitted changes; commit or discard them before enrolling keys");
  const origin = run("git", ["-C", root, "remote", "get-url", "origin"]);
  if (origin.status !== 0) return { push: false, offline: true, reason: "no origin remote is configured" };
  const branchResult = run("git", ["-C", root, "branch", "--show-current"]);
  const branch = String(branchResult.stdout ?? "").trim();
  const upstreamResult = run("git", ["-C", root, "rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"]);
  if (branchResult.status !== 0 || !branch || upstreamResult.status !== 0) throw new Error("current branch must track origin/<same branch> before ownership enrollment");
  const upstream = String(upstreamResult.stdout ?? "").trim();
  if (upstream !== `origin/${branch}`) throw new Error("current branch must track origin/<same branch> before ownership enrollment");
  const fetch = runNetworkGit(run, ["-C", root, "fetch", "--no-tags", "origin", branch]);
  if (fetch.status !== 0) {
    if (networkUnavailable(fetch)) return { push: false, offline: true, reason: "network unavailable" };
    throw new Error("could not fetch origin; check Git access and branch configuration");
  }
  if (metadataIsDirty(root, run)) throw new Error("ownership metadata changed during upstream sync");
  const merge = run("git", ["-C", root, "merge", "--ff-only", "FETCH_HEAD"]);
  if (merge.status !== 0) throw new Error("origin diverged or cannot fast-forward safely; reconcile branch before key enrollment");
  return { push: true, offline: false, branch };
}
function assertSafeMetadataParents(root, rel) {
  const candidates = [root];
  let current = root;
  for (const part of path.dirname(rel).split(/[\\/]/).filter(Boolean)) {
    current = path.join(current, part);
    candidates.push(current);
  }
  for (const target of candidates) {
    try {
      const stat = lstatSync(target);
      if (stat.isSymbolicLink() || (process.platform === "win32" && (stat.attributes & 0x400))) throw new Error(`ownership metadata parent is a symlink or reparse point: ${target}`);
      if (!stat.isDirectory()) throw new Error(`ownership metadata parent is not a directory: ${target}`);
    } catch (error) {
      if (error.code !== "ENOENT") throw error;
    }
  }
}
function atomicWrite(file, value) {
  mkdirSync(path.dirname(file), { recursive: true });
  const tmp = `${file}.${process.pid}.${Date.now()}.tmp`;
  writeFileSync(tmp, value, { flag: "wx", mode: 0o600 });
  renameSync(tmp, file);
}
function installedLegionPath() {
  if (process.platform === "win32") {
    if (!process.env.LOCALAPPDATA) throw new Error("installed Legion minimize hook is active but LOCALAPPDATA is unavailable");
    return path.join(process.env.LOCALAPPDATA, "Orthic Labs", "Legion", "current", "bin", "legion.exe");
  }
  if (process.platform === "darwin") {
    if (!process.env.HOME) throw new Error("installed Legion minimize hook is active but HOME is unavailable");
    return path.join(process.env.HOME, "Library", "Application Support", "Orthic Labs", "Legion", "current", "bin", "legion");
  }
  throw new Error("installed Legion minimize hook is active on unsupported platform");
}
function detectMinimizeHook(root, run, deps) {
  const hookPathResult = run("git", ["-C", root, "rev-parse", "--git-path", "hooks/pre-commit"]);
  if (hookPathResult.status !== 0) throw new Error("could not resolve effective pre-commit hook path");
  const hookPath = path.resolve(root, String(hookPathResult.stdout ?? "").trim());
  let source = "";
  try {
    const stat = lstatSync(hookPath);
    if (stat.isSymbolicLink() || !stat.isFile()) throw new Error("effective pre-commit hook is not a regular file");
    source = readFileSync(hookPath, "utf8");
  } catch (error) { if (error.code !== "ENOENT") throw error; }
  if (!/minimize\s+commit\s+verify/.test(source) || !/commit-receipt\.json/.test(source)) return null;
  const legion = deps.legionPath ?? installedLegionPath();
  if (!deps.legionPath) {
    try { accessSync(legion, process.platform === "win32" ? undefined : 1); }
    catch { throw new Error(`installed Legion minimize hook is active but stable CLI is unavailable: ${legion}`); }
  }
  return { legion };
}
function withIsolatedIndex(root, run, indexPath, args) {
  return run("git", ["-C", root, ...args], { env: { ...process.env, GIT_INDEX_FILE: indexPath } });
}
function runMinimizeReceipt(root, run, rel, hook) {
  if (!hook) return null;
  assertSafeMetadataParents(root, ".audit/minimize/commit-receipt.json");
  const directory = path.join(root, ".audit", "minimize");
  mkdirSync(directory, { recursive: true });
  const review = path.join(directory, "commit-review.json");
  const receipt = path.join(directory, "commit-receipt.json");
  const prior = [review, receipt].map((file) => {
    try {
      const stat = lstatSync(file);
      if (stat.isSymbolicLink() || !stat.isFile()) throw new Error("minimize receipt path must be a regular file");
      return { file, bytes: readFileSync(file), mode: stat.mode & 0o777 };
    } catch (error) { if (error.code === "ENOENT") return { file, absent: true }; throw error; }
  });
  const indexPath = path.join(commandTemp, "minimize-candidate.index");
  try {
    let result = withIsolatedIndex(root, run, indexPath, ["read-tree", "HEAD"]);
    if (result.status !== 0) throw new Error("could not prepare isolated metadata commit index");
    result = withIsolatedIndex(root, run, indexPath, ["add", "--", rel]);
    if (result.status !== 0) throw new Error("could not add ownership metadata to isolated commit index");
    const env = { ...process.env, GIT_INDEX_FILE: indexPath };
    for (const args of [["minimize", "commit", "init-review", review], ["minimize", "commit", "receipt", review, receipt]]) {
      result = run(hook.legion, args, { cwd: root, env });
      if (result.status !== 0) throw new Error(String(result.stderr || "Legion could not prepare required minimize commit receipt").trim());
    }
    hook.prior = prior;
    return indexPath;
  } catch (error) {
    restoreGeneratedFiles(prior);
    throw error;
  }
}
function restoreGeneratedFiles(prior) {
  for (const item of prior) {
    if (item.absent) { if (existsSync(item.file)) unlinkSync(item.file); }
    else writeFileSync(item.file, item.bytes, { mode: item.mode });
  }
}
function saveAndCommit(root, run, rel, value, expectedHead, transport, print, hook) {
  ensureCleanMetadata(root, run, [rel]);
  if (expectedHead && git(run, root, ["rev-parse", "HEAD"]) !== expectedHead) throw new Error("repository HEAD changed during ownership ceremony");
  atomicWrite(path.join(root, rel), value);
  const head = git(run, root, ["rev-parse", "HEAD"]);
  if (expectedHead && head !== expectedHead) throw new Error("repository HEAD changed before metadata commit");
  const indexPath = runMinimizeReceipt(root, run, rel, hook);
  const env = indexPath ? { env: { ...process.env, GIT_INDEX_FILE: indexPath } } : {};
  if (!indexPath) git(run, root, ["add", "--", rel]);
  let commit;
  try { commit = run("git", ["-C", root, "commit", "--only", "-m", `right-release: update ${rel}`, "--", rel], env); }
  finally { if (hook) restoreGeneratedFiles(hook.prior ?? []); }
  if (commit.status !== 0) throw new Error(String(commit.stderr || "exact-path metadata commit failed").trim());
  if (indexPath) {
    const reset = run("git", ["-C", root, "reset", "--quiet", "HEAD", "--", rel]);
    if (reset.status !== 0) throw new Error("metadata commit succeeded but its generated path could not be reconciled in the repository index");
  }
  if (git(run, root, ["rev-parse", "HEAD"]) === head) throw new Error("metadata commit did not advance repository HEAD");
  if (transport?.offline) print(transport.reason === "network unavailable"
    ? "Ownership metadata committed locally; queued offline (network unavailable)."
    : `Ownership metadata committed locally (${transport.reason}; local-only).`);
  else if (transport?.push) {
    const push = runNetworkGit(run, ["-C", root, "push", "origin", transport.branch]);
    if (push.status !== 0) {
      if (networkUnavailable(push)) print("Ownership metadata committed locally; push queued offline (network unavailable).");
      else print("Ownership metadata committed locally; push was rejected and requires Git reconciliation.");
    }
  }
}
function helperFor(run) {
  if (!platform) throw new Error(`ownership key management unsupported platform: ${process.platform}`);
  const config = parse(readFileSync(path.join(here, "ownership-helper-paths.json"), "utf8"), "ownership helper config");
  const item = config?.schema === 1 ? config[platform] : null;
  if (!item || !path.isAbsolute(item.path) || !HEX.test(item.sha256 ?? "")) throw new Error(`ownership helper config lacks a valid ${platform} entry`);
  const bytes = readFileSync(item.path);
  if (sha(bytes) !== item.sha256) throw new Error("ownership native helper hash does not match pinned config");
  return item.path;
}
function invokeHelper(run, helper, args, interactiveRun = run) {
  const interactive = ["--key-operation", "--enroll", "--enroll-root"].includes(args[0]);
  const r = interactive ? interactiveRun(helper, args, { shell: false, windowsHide: false }) : run(helper, args);
  if (r.status !== 0 || String(r.stderr ?? "").includes("RightKit ownership keychain preflight:")) {
    throw new Error(String(r.stderr || "native ownership operation cancelled or failed").trim());
  }
  return String(r.stdout ?? "");
}
function helperFile(run, helper, args, outPath, what) {
  invokeHelper(run, helper, args, run.interactive ?? run);
  return parse(readFileSync(outPath, "utf8"), what);
}
function readRequestFile(file) {
  const stat = lstatSync(file);
  if (stat.isSymbolicLink() || !stat.isFile() || stat.size < 2 || stat.size > 2 * 1024 * 1024) throw new Error("key request must be a bounded regular file");
  return parse(readFileSync(file, "utf8"), "key request");
}
function validateNative(out, expectedHost = platform) {
  if (!out || out.host !== expectedHost || typeof out.keyId !== "string" || typeof out.publicKeyPem !== "string") throw new Error("native helper returned malformed key data");
  const fingerprint = keyFingerprint(out.publicKeyPem);
  if (out.keyId !== `${expectedHost === "windows" ? "win" : "mac"}-${fingerprint}`) throw new Error("native helper key id does not match its public key");
  return { keyId: fingerprint, publicKeyPem: out.publicKeyPem, platform: expectedHost };
}
function elevate(run, requestPath, digest, { modulePath = fileURLToPath(import.meta.url), nodeExe = process.execPath, hostPlatform = process.platform, audit = (target) => approvalTestOnly.protectedLocation(target, hostPlatform), label, tty = hasTerminal() } = {}) {
  let child, wrapper, registry, currentNode;
  const refuse = (message) => { throw Object.assign(new Error(`RK_OWNERSHIP_ELEVATION_UNPROTECTED: ${message}`), { code: "RK_OWNERSHIP_ELEVATION_UNPROTECTED", type: "RK_OWNERSHIP_ELEVATION_UNPROTECTED" }); };
  try {
    if (!path.isAbsolute(requestPath) || !HEX.test(digest ?? "")) refuse("request path or SHA-256 is malformed");
    const requestStat = lstatSync(requestPath);
    if (!requestStat.isFile() || requestStat.isSymbolicLink() || (hostPlatform === "win32" && (requestStat.attributes & 0x400)) || requestStat.size > 2 * 1024 * 1024) refuse("request must be a bounded regular non-linked file");
    if (sha(readFileSync(requestPath)) !== digest) refuse("request bytes do not match pinned SHA-256");
    const trustPath = DEFAULT_TRUST_PATH[hostPlatform];
    if (!trustPath) refuse(`ownership trust path is unavailable for ${hostPlatform}`);
    const trustDirectory = path.dirname(trustPath);
    for (const target of [trustDirectory, trustPath]) {
      try {
        const stat = lstatSync(target);
        if (stat.isSymbolicLink() || (hostPlatform === "win32" && (stat.attributes & 0x400))) refuse(`protected trust path is linked: ${target}`);
        if (target === trustDirectory && !stat.isDirectory()) refuse("protected trust location is not a directory");
        if (target === trustPath && !stat.isFile()) refuse("protected trust file is not a regular file");
      } catch (error) { if (error.code !== "ENOENT") throw error; }
    }
    const protectedTarget = existsSync(trustPath) ? trustPath : existsSync(trustDirectory) ? trustDirectory : null;
    if (protectedTarget) {
      const protection = audit(protectedTarget);
      if (protection?.ok !== true) refuse((protection?.problems ?? ["protected trust location is unprotected"]).join("; "));
    }
    const source = realpathSync(modulePath);
    const sourceDir = path.dirname(source);
    child = path.join(sourceDir, "ownership-key-trust.mjs");
    wrapper = path.join(sourceDir, "ownership-key-authority.mjs");
    registry = path.join(sourceDir, "ownership-repositories.json");
    currentNode = realpathSync(nodeExe);
    for (const target of [source, child, wrapper, registry, currentNode]) {
      const stat = lstatSync(target);
      if (!stat.isFile() || stat.isSymbolicLink() || (hostPlatform === "win32" && (stat.attributes & 0x400)) || realpathSync(target) !== target) refuse(`elevated input must be a regular non-linked file: ${target}`);
    }
  } catch (error) {
    if (error?.code === "RK_OWNERSHIP_ELEVATION_UNPROTECTED") throw error;
    refuse(`installed ownership elevation cannot be verified: ${error.message}`);
  }
  const args = [wrapper, "--trust-script", child, "--request", requestPath, "--sha256", digest];
  if (hostPlatform === "win32") {
    const invocation = buildWindowsElevationInvocation(currentNode, child, requestPath, digest, wrapper);
    const r = run(invocation.parentExecutable, invocation.parentArgs, { env: windowsPowerShellEnvironment() });
    if (r.status !== 0) throw new Error("elevated trust update failed or was cancelled");
  } else {
    const i = buildMacInvocation({ tty, nodeExe: currentNode, args, label });
    let r;
    try { r = run(i.command, i.args, i.options); }
    finally { if (tty) { try { run("/usr/bin/sudo", ["-K"], { stdio: "inherit" }); } catch {} } }
    if (r.status !== 0) throw new Error("elevated trust update failed or was cancelled");
  }
}
function buildWindowsElevationInvocation(nodeExe, trustScript, requestPath, digest, checkpointScript) {
  const powershell = "C:\\Windows\\System32\\WindowsPowerShell\\v1.0\\powershell.exe";
  const quote = (value) => `'${value.replaceAll("'", "''")}'`;
  const childArgs = checkpointScript ? [nodeExe, checkpointScript, "--trust-script", trustScript, "--request", requestPath, "--sha256", digest] : [nodeExe, trustScript, "--request", requestPath, "--sha256", digest];
  const childScript = `& ${childArgs.map(quote).join(" ")}; exit $LASTEXITCODE`;
  const childEncoded = Buffer.from(childScript, "utf16le").toString("base64");
  const parentScript = `$ErrorActionPreference='Stop'; try { $env:NODE_OPTIONS=$null; $env:NODE_PATH=$null; $p=Start-Process -FilePath ${quote(powershell)} -ArgumentList ${quote(`-NoProfile -EncodedCommand ${childEncoded}`)} -Verb RunAs -WindowStyle Hidden -Wait -PassThru; exit $p.ExitCode } catch { exit 1 }`;
  return { parentExecutable: powershell, parentArgs: ["-NoProfile", "-EncodedCommand", Buffer.from(parentScript, "utf16le").toString("base64")], childScript };
}
export const __testOnly = Object.freeze({ elevate, buildWindowsElevationInvocation, assertSafeMetadataParents, detectMinimizeHook, trustSyncGate, installCheckpoint });
function trustState() { return loadTrustedExternal(); }
function currentLogTrust(root, log, trust = trustState(), authority, injected = false) {
  if (!trust.trust?.rootFingerprint) throw new Error("protected root trust is not enrolled on this host");
  if (!sameAuthority(trust.trust.keyAuthority, authority)) throw new Error("protected key authority is bound to a different repository checkout");
  if (!trust.trust.keyAuthority) throw new Error("protected key trust has no repository authority");
  if (!injected) {
    const committed = readCommittedKeyAuthority(trust.trust);
    if (!committed.ok || committed.authority.repositoryIdentity !== authority.repositoryIdentity
      || !sameAuthority(trust.trust.keyAuthority, authority) || !Buffer.from(committed.bytes).equals(Buffer.from(json(log)))) {
      throw new Error(`committed ownership key authority rejected: ${(committed.problems ?? []).join("; ") || "local log differs from committed bytes"}`);
    }
  }
  const resolved = resolveKeyLogTrust(trust.trust, Buffer.from(json(log)));
  if (!resolved.ok) throw new Error(`ownership key log rejected: ${resolved.problems.join("; ")}`);
  return resolved;
}
function makeTrustRequest(log, rootFingerprint, baseline, authority) {
  const file = temp("trust-request");
  const bytes = Buffer.from(json({ schema: 1, log, rootFingerprint, baseline, authority }));
  writeFileSync(file, bytes, { flag: "wx", mode: 0o600 });
  return { file, digest: sha(bytes) };
}

export async function main(args, deps = {}) {
  const runner = deps.runner ?? spawnSync;
  const interactiveRun = deps.runner ?? spawnSyncInteractive;
  const run = (...runArgs) => runner(...runArgs);
  run.interactive = interactiveRun;
  const print = deps.print ?? console.log;
  const app = path.resolve(flag(args, "--app") ?? process.cwd());
  const root = deps.root ?? repoRoot(app, run);
  try {
    if (!platform) throw new Error(`ownership key management unsupported platform: ${process.platform}`);
    // Delegate BEFORE taking the command lock: the desktop-session child needs that same lock.
    if (args[0] === "trust-sync" && needsDesktopDelegation({ hostPlatform: deps.hostPlatform, env: deps.env, run, elevatedCheck: deps.elevatedCheck })) return delegateTrustSync(deps, run, print);
    const helper = deps.helper ?? helperFor(run);
    const lockPath = deps.lockPath ?? path.join(os.tmpdir(), "rightkit-key-command.lock");
    let lockFd;
    lockFd = acquireCommandLock(lockPath);
    try {
      commandTemp = mkdtempSync(path.join(os.tmpdir(), "rightkit-key-command-"));
      const authority = resolveAuthority(root, run, deps.authority);
      const hook = detectMinimizeHook(root, run, deps);
      const transport = syncBeforeCeremony(root, run);
      const headSnapshot = git(run, root, ["rev-parse", "HEAD"]);
      const verb = args[0];
      if (verb === "enroll") return await enroll(args, { ...deps, run, print, root, helper, headSnapshot, transport, authority, hook });
      if (verb === "approve-key") return await approve(args, { ...deps, run, print, root, helper, headSnapshot, transport, authority, hook });
      if (verb === "revoke-key") return await revoke(args, { ...deps, run, print, root, helper, headSnapshot, transport, authority, hook });
      if (verb === "trust-sync") return await trustSync(args, { ...deps, run, print, root, helper, headSnapshot, transport, authority, hook });
      throw new Error("expected enroll, approve-key, revoke-key, or trust-sync");
    } finally {
      closeSync(lockFd);
      rmSync(lockPath, { force: true });
      if (commandTemp) rmSync(commandTemp, { recursive: true, force: true });
      commandTemp = undefined;
    }
  } catch (error) { print(`[right-release] ownership ${args[0] ?? "keys"} failed: ${error.message}`); return 1; }
}
const pidAlive = (pid) => { try { process.kill(pid, 0); return true; } catch (e) { return e.code === "EPERM"; } };
/** Exclusive lock file holding the owner pid; a lock whose pid is dead (killed run) is reclaimed. */
export function acquireCommandLock(lockPath, { alive = pidAlive, now = Date.now } = {}) {
  for (let attempt = 0; attempt < 2; attempt += 1) {
    try {
      const fd = openSync(lockPath, "wx", 0o600);
      writeSync(fd, String(process.pid));
      return fd;
    } catch (error) {
      if (error.code !== "EEXIST") throw new Error("another ownership key command is active for this repository");
      let stale = false;
      try {
        const pid = Number.parseInt(readFileSync(lockPath, "utf8").trim(), 10);
        stale = Number.isInteger(pid) && pid > 0 ? !alive(pid) : now() - lstatSync(lockPath).mtimeMs > 60_000;
      } catch { stale = false; }
      if (!stale) throw new Error("another ownership key command is active for this repository");
      try { unlinkSync(lockPath); } catch {}
    }
  }
  throw new Error("another ownership key command is active for this repository");
}
function flag(args, name) { const i = args.indexOf(name); return i < 0 ? undefined : args[i + 1]; }

async function enroll(args, d) {
  const { run, print, root, helper } = d;
  const name = flag(args, "--name") ?? "";
  assertSafeMetadataParents(root, LOG);
  assertSafeMetadataParents(root, `${REQUEST_DIR}/pending.json`);
  const log = readLog(root);
  if (log.events.length === 0) {
    if (!args.includes("--root")) throw new Error("empty key log requires explicit --root");
    ensureCleanMetadata(root, run, [LOG]);
    const baseline = d.baseline ?? parse(readFileSync(path.join(here, "ownership-default-policy.json"), "utf8"), "ownership default policy");
    const out = temp("genesis-signed");
    const nativeEvent = helperFile(run, helper, ["--enroll-root", "--name", name || "Root device", "--out", out], out, "signed genesis event");
    const record = nativeEvent?.payload?.subject;
    if (!record || record.platform !== platform || record.deviceName !== (name || "Root device") || record.keyId !== keyFingerprint(record.publicKeyPem)) throw new Error("native helper returned malformed signed root event");
    try {
      // Root enrollment uses one native authorization call; helper returns its signed event beside key data.
      const next = { schema: 1, events: [nativeEvent] };
      const checked = verifyKeyLog(next, { rootFingerprint: record.keyId });
      if (!checked.ok) throw new Error(`native root signature invalid: ${checked.problems.join("; ")}`);
      saveAndCommit(root, run, LOG, json(next), d.headSnapshot, d.transport, print, d.hook);
      const request = makeTrustRequest(next, record.keyId, baseline, d.authority);
      try { (d.elevate ?? elevate)(run, request.file, request.digest); } finally { await import("node:fs").then(({ unlinkSync }) => unlinkSync(request.file)).catch(() => {}); }
      postVerify(root, next, d);
      print(`Ownership root enrolled: ${record.keyId}; active keys: 1`);
      return 0;
    } finally { await import("node:fs").then(({ unlinkSync }) => unlinkSync(out)).catch(() => {}); }
  }
  if (args.includes("--root")) return resumeRootEnrollment(log, d);
  const pinned = flag(args, "--root-fingerprint");
  if (!HEX.test(pinned ?? "")) throw new Error("additional enrollment requires --root-fingerprint");
  const checked = verifyKeyLog(log, { rootFingerprint: pinned });
  if (!checked.ok) throw new Error(`local key log rejected: ${checked.problems.join("; ")}`);
  const hostTrust = (d.trustLoader ?? trustState)();
  if (hostTrust?.trust?.keyAuthority && !sameAuthority(hostTrust.trust.keyAuthority, d.authority)) throw new Error("protected key authority is bound to a different repository checkout");
  if (hostTrust?.trust?.rootFingerprint) currentLogTrust(root, log, hostTrust, d.authority, Boolean(d.resolveTrust));
  const publicPath = temp("public");
  let publicRecord;
  try {
    publicRecord = validateNative(helperFile(run, helper, ["--public-key", "--out", publicPath], publicPath, "native public key"));
  } catch { /* Fresh devices may not have a public key; pending requests are checked before enrollment resumes. */ }
  if (!publicRecord) {
    const trust = (d.trustLoader ?? trustState)();
    const requestDir = path.join(root, REQUEST_DIR);
    if (!trust?.trust && existsSync(requestDir)) {
      for (const name of readdirSync(requestDir)) {
        if (!name.endsWith(".json")) continue;
        const prior = readRequestFile(path.join(requestDir, name));
        if (verifyKeyRequest(prior, { rootFingerprint: pinned }).ok) throw new Error("pending request exists but native public-key discovery failed; refusing to create a second native key");
      }
    }
  }
  if (publicRecord) {
    const pendingRel = `${REQUEST_DIR}/${publicRecord.keyId}.json`;
    const pendingPath = path.join(root, pendingRel);
    if (existsSync(pendingPath)) {
      const request = readRequestFile(pendingPath);
      const valid = verifyKeyRequest(request, { rootFingerprint: pinned });
      if (!valid.ok || request.key.keyId !== publicRecord.keyId || request.key.platform !== publicRecord.platform) throw new Error("existing enrollment request does not match this native key and root pin");
      ensureCleanMetadata(root, run, [LOG, pendingRel]);
      const trust = (d.trustLoader ?? trustState)();
      if (trust?.trust) throw new Error("matching request already exists and protected trust is present; refusing a second enrollment");
      const baseline = d.baseline ?? parse(readFileSync(path.join(here, "ownership-default-policy.json"), "utf8"), "ownership default policy");
      await installCheckpoint(log, pinned, baseline, d);
      postVerify(root, log, d);
      print(`Enrollment recovery completed for ${publicRecord.keyId}; request ${pendingRel} remains pending approval`);
      return 0;
    }
  }
  const outPath = temp("enroll");
  const native = helperFile(run, helper, ["--enroll", "--out", outPath], outPath, "native enrollment key");
  const record = { ...validateNative(native), deviceName: name || `${platform} device` };
  if (checked.allKeys.some((key) => key.keyId === record.keyId)) throw new Error("this native key already appears in the key log");
  const request = { schema: 1, type: "rightkit.ownership-key-request.v1", rootFingerprint: pinned, key: record };
  const verified = verifyKeyRequest(request, { rootFingerprint: pinned });
  if (!verified.ok) throw new Error(verified.problems.join("; "));
  const reqRel = `${REQUEST_DIR}/${record.keyId}.json`;
  ensureCleanMetadata(root, run, [reqRel]);
  saveAndCommit(root, run, reqRel, json(request), d.headSnapshot, d.transport, print, d.hook);
  const baseline = d.baseline ?? parse(readFileSync(path.join(here, "ownership-default-policy.json"), "utf8"), "ownership default policy");
  const trustReq = makeTrustRequest(log, pinned, baseline, d.authority);
  try { (d.elevate ?? elevate)(run, trustReq.file, trustReq.digest); }
  finally { await import("node:fs").then(({ unlinkSync }) => unlinkSync(trustReq.file)).catch(() => {}); }
  postVerify(root, log, d);
  print(`Enrollment request created: ${record.keyId}; active keys: ${checked.keys.length}; request ${reqRel}; approve on active host`);
  return 0;
}
async function resumeRootEnrollment(log, d) {
  const { run, print, root, helper } = d;
  if (!log.events.length) throw new Error("empty key log requires explicit --root initialization");
  const trust = (d.trustLoader ?? trustState)();
  if (trust?.trust) throw new Error("--root cannot replace an existing protected trust root");
  ensureCleanMetadata(root, run, [LOG]);
  const publicPath = temp("public");
  const publicKey = validateNative(helperFile(run, helper, ["--public-key", "--out", publicPath], publicPath, "native public key"));
  const pin = log.events[0]?.payload?.rootFingerprint;
  if (publicKey.keyId !== pin || publicKey.platform !== log.events[0]?.payload?.subject?.platform) throw new Error("existing root log does not match this host's unauthenticated native public key");
  const state = verifyKeyLog(log, { rootFingerprint: pin });
  if (!state.ok || state.keys[0]?.keyId !== pin) throw new Error(`existing root log is invalid: ${state.problems.join("; ")}`);
  const baseline = d.baseline ?? parse(readFileSync(path.join(here, "ownership-default-policy.json"), "utf8"), "ownership default policy");
  await installCheckpoint(log, pin, baseline, d);
  postVerify(root, log, d);
  print(`Ownership root enrollment recovery completed: ${pin}; active keys: ${state.keys.length}`);
  return 0;
}
function delegateTrustSync(deps, run, print) {
  const result = delegateToDesktop({ run, command: deps.desktopCommand ?? [process.execPath, ...(process.argv[1] ? [path.resolve(process.argv[1])] : []), ...process.argv.slice(2)], ...(deps.desktopOptions ?? {}) });
  if (result.output) print(result.output.trimEnd());
  if (result.status !== 0) throw new Error(`desktop-session trust-sync failed (exit ${result.status}); see output above`);
  return 0;
}
/** Advance this host's protected checkpoint to the committed key log (after another host approved or revoked a key). */
async function trustSync(_args, d) {
  d = { ...d, label: "RightKit ownership trust-sync", gate: trustSyncGate(d) };
  const log = readLog(d.root);
  const pin = log.events[0]?.payload?.rootFingerprint;
  const state = verifyKeyLog(log, { rootFingerprint: pin });
  if (!state.ok) throw new Error(`committed key log is invalid: ${state.problems.join("; ")}`);
  const trust = (d.trustLoader ?? trustState)();
  if (!trust?.trust?.rootFingerprint) throw new Error("protected root trust is not enrolled on this host; run ownership enroll");
  if (trust.trust.rootFingerprint !== pin) throw new Error("committed key log root does not match this host's protected trust");
  const baseline = d.baseline ?? parse(readFileSync(path.join(here, "ownership-default-policy.json"), "utf8"), "ownership default policy");
  const installed = await installCheckpoint(log, pin, baseline, d);
  if (installed === false) { d.print("trust already current"); return 0; }
  postVerify(d.root, log, d);
  d.print(`Ownership trust synced to key-log sequence ${log.events.length - 1}; active keys: ${state.keys.length}`);
  return 0;
}
/**
 * Informed-consent gate: summarize the exact request bytes (digest-checked) against the installed trust,
 * skip when nothing changes, otherwise require explicit approval before any elevation prompt.
 */
function trustSyncGate(d) {
  return (request) => {
    const existing = (d.trustLoader ?? trustState)()?.trust ?? null;
    const summary = (d.summarize ?? summarizeRequestFile)({ file: request.file, digest: request.digest, existing, ...(d.summaryInputs ?? {}) });
    if (!summary.changed) return false;
    (d.confirm ?? confirmTrustChange)({ lines: summary.lines, run: d.run, interactiveRun: d.run.interactive, print: d.print, hostPlatform: d.hostPlatform, ...(d.confirmOptions ?? {}) });
    return true;
  };
}
async function installCheckpoint(log, rootFingerprint, baseline, d) {
  const request = makeTrustRequest(log, rootFingerprint, baseline, d.authority);
  try {
    if (d.gate && (await d.gate(request)) === false) return false;
    await (d.elevate ?? elevate)(d.run, request.file, request.digest, d.label ? { label: d.label } : undefined);
    return true;
  }
  finally { await import("node:fs").then(({ unlinkSync }) => unlinkSync(request.file)).catch(() => {}); }
}

async function approve(args, d) {
  const { run, print, root, helper } = d;
  assertSafeMetadataParents(root, LOG);
  assertSafeMetadataParents(root, `${REQUEST_DIR}/pending.json`);
  const target = args[1];
  if (!target) throw new Error("approve-key requires a fingerprint or request JSON path");
  const log = readLog(root);
  const pin = log.events[0]?.payload?.rootFingerprint;
  if (!pin) throw new Error("no root key log exists");
  currentLogTrust(root, log, d.trustLoader?.() ?? trustState(), d.authority, Boolean(d.resolveTrust));
  let request;
  if (HEX.test(target)) {
    const file = path.join(root, REQUEST_DIR, `${target}.json`);
    request = readRequestFile(file);
  } else {
    const file = path.resolve(root, target);
    if (!file.startsWith(`${path.join(root, REQUEST_DIR)}${path.sep}`)) throw new Error("request path must be inside .rightkit/key-requests");
    request = readRequestFile(file);
  }
  const valid = verifyKeyRequest(request, { rootFingerprint: pin });
  if (!valid.ok) throw new Error(valid.problems.join("; "));
  if (verifyKeyLog(log, { rootFingerprint: pin }).allKeys.some((key) => key.keyId === request.key.keyId)) throw new Error("requested key already appears in log");
  const signerPath = temp("public");
  const signerData = helperFile(run, helper, ["--public-key", "--out", signerPath], signerPath, "native public key");
  const signer = validateNative(signerData);
  const state = verifyKeyLog(log, { rootFingerprint: pin });
  if (!state.keys.some((key) => key.keyId === signer.keyId)) throw new Error("this host's key is not active in protected key log");
  const next = await (d.signEvent ? d.signEvent(log, "add", request.key, signer) : signWithHelper(run, helper, log, "add", request.key, signer));
  const rel = LOG;
  saveAndCommit(root, run, rel, json(next), d.headSnapshot, d.transport, print, d.hook);
  await updateCheckpoint(next, pin, d);
  postVerify(root, next, d);
  print(`Ownership key approved: ${request.key.keyId}; active keys: ${verifyKeyLog(next, { rootFingerprint: pin }).keys.length}`);
  return 0;
}
async function revoke(args, d) {
  const { run, print, root, helper } = d;
  assertSafeMetadataParents(root, LOG);
  const keyId = args[1];
  if (!HEX.test(keyId ?? "")) throw new Error("revoke-key requires a lowercase SHA-256 fingerprint");
  const log = readLog(root), pin = log.events[0]?.payload?.rootFingerprint;
  if (!pin) throw new Error("no root key log exists");
  currentLogTrust(root, log, d.trustLoader?.() ?? trustState(), d.authority, Boolean(d.resolveTrust));
  const signerPath = temp("public");
  const signerData = helperFile(run, helper, ["--public-key", "--out", signerPath], signerPath, "native public key");
  const signer = validateNative(signerData);
  const state = verifyKeyLog(log, { rootFingerprint: pin });
  if (!state.keys.some((key) => key.keyId === signer.keyId)) throw new Error("this host's key is not active in protected key log");
  const next = await (d.signEvent ? d.signEvent(log, "revoke", { keyId }, signer) : signWithHelper(run, helper, log, "revoke", { keyId }, signer));
  saveAndCommit(root, run, LOG, json(next), d.headSnapshot, d.transport, print, d.hook);
  await updateCheckpoint(next, pin, d);
  postVerify(root, next, d);
  print(`Ownership key revoked: ${keyId}; active keys: ${verifyKeyLog(next, { rootFingerprint: pin }).keys.length}`);
  return 0;
}
async function signWithHelper(run, helper, log, operation, subject, signer) {
  const draft = createKeyEvent(log, { operation, subject, signerKeyId: signer.keyId });
  const input = temp("draft"), output = temp("draft-signed");
  writeFileSync(input, json(draft), { flag: "wx", mode: 0o600 });
  try {
    const event = helperFile(run, helper, ["--key-operation", input, "--out", output], output, "signed key event");
    if (canonical(event?.payload) !== canonical(draft.payload)) throw new Error("native signer changed key event payload");
    const next = { schema: 1, events: [...log.events, event] };
    const result = verifyKeyLog(next, { rootFingerprint: draft.payload.rootFingerprint });
    if (!result.ok) throw new Error(`signed key event rejected: ${result.problems.join("; ")}`);
    return next;
  } finally { await import("node:fs").then(({ unlinkSync }) => unlinkSync(input)).catch(() => {}); await import("node:fs").then(({ unlinkSync }) => unlinkSync(output)).catch(() => {}); }
}
async function updateCheckpoint(log, rootFingerprint, d) {
  const request = d.trustRequest ? d.trustRequest(log, rootFingerprint) : (() => {
    const file = temp("trust");
    const bytes = Buffer.from(json({ schema: 1, log, rootFingerprint, baseline: null, authority: d.authority })); writeFileSync(file, bytes, { flag: "wx", mode: 0o600 }); return { file, digest: sha(bytes) };
  })();
  try { await (d.elevate ?? elevate)(d.run, request.file, request.digest); }
  finally { await import("node:fs").then(({ unlinkSync }) => unlinkSync(request.file)).catch(() => {}); }
}
function postVerify(root, log, d) {
  const state = (d.trustLoader ?? trustState)();
  if (!sameAuthority(state.trust?.keyAuthority, d.authority)) throw new Error("installed key authority does not match this repository checkout");
  const committed = d.resolveTrust ? { ok: true, bytes: Buffer.from(json(log)), authority: { repositoryIdentity: d.authority.repositoryIdentity }, problems: [] } : readCommittedKeyAuthority(state.trust);
  if (!committed.ok || committed.authority.repositoryIdentity !== d.authority.repositoryIdentity
    || !Buffer.from(committed.bytes).equals(Buffer.from(json(log)))) throw new Error(`committed ownership key authority rejected: ${(committed.problems ?? []).join("; ") || "committed log differs from installed log"}`);
  const checked = (d.resolveTrust ?? resolveKeyLogTrust)(state.trust, committed.bytes);
  if (!checked.ok) throw new Error(`post-install verification failed: ${checked.problems.join("; ")}`);
}
