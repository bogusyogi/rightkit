import { createHash } from "node:crypto";
import { realpathSync, statSync } from "node:fs";
import path from "node:path";
import { spawnSync } from "./background-process.mjs";
import { canonicalRepositoryIdentity, resolveKeyLogTrust } from "./ownership-approval.mjs";

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
  const checkoutPath = realpathSync(authority.checkoutPath);
  if (!statSync(checkoutPath).isDirectory()) throw new Error("protected key authority checkoutPath must be a directory");
  return { repositoryIdentity, checkoutPath };
}

/** Read one exact key-log blob from the protected authority checkout's Git HEAD. */
export function readCommittedKeyAuthority(trust) {
  try {
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
    const resolved = resolveKeyLogTrust(trust, result.bytes);
    if (!resolved.ok) throw new Error(`authority key log verification failed: ${resolved.problems.join("; ")}`);
    return { ok: true, problems: [], bytes: Buffer.from(result.bytes), digest: result.digest, rootFingerprint: result.rootFingerprint, keyCheckpoint: result.keyCheckpoint, resolvedTrust: resolved.trust, authority: { repositoryIdentity: expected.repositoryIdentity, commit, tree } };
  } catch (error) {
    return { ok: false, problems: [error?.message ?? "protected key authority could not be read"] };
  }
}

export const __testOnly = Object.freeze({ MAX_LOG_BYTES });
