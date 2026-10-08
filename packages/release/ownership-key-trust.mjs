import { createHash } from "node:crypto";
import { closeSync, constants, fchmodSync, fstatSync, fsyncSync, lstatSync, mkdirSync, openSync, readFileSync, realpathSync, renameSync, unlinkSync, writeFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync, windowsPowerShellEnvironment } from "./background-process.mjs";
import { canonicalJson, canonicalRepositoryIdentity, __testOnly as approvalTestOnly, evaluateWindowsAcl, validateTrustRegistry } from "./ownership-approval.mjs";
import { verifyKeyLog } from "./ownership-key-log.mjs";
import { SCANNER_VERSION } from "./ownership.mjs";

const HEX = /^[a-f0-9]{64}$/;
const MAX_REQUEST_BYTES = 2 * 1024 * 1024;
const MAX_TRUST_BYTES = 2 * 1024 * 1024;
export const OWNERSHIP_TRUST_PATH = Object.freeze({
  win32: "C:\\ProgramData\\Orthic Labs\\RightKit\\ownership\\trust.json",
  darwin: "/Library/Application Support/Orthic Labs/RightKit/ownership/trust.json",
});

function copyJson(value) { return JSON.parse(JSON.stringify(value)); }
function exactObject(value, keys) { return value && typeof value === "object" && !Array.isArray(value) && Object.keys(value).sort().join("\0") === [...keys].sort().join("\0"); }
function sameCheckoutPath(left, right) {
  const a = path.resolve(left), b = path.resolve(right);
  return process.platform === "win32" ? a.toLowerCase() === b.toLowerCase() : a === b;
}

export function normalizeKeyAuthority(authority) {
  if (!exactObject(authority, ["repositoryIdentity", "checkoutPath"])) throw new Error("key authority fields must be exactly repositoryIdentity and checkoutPath");
  let repositoryIdentity;
  try { repositoryIdentity = canonicalRepositoryIdentity(authority.repositoryIdentity); }
  catch { throw new Error("key authority repository identity is invalid"); }
  if (repositoryIdentity !== authority.repositoryIdentity) throw new Error("key authority repository identity must be canonical HTTPS");
  if (typeof authority.checkoutPath !== "string" || !path.isAbsolute(authority.checkoutPath) || authority.checkoutPath.includes("\0")) throw new Error("key authority checkout path must be absolute");
  const checkoutPath = path.resolve(authority.checkoutPath);
  if (!checkoutPath) throw new Error("key authority checkout path is invalid");
  return { repositoryIdentity, checkoutPath };
}

/** Merge a fully verified key log into protected policy without changing policy fields. */
export function prepareTrustUpdate(existing, { log, rootFingerprint, baseline, authority } = {}) {
  if (!HEX.test(rootFingerprint ?? "")) throw new Error("root fingerprint must be a lowercase SHA-256 SPKI fingerprint");
  const current = existing == null ? null : copyJson(existing);
  if (current && (current.schema !== 1 || typeof current !== "object" || Array.isArray(current))) throw new Error("protected trust schema is unsupported");
  if (current && Object.hasOwn(current, "rootFingerprint") && !current.keyCheckpoint) throw new Error("protected root trust lacks checkpoint; refusing update");
  if (!current && baseline && (Object.hasOwn(baseline, "rootFingerprint") || Object.hasOwn(baseline, "keyCheckpoint") || Object.hasOwn(baseline, "keyAuthority"))) throw new Error("first-install baseline may contain policy only");
  if (current && !validateTrustRegistry(current).ok) throw new Error(`existing protected policy is invalid: ${validateTrustRegistry(current).problems.join("; ")}`);
  if (!current && baseline && !validateTrustRegistry(baseline).ok) throw new Error(`first-install policy baseline is invalid: ${validateTrustRegistry(baseline).problems.join("; ")}`);
  if (current?.rootFingerprint && current.rootFingerprint !== rootFingerprint) throw new Error("protected root pin differs; refusing root rotation");
  const incomingAuthority = authority === undefined ? undefined : normalizeKeyAuthority(authority);
  const existingAuthority = current?.keyAuthority === undefined ? undefined : normalizeKeyAuthority(current.keyAuthority);
  if (existingAuthority && (!sameCheckoutPath(existingAuthority.checkoutPath, current.keyAuthority.checkoutPath) || existingAuthority.repositoryIdentity !== current.keyAuthority.repositoryIdentity)) throw new Error("protected key authority is not canonical");
  if (existingAuthority && incomingAuthority && (existingAuthority.repositoryIdentity !== incomingAuthority.repositoryIdentity || !sameCheckoutPath(existingAuthority.checkoutPath, incomingAuthority.checkoutPath))) throw new Error("protected key authority differs; refusing repository or checkout relocation");
  const priorCheckpoint = current?.keyCheckpoint;
  const state = verifyKeyLog(log, { rootFingerprint, ...(priorCheckpoint ? { checkpoint: priorCheckpoint } : {}) });
  if (!state.ok) throw new Error(`key log rejected: ${state.problems.join("; ")}`);
  if (!state.checkpoint || state.checkpoint.sequence < 0 || !HEX.test(state.checkpoint.head ?? "")) throw new Error("key log has no root checkpoint");
  let policy;
  if (current) policy = current;
  else {
    if (!baseline || typeof baseline !== "object" || Array.isArray(baseline) || baseline.schema !== 1) throw new Error("first root pin requires a schema 1 policy baseline");
    policy = copyJson(baseline);
  }
  const selectedAuthority = current?.keyAuthority ?? incomingAuthority;
  return { ...policy, schema: 1, rootFingerprint, keyCheckpoint: state.checkpoint, ...(selectedAuthority ? { keyAuthority: selectedAuthority } : {}) };
}

