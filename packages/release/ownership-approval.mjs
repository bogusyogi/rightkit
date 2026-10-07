// Signed RightKit ownership exception records. Trust is deliberately external to repositories.
import { createHash, createPublicKey, verify as cryptoVerify } from "node:crypto";
import { existsSync, lstatSync, readFileSync, statSync } from "node:fs";
import path from "node:path";
import { spawnSync } from "./background-process.mjs";
import { verifyKeyLog, verifyOwnershipSignature } from "./ownership-key-log.mjs";

export const APPROVAL_SCHEMA = 1;
export const REQUIRED_HOSTS = Object.freeze(["windows", "macos"]);
export const DEFAULT_TRUST_PATH = Object.freeze({
  win32: path.join("C:\\ProgramData", "Orthic Labs", "RightKit", "ownership", "trust.json"),
  darwin: "/Library/Application Support/Orthic Labs/RightKit/ownership/trust.json",
});
const HEX = /^[a-f0-9]{64}$/;
const keyLogCache = new Map();
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

export function canonicalRepositoryIdentity(value) {
  if (typeof value !== "string" || !value.trim()) throw new Error("repository identity is required");
  let raw = value.trim();
  const scp = raw.match(/^([^@/:]+)@([^:]+):(.+)$/);
  if (scp) raw = `ssh://${scp[2]}/${scp[3]}`;
  let url;
  try { url = new URL(raw); } catch { throw new Error("repository identity must be an absolute HTTPS/SSH URL or SSH scp remote"); }
  if (!["https:", "ssh:"].includes(url.protocol) || !url.hostname || (url.protocol === "https:" && (url.username || url.password)) || url.password || url.search || url.hash) throw new Error("unsupported repository identity URL");
  const hostname = url.hostname.toLowerCase();
  let repoPath = url.pathname.replace(/^\/+/, "").replace(/\/+$/, "").replace(/\.git$/i, "");
  if (hostname === "github.com") repoPath = repoPath.toLowerCase();
  if (!repoPath || repoPath.split("/").some((x) => !x || x === "." || x === "..")) throw new Error("invalid repository identity path");
  return `https://${hostname}${url.port ? `:${url.port}` : ""}/${repoPath}`;
}

export function canonicalJson(value) {
  if (value === null || typeof value !== "object") return JSON.stringify(value);
  if (Array.isArray(value)) return `[${value.map(canonicalJson).join(",")}]`;
  return `{${Object.keys(value).sort().map((k) => `${JSON.stringify(k)}:${canonicalJson(value[k])}`).join(",")}}`;
}
export function sha256(value) {
  const bytes = Buffer.isBuffer(value) || value instanceof Uint8Array;
  return createHash("sha256").update(bytes ? value : typeof value === "string" ? value : canonicalJson(value)).digest("hex");
}
export function keyIdForPublicKey(host, publicKey) {
  if (!REQUIRED_HOSTS.includes(host)) throw new Error("unknown ownership approval host");
  const key = publicKey?.type === "public" ? publicKey : createPublicKey(publicKey);
  const prefix = host === "windows" ? "win-" : "mac-";
  return prefix + createHash("sha256").update(key.export({ type: "spki", format: "der" })).digest("hex");
}
export function occurrenceHash(finding) {
  return sha256({ fingerprint: finding.fingerprint, file: finding.file, line: finding.line ?? 0, text: finding.text ?? "" });
}
export function approvalPayload(record) {
  return record?.payload;
}

