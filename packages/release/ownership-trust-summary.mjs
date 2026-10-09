// Human-readable summary of what a protected ownership trust update will change. It runs the SAME pure
// functions the elevated child runs (prepare -> registry merge -> floor raise) over the exact request
// bytes about to be installed, so the approval dialog describes the real change.
import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { canonicalJson } from "./ownership-approval.mjs";
import { verifyKeyLog } from "./ownership-key-log.mjs";
import { diffAllowlists } from "./ownership-dependency-allowlist.mjs";
import { diffPolicyBundles } from "./ownership-policy-bundle.mjs";
import { installedPolicyInputs, mergeRepositoriesFromInstalledRelease, prepareTrustUpdate, raiseFloorsToInstalledRelease } from "./ownership-key-trust.mjs";

const here = path.dirname(fileURLToPath(import.meta.url));
const short = (hex, n = 12) => (typeof hex === "string" && hex ? hex.replace(/^(?:win-|mac-)/i, "").slice(0, n) : "none");
const repoName = (identity) => String(identity).replace(/^https:\/\/github\.com\//i, "");
const diffSets = (before, after) => ({ added: [...after].filter((x) => !before.has(x)).sort(), removed: [...before].filter((x) => !after.has(x)).sort() });

const NAME_LIMIT = 12;
const names = (list) => list.length > NAME_LIMIT ? `${list.slice(0, NAME_LIMIT).join(", ")}, +${list.length - NAME_LIMIT} more` : list.join(", ");

/** Semantic description of a policy-bundle change: scanner files, allowlist entries, registry and catalog digests. */
function policyLines(existing, next) {
  const before = existing?.policyBundle, after = next.policyBundle;
  const lines = [];
  if (!after) { if (before) lines.push("Policy bundle: no longer pinned by this update"); return lines; }
  const bundle = diffPolicyBundles(before, after);
  if (!before) lines.push(`Policy bundle: unpinned -> ${short(after.digest, 12)} (pins ${after.files.length} scanner source files, scanner ${after.scannerVersion}, catalog, repository registry and dependency allowlist; enforced by release gates from now on)`);
  else if (before.digest !== after.digest) lines.push(`Policy bundle: ${short(before.digest, 12)} -> ${short(after.digest, 12)}`);
  else return lines;
  if (before) {
    if (bundle.scannerVersion) lines.push(`  scanner version: ${bundle.scannerVersion.from ?? "none"} -> ${bundle.scannerVersion.to}`);
    if (bundle.filesChanged.length) lines.push(`  scanner/verifier files changed (${bundle.filesChanged.length}): ${names(bundle.filesChanged)}`);
    if (bundle.filesAdded.length) lines.push(`  scanner/verifier files added (${bundle.filesAdded.length}): ${names(bundle.filesAdded)}`);
    if (bundle.filesRemoved.length) lines.push(`  scanner/verifier files removed (${bundle.filesRemoved.length}): ${names(bundle.filesRemoved)}`);
    if (bundle.catalogChanged) lines.push("  catalog contents changed");
    if (bundle.registryChanged) lines.push("  repository registry file changed");
  }
  const allow = diffAllowlists(existing?.dependencyAllowlist ?? null, next.dependencyAllowlist ?? null);
  if (!before && next.dependencyAllowlist) {
    const total = (next.dependencyAllowlist.repositories ?? []).reduce((n, r) => n + (r.dependencies?.length ?? 0), 0);
    lines.push(`  dependency allowlist: ${next.dependencyAllowlist.global.length} global + ${total} per-repository entries across ${next.dependencyAllowlist.repositories.length} repositories (first activation; review the file in packages/release)`);
  } else if (allow.changed) {
    if (allow.global.added.length) lines.push(`  allowlist global added (${allow.global.added.length}): ${names(allow.global.added)}`);
    if (allow.global.removed.length) lines.push(`  allowlist global removed (${allow.global.removed.length}): ${names(allow.global.removed)}`);
    for (const repo of allow.repositories) {
      const label = repoName(repo.repositoryIdentity);
      if (repo.baseline === "added") lines.push(`  allowlist baseline added for ${label} (${repo.added.length} entries)`);
      else if (repo.baseline === "removed") lines.push(`  allowlist baseline removed for ${label}`);
      if (repo.baseline !== "changed") continue;
      if (repo.added.length) lines.push(`  allowlist ${label} added (${repo.added.length}): ${names(repo.added)}`);
      if (repo.removed.length) lines.push(`  allowlist ${label} removed (${repo.removed.length}): ${names(repo.removed)}`);
    }
  }
  return lines;
}

function readInstalledManifest() { return JSON.parse(readFileSync(path.join(here, "rightkit-ownership.json"), "utf8")); }

function keysAt(log, rootFingerprint, sequence) {
  const events = log.events.slice(0, Math.max(0, sequence) + 1);
  const state = verifyKeyLog({ schema: 1, events }, { rootFingerprint });
  return new Map((state.ok ? state.keys : []).map((k) => [k.keyId, k]));
}

/**
 * @param {{ existing: object, requestBytes: Buffer|string, manifest?: object, registry?: object, scannerVersion?: string }} input
 * @returns {{ changed: boolean, lines: string[], payloadDigest: string, next: object }}
 */
export function summarizeTrustChange({ existing, requestBytes, manifest, registry, scannerVersion, policy }) {
  const bytes = Buffer.from(requestBytes);
  const payloadDigest = createHash("sha256").update(bytes).digest("hex");
  const request = JSON.parse(bytes.toString("utf8"));
  const catalog = manifest ?? readInstalledManifest();
  const prepared = prepareTrustUpdate(existing, request);
  const merged = mergeRepositoriesFromInstalledRelease(prepared, { firstInstall: existing == null, ...(registry ? { registry } : {}) });
  const next = raiseFloorsToInstalledRelease(merged.trust, { manifest: catalog, ...(scannerVersion ? { scannerVersion } : {}), ...(policy || registry ? { policy: policy ?? { ...installedPolicyInputs(), registry } } : {}) });
  const changed = canonicalJson(existing ?? null) !== canonicalJson(next);
  const lines = [];
  if (!changed) return { changed, lines: ["Ownership trust already current."], payloadDigest, next };

  const repos = diffSets(new Set((existing?.repositories ?? []).map((r) => r.repositoryIdentity)), new Set(next.repositories.map((r) => r.repositoryIdentity)));
  if (repos.added.length) lines.push(`Repositories added (${repos.added.length}): ${repos.added.map(repoName).join(", ")}`);
  if (repos.removed.length) lines.push(`Repositories removed (${repos.removed.length}): ${repos.removed.map(repoName).join(", ")}`);
  for (const w of merged.warnings) lines.push(`Warning: ${w.message}`);

  const caps = Array.isArray(catalog.capabilities) ? catalog.capabilities : [];
  const fingerprints = caps.reduce((n, c) => n + (Array.isArray(c.fingerprints) ? c.fingerprints.length : 0), 0);
  const oldDigest = existing?.minimumCatalogDigest;
  if (oldDigest !== next.minimumCatalogDigest || existing?.minimumCatalogVersion !== next.minimumCatalogVersion) {
    lines.push(`Catalog: v${existing?.minimumCatalogVersion ?? "none"} ${short(oldDigest, 8)} -> v${next.minimumCatalogVersion} ${short(next.minimumCatalogDigest, 8)} (${caps.length} capabilities, ${fingerprints} fingerprints; previous catalog contents are not stored, so per-capability diffs are unavailable)`);
  }
  lines.push(...policyLines(existing, next));
  if (existing?.minimumScannerVersion !== next.minimumScannerVersion) lines.push(`Scanner floor: ${existing?.minimumScannerVersion ?? "none"} -> ${next.minimumScannerVersion}`);

  const oldSeq = existing?.keyCheckpoint?.sequence ?? -1;
  const newSeq = next.keyCheckpoint.sequence;
  if (oldSeq !== newSeq || existing?.keyCheckpoint?.head !== next.keyCheckpoint.head) {
    lines.push(`Key log sequence: ${oldSeq < 0 ? "none" : oldSeq} -> ${newSeq}`);
    const rootFingerprint = request.rootFingerprint;
    const before = oldSeq < 0 ? new Map() : keysAt(request.log, rootFingerprint, oldSeq);
    const after = keysAt(request.log, rootFingerprint, newSeq);
    const fmt = (k) => `${short(k.keyId)} (${k.platform ?? "unknown host"})`;
    const added = [...after.values()].filter((k) => !before.has(k.keyId));
    const revoked = [...before.values()].filter((k) => !after.has(k.keyId));
    if (added.length) lines.push(`Approval keys added: ${added.map(fmt).join(", ")}`);
    if (revoked.length) lines.push(`Approval keys revoked: ${revoked.map(fmt).join(", ")}`);
    if (!added.length && !revoked.length) lines.push("Approval keys: no change in active keys");
  }
  lines.push(`Payload SHA-256: ${short(payloadDigest, 16)}`);
  return { changed, lines, payloadDigest, next };
}

/** Parse + verify the pinned request file, then summarize exactly those bytes. */
export function summarizeRequestFile({ file, digest, existing, ...rest }) {
  const bytes = readFileSync(file);
  if (createHash("sha256").update(bytes).digest("hex") !== digest) throw new Error("trust request bytes do not match pinned SHA-256; refusing to summarize");
  return summarizeTrustChange({ existing, requestBytes: bytes, ...rest });
}