function runNativeGit(checkoutPath, args, runner = spawnSync) {
  const nullConfig = process.platform === "win32" ? "NUL" : "/dev/null";
  const nativeGit = process.platform === "win32" ? "C:\\Program Files\\Git\\cmd\\git.exe" : process.platform === "darwin" ? "/usr/bin/git" : "/usr/bin/git";
  const fixedPath = process.platform === "win32" ? "C:\\Windows\\System32;C:\\Windows" : "/usr/bin:/bin:/usr/sbin:/sbin";
  const env = {
    PATH: fixedPath,
    ...(process.platform === "win32" ? { SystemRoot: "C:\\Windows", WINDIR: "C:\\Windows" } : {}),
    GIT_CONFIG_NOSYSTEM: "1",
    GIT_CONFIG_GLOBAL: nullConfig,
    GIT_TERMINAL_PROMPT: "0",
    GIT_OPTIONAL_LOCKS: "0",
    GIT_NO_REPLACE_OBJECTS: "1",
    GIT_PAGER: "cat",
  };
  const r = runner(nativeGit, ["--no-replace-objects", "-c", "core.fsmonitor=false", "-c", `core.hooksPath=${nullConfig}`, "-c", `safe.directory=${checkoutPath}`, "-C", checkoutPath, ...args], { encoding: null, windowsHide: true, timeout: 10000, maxBuffer: MAX_REQUEST_BYTES, env });
  if (r.error || r.status !== 0 || !Buffer.isBuffer(r.stdout) || r.stdout.length > MAX_REQUEST_BYTES) throw new Error(`bounded native Git query failed: ${args[0]}`);
  return r.stdout;
}

