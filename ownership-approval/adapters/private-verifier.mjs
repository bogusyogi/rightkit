#!/usr/bin/env node
// Installer-owned verifier entrypoint. The installer places this and the
// imported release package beneath the fixed, administrator-protected root.
import { realpathSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "../../packages/release/background-process.mjs";
import { createCommittedOwnershipEvidence } from "../../packages/release/ownership-evidence.mjs";
import { canonicalRepositoryIdentity, loadTrustedExternal, sha256 } from "../../packages/release/ownership-approval.mjs";
import { committedKeyLog, sourceTreeDigest, RECEIPT_SCHEMA, RECEIPT_TYPE } from "./receipt.mjs";
import { nativeGitExecutable, protectedChildEnvironment } from "./runtime-environment.mjs";
import { readCommittedKeyAuthority } from "../../packages/release/ownership-key-authority.mjs";

function arg(args, key) { const i = args.indexOf(key); return i < 0 ? undefined : args[i + 1]; }
function git(root, args) {
  const result = spawnSync(nativeGitExecutable(), ["-c", "core.fsmonitor=false", "-C", root, ...args], { encoding: "buffer", windowsHide: true, timeout: 15_000, env: protectedChildEnvironment() });
  if (result?.status !== 0) throw new Error(`git ${args.join(" ")} failed`);
  return Buffer.isBuffer(result.stdout) ? result.stdout : Buffer.from(result.stdout ?? "");
}

export function verifyPrivateRepository(repoRoot, expectedCommit, expectedTree) {
  const root = path.resolve(repoRoot);
  const commit = git(root, ["rev-parse", "HEAD"]).toString("utf8").trim();
  const tree = git(root, ["rev-parse", `${commit}^{tree}`]).toString("utf8").trim();
  if (commit !== expectedCommit || tree !== expectedTree) throw new Error("checkout changed between adapter and verifier identity reads");
  if (git(root, ["status", "--porcelain=v1", "--untracked-files=all"]).toString("utf8").trim()) throw new Error("working tree is dirty");
  const origin = git(root, ["remote", "get-url", "origin"]).toString("utf8").trim();
  const repositoryIdentity = canonicalRepositoryIdentity(origin);
  const sourceDigest = sourceTreeDigest(git(root, ["ls-tree", "-rz", "--full-tree", commit]));
  const protectedTrust = loadTrustedExternal().trust;
  const hasKeyLog = git(root, ["ls-tree", "-r", "--name-only", commit, "--", ".rightkit/ownership-keys.json"]).toString("utf8").trim() !== "";
  let keyLog = null;
  if (protectedTrust?.keyAuthority) {
    const authority = readCommittedKeyAuthority(protectedTrust);
    if (!authority.ok) throw new Error(`protected key authority rejected: ${authority.problems.join("; ")}`);
    keyLog = authority;
  } else if (hasKeyLog) keyLog = committedKeyLog(root, commit, git, protectedTrust?.rootFingerprint, protectedTrust?.keyCheckpoint);
  const checked = createCommittedOwnershipEvidence(root, root);
  if (checked.evidence.visibility !== "private") throw new Error("registered repository visibility is not private");
  if (!checked.ok) throw new Error(`ownership scan or protected floor failed: ${[...(checked.result.problems ?? []), ...(checked.floor.problems ?? [])].join("; ")}`);
  if (checked.evidence.repositoryIdentity !== repositoryIdentity || checked.evidence.commit !== commit || checked.evidence.tree !== tree) throw new Error("scanner identity differs from independently observed Git source");
  if (keyLog && (checked.evidence.keyLogDigest !== keyLog.digest || checked.evidence.rootFingerprint !== keyLog.rootFingerprint
    || checked.evidence.keyCheckpoint?.sequence !== keyLog.keyCheckpoint.sequence || checked.evidence.keyCheckpoint?.head !== keyLog.keyCheckpoint.head)) throw new Error("scanner evidence differs from independently verified committed key log");
  if (keyLog?.authority && (checked.evidence.keyAuthority?.repositoryIdentity !== keyLog.authority.repositoryIdentity
    || checked.evidence.keyAuthority?.commit !== keyLog.authority.commit || checked.evidence.keyAuthority?.tree !== keyLog.authority.tree)) throw new Error("scanner evidence differs from independently verified key-log authority");
  const trust = protectedTrust;
  const floorInputs = {
    minimumScannerVersion: trust.minimumScannerVersion,
    minimumCatalogVersion: trust.minimumCatalogVersion,
    minimumCatalogDigest: trust.minimumCatalogDigest ?? null,
  };
  return {
    schema: RECEIPT_SCHEMA,
    type: RECEIPT_TYPE,
    ok: true,
    repositoryIdentity,
    commit,
    tree,
    sourceDigest,
    visibility: checked.evidence.visibility,
    registeredRoots: checked.evidence.registeredRoots,
    scannerVersion: checked.evidence.scannerVersion,
    catalogVersion: checked.evidence.catalogVersion,
    catalogDigest: checked.evidence.catalogDigest,
    contentDigest: checked.evidence.contentDigest,
    floors: { ok: checked.evidence.floorOk, digest: sha256(floorInputs) },
    registrationDigest: checked.evidence.registrationDigest,
    ...(keyLog ? { keyLogDigest: keyLog.digest, rootFingerprint: keyLog.rootFingerprint, keyCheckpoint: keyLog.keyCheckpoint, ...(keyLog.authority ? { keyAuthority: keyLog.authority } : {}) } : {}),
    checkedAt: checked.evidence.checkedAt,
    protection: { ok: true, source: "os-protected-verifier" },
  };
}

function main(args) {
  try {
    const receipt = verifyPrivateRepository(arg(args, "--repo-root"), arg(args, "--expected-commit"), arg(args, "--expected-tree"));
    process.stdout.write(`${JSON.stringify(receipt)}\n`);
  } catch (error) {
    process.stderr.write(`${JSON.stringify({ ok: false, error: { type: "protected-ownership-rejected", message: error.message } })}\n`);
    process.exitCode = 1;
  }
}

const realOrSelf = (file) => { try { return realpathSync(file); } catch { return path.resolve(file); } };
if (process.argv[1] && realOrSelf(process.argv[1]) === realOrSelf(fileURLToPath(import.meta.url))) main(process.argv.slice(2));
