// Human-readable summary of what a protected ownership trust update will change. It runs the SAME pure
// functions the elevated child runs (prepare -> registry merge -> floor raise) over the exact request
// bytes about to be installed, so the approval dialog describes the real change.
import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { canonicalJson } from "./ownership-approval.mjs";
import { verifyKeyLog } from "./ownership-key-log.mjs";
import { mergeRepositoriesFromInstalledRelease, prepareTrustUpdate, raiseFloorsToInstalledRelease } from "./ownership-key-trust.mjs";

const here = path.dirname(fileURLToPath(import.meta.url));
const short = (hex, n = 12) => (typeof hex === "string" && hex ? hex.replace(/^(?:win-|mac-)/i, "").slice(0, n) : "none");
const repoName = (identity) => String(identity).replace(/^https:\/\/github\.com\//i, "");
const diffSets = (before, after) => ({ added: [...after].filter((x) => !before.has(x)).sort(), removed: [...before].filter((x) => !after.has(x)).sort() });

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
export function summarizeTrustChange({ existing, requestBytes, manifest, registry, scannerVersion }) {
  const bytes = Buffer.from(requestBytes);
  const payloadDigest = createHash("sha256").update(bytes).digest("hex");
  const request = JSON.parse(bytes.toString("utf8"));
  const catalog = manifest ?? readInstalledManifest();
  const prepared = prepareTrustUpdate(existing, request);
  const merged = mergeRepositoriesFromInstalledRelease(prepared, { firstInstall: existing == null, ...(registry ? { registry } : {}) });
  const next = raiseFloorsToInstalledRelease(merged.trust, { manifest: catalog, ...(scannerVersion ? { scannerVersion } : {}) });
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
