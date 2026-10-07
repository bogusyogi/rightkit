// Standalone trust schema used by candidate preparation & OS installer.
import { createHash, createPublicKey } from "node:crypto";

const HOSTS = ["windows", "macos"];
const HEX = /^[a-f0-9]{64}$/;

const WINDOWS_TRUSTED_SIDS = new Set(["S-1-5-18", "S-1-5-32-544"]);
const WINDOWS_READ_MASK = 0x001200a9;
const WINDOWS_ANCESTOR_CREATE_MASK = 0x00000116;
const WINDOWS_GENERIC_WRITE = 0x40000000;
const WINDOWS_GENERIC_ALL = 0x10000000;

/** Evaluate one native Windows security descriptor using numeric SIDs & masks. */
export function evaluateWindowsAcl(info, { target, requireNonElevated = true } = {}) {
  const problems = [];
  const fail = (message) => problems.push(message);
  const ownerSID = info?.ownerSID;
  const ownerName = info?.ownerName;
  const trustedInstallerOwner = ownerSID === "S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464";
  const trustedOwner = WINDOWS_TRUSTED_SIDS.has(ownerSID) || trustedInstallerOwner;
  if (!/^S-1-\d+(?:-\d+)+$/.test(ownerSID ?? "") || !trustedOwner) fail("owner is not SYSTEM, Administrators, or the native TrustedInstaller service owner");
  if (/ReparsePoint/i.test(info?.attributes ?? "")) fail("path object is a reparse point");
  if (requireNonElevated && info?.elevated !== false) fail("current process is elevated or elevation state is unknown");

  const normalizedTarget = String(target ?? "").replace(/\//g, "\\").replace(/\\+$/, "").toLowerCase();
  const isDriveRoot = /^[a-z]:$/.test(normalizedTarget);
  const isProgramData = /^[a-z]:\\programdata$/.test(normalizedTarget);
  const permitCreation = (isDriveRoot || isProgramData) && info?.directory === true && info?.nonempty === true;
  if ((isDriveRoot || isProgramData) && !permitCreation) fail("system ancestor must be an existing nonempty directory before create rights are accepted");
  if (!Array.isArray(info?.access) || !info.access.length) fail("ACL access entries are missing, empty, or malformed");
  for (const ace of Array.isArray(info?.access) ? info.access : []) {
    const sid = ace?.identitySID;
    const mask = ace?.mask;
    const propagationFlags = ace?.propagationFlags;
    if (!/^S-1-\d+(?:-\d+)+$/.test(sid ?? "") || !Number.isInteger(mask) || mask < -2147483648 || mask > 4294967295 || !Number.isInteger(propagationFlags) || propagationFlags < 0 || propagationFlags > 3 || !["Allow", "Deny"].includes(ace?.type)) {
      fail("ACL contains a malformed ACE");
      continue;
    }
    if ((propagationFlags & 2) !== 0 || ace.type !== "Allow") continue;
    if (WINDOWS_TRUSTED_SIDS.has(sid) || (trustedInstallerOwner && sid === ownerSID)) continue;
    const unsignedMask = mask >>> 0;
    if ((unsignedMask & (WINDOWS_GENERIC_WRITE | WINDOWS_GENERIC_ALL)) !== 0) {
      fail(`untrusted principal ${sid} has generic write or all access`);
      continue;
    }
    const allowedMask = permitCreation ? (WINDOWS_READ_MASK | WINDOWS_ANCESTOR_CREATE_MASK) : WINDOWS_READ_MASK;
    if ((unsignedMask & ~allowedMask) !== 0) fail(`untrusted principal ${sid} has write, delete, control, or unknown access mask 0x${unsignedMask.toString(16)}`);
  }
  return { ok: problems.length === 0, problems };
}

export function sha256(value) {
  return createHash("sha256").update(value).digest("hex");
}

export function canonicalJson(value) {
  if (value === null || typeof value !== "object") return JSON.stringify(value);
  if (Array.isArray(value)) return `[${value.map(canonicalJson).join(",")}]`;
  return `{${Object.keys(value).sort().map((key) => `${JSON.stringify(key)}:${canonicalJson(value[key])}`).join(",")}}`;
}

export function keyIdForPublicKey(host, publicKeyPem) {
  if (!HOSTS.includes(host)) throw new Error("unknown ownership approval host");
  const key = createPublicKey(publicKeyPem);
  const prefix = host === "windows" ? "win-" : "mac-";
  return prefix + sha256(key.export({ type: "spki", format: "der" }));
}

function canonicalRepositoryIdentity(value) {
  if (typeof value !== "string" || !value) throw new Error("repository identity is required");
  let url;
  try { url = new URL(value); } catch { throw new Error("repository identity is not a URL"); }
  if (url.protocol !== "https:" || !url.hostname || url.username || url.password || url.search || url.hash) throw new Error("repository identity must be canonical HTTPS");
  const host = url.hostname.toLowerCase();
  let repoPath = url.pathname.replace(/^\/+/, "").replace(/\/+$/, "").replace(/\.git$/i, "");
  if (host === "github.com") repoPath = repoPath.toLowerCase();
  if (!repoPath || repoPath.split("/").some((part) => !part || part === "." || part === "..")) throw new Error("repository identity path is invalid");
  return `https://${host}${url.port ? `:${url.port}` : ""}/${repoPath}`;
}

function exactPath(value) {
  return typeof value === "string" && (value === "." || (!value.startsWith("/") && !value.includes("\\") && !value.split("/").some((part) => !part || part === "." || part === "..") && !/[*?\[\]{}]/.test(value)));
}

export function validateTrustRegistry(trust) {
  const problems = [];
  if (trust?.schema !== 1) problems.push("protected trust schema must be 1");
  if (!Array.isArray(trust?.revokedKeyIds) || trust.revokedKeyIds.some((id) => typeof id !== "string" || !id)) problems.push("trust revocation list is malformed");
  if (!Array.isArray(trust?.verifiedTrackingEvidence) || trust.verifiedTrackingEvidence.some((item) => !HEX.test(item))) problems.push("trust tracking evidence list is malformed");
  if (!/^\d+(?:\.\d+)*$/.test(trust?.minimumScannerVersion ?? "") || !/^\d+(?:\.\d+)*$/.test(trust?.minimumCatalogVersion ?? "")) problems.push("scanner/catalog floors are missing or malformed");
  if (trust?.minimumCatalogDigest !== undefined && !HEX.test(trust.minimumCatalogDigest)) problems.push("minimum catalog digest is malformed");
  if (!Array.isArray(trust?.repositories)) return { ok: false, problems: [...problems, "protected repository registry missing"] };
  const seen = new Set();
  for (const entry of trust.repositories) {
    let identity;
    try { identity = canonicalRepositoryIdentity(entry?.repositoryIdentity); } catch { problems.push("repository registry contains invalid identity"); continue; }
    if (identity !== entry.repositoryIdentity || seen.has(identity)) problems.push(`repository registry identity is noncanonical or duplicated: ${entry.repositoryIdentity}`);
    seen.add(identity);
    if (!Array.isArray(entry.roots) || !entry.roots.length || entry.roots.some((root) => !exactPath(root))) problems.push(`repository registry roots must be exact paths: ${identity}`);
    if (!["public", "private"].includes(entry.visibility)) problems.push(`repository visibility is invalid: ${identity}`);
  }
  if (!Array.isArray(trust.provenance ?? [])) problems.push("protected provenance registry is malformed");
  for (const record of trust.provenance ?? []) {
    let canonical = false;
    try { canonical = canonicalRepositoryIdentity(record?.repositoryIdentity) === record.repositoryIdentity; } catch {}
    if (!canonical || !Array.isArray(record?.paths) || !record.paths.length || record.paths.some((item) => !exactPath(item)) || !HEX.test(record.contentDigest ?? "") || !["third-party", "generated"].includes(record.kind) || typeof record.requestId !== "string" || !record.requestId) {
      problems.push("protected provenance entry is incomplete or malformed");
    }
  }
  return { ok: problems.length === 0, problems };
}

export function validateEnrollmentExport(value, expectedHost) {
  if (!HOSTS.includes(expectedHost)) throw new Error(`unsupported host: ${expectedHost}`);
  if (!value || typeof value !== "object" || Array.isArray(value) || Object.keys(value).sort().join(",") !== "host,keyId,publicKeyPem") {
    throw new Error(`${expectedHost} export must contain exactly host, keyId & publicKeyPem`);
  }
  if (value.host !== expectedHost || typeof value.keyId !== "string" || typeof value.publicKeyPem !== "string") throw new Error(`${expectedHost} export host or key fields are invalid`);
  if (!/^-----BEGIN PUBLIC KEY-----\r?\n[A-Za-z0-9+/=\r\n]+-----END PUBLIC KEY-----\r?\n?$/.test(value.publicKeyPem)) throw new Error(`${expectedHost} export must contain public SubjectPublicKeyInfo PEM only`);
  let key;
  try { key = createPublicKey(value.publicKeyPem); } catch { throw new Error(`${expectedHost} public key PEM is invalid`); }
  if (key.type !== "public" || key.asymmetricKeyType !== "ec" || key.asymmetricKeyDetails?.namedCurve !== "prime256v1") throw new Error(`${expectedHost} key must be a public P-256 key`);
  const expectedId = keyIdForPublicKey(expectedHost, key.export({ type: "spki", format: "pem" }));
  if (value.keyId !== expectedId) throw new Error(`${expectedHost} key ID does not match SPKI DER`);
  return { keyId: expectedId, publicKeyPem: value.publicKeyPem };
}

export function prepareTrustCandidate({ baseline, windowsExport, macosExport }) {
  const registry = validateTrustRegistry(baseline);
  if (!registry.ok) throw new Error(`baseline trust is invalid: ${registry.problems.join("; ")}`);
  const windows = validateEnrollmentExport(windowsExport, "windows");
  const macos = validateEnrollmentExport(macosExport, "macos");
  if (windows.keyId.slice(4) === macos.keyId.slice(4)) throw new Error("Windows & macOS exports must be distinct keys");
  if ((baseline.revokedKeyIds ?? []).includes(windows.keyId) || (baseline.revokedKeyIds ?? []).includes(macos.keyId)) throw new Error("an exported approval key is present in the protected revocation list");
  const existing = baseline.hosts ?? {};
  if (Object.keys(existing).some((host) => !HOSTS.includes(host))) throw new Error("baseline contains an unsupported ownership host entry");
  for (const host of HOSTS) {
    const current = existing[host];
    if (current && (current.keyId || current.publicKeyPem)) throw new Error(`baseline already contains a ${host} key; migration requires administrator review`);
  }
  return {
    ...baseline,
    hosts: {
      ...existing,
      windows: { keyId: windows.keyId, publicKeyPem: windows.publicKeyPem },
      macos: { keyId: macos.keyId, publicKeyPem: macos.publicKeyPem },
    },
  };
}

export function validateTrustCandidate(trust) {
  const registry = validateTrustRegistry(trust);
  if (!registry.ok) throw new Error(`trust candidate schema invalid: ${registry.problems.join("; ")}`);
  if (!trust.hosts || Object.keys(trust.hosts).sort().join(",") !== "macos,windows") throw new Error("trust candidate must contain exactly Windows & macOS host enrollments");
  const keys = HOSTS.map((host) => {
    const value = trust.hosts?.[host];
    if (!value || Object.keys(value).sort().join(",") !== "keyId,publicKeyPem") throw new Error(`${host} enrollment must contain exactly keyId & publicKeyPem`);
    const validated = validateEnrollmentExport({ host, ...value }, host);
    if ((trust.revokedKeyIds ?? []).includes(validated.keyId)) throw new Error(`${host} approval key is revoked`);
    return validated;
  });
  if (keys[0].keyId.slice(4) === keys[1].keyId.slice(4)) throw new Error("Windows & macOS enrollments must be distinct keys");
  return trust;
}