function readCommittedAuthorityData(authority, rootFingerprint, checkpoint, runner = spawnSync) {
  const normalized = normalizeKeyAuthority(authority);
  let realCheckout;
  try { realCheckout = realpathSync.native(normalized.checkoutPath); }
  catch { throw new Error("key authority checkout path does not exist"); }
  if (!sameCheckoutPath(realCheckout, normalized.checkoutPath)) throw new Error("key authority checkout path is not its canonical local path");
  const top = runNativeGit(realCheckout, ["rev-parse", "--show-toplevel"], runner).toString("utf8").trim();
  if (!sameCheckoutPath(realpathSync.native(top), realCheckout)) throw new Error("key authority path must be repository root");
  const origin = runNativeGit(realCheckout, ["config", "--local", "--get", "remote.origin.url"], runner).toString("utf8").trim();
  let canonicalOrigin;
  try { canonicalOrigin = canonicalRepositoryIdentity(origin); }
  catch { throw new Error("key authority repository origin is invalid"); }
  if (canonicalOrigin !== normalized.repositoryIdentity) throw new Error("key authority repository origin does not match protected identity");
  const commit = runNativeGit(realCheckout, ["rev-parse", "--verify", "HEAD^{commit}"], runner).toString("utf8").trim();
  if (!/^[a-f0-9]{40,64}$/.test(commit)) throw new Error("repository HEAD commit identifier is malformed");
  const tree = runNativeGit(realCheckout, ["rev-parse", "--verify", `${commit}^{tree}`], runner).toString("utf8").trim();
  const treeEntry = runNativeGit(realCheckout, ["ls-tree", "-z", "--full-tree", commit, "--", ".rightkit/ownership-keys.json"], runner).toString("utf8");
  const entries = treeEntry.split("\0").filter(Boolean);
  if (entries.length !== 1) throw new Error("committed ownership key log is missing or ambiguous");
  const match = entries[0].match(/^(100644|100755) blob ([a-f0-9]{40,64})\t\.rightkit\/ownership-keys\.json$/);
  if (!match) throw new Error("committed ownership key log must be a regular blob, not a gitlink or special entry");
  const committedBytes = runNativeGit(realCheckout, ["show", `${commit}:.rightkit/ownership-keys.json`], runner);
  if (committedBytes.length > MAX_REQUEST_BYTES) throw new Error("committed ownership key log exceeds size limit");
  const worktreePath = path.join(realCheckout, ".rightkit", "ownership-keys.json");
  let worktreeStat;
  try { worktreeStat = lstatSync(worktreePath); } catch { throw new Error("committed ownership key log is missing from protected checkout"); }
  if (!worktreeStat.isFile() || worktreeStat.isSymbolicLink() || (process.platform === "win32" && (worktreeStat.attributes & 0x400)) || worktreeStat.size > MAX_REQUEST_BYTES) throw new Error("working checkout ownership key log must be a bounded regular file");
  const currentBytes = readFileSync(worktreePath);
  if (!currentBytes.equals(committedBytes)) throw new Error("working checkout ownership key log differs from committed bytes");
  let committedLog;
  try { committedLog = JSON.parse(committedBytes.toString("utf8")); }
  catch { throw new Error("committed ownership key log is malformed JSON"); }
  const state = verifyKeyLog(committedLog, { rootFingerprint, checkpoint });
  if (!state.ok) throw new Error(`committed key log rejected: ${state.problems.join("; ")}`);
  if (!/^[a-f0-9]{40,64}$/.test(tree)) throw new Error("repository tree identifier is malformed");
  return { log: committedLog, bytes: committedBytes, digest: createHash("sha256").update(committedBytes).digest("hex"), rootFingerprint: state.rootFingerprint, keyCheckpoint: state.checkpoint, authority: { repositoryIdentity: normalized.repositoryIdentity, commit, tree } };
}

function validateCommittedAuthority(authority, log, rootFingerprint, checkpoint, runner = spawnSync) {
  const committed = readCommittedAuthorityData(authority, rootFingerprint, checkpoint, runner);
  if (canonicalJson(committed.log) !== canonicalJson(log)) throw new Error("request key log differs from committed ownership key log");
  return committed.authority;
}

/** Read & verify authority-bound committed key state from its pinned local checkout. */
export function readCommittedKeyAuthority(trust) {
  try {
    if (!trust?.keyAuthority || !HEX.test(trust.rootFingerprint ?? "") || !trust.keyCheckpoint) throw new Error("protected key authority, root pin, or checkpoint is unavailable");
    return { ok: true, ...readCommittedAuthorityData(trust.keyAuthority, trust.rootFingerprint, trust.keyCheckpoint) };
  } catch (error) { return { ok: false, problems: [error.message] }; }
}

function ensureNoLinks(file) {
  const parsed = path.parse(file);
  let current = parsed.root;
  const pieces = file.slice(parsed.root.length).split(path.sep).filter(Boolean);
  for (const part of pieces) {
    current = path.join(current, part);
    try {
      const st = lstatSync(current);
      if (st.isSymbolicLink() || (process.platform === "win32" && (st.attributes & 0x400))) throw new Error(`trust path contains a symlink or reparse point: ${current}`);
    } catch (error) {
      if (error.code !== "ENOENT") throw error;
    }
  }
}

