import { createHash } from "node:crypto";
import { closeSync, constants, fsyncSync, lstatSync, mkdirSync, openSync, readFileSync, realpathSync, renameSync, statSync, unlinkSync, writeFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "./background-process.mjs";
import { canonicalRepositoryIdentity, resolveKeyLogTrust, trustedPath, __testOnly as approvalTestOnly } from "./ownership-approval.mjs";

import { performProtectedTrustUpdate } from "./ownership-key-trust.mjs";

const HEX = /^[a-f0-9]{64}$/;
const LOG_PATH = ".rightkit/ownership-keys.json";
const MAX_LOG_BYTES = 2 * 1024 * 1024;
const executable = process.platform === "win32" ? "C:\\Program Files\\Git\\cmd\\git.exe" : "/usr/bin/git";
const cache = new Map();

function gitEnv() {
  const env = Object.fromEntries(Object.entries(process.env).filter(([key]) => !/^GIT_/i.test(key)));
  env.GIT_CONFIG_NOSYSTEM = "1";
  env.GIT_CONFIG_GLOBAL = process.platform === "win32" ? "NUL" : "/dev/null";
  env.GIT_OPTIONAL_LOCKS = "0";
  env.GIT_TERMINAL_PROMPT = "0";
  return env;
}

function git(root, args, encoding = "utf8") {
  const safeDirectory = process.platform === "win32" ? root.replaceAll("\\", "/") : root;
  const result = spawnSync(executable, ["-C", root, "--no-replace-objects", "-c", `safe.directory=${safeDirectory}`, "-c", "core.fsmonitor=false", "-c", `core.hooksPath=${process.platform === "win32" ? "NUL" : "/dev/null"}`, ...args], {
    encoding, env: gitEnv(), windowsHide: true, maxBuffer: MAX_LOG_BYTES + 4096, timeout: 15_000,
  });
  if (result?.error || result?.status !== 0) throw new Error(`authority Git ${args[0]} failed`);
  return result.stdout;
}

function validateAuthority(authority) {
  if (!authority || typeof authority !== "object" || Array.isArray(authority)
    || Object.keys(authority).sort().join("\0") !== ["checkoutPath", "repositoryIdentity"].sort().join("\0")) throw new Error("protected key authority fields are malformed");
  if (typeof authority.checkoutPath !== "string" || !path.isAbsolute(authority.checkoutPath) || authority.checkoutPath.includes("\0")) throw new Error("protected key authority checkoutPath must be absolute");
  const repositoryIdentity = canonicalRepositoryIdentity(authority.repositoryIdentity);
  if (repositoryIdentity !== authority.repositoryIdentity) throw new Error("protected key authority identity must be canonical");
  const checkoutPath = realpathSync.native(authority.checkoutPath);
  if (!statSync(checkoutPath).isDirectory()) throw new Error("protected key authority checkoutPath must be a directory");
  return { repositoryIdentity, checkoutPath };
}

function checkpointPath(file, rootFingerprint) {
  if (!HEX.test(rootFingerprint ?? "")) throw new Error("protected checkpoint root fingerprint is malformed");
  return path.join(path.dirname(file), "checkpoints", `${rootFingerprint}.json`);
}
function auditPath(file, requireNonElevated = true) {
  const result = approvalTestOnly.protectedLocation(file, process.platform, { requireNonElevated });
  if (!result.ok) throw new Error(`protected checkpoint audit failed: ${result.problems.join("; ")}`);
}
function boundedJson(file, audit) {
  const st = lstatSync(file);
  if (!st.isFile() || st.isSymbolicLink() || st.size > MAX_LOG_BYTES) throw new Error("protected checkpoint input must be a bounded regular file");
  audit(file);
  return JSON.parse(readFileSync(file, "utf8"));
}
function validCheckpoint(value) {
  return value && Object.keys(value).sort().join("\0") === "head\0sequence" && Number.isSafeInteger(value.sequence) && value.sequence >= 0 && HEX.test(value.head ?? "");
}
function resolveProtectedCheckpoint(trust, bytes, file, audit) {
  const disk = boundedJson(file, audit);
  if (!disk || disk.rootFingerprint !== trust.rootFingerprint || !validCheckpoint(disk.keyCheckpoint)) throw new Error("protected checkpoint trust pin is malformed or differs from reader pin");
  const floors = [trust.keyCheckpoint, disk.keyCheckpoint];
  const statePath = checkpointPath(file, trust.rootFingerprint);
  let state;
  try { state = boundedJson(statePath, audit); }
  catch (error) { if (error.code !== "ENOENT") throw error; }
  if (state) {
    if (Object.keys(state).sort().join("\0") !== "keyCheckpoint\0rootFingerprint\0schema" || state.schema !== 1 || state.rootFingerprint !== trust.rootFingerprint || !validCheckpoint(state.keyCheckpoint)) throw new Error("protected checkpoint state is malformed");
    floors.push(state.keyCheckpoint);
  }
  let resolved;
  for (const keyCheckpoint of floors) {
    resolved = resolveKeyLogTrust({ ...trust, keyCheckpoint }, bytes);
    if (!resolved.ok) throw new Error(`authority key log verification failed: ${resolved.problems.join("; ")}`);
  }
  return { resolved, statePath, state };
}
function syncDirectory(directory) {
  let fd;
  try { fd = openSync(directory, "r"); fsyncSync(fd); }
  catch (error) { if (process.platform !== "win32" || !["EISDIR", "EPERM"].includes(error.code)) throw error; }
  finally { if (fd !== undefined) closeSync(fd); }
}
// Called only inside the existing elevated trust-update ceremony.
function persistCheckpoint(trust, bytes, file, audit) {
  const directory = path.dirname(checkpointPath(file, trust.rootFingerprint));
  audit(path.dirname(file));
  try { mkdirSync(directory, { mode: 0o755 }); } catch (error) { if (error.code !== "EEXIST") throw error; }
  if (!lstatSync(directory).isDirectory() || lstatSync(directory).isSymbolicLink()) throw new Error("protected checkpoint directory is missing or linked");
  audit(directory);
  const lock = path.join(directory, `${trust.rootFingerprint}.lock`);
  let lockFd;
  try { lockFd = openSync(lock, constants.O_WRONLY | constants.O_CREAT | constants.O_EXCL | (constants.O_NOFOLLOW ?? 0), 0o600); }
  catch (error) { throw new Error(error.code === "EEXIST" ? "protected checkpoint state lock already exists; service/admin recovery is required" : `cannot lock protected checkpoint state: ${error.message}`); }
  const temp = path.join(directory, `.${trust.rootFingerprint}.${process.pid}.${Date.now()}.tmp`);
  let fd;
  try {
    writeFileSync(lockFd, `${process.pid}\n`); fsyncSync(lockFd);
    const current = resolveProtectedCheckpoint(trust, bytes, file, audit);
    const state = { schema: 1, rootFingerprint: trust.rootFingerprint, keyCheckpoint: current.resolved.checkpoint };
    fd = openSync(temp, constants.O_WRONLY | constants.O_CREAT | constants.O_EXCL | (constants.O_NOFOLLOW ?? 0), 0o644);
    writeFileSync(fd, `${JSON.stringify(state)}\n`); fsyncSync(fd); closeSync(fd); fd = undefined;
    audit(temp);
    renameSync(temp, current.statePath); syncDirectory(directory);
    const readback = resolveProtectedCheckpoint(trust, bytes, file, audit);
    if (readback.state?.keyCheckpoint.sequence !== state.keyCheckpoint.sequence || readback.state?.keyCheckpoint.head !== state.keyCheckpoint.head) throw new Error("protected checkpoint readback differs from verified log");
    return state.keyCheckpoint;
  } finally {
    if (fd !== undefined) closeSync(fd);
    try { unlinkSync(temp); } catch (error) { if (error.code !== "ENOENT") throw error; }
    closeSync(lockFd); unlinkSync(lock); syncDirectory(directory);
  }
}
/** Read one exact key-log blob from the protected authority checkout's Git HEAD. */
export function readCommittedKeyAuthority(trust) { return readAuthority(trust); }
function readAuthority(trust, { file, audit = auditPath } = {}) {
  try {
    file ??= trustedPath();
    const expected = validateAuthority(trust?.keyAuthority);
    const top = String(git(expected.checkoutPath, ["rev-parse", "--show-toplevel"])).trim();
    const normalizedTop = process.platform === "win32" ? path.resolve(top).toLowerCase() : path.resolve(top);
    const normalizedExpected = process.platform === "win32" ? expected.checkoutPath.toLowerCase() : expected.checkoutPath;
    if (normalizedTop !== normalizedExpected) throw new Error("protected key authority checkoutPath must be exact Git repository root");
    const origin = String(git(expected.checkoutPath, ["config", "--get", "remote.origin.url"])).trim();
    if (canonicalRepositoryIdentity(origin) !== expected.repositoryIdentity) throw new Error("authority repository origin does not match protected identity");
    const commit = String(git(expected.checkoutPath, ["rev-parse", "--verify", "HEAD^{commit}"])).trim();
    const tree = String(git(expected.checkoutPath, ["rev-parse", "--verify", `${commit}^{tree}`])).trim();
    const listing = git(expected.checkoutPath, ["ls-tree", "-z", commit, "--", LOG_PATH], "buffer");
    const rows = listing.toString("utf8").split("\0").filter(Boolean);
    if (rows.length !== 1) throw new Error("authority committed key log is missing or ambiguous");
    const match = rows[0].match(/^(100644|100755) blob ([0-9a-f]{40}|[0-9a-f]{64})\t\.rightkit\/ownership-keys\.json$/);
    if (!match) throw new Error("authority key log must be a regular committed blob, not a symlink or unsupported mode");
    const oid = match[2];
    const size = Number(String(git(expected.checkoutPath, ["cat-file", "-s", oid])).trim());
    if (!Number.isSafeInteger(size) || size < 0 || size > MAX_LOG_BYTES) throw new Error("authority key log exceeds 2 MiB or has invalid size");
    const bytes = git(expected.checkoutPath, ["cat-file", "blob", oid], "buffer");
    if (!Buffer.isBuffer(bytes) || bytes.length !== size) throw new Error("authority key log blob size changed during read");
    const algorithm = oid.length === 64 ? "sha256" : "sha1";
    const blobOid = createHash(algorithm).update(`blob ${bytes.length}\0`).update(bytes).digest("hex");
    if (blobOid !== oid) throw new Error("authority key log blob hash does not match committed tree");
    const key = `${oid}:${trust.rootFingerprint}:${trust.keyCheckpoint?.sequence}:${trust.keyCheckpoint?.head}`;
    let result = cache.get(key);
    if (!result) {
      const resolved = resolveKeyLogTrust(trust, bytes);
      if (!resolved.ok) throw new Error(`authority key log verification failed: ${resolved.problems.join("; ")}`);
      result = { bytes, digest: resolved.keyLogDigest, rootFingerprint: resolved.trust.rootFingerprint, keyCheckpoint: resolved.checkpoint };
      cache.set(key, result);
      if (cache.size > 64) cache.delete(cache.keys().next().value);
    }
    // Re-resolve on each call so current protected policy is always merged into
    // the cached chain result; approval-policy changes never reuse stale trust.
    const protectedState = resolveProtectedCheckpoint(trust, result.bytes, file, audit);
    const resolved = protectedState.resolved;
    if (!protectedState.state || protectedState.state.keyCheckpoint.sequence < resolved.checkpoint.sequence) throw new Error("checkpoint advance required: run the elevated ownership trust update on this host");
    return { ok: true, problems: [], bytes: Buffer.from(result.bytes), digest: result.digest, rootFingerprint: result.rootFingerprint, keyCheckpoint: resolved.checkpoint, resolvedTrust: resolved.trust, authority: { repositoryIdentity: expected.repositoryIdentity, commit, tree } };
  } catch (error) {
    return { ok: false, problems: [error?.message ?? "protected key authority could not be read"] };
  }
}

export const __testOnly = Object.freeze({ MAX_LOG_BYTES, checkpointPath, readAuthority, persistCheckpoint });

// Wrapper preserves the existing updater's elevation, request digest, lock & policy checks.
const realOrSelf = (file) => { try { return realpathSync(file); } catch { return path.resolve(file); } };
if (process.argv[1] && realOrSelf(process.argv[1]) === realOrSelf(fileURLToPath(import.meta.url))) {
  try {
    if (process.argv.length !== 8 || process.argv[2] !== "--trust-script" || process.argv[4] !== "--request" || process.argv[6] !== "--sha256") throw new Error("usage: ownership-key-authority.mjs --trust-script <installed-script> --request <absolute-json> --sha256 <digest>");
    const script = path.join(path.dirname(fileURLToPath(import.meta.url)), "ownership-key-trust.mjs");
    if (path.resolve(process.argv[3]) !== script || realpathSync(script) !== script) throw new Error("protected trust updater must be the installed sibling script");
    const requestPath = process.argv[5], digest = process.argv[7];
    if (!path.isAbsolute(requestPath) || !HEX.test(digest ?? "")) throw new Error("checkpoint update request path or SHA-256 is malformed");
    const st = lstatSync(requestPath);
    if (!st.isFile() || st.isSymbolicLink() || st.size > MAX_LOG_BYTES) throw new Error("checkpoint update request must be a bounded regular file");
    const bytes = readFileSync(requestPath);
    if (createHash("sha256").update(bytes).digest("hex") !== digest) throw new Error("checkpoint update request SHA-256 mismatch");
    const request = JSON.parse(bytes.toString("utf8"));
    if (!request || request.schema !== 1 || Object.keys(request).some((key) => !["schema", "log", "rootFingerprint", "baseline", "authority"].includes(key))) throw new Error("checkpoint update request schema is malformed");
    const file = trustedPath();
    // Check existing sidecar before the updater changes protected trust.
    try { const existing = boundedJson(file, (target) => auditPath(target, false)); resolveProtectedCheckpoint(existing, Buffer.from(JSON.stringify(request.log)), file, (target) => auditPath(target, false)); }
    catch (error) { if (error.code !== "ENOENT") throw error; }
    const updated = performProtectedTrustUpdate(request);
    const checkpoint = persistCheckpoint(updated.trust, Buffer.from(JSON.stringify(request.log)), file, (target) => auditPath(target, false));
    for (const item of updated.warnings) process.stderr.write(`${item.code}: ${item.message}\n`);
    process.stdout.write(`${JSON.stringify({ path: file, checkpoint, warnings: updated.warnings })}\n`);
  } catch (error) { process.stderr.write(`${error.message}\n`); process.exitCode = 2; }
}