function validateRecord(record) {
  const p = [];
  if (!record || record.schema !== APPROVAL_SCHEMA || !record.payload || !record.signature) return ["malformed or unsigned approval record"];
  const r = record.payload;
  for (const key of ["repositoryIdentity", "catalogDigest", "fingerprint", "paths", "occurrenceHashes", "reason", "owner", "expires", "requestId", "trackingRef"]) {
    if (r[key] === undefined || r[key] === "" || (Array.isArray(r[key]) && !r[key].length)) p.push(`missing ${key}`);
  }
  if (typeof r.repositoryIdentity !== "string" || r.repositoryIdentity.length > 512) p.push("invalid repository identity");
  if (!HEX.test(r.catalogDigest ?? "")) p.push("invalid catalog digest");
  if (!Array.isArray(r.paths) || r.paths.some((x) => typeof x !== "string" || !x || x.startsWith("/") || x.includes("\\") || x.split("/").some((s) => !s || s === "." || s === "..") || /[*?\[\]{}]/.test(x))) p.push("paths must be exact repository-relative paths");
  if (!Array.isArray(r.occurrenceHashes) || r.occurrenceHashes.some((x) => !HEX.test(x)) || new Set(r.occurrenceHashes).size !== r.occurrenceHashes.length) p.push("invalid occurrence hashes");
  if (!/^\d{4}-\d{2}-\d{2}$/.test(r.expires ?? "") || Number.isNaN(Date.parse(`${r.expires}T00:00:00Z`)) || new Date(`${r.expires}T00:00:00Z`).toISOString().slice(0, 10) !== r.expires) p.push("invalid expiry date");
  if (typeof r.requestId !== "string" || !/^[A-Za-z0-9][A-Za-z0-9._:-]{2,127}$/.test(r.requestId)) p.push("invalid request id");
  let trackingUrl;
  try { trackingUrl = new URL(r.trackingRef?.url); } catch {}
  if (!trackingUrl || trackingUrl.protocol !== "https:" || !trackingUrl.hostname || trackingUrl.username || trackingUrl.password || typeof r.trackingRef.id !== "string" || !r.trackingRef.id || r.trackingRef.verified !== true) p.push("tracking reference must be verified and https-backed");
  if (typeof r.reason !== "string" || !r.reason.trim() || r.reason.length > 4000) p.push("invalid approval reason");
  if (typeof r.owner !== "string" || !r.owner.trim() || r.owner.length > 256) p.push("invalid owner");
  if (!record.signature.keyId || !record.signature.host || typeof record.signature.value !== "string" || !/^(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?$/.test(record.signature.value) || Buffer.from(record.signature.value, "base64").toString("base64") !== record.signature.value) p.push("malformed signature envelope");
  return p;
}

function exactRepositoryPath(value) {
  return typeof value === "string" && (value === "." || (!value.startsWith("/") && !value.includes("\\") && !value.split("/").some((s) => !s || s === "." || s === "..") && !/[*?\[\]{}]/.test(value)));
}
function withinRoot(file, root) { return root === "." || file === root || file.startsWith(`${root}/`); }
export function validateTrustRegistry(trust) {
  const problems = [];
  if (trust?.schema !== 1) problems.push("protected trust schema must be 1");
  if (!Array.isArray(trust?.revokedKeyIds) || !Array.isArray(trust?.verifiedTrackingEvidence) || trust.verifiedTrackingEvidence.some((x) => !HEX.test(x))) problems.push("trust revocation or tracking evidence lists are malformed");
  if (!/^\d+(?:\.\d+)*$/.test(trust?.minimumScannerVersion ?? "") || !/^\d+(?:\.\d+)*$/.test(trust?.minimumCatalogVersion ?? "")) problems.push("scanner/catalog floors are missing or malformed");
  if (trust?.minimumCatalogDigest !== undefined && !HEX.test(trust.minimumCatalogDigest)) problems.push("minimum catalog digest is malformed");
  if (trust?.rootFingerprint !== undefined || trust?.keyCheckpoint !== undefined) {
    if (!HEX.test(trust.rootFingerprint ?? "") || !trust.keyCheckpoint || !Number.isSafeInteger(trust.keyCheckpoint.sequence) || trust.keyCheckpoint.sequence < 0 || !HEX.test(trust.keyCheckpoint.head ?? "")) problems.push("protected key-log root pin or checkpoint is malformed");
  }
  if (trust?.keyAuthority !== undefined) {
    const authority = trust.keyAuthority;
    if (!authority || typeof authority !== "object" || Array.isArray(authority) || Object.keys(authority).sort().join("\0") !== ["checkoutPath", "repositoryIdentity"].sort().join("\0")
      || typeof authority.checkoutPath !== "string" || !path.isAbsolute(authority.checkoutPath)) problems.push("protected key authority is malformed");
    try { if (canonicalRepositoryIdentity(authority.repositoryIdentity) !== authority.repositoryIdentity) problems.push("protected key authority identity is noncanonical"); }
    catch { problems.push("protected key authority identity is invalid"); }
  }
  if (!Array.isArray(trust?.repositories)) return { ok: false, problems: ["protected repository registry missing"] };
  const seen = new Set();
  for (const entry of trust.repositories) {
    let identity;
    try { identity = canonicalRepositoryIdentity(entry.repositoryIdentity); } catch { problems.push("repository registry contains invalid identity"); continue; }
    if (identity !== entry.repositoryIdentity || seen.has(identity)) problems.push(`repository registry identity is noncanonical or duplicated: ${entry.repositoryIdentity}`);
    seen.add(identity);
    if (!Array.isArray(entry.roots) || !entry.roots.length || entry.roots.some((root) => !exactRepositoryPath(root))) problems.push(`repository registry roots must be exact paths: ${identity}`);
    if (!["public", "private"].includes(entry.visibility)) problems.push(`repository visibility is invalid: ${identity}`);
  }
  if (!Array.isArray(trust.provenance ?? [])) problems.push("protected provenance registry is malformed");
  for (const record of trust.provenance ?? []) {
    if (!record.repositoryIdentity || !Array.isArray(record.paths) || !record.paths.length || record.paths.some((p) => !exactRepositoryPath(p)) || !HEX.test(record.contentDigest ?? "") || !["third-party", "generated"].includes(record.kind) || !record.requestId) problems.push("protected provenance entry is incomplete or malformed");
    try { if (canonicalRepositoryIdentity(record.repositoryIdentity) !== record.repositoryIdentity) problems.push("provenance repository identity is noncanonical"); } catch { problems.push("provenance repository identity is invalid"); }
  }
  return { ok: problems.length === 0, problems };
}

/** Resolve protected N-device keys against the supplied immutable log bytes. */
export function resolveKeyLogTrust(trust, logBytes) {
  if (!trust?.rootFingerprint) return { ok: false, problems: ["protected key-log root pin is missing"], trust };
  if (!HEX.test(trust.rootFingerprint) || !trust.keyCheckpoint || !Number.isSafeInteger(trust.keyCheckpoint.sequence) || trust.keyCheckpoint.sequence < 0 || !HEX.test(trust.keyCheckpoint.head ?? "")) return { ok: false, problems: ["protected key-log root pin or checkpoint is malformed"], trust };
  let log;
  try { log = JSON.parse(Buffer.isBuffer(logBytes) ? logBytes.toString("utf8") : String(logBytes)); }
  catch { return { ok: false, problems: ["committed ownership key log is missing or invalid JSON"], trust }; }
  const logDigest = sha256(logBytes);
  const cacheKey = `${logDigest}:${trust.rootFingerprint}:${trust.keyCheckpoint.sequence}:${trust.keyCheckpoint.head}`;
  let verified = keyLogCache.get(cacheKey);
  if (!verified) {
    verified = verifyKeyLog(log, { rootFingerprint: trust.rootFingerprint, checkpoint: trust.keyCheckpoint });
    keyLogCache.set(cacheKey, verified);
    if (keyLogCache.size > 64) keyLogCache.delete(keyLogCache.keys().next().value);
  }
  if (!verified.ok) return { ok: false, problems: verified.problems, trust };
  const completedPlatforms = new Set(verified.allKeys.map((key) => key.platform));
  const enrollmentComplete = REQUIRED_HOSTS.every((platform) => completedPlatforms.has(platform));
  return { ok: true, trust: { ...trust, ownershipKeys: verified.keys, ownershipAllKeys: verified.allKeys, enrollmentComplete, revokedKeyIds: [...new Set([...(trust.revokedKeyIds ?? []), ...verified.revokedKeyIds])] }, keyLogDigest: logDigest, checkpoint: verified.checkpoint };
}
export function verifyRepositoryRegistration(trust, repositoryIdentity, { files = [] } = {}) {
  const validation = validateTrustRegistry(trust);
  if (!validation.ok) return { ok: false, problems: validation.problems };
  let identity;
  try { identity = canonicalRepositoryIdentity(repositoryIdentity); } catch { return { ok: false, problems: ["invalid repository identity"] }; }
  const registration = trust.repositories.find((entry) => entry.repositoryIdentity === identity);
  if (!registration) return { ok: false, problems: ["repository is not registered in protected trust"] };
  const problems = files.filter((file) => !exactRepositoryPath(file) || !registration.roots.some((root) => withinRoot(file, root))).map((file) => `path outside registered repository roots or malformed: ${file}`);
  return { ok: problems.length === 0, problems, registration };
}
export function provenanceContentDigest(files) {
  return sha256([...files].map(({ path: file, content }) => ({ path: file, sha256: createHash("sha256").update(content).digest("hex") })).sort((a, b) => a.path.localeCompare(b.path)));
}
export function verifyProtectedProvenance(trust, { repositoryIdentity, paths, files, kind, requestId } = {}) {
  const validation = validateTrustRegistry(trust);
  if (!validation.ok) return { ok: false, problems: validation.problems };
  let identity;
  try { identity = canonicalRepositoryIdentity(repositoryIdentity); } catch { return { ok: false, problems: ["invalid repository identity"] }; }
  if (!Array.isArray(paths) || !paths.length || paths.some((p) => !exactRepositoryPath(p))) return { ok: false, problems: ["provenance paths must be exact repository-relative paths"] };
  const sortedPaths = [...new Set(paths)].sort();
  if (sortedPaths.length !== paths.length) return { ok: false, problems: ["duplicate provenance paths"] };
  if (!Array.isArray(files) || files.length !== sortedPaths.length || files.some((f) => !f || typeof f.path !== "string" || typeof f.content !== "string" && !Buffer.isBuffer(f.content)) || canonicalJson(files.map((f) => f.path).sort()) !== canonicalJson(sortedPaths)) return { ok: false, problems: ["provenance content must exactly cover every declared path"] };
  const registration = verifyRepositoryRegistration(trust, identity, { files: sortedPaths });
  if (!registration.ok) return registration;
  const contentDigest = provenanceContentDigest(files);
  const match = (trust.provenance ?? []).find((p) => p.repositoryIdentity === identity && p.kind === kind && p.requestId === requestId && p.contentDigest === contentDigest && canonicalJson([...p.paths].sort()) === canonicalJson(sortedPaths));
  return match ? { ok: true, problems: [], record: match } : { ok: false, problems: ["no protected provenance record matches paths and content"] };
}

export function createDraft({ repositoryIdentity, catalogDigest, fingerprint, findings, paths, reason, owner, expires, requestId, trackingRef }) {
  const payload = { repositoryIdentity: canonicalRepositoryIdentity(repositoryIdentity), catalogDigest, fingerprint, paths: [...new Set(paths ?? [])].sort(), occurrenceHashes: findings.map(occurrenceHash).sort(), reason, owner, expires, requestId, trackingRef };
  return { schema: APPROVAL_SCHEMA, payload, signature: null };
}

// External trust can be loaded only from the platform-owned canonical location.
export function trustedPath() {
  const platform = process.platform;
  const selected = DEFAULT_TRUST_PATH[platform];
  if (!selected) throw new Error(`ownership approval unsupported platform: ${platform}`);
  return selected;
}
function versionAtLeast(actual, minimum) {
  const parse = (v) => String(v ?? "").split(".").map((x) => /^\d+$/.test(x) ? Number(x) : NaN);
  const a = parse(actual), b = parse(minimum);
  if (!a.length || !b.length || [...a, ...b].some(Number.isNaN)) return false;
  for (let i = 0; i < Math.max(a.length, b.length); i++) {
    if ((a[i] ?? 0) > (b[i] ?? 0)) return true;
    if ((a[i] ?? 0) < (b[i] ?? 0)) return false;
  }
  return true;
}
export function validateOwnershipFloor(trust, { scannerVersion, catalogVersion, catalogDigest } = {}) {
  const problems = [];
  if (!trust) return { ok: false, problems: ["external ownership trust unavailable"] };
  if (!versionAtLeast(scannerVersion, trust.minimumScannerVersion)) problems.push("scanner version missing, malformed, or below protected minimum");
  if (!versionAtLeast(catalogVersion, trust.minimumCatalogVersion)) problems.push("catalog version missing, malformed, or below protected minimum");
  if (trust.minimumCatalogDigest && trust.minimumCatalogDigest !== catalogDigest) problems.push("catalog digest below protected pinned digest");
  return { ok: problems.length === 0, problems };
}
function protectedLocation(file, platform) {
  const problems = [];
  if (platform === "win32") {
    const dirs = [];
    let current = path.dirname(file);
    while (true) {
      dirs.push(current);
      if (current.toLowerCase() === "c:\\") break;
      current = path.dirname(current);
    }
    dirs.push(file);
    for (const target of dirs) {
      try { if (lstatSync(target).isSymbolicLink()) problems.push(`trust path contains a reparse/symlink: ${target}`); } catch { problems.push(`cannot inspect trust path: ${target}`); continue; }
      const script = "$ErrorActionPreference='Stop'; $a=Get-Acl -LiteralPath $env:RK_TRUST_CHECK_TARGET; $item=Get-Item -Force -LiteralPath $env:RK_TRUST_CHECK_TARGET; $id=[Security.Principal.WindowsIdentity]::GetCurrent(); $p=New-Object Security.Principal.WindowsPrincipal($id); $ownerSid=$a.GetOwner([Security.Principal.SecurityIdentifier]).Value; $ownerName=$a.Owner; $out=[ordered]@{ownerSID=$ownerSid;ownerName=$ownerName;attributes=$item.Attributes.ToString();directory=[bool]$item.PSIsContainer;nonempty=(!$item.PSIsContainer -or [System.IO.Directory]::EnumerateFileSystemEntries($item.FullName).GetEnumerator().MoveNext());elevated=$p.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator);access=@($a.Access | ForEach-Object { [ordered]@{identitySID=$_.IdentityReference.Translate([Security.Principal.SecurityIdentifier]).Value;type=$_.AccessControlType.ToString();mask=[int64]$_.FileSystemRights.value__;propagationFlags=[int]$_.PropagationFlags} })}; $out | ConvertTo-Json -Compress -Depth 5";
      const r = spawnSync("C:\\Windows\\System32\\WindowsPowerShell\\v1.0\\powershell.exe", ["-NoProfile", "-NonInteractive", "-Command", script], { encoding: "utf8", windowsHide: true, env: { ...process.env, RK_TRUST_CHECK_TARGET: target } });
      if (r.status !== 0) { problems.push(`cannot inspect trust ACL: ${target}`); continue; }
      try {
        const acl = JSON.parse(r.stdout.trim());
        const result = evaluateWindowsAcl(acl, { target });
        problems.push(...result.problems.map((problem) => `${problem}: ${target}`));
      } catch { problems.push(`malformed ACL result: ${target}`); }
    }
    return { ok: problems.length === 0, problems };
  }
  let current = file;
  while (true) {
    try { if (lstatSync(current).isSymbolicLink()) problems.push(`trust path contains a symlink: ${current}`); } catch { problems.push(`cannot inspect trust path: ${current}`); }
    try {
      const st = statSync(current);
      if (st.uid !== 0 || (st.mode & 0o022)) problems.push(`trust location is not root-owned/private: ${current}`);
    } catch { problems.push(`cannot inspect trust location: ${current}`); }
    const acl = spawnSync("/bin/ls", ["-lde", current], { encoding: "utf8", windowsHide: true });
    if (acl.status !== 0) problems.push(`cannot inspect trust ACL: ${current}`);
    else for (const line of acl.stdout.split(/\r?\n/).slice(1)) {
      if (!/allow/i.test(line) || !/\b(?:write|delete|append|writeattr|writeextattr|chown|delete_child|add_file|add_subdirectory)\b/i.test(line)) continue;
      if (!/^\s*\d+:\s+user:root\s+allow\b/i.test(line)) problems.push(`trust ACL grants write authority to a non-root principal: ${current}`);
    }
    const parent = path.dirname(current);
    if (parent === current || current === "/") break;
    current = parent;
  }
  return { ok: problems.length === 0, problems };
}
export function loadTrustedExternal() {
  const platform = process.platform;
  const file = trustedPath();
  if (!existsSync(file)) return { enrolled: false, pending: true, protection: { ok: false, reason: "external trust file is absent" }, trust: null, path: file };
  if (lstatSync(file).isSymbolicLink()) throw new Error("ownership trust file may not be a symlink");
  const st = statSync(file);
  if (!st.isFile()) throw new Error("ownership trust file must be a regular protected file");
  const protection = protectedLocation(file, platform);
  if (!protection.ok) throw new Error(protection.problems.join("; "));
  const trust = JSON.parse(readFileSync(file, "utf8"));
  if (trust.schema !== 1) throw new Error("ownership trust file schema is unsupported");
  const trustCheck = validateTrustRegistry(trust);
  if (!trustCheck.ok) throw new Error(trustCheck.problems.join("; "));
  for (const host of REQUIRED_HOSTS) {
    const entry = trust.hosts?.[host];
    if (!entry?.keyId && !entry?.publicKeyPem) continue;
    if (!entry.keyId || !entry.publicKeyPem || entry.keyId !== keyIdForPublicKey(host, entry.publicKeyPem)) throw new Error(`invalid or mismatched ${host} approval key enrollment`);
    const key = createPublicKey(entry.publicKeyPem);
    if (key.asymmetricKeyType !== "ec" || key.asymmetricKeyDetails?.namedCurve !== "prime256v1") throw new Error(`${host} approval key must be P-256`);
  }
  const protectionResult = { ok: true, mechanism: platform === "win32" ? "PowerShell ACL owner/ACE audit" : "owner/mode/ACL audit" };
  const hostKeys = REQUIRED_HOSTS.map((host) => trust.hosts?.[host]);
  const enrolled = trust.rootFingerprint
    ? Boolean(HEX.test(trust.rootFingerprint) && trust.keyCheckpoint && Number.isSafeInteger(trust.keyCheckpoint.sequence) && trust.keyCheckpoint.sequence >= 0 && HEX.test(trust.keyCheckpoint.head ?? "")) // Provisional; scan paths replace this from verified log history.
    : hostKeys.every((host) => host?.keyId && host.publicKeyPem && !trust.revokedKeyIds?.includes(host.keyId))
      && hostKeys[0]?.keyId !== hostKeys[1]?.keyId && hostKeys[0]?.publicKeyPem !== hostKeys[1]?.publicKeyPem;
  return { enrolled, pending: !enrolled, protection: protectionResult, trust, path: file };
}

// Test-only seam: production callers must use loadTrustedExternal's fixed canonical path.
export const __testOnly = Object.freeze({ protectedLocation });

function signatureProblems(record, trust) {
  const s = record.signature;
  if (!REQUIRED_HOSTS.includes(s.host)) return ["unknown signing host"];
  if (trust.revokedKeyIds?.includes(s.keyId)) return ["signing key is revoked"];
  if (trust.rootFingerprint) {
    const normalizedId = String(s.keyId).replace(/^(?:win-|mac-)/, "");
    const key = trust.ownershipKeys?.find((candidate) => candidate.keyId === normalizedId && candidate.platform === s.host);
    if (!key) return ["signing key is not active in protected ownership key log"];
    try {
      const valid = verifyOwnershipSignature(key.publicKeyPem, Buffer.from(canonicalJson(approvalPayload(record))), Buffer.from(s.value, "base64"));
      return valid ? [] : ["invalid signature"];
    } catch { return ["malformed signature or public key"]; }
  }
  const host = trust.hosts?.[s.host];
  if (!host || host.keyId !== s.keyId || !host.publicKeyPem) return ["signing key is not enrolled for host"];
  const keys = REQUIRED_HOSTS.map((name) => trust.hosts?.[name]);
  if (!keys.every((k) => k?.keyId && k.publicKeyPem && !trust.revokedKeyIds?.includes(k.keyId)) || keys[0]?.keyId === keys[1]?.keyId || keys[0]?.publicKeyPem === keys[1]?.publicKeyPem) return ["pending: distinct Windows and macOS approval keys must be enrolled"];
  try {
    const publicKey = createPublicKey(host.publicKeyPem);
    if (s.keyId !== keyIdForPublicKey(s.host, publicKey)) return ["signing key ID does not match enrolled public key"];
    const valid = cryptoVerify("sha256", Buffer.from(canonicalJson(approvalPayload(record))), publicKey, Buffer.from(s.value, "base64"));
    return valid ? [] : ["invalid signature"];
  } catch { return ["malformed signature or public key"];
  }
}

/** Verify signature, protected enrollment, floor, and exact scan coverage. */
export function verifyException(record, { repositoryIdentity, catalogDigest, findings, trust, today = new Date().toISOString().slice(0, 10), scannerVersion, catalogVersion } = {}) {
  const problems = validateRecord(record);
  if (problems.length) return { ok: false, problems };
  if (!trust) return { ok: false, problems: ["pending: external ownership trust is not enrolled"] };
  const p = signatureProblems(record, trust);
  if (p.length) return { ok: false, problems: p };
  const r = record.payload;
  const trackingEvidence = sha256({ repositoryIdentity: r.repositoryIdentity, requestId: r.requestId, trackingRef: r.trackingRef });
  if (!trust.verifiedTrackingEvidence?.includes(trackingEvidence)) problems.push("tracking reference lacks protected verification evidence");
  let recordIdentity, currentIdentity;
  try { recordIdentity = canonicalRepositoryIdentity(r.repositoryIdentity); currentIdentity = canonicalRepositoryIdentity(repositoryIdentity); }
  catch { problems.push("invalid repository identity"); }
  if (recordIdentity && recordIdentity !== r.repositoryIdentity) problems.push("approval repository identity is not canonical");
  if (recordIdentity && currentIdentity && recordIdentity !== currentIdentity) problems.push("repository identity changed");
  const registration = verifyRepositoryRegistration(trust, r.repositoryIdentity, { files: (findings ?? []).map((f) => f.file) });
  problems.push(...registration.problems);
  if (r.catalogDigest !== catalogDigest) problems.push("ownership catalog digest changed");
  if (!/^\d{4}-\d{2}-\d{2}$/.test(today) || Number.isNaN(Date.parse(`${today}T00:00:00Z`)) || new Date(`${today}T00:00:00Z`).toISOString().slice(0, 10) !== today) problems.push("invalid verification date");
  else if (r.expires < today) problems.push(`approval expired ${r.expires}`);
  const floor = validateOwnershipFloor(trust, { scannerVersion, catalogVersion, catalogDigest });
  problems.push(...floor.problems);
  const selected = (findings ?? []).filter((f) => f.fingerprint === r.fingerprint && r.paths.includes(f.file));
  const actual = selected.map(occurrenceHash).sort();
  if (canonicalJson(actual) !== canonicalJson([...r.occurrenceHashes].sort())) problems.push("baseline occurrence set changed");
  if (!actual.length) problems.push("approval is stale and matches no current occurrences");
  return { ok: problems.length === 0, problems };
}

export function signPayloadForNative(record, { keyId, host, signature }) {
  if (!record?.payload || !signature) throw new Error("native signer must return a signature over displayed approval payload");
  return { schema: APPROVAL_SCHEMA, payload: record.payload, signature: { keyId, host, value: signature } };
}