function assertElevated() {
  if (process.platform === "win32") {
    const r = spawnSync("C:\\Windows\\System32\\WindowsPowerShell\\v1.0\\powershell.exe", ["-NoProfile", "-NonInteractive", "-Command", "$p=New-Object Security.Principal.WindowsPrincipal([Security.Principal.WindowsIdentity]::GetCurrent()); if (!$p.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) { exit 9 }"], { env: windowsPowerShellEnvironment() });
    if (r.status !== 0) throw new Error("protected trust updater requires an elevated Administrator process");
  } else if (process.platform === "darwin") {
    if (process.getuid?.() !== 0) throw new Error("protected trust updater requires root");
  } else throw new Error(`protected trust update unsupported platform: ${process.platform}`);
}

function hardenWindows(directory, file, runner = spawnSync) {
  const script = "$ErrorActionPreference='Stop'; $sidSys=New-Object Security.Principal.SecurityIdentifier('S-1-5-18'); $sidAdm=New-Object Security.Principal.SecurityIdentifier('S-1-5-32-544'); $sidUsers=New-Object Security.Principal.SecurityIdentifier('S-1-5-32-545'); foreach($p in @($env:RK_TRUST_DIR,$env:RK_TRUST_FILE)){ $a=New-Object Security.AccessControl.DirectorySecurity; if($p -eq $env:RK_TRUST_FILE){$a=New-Object Security.AccessControl.FileSecurity}; $a.SetAccessRuleProtection($true,$false); $a.SetOwner($sidAdm); $inherit=if($p -eq $env:RK_TRUST_FILE){[Security.AccessControl.InheritanceFlags]::None}else{[Security.AccessControl.InheritanceFlags]'ContainerInherit,ObjectInherit'}; foreach($sid in @($sidSys,$sidAdm)){ $rule=New-Object Security.AccessControl.FileSystemAccessRule($sid,'FullControl',$inherit,[Security.AccessControl.PropagationFlags]::None,'Allow'); $a.AddAccessRule($rule) }; $read=New-Object Security.AccessControl.FileSystemAccessRule($sidUsers,'ReadAndExecute',$inherit,[Security.AccessControl.PropagationFlags]::None,'Allow'); $a.AddAccessRule($read); Set-Acl -LiteralPath $p -AclObject $a }; $a=Get-Acl -LiteralPath $env:RK_TRUST_DIR; $a.SetOwner($sidSys); Set-Acl -LiteralPath $env:RK_TRUST_DIR -AclObject $a";
  const r = runner("C:\\Windows\\System32\\WindowsPowerShell\\v1.0\\powershell.exe", ["-NoProfile", "-NonInteractive", "-Command", script], { encoding: "utf8", windowsHide: true, env: windowsPowerShellEnvironment({ RK_TRUST_DIR: directory, RK_TRUST_FILE: file }) });
  if (r.status !== 0) throw new Error("could not secure protected trust ACL");
}

export const __testOnly = Object.freeze({
  runWindowsAclSetupInDisposableDirectory(directory, runner) {
    const file = path.join(directory, "trust.json");
    hardenWindows(directory, file, runner);
    return { directory, file };
  },
  validateCommittedAuthority,
});

function runWindowsAclScript(script, environment) {
  const r = spawnSync("C:\\Windows\\System32\\WindowsPowerShell\\v1.0\\powershell.exe", ["-NoProfile", "-NonInteractive", "-Command", script], { encoding: "utf8", windowsHide: true, env: windowsPowerShellEnvironment(environment) });
  if (r.status !== 0) throw new Error("could not create or secure protected ownership path");
}

function setWindowsDirectoryAcl(directory) {
  const script = "$ErrorActionPreference='Stop'; $d=$env:RK_TRUST_DIR; $sidSys=New-Object Security.Principal.SecurityIdentifier('S-1-5-18'); $sidAdm=New-Object Security.Principal.SecurityIdentifier('S-1-5-32-544'); $sidUsers=New-Object Security.Principal.SecurityIdentifier('S-1-5-32-545'); $a=New-Object Security.AccessControl.DirectorySecurity; $a.SetAccessRuleProtection($true,$false); $a.SetOwner($sidAdm); foreach($sid in @($sidSys,$sidAdm)){ $a.AddAccessRule((New-Object Security.AccessControl.FileSystemAccessRule($sid,'FullControl','ContainerInherit,ObjectInherit','None','Allow'))) }; $a.AddAccessRule((New-Object Security.AccessControl.FileSystemAccessRule($sidUsers,'ReadAndExecute','ContainerInherit,ObjectInherit','None','Allow'))); Set-Acl -LiteralPath $d -AclObject $a";
  runWindowsAclScript(script, { RK_TRUST_DIR: directory });
}

function prepareDirectory(directory) {
  ensureNoLinks(directory);
  if (process.platform === "win32") {
    const root = path.parse(directory).root;
    const remainder = directory.slice(root.length).split(path.sep).filter(Boolean);
    let current = root;
    for (const part of remainder) {
      current = path.join(current, part);
      let created = false;
      try { const st = lstatSync(current); if (!st.isDirectory() || st.isSymbolicLink() || (st.attributes & 0x400)) throw new Error(`protected trust parent is unsafe: ${current}`); }
      catch (error) { if (error.code !== "ENOENT") throw error; mkdirSync(current); created = true; }
      if (created) setWindowsDirectoryAcl(current);
      else auditProtected(current);
    }
  } else {
    const root = "/";
    const remainder = directory.slice(root.length).split(path.sep).filter(Boolean);
    let current = root;
    for (const part of remainder) {
      current = path.join(current, part);
      let created = false;
      try { const st = lstatSync(current); if (!st.isDirectory() || st.isSymbolicLink()) throw new Error(`protected trust parent is unsafe: ${current}`); }
      catch (error) { if (error.code !== "ENOENT") throw error; mkdirSync(current, { mode: 0o755 }); created = true; }
      if (created) {
        const chown = spawnSync("/usr/sbin/chown", ["root:wheel", current]);
        const chmod = spawnSync("/bin/chmod", ["755", current]);
        if (chown.status !== 0 || chmod.status !== 0) throw new Error(`could not secure protected ownership directory: ${current}`);
      } else auditProtected(current);
    }
  }
}

function auditProtected(file) {
  if (process.platform === "win32") {
    let current = path.dirname(file);
    const targets = [];
    while (true) { targets.push(current); if (current.toLowerCase() === "c:\\") break; current = path.dirname(current); }
    targets.push(file);
    const script = "$ErrorActionPreference='Stop'; $a=Get-Acl -LiteralPath $env:RK_TRUST_CHECK_TARGET; $item=Get-Item -Force -LiteralPath $env:RK_TRUST_CHECK_TARGET; $out=[ordered]@{ownerSID=$a.GetOwner([Security.Principal.SecurityIdentifier]).Value;ownerName=$a.Owner;attributes=$item.Attributes.ToString();directory=[bool]$item.PSIsContainer;nonempty=(!$item.PSIsContainer -or [System.IO.Directory]::EnumerateFileSystemEntries($item.FullName).GetEnumerator().MoveNext());elevated=$true;access=@($a.Access | ForEach-Object { [ordered]@{identitySID=$_.IdentityReference.Translate([Security.Principal.SecurityIdentifier]).Value;type=$_.AccessControlType.ToString();mask=[int64]$_.FileSystemRights.value__;propagationFlags=[int]$_.PropagationFlags} })}; $out | ConvertTo-Json -Compress -Depth 5";
    const problems = [];
    for (const target of targets) {
      try { if (lstatSync(target).isSymbolicLink()) problems.push(`trust path contains a reparse/symlink: ${target}`); } catch { problems.push(`cannot inspect trust path: ${target}`); continue; }
      const r = spawnSync("C:\\Windows\\System32\\WindowsPowerShell\\v1.0\\powershell.exe", ["-NoProfile", "-NonInteractive", "-Command", script], { encoding: "utf8", windowsHide: true, env: windowsPowerShellEnvironment({ RK_TRUST_CHECK_TARGET: target }) });
      if (r.status !== 0) { problems.push(`cannot inspect trust ACL: ${target}`); continue; }
      try { problems.push(...evaluateWindowsAcl(JSON.parse(r.stdout.trim()), { target, requireNonElevated: false }).problems.map((problem) => `${problem}: ${target}`)); }
      catch { problems.push(`malformed ACL result: ${target}`); }
    }
    if (problems.length) throw new Error(`protected trust audit failed: ${problems.join("; ")}`);
    return;
  }
  const result = approvalTestOnly.protectedLocation(file, process.platform);
  if (!result.ok) throw new Error(`protected trust audit failed: ${result.problems.join("; ")}`);
}

function readExisting(file) {
  try {
    const st = lstatSync(file);
    if (st.isSymbolicLink() || !st.isFile() || st.size > MAX_TRUST_BYTES) throw new Error("existing protected trust must be a bounded regular file");
    return JSON.parse(readFileSync(file, "utf8"));
  } catch (error) { if (error.code === "ENOENT") return null; throw error; }
}

function acquireUpdateLock(directory) {
  const lock = path.join(directory, ".key-trust-update.lock");
  let fd;
  try { fd = openSync(lock, constants.O_WRONLY | constants.O_CREAT | constants.O_EXCL | (constants.O_NOFOLLOW ?? 0), 0o600); }
  catch (error) { if (error.code === "EEXIST") throw new Error("another ownership trust update is active; refusing concurrent update"); throw error; }
  try { writeFileSync(fd, `${process.pid}\n`); fsyncSync(fd); }
  catch (error) { closeSync(fd); try { unlinkSync(lock); } catch {} throw error; }
  closeSync(fd);
  return () => { try { unlinkSync(lock); } catch (error) { if (error.code !== "ENOENT") throw error; } };
}

function atomicProtectedWrite(file, trust) {
  const directory = path.dirname(file);
  const temp = path.join(directory, `.trust-${process.pid}-${Date.now()}.tmp`);
  let fd;
  try {
    fd = openSync(temp, constants.O_WRONLY | constants.O_CREAT | constants.O_EXCL | (constants.O_NOFOLLOW ?? 0), 0o600);
    writeFileSync(fd, `${JSON.stringify(trust, null, 2)}\n`);
    fsyncSync(fd);
    closeSync(fd); fd = undefined;
    if (process.platform === "win32") hardenWindows(directory, temp);
    else {
      const chown = spawnSync("/usr/sbin/chown", ["root:wheel", temp]);
      if (chown.status !== 0) throw new Error("could not set protected trust file owner");
      const modeFd = openSync(temp, constants.O_RDONLY);
      try { fchmodSync(modeFd, 0o644); } finally { closeSync(modeFd); }
    }
    renameSync(temp, file);
    if (process.platform !== "win32") {
      const dirFd = openSync(directory, constants.O_RDONLY);
      try { fsyncSync(dirFd); } finally { closeSync(dirFd); }
    }
  } catch (error) {
    if (fd !== undefined) closeSync(fd);
    try { unlinkSync(temp); } catch {}
    throw error;
  }
}

/** Elevated child entrypoint. Writes only the fixed native trust path. */
const versionParts = (value) => String(value).split(".").map(Number);
function versionAtLeast(value, floor) {
  const a = versionParts(value), b = versionParts(floor);
  for (let i = 0; i < Math.max(a.length, b.length); i += 1) { const x = a[i] ?? 0, y = b[i] ?? 0; if (x !== y) return x > y; }
  return true;
}
/**
 * Floors follow the installed (digest-verified) RightKit release this elevated code runs from:
 * its own catalog digest/version and scanner version. Floors only ever rise; an older installed
 * release leaves the existing floors untouched. The request never supplies floors.
 */
export function raiseFloorsToInstalledRelease(trust, { manifest = installedCatalog(), scannerVersion = SCANNER_VERSION } = {}) {
  const catalogVersion = String(manifest?.version ?? "");
  if (!/^\d+(?:\.\d+)*$/.test(catalogVersion) || !/^\d+(?:\.\d+)*$/.test(scannerVersion)) return trust;
  if (!versionAtLeast(catalogVersion, trust.minimumCatalogVersion ?? "0") || !versionAtLeast(scannerVersion, trust.minimumScannerVersion ?? "0")) return trust;
  const raised = { ...trust, minimumCatalogVersion: catalogVersion, minimumScannerVersion: scannerVersion, minimumCatalogDigest: createHash("sha256").update(canonicalJson(manifest)).digest("hex") };
  if (!validateTrustRegistry(raised).ok) return trust;
  return raised;
}
function installedCatalog() { return JSON.parse(readFileSync(path.join(path.dirname(fileURLToPath(import.meta.url)), "rightkit-ownership.json"), "utf8")); }

/** Typed, non-blocking warnings from the registry merge. The trust update still applies everything else. */
export const REPOSITORY_DRIFT_CODE = "RK_OWNERSHIP_REPOSITORY_DRIFT";
export const REPOSITORY_REGISTRY_MALFORMED_CODE = "RK_OWNERSHIP_REPOSITORY_REGISTRY_MALFORMED";
const warning = (code, message, repositoryIdentity) => ({ code, type: code, message, ...(repositoryIdentity ? { repositoryIdentity } : {}) });
const REPOSITORY_REGISTRY_FILE = "ownership-repositories.json";
const MAX_REGISTRY_BYTES = 256 * 1024;
const REPOSITORY_FIELDS = ["repositoryIdentity", "roots", "visibility"];
function installedRepositoryRegistry() {
  const file = path.join(path.dirname(fileURLToPath(import.meta.url)), REPOSITORY_REGISTRY_FILE);
  const stat = lstatSync(file);
  if (!stat.isFile() || stat.isSymbolicLink() || (process.platform === "win32" && (stat.attributes & 0x400)) || stat.size > MAX_REGISTRY_BYTES) throw new Error("ownership repository registry must be a bounded regular file");
  try { return JSON.parse(readFileSync(file, "utf8")); }
  catch { throw new Error("ownership repository registry is malformed JSON"); }
}
/** Fail closed: every entry must be exact, canonical, unique, and pass validateTrustRegistry's repository rules. */
function validatedRegistryEntries(registry) {
  if (!exactObject(registry, ["schema", "repositories"]) || registry.schema !== 1 || !Array.isArray(registry.repositories) || !registry.repositories.length) throw new Error("ownership repository registry schema is malformed");
  const identities = new Set();
  for (const entry of registry.repositories) {
    if (!exactObject(entry, REPOSITORY_FIELDS)) throw new Error("ownership repository registry entries must have exactly repositoryIdentity, roots and visibility");
    let identity;
    try { identity = canonicalRepositoryIdentity(entry.repositoryIdentity); }
    catch { throw new Error("ownership repository registry identity is invalid"); }
    if (identity !== entry.repositoryIdentity) throw new Error(`ownership repository registry identity is not canonical: ${entry.repositoryIdentity}`);
    if (identities.has(identity)) throw new Error(`ownership repository registry duplicates ${identity}`);
    identities.add(identity);
  }
  // Floors are placeholders here: only the repository rules of validateTrustRegistry are being checked.
  const probe = validateTrustRegistry({ schema: 1, minimumScannerVersion: "0", minimumCatalogVersion: "0", revokedKeyIds: [], verifiedTrackingEvidence: [], repositories: registry.repositories, provenance: [] });
  if (!probe.ok) throw new Error(`ownership repository registry is invalid: ${probe.problems.join("; ")}`);
  return registry.repositories.map(copyJson);
}
/**
 * Additions only, from the committed sibling registry. The trust request never supplies registrations.
 * Returns { trust, warnings }; it never throws for registry problems, so revocations and floors always apply.
 * - An identity already registered with different roots or visibility keeps its protected entry unchanged,
 *   that registry entry is skipped, and a RK_OWNERSHIP_REPOSITORY_DRIFT warning is returned.
 * - A malformed registry file skips the whole registry merge with a RK_OWNERSHIP_REPOSITORY_REGISTRY_MALFORMED warning.
 * Provenance, exceptions, and every other field are untouched. firstInstall starts from no registrations.
 */
export function mergeRepositoriesFromInstalledRelease(trust, { firstInstall = false, registry } = {}) {
  if (!Array.isArray(trust?.repositories)) throw new Error("protected repository registry missing");
  const existing = firstInstall ? [] : trust.repositories.map(copyJson);
  let incoming;
  try { incoming = validatedRegistryEntries(registry ?? installedRepositoryRegistry()); }
  catch (error) { return { trust: { ...trust, repositories: existing }, warnings: [warning(REPOSITORY_REGISTRY_MALFORMED_CODE, `repository registration merge skipped: ${error.message}`)] }; }
  const byIdentity = new Map(existing.map((entry) => [entry.repositoryIdentity, entry]));
  const additions = [];
  const warnings = [];
  for (const entry of incoming) {
    const current = byIdentity.get(entry.repositoryIdentity);
    if (!current) { additions.push(entry); continue; }
    if (canonicalJson(current.roots) !== canonicalJson(entry.roots) || current.visibility !== entry.visibility) {
      warnings.push(warning(REPOSITORY_DRIFT_CODE, `protected repository registration differs from the installed release; kept protected entry, skipped registry entry: ${entry.repositoryIdentity}`, entry.repositoryIdentity));
    }
  }
  const merged = { ...trust, repositories: [...existing, ...additions] };
  const check = validateTrustRegistry(merged);
  if (!check.ok) throw new Error(`merged protected repository registry is invalid: ${check.problems.join("; ")}`);
  return { trust: merged, warnings };
}

export function performProtectedTrustUpdate({ log, rootFingerprint, baseline, authority } = {}) {
  assertElevated();
  const file = OWNERSHIP_TRUST_PATH[process.platform];
  const directory = path.dirname(file);
  prepareDirectory(directory);
  auditProtected(directory);
  ensureNoLinks(file);
  const releaseLock = acquireUpdateLock(directory);
  try {
    // Re-read under the protected lock so every accepted checkpoint follows latest disk state.
    let existing = null;
    try { lstatSync(file); auditProtected(file); existing = readExisting(file); } catch (error) { if (error.code !== "ENOENT") throw error; existing = null; }
    const prepared = prepareTrustUpdate(existing, { log, rootFingerprint, baseline, authority });
    const merged = mergeRepositoriesFromInstalledRelease(prepared, { firstInstall: existing == null });
    const trust = raiseFloorsToInstalledRelease(merged.trust);
    if (trust.keyAuthority) validateCommittedAuthority(trust.keyAuthority, log, trust.rootFingerprint, trust.keyCheckpoint);
    atomicProtectedWrite(file, trust);
    auditProtected(file);
    return { path: file, trust, warnings: merged.warnings };
  } finally { releaseLock(); }
}

function readPinnedRequest(requestPath, expectedDigest) {
  if (!path.isAbsolute(requestPath) || !HEX.test(expectedDigest ?? "")) throw new Error("request path and SHA-256 digest are required");
  const fd = openSync(requestPath, constants.O_RDONLY | (constants.O_NOFOLLOW ?? 0));
  try {
    const st = fstatSync(fd);
    if (!st.isFile() || st.size < 2 || st.size > MAX_REQUEST_BYTES) throw new Error("request must be a bounded regular file");
    const bytes = readFileSync(fd);
    if (createHash("sha256").update(bytes).digest("hex") !== expectedDigest) throw new Error("request bytes do not match pinned SHA-256");
    return JSON.parse(bytes.toString("utf8"));
  } finally { closeSync(fd); }
}

const realOrSelf = (file) => { try { return realpathSync(file); } catch { return path.resolve(file); } };
if (process.argv[1] && realOrSelf(process.argv[1]) === realOrSelf(fileURLToPath(import.meta.url))) {
  try {
    if (process.argv.length !== 6 || process.argv[2] !== "--request" || process.argv[4] !== "--sha256") throw new Error("usage: ownership-key-trust.mjs --request <absolute-json> --sha256 <digest>");
    const request = readPinnedRequest(process.argv[3], process.argv[5]);
    if (!exactObject(request, ["schema", "log", "rootFingerprint", ...(Object.hasOwn(request ?? {}, "baseline") ? ["baseline"] : []), ...(Object.hasOwn(request ?? {}, "authority") ? ["authority"] : [])]) || request.schema !== 1 || !Array.isArray(request.log?.events) || request.log.schema !== 1) throw new Error("key trust request schema is malformed");
    const result = performProtectedTrustUpdate({ log: request.log, rootFingerprint: request.rootFingerprint, baseline: request.baseline, authority: request.authority });
    for (const item of result.warnings) process.stderr.write(`${item.code}: ${item.message}\n`);
    process.stdout.write(`${JSON.stringify({ path: result.path, checkpoint: result.trust.keyCheckpoint, warnings: result.warnings })}\n`);
  } catch (error) { process.stderr.write(`${error.message}\n`); process.exitCode = 2; }
}
