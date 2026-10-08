#!/usr/bin/env node
// The adapter invokes only the fixed machine-installed verifier. It never loads
// trust, floors, catalog, or scanner code from the repository being checked.
import { spawnSync } from "node:child_process";
import { lstatSync, readFileSync, realpathSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { catalogDigest, committedKeyLog, sourceTreeDigest, validateProtectedReceipt } from "./receipt.mjs";
import { loadTrustedExternal } from "../../packages/release/ownership-approval.mjs";
import { verifyProtectedFileManifest } from "./package-manifest.mjs";
import { nativeGitExecutable, protectedChildEnvironment } from "./runtime-environment.mjs";
import { readCommittedKeyAuthority } from "../../packages/release/ownership-key-authority.mjs";
import { evaluateWindowsAcl } from "../trust-schema.mjs";
import { SCANNER_VERSION } from "../../packages/release/ownership.mjs";

const INSTALL_ROOTS = Object.freeze({
  win32: "C:\\Program Files\\Orthic Labs\\RightKit",
  darwin: "/Library/Application Support/Orthic Labs/RightKit",
});
const RUNTIME_PATHS = Object.freeze({
  win32: path.join(INSTALL_ROOTS.win32, "runtime", "node.exe"),
  darwin: path.join(INSTALL_ROOTS.darwin, "runtime", "bin", "node"),
});
const VERIFIER_PATHS = Object.freeze({
  win32: path.join(INSTALL_ROOTS.win32, "ownership", "adapters", "private-verifier.mjs"),
  darwin: path.join(INSTALL_ROOTS.darwin, "ownership", "adapters", "private-verifier.mjs"),
});
const PUBLIC_PRODUCER_PATHS = Object.freeze({
  win32: path.join(INSTALL_ROOTS.win32, "ownership", "adapters", "public-check-producer.mjs"),
  darwin: path.join(INSTALL_ROOTS.darwin, "ownership", "adapters", "public-check-producer.mjs"),
});
const WINDOWS_SYSTEM_SIDS = new Set(["S-1-5-18", "S-1-5-32-544", "S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464"]);

const WINDOWS_AUDIT = String.raw`$ErrorActionPreference='Stop'
$root=$env:RK_OWNERSHIP_ROOT
$targets=@(Get-ChildItem -LiteralPath $root -Force -Recurse) + @(Get-Item -LiteralPath $root -Force)
$parent=Split-Path -Parent $root
while($parent){$targets+=Get-Item -LiteralPath $parent -Force;$next=Split-Path -Parent $parent;if($next -eq $parent){break};$parent=$next}
$seen=@{}
$items=@(foreach($item in $targets){
  if($seen.ContainsKey($item.FullName)){continue};$seen[$item.FullName]=$true
  $attributes=$item.Attributes.ToString()
  try{$acl=Get-Acl -LiteralPath $item.FullName;$ownerSID=$acl.GetOwner([Security.Principal.SecurityIdentifier]).Value
    $access=@($acl.Access | ForEach-Object {[ordered]@{identitySID=$_.IdentityReference.Translate([Security.Principal.SecurityIdentifier]).Value;type=$_.AccessControlType.ToString();mask=[int64]$_.FileSystemRights.value__;propagationFlags=[int]$_.PropagationFlags}})
    $nonempty=(!$item.PSIsContainer -or [System.IO.Directory]::EnumerateFileSystemEntries($item.FullName).GetEnumerator().MoveNext())
    [ordered]@{path=$item.FullName;ownerSID=$ownerSID;ownerName=$acl.Owner;attributes=$attributes;directory=[bool]$item.PSIsContainer;nonempty=[bool]$nonempty;access=$access}
  }catch{[ordered]@{path=$item.FullName;ownerSID='';ownerName='';attributes=$attributes;directory=[bool]$item.PSIsContainer;nonempty=$false;access=$null}}
})
$id=[Security.Principal.WindowsIdentity]::GetCurrent();$principal=New-Object Security.Principal.WindowsPrincipal($id)
$groups=@($id.Groups | ForEach-Object {$_.Translate([Security.Principal.SecurityIdentifier]).Value})
$public=$null
if($env:RK_OWNERSHIP_PUBLIC_CONFIG){
  $cfg=Get-Content -Raw -LiteralPath $env:RK_OWNERSHIP_PUBLIC_CONFIG | ConvertFrom-Json;$key=Get-Item -Force -LiteralPath $env:RK_OWNERSHIP_PUBLIC_KEY;$acl=Get-Acl -LiteralPath $key.FullName
  $access=@($acl.Access | ForEach-Object {[ordered]@{identitySID=$_.IdentityReference.Translate([Security.Principal.SecurityIdentifier]).Value;type=$_.AccessControlType.ToString();mask=[int64]$_.FileSystemRights.value__;propagationFlags=[int]$_.PropagationFlags}})
  $keyInfo=[ordered]@{path=$key.FullName;ownerSID=$acl.GetOwner([Security.Principal.SecurityIdentifier]).Value;ownerName=$acl.Owner;attributes=$key.Attributes.ToString();directory=$false;nonempty=$true;access=$access}
  $checkoutItems=@()
  if($env:RK_OWNERSHIP_CHECKOUT){$checkout=Get-Item -LiteralPath $env:RK_OWNERSHIP_CHECKOUT -Force;foreach($item in @($checkout)+@(Get-ChildItem -LiteralPath $checkout.FullName -Force -Recurse)){ $checkoutAcl=Get-Acl -LiteralPath $item.FullName;$checkoutAccess=@($checkoutAcl.Access | ForEach-Object {[ordered]@{identitySID=$_.IdentityReference.Translate([Security.Principal.SecurityIdentifier]).Value;type=$_.AccessControlType.ToString();mask=[int64]$_.FileSystemRights.value__;propagationFlags=[int]$_.PropagationFlags}});$checkoutItems+=[ordered]@{path=$item.FullName;ownerSID=$checkoutAcl.GetOwner([Security.Principal.SecurityIdentifier]).Value;ownerName=$checkoutAcl.Owner;attributes=$item.Attributes.ToString();directory=[bool]$item.PSIsContainer;nonempty=(!$item.PSIsContainer -or [System.IO.Directory]::EnumerateFileSystemEntries($item.FullName).GetEnumerator().MoveNext());elevated=$false;ancestor=$false;access=$checkoutAccess} };$parent=Split-Path -Parent $checkout.FullName;while($parent){$item=Get-Item -LiteralPath $parent -Force;$checkoutAcl=Get-Acl -LiteralPath $item.FullName;$checkoutAccess=@($checkoutAcl.Access | ForEach-Object {[ordered]@{identitySID=$_.IdentityReference.Translate([Security.Principal.SecurityIdentifier]).Value;type=$_.AccessControlType.ToString();mask=[int64]$_.FileSystemRights.value__;propagationFlags=[int]$_.PropagationFlags}});$checkoutItems+=[ordered]@{path=$item.FullName;ownerSID=$checkoutAcl.GetOwner([Security.Principal.SecurityIdentifier]).Value;ownerName=$checkoutAcl.Owner;attributes=$item.Attributes.ToString();directory=$true;nonempty=$true;elevated=$false;ancestor=$true;access=$checkoutAccess};$next=Split-Path -Parent $parent;if($next -eq $parent){break};$parent=$next} }
  $public=[ordered]@{config=$cfg;key=$keyInfo;checkout=$checkoutItems}
}
@{items=$items;identitySID=$id.User.Translate([Security.Principal.SecurityIdentifier]).Value;groupSIDs=$groups;elevated=$principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator);public=$public}|ConvertTo-Json -Compress -Depth 8`;

function auditProtectedInstallImpl({ platform = process.platform, root = INSTALL_ROOTS[platform], runtime = RUNTIME_PATHS[platform], entry = process.argv[1], processPath = process.execPath, runner = spawnSync, isPublic = false, repoRoot } = {}) {
  const problems = [];
  if (!root || !runtime) return { ok: false, problems: [`protected verifier install is unsupported on ${platform}`] };
  let publicConfig;
  if (isPublic) {
    if (typeof repoRoot !== "string" || !path.isAbsolute(repoRoot)) return { ok: false, problems: ["public protected checkout path is required"] };
    try {
      publicConfig = JSON.parse(readFileSync(path.join(root, "ownership", "public-check.json"), "utf8"));
      const service = publicConfig.service;
      if (!service || !/^S-1-\d+(?:-\d+)+$/.test(service.windowsSid ?? "") || !Array.isArray(service.agentWindowsSids) || !service.agentWindowsSids.length || !path.win32.isAbsolute(service.windowsCheckoutRoot ?? "") || !Number.isSafeInteger(service.macosUid) || !Array.isArray(service.agentMacosUids) || !service.agentMacosUids.length || !path.posix.isAbsolute(service.macosCheckoutRoot ?? "")) throw new Error("missing isolated service identities or checkout roots");
      const pathApi = platform === "win32" ? path.win32 : path.posix;
      const checkoutPath = pathApi.resolve(repoRoot);
      const checkoutBase = pathApi.resolve(platform === "win32" ? service.windowsCheckoutRoot : service.macosCheckoutRoot);
      const relative = pathApi.relative(checkoutBase, checkoutPath);
      if (relative.startsWith("..") || pathApi.isAbsolute(relative)) throw new Error("requested checkout is outside protected service checkout root");
    } catch (error) { return { ok: false, problems: [`protected public service config failed: ${error.message}`] }; }
  }
  const entryName = path.basename(entry ?? "");
  const expectedEntry = entryName && ["private-local.mjs", "private-verifier.mjs", "public-check-producer.mjs"].includes(entryName)
    ? path.join(root, "ownership", "adapters", entryName)
    : undefined;
  if (!expectedEntry) problems.push("adapter entry name is not an allowed fixed program");
  for (const [label, target, expected] of [["runtime", processPath, runtime], ["entry", entry, expectedEntry]]) {
    try {
      if (path.resolve(target) !== path.resolve(expected) || realpathSync(target) !== path.resolve(expected)) problems.push(`${label} is not the fixed installer-owned path`);
      if (lstatSync(target).isSymbolicLink()) problems.push(`${label} is a link or reparse point`);
    } catch { problems.push(`${label} cannot be inspected`); }
  }
  let result;
  if (platform === "win32") {
    const ownershipRoot = path.join(root, "ownership");
    result = runner("C:\\Windows\\System32\\WindowsPowerShell\\v1.0\\powershell.exe", ["-NoProfile", "-NonInteractive", "-Command", WINDOWS_AUDIT], { encoding: "utf8", windowsHide: true, timeout: 30_000, maxBuffer: 16 * 1024 * 1024, env: { ...protectedChildEnvironment(), RK_OWNERSHIP_ROOT: root, ...(isPublic ? { RK_OWNERSHIP_PUBLIC_CONFIG: path.join(ownershipRoot, "public-check.json"), RK_OWNERSHIP_PUBLIC_KEY: path.join(ownershipRoot, "public-check-app.pem"), RK_OWNERSHIP_CHECKOUT: path.resolve(repoRoot) } : {}) } });
  } else if (platform === "darwin") {
    const macServiceName = isPublic ? os.userInfo().username : "";
    if (isPublic && !/^[A-Za-z0-9_-]+$/.test(macServiceName)) problems.push("public macOS service short name must use only letters, numbers, underscores, or hyphens");
    const script = `set -eu
check_one(){ p="$1"; test ! -L "$p" || return 21; owner="$(stat -f %u "$p")"; mode="$(stat -f %Lp "$p")"; case "$mode" in ""|*[!0-7]*) return 23;; esac; if [ "$p" = "$RK_OWNERSHIP_PUBLIC_KEY" ]; then test "$owner" = "$RK_SERVICE_UID" || test "$owner" = 0 || return 22; test $(( 0$mode & 077 )) -eq 0 || return 23; /bin/ls -lde "$p" | /usr/bin/awk -v svc="$RK_SERVICE_NAME" 'NR>1 && /allow/ && /(writesecurity|chown)/ && !($0 ~ /^[[:space:]]*[0-9]+: user:root allow([[:space:]]|$)/) {bad=1} NR>1 && /allow/ && !($0 ~ /^[[:space:]]*[0-9]+: user:root allow/ || $0 ~ "^[[:space:]]*[0-9]+: user:" svc " allow") {bad=1} NR>1 && /allow/ && /(writesecurity|chown|write|delete|append|writeattr|writeextattr|delete_child|add_file|add_subdirectory)/ && !($0 ~ /^[[:space:]]*[0-9]+: user:root allow/) {bad=1} END {exit bad}' || return 26; elif [ "$p" = "$RK_CHECKPOINTS_DIR" ] || case "$p" in "$RK_CHECKPOINTS_DIR"/*) true;; *) false;; esac; then case "$p" in "$RK_CHECKPOINTS_DIR") test "$owner" = 0 || return 22;; *) test "$owner" = 0 || test "$owner" = "$RK_SERVICE_UID" || return 22;; esac; test $(( 0$mode & 022 )) -eq 0 || return 23; /bin/ls -lde "$p" | /usr/bin/awk -v svc="$RK_SERVICE_NAME" 'NR>1 && /allow/ && /(writesecurity|chown)/ && !($0 ~ /^[[:space:]]*[0-9]+: user:root allow([[:space:]]|$)/) {bad=1} NR>1 && /allow/ && !($0 ~ /^[[:space:]]*[0-9]+: user:root allow/ || $0 ~ "^[[:space:]]*[0-9]+: user:" svc " allow") {bad=1} NR>1 && /allow/ && /(writesecurity|chown|write|delete|append|writeattr|writeextattr|delete_child|add_file|add_subdirectory)/ && !($0 ~ /^[[:space:]]*[0-9]+: user:root allow/ || $0 ~ "^[[:space:]]*[0-9]+: user:" svc " allow") {bad=1} END {exit bad}' || return 26; /bin/ls -lde "$p" | /usr/bin/awk -v svc="$RK_SERVICE_NAME" 'NR>1 && $0 ~ "^[[:space:]]*[0-9]+: user:" svc " allow" && /(write|add_file)/ {ok=1} END {exit !ok}' || return 27; if [ "$p" = "$RK_CHECKPOINTS_DIR" ]; then /bin/ls -lde "$p" | /usr/bin/awk -v svc="$RK_SERVICE_NAME" 'NR>1 && $0 ~ "^[[:space:]]*[0-9]+: user:" svc " allow" && /add_file/ {a=1} NR>1 && $0 ~ "^[[:space:]]*[0-9]+: user:" svc " allow" && /delete_child/ {d=1} END {exit !(a && d)}' || return 41; fi; else test "$owner" = 0 || return 22; test $(( 0$mode & 022 )) -eq 0 || return 23; /bin/ls -lde "$p" | /usr/bin/awk 'NR>1 && /allow/ && /(writesecurity|chown)/ && !($0 ~ /^[[:space:]]*[0-9]+: user:root allow([[:space:]]|$)/) {bad=1} NR>1 && /allow/ && /(writesecurity|chown|write|delete|append|writeattr|writeextattr|delete_child|add_file|add_subdirectory)/ && !($0 ~ /^[[:space:]]*[0-9]+: user:root allow/) {bad=1} END {exit bad}' || return 26; fi; }
root="$RK_OWNERSHIP_ROOT"
if [ "$RK_PUBLIC_MODE" = 1 ]; then test "$(id -u)" = "$RK_SERVICE_UID" || exit 27; case ",$RK_AGENT_UIDS," in *",$(id -u),"*) exit 28;; esac; case " $(id -Gn) " in *" admin "*) exit 29;; esac; if [ "$RK_SIGNED_KEYLOG" = 1 ]; then test -d "$RK_CHECKPOINTS_DIR" || exit 40; fi; fi
find "$root" -print0 | while IFS= read -r -d '' p; do check_one "$p"; done
if [ "$RK_PUBLIC_MODE" = 1 ]; then
  checkout="$RK_OWNERSHIP_CHECKOUT"; test -d "$checkout" || exit 30
  find "$checkout" -print0 | while IFS= read -r -d '' p; do test ! -L "$p" || exit 31; test "$(stat -f %u "$p")" = "$RK_SERVICE_UID" || exit 32; mode="$(stat -f %Lp "$p")"; case "$mode" in ""|*[!0-7]*) exit 33;; esac; test $(( 0$mode & 022 )) -eq 0 || exit 33; /bin/ls -lde "$p" | /usr/bin/awk -v svc="$RK_SERVICE_NAME" 'NR>1 && /allow/ && /(writesecurity|chown)/ && !($0 ~ /^[[:space:]]*[0-9]+: user:root allow([[:space:]]|$)/) {bad=1} NR>1 && /allow/ && /(writesecurity|chown|write|delete|append|writeattr|writeextattr|delete_child|add_file|add_subdirectory)/ && !($0 ~ /^[[:space:]]*[0-9]+: user:root allow/ || $0 ~ "^[[:space:]]*[0-9]+: user:" svc " allow") {bad=1} END {exit bad}' || exit 34; done
  base="$RK_CHECKOUT_BASE"; case "$checkout/" in "$base/"*) ;; *) exit 35;; esac
  p="$(dirname "$checkout")"; while :; do test ! -L "$p" || exit 36; mode="$(stat -f %Lp "$p")"; case "$mode" in ""|*[!0-7]*) exit 33;; esac; test $(( 0$mode & 022 )) -eq 0 || exit 38; if [ "$p" = "$base" ]; then test "$(stat -f %u "$p")" = "$RK_SERVICE_UID" || exit 37; /bin/ls -lde "$p" | /usr/bin/awk -v svc="$RK_SERVICE_NAME" 'NR>1 && /allow/ && /(writesecurity|chown)/ && !($0 ~ /^[[:space:]]*[0-9]+: user:root allow([[:space:]]|$)/) {bad=1} NR>1 && /allow/ && /(writesecurity|chown|write|delete|append|writeattr|writeextattr|delete_child|add_file|add_subdirectory)/ && !($0 ~ /^[[:space:]]*[0-9]+: user:root allow/ || $0 ~ "^[[:space:]]*[0-9]+: user:" svc " allow") {bad=1} END {exit bad}' || exit 39; else test "$(stat -f %u "$p")" = 0 || exit 37; /bin/ls -lde "$p" | /usr/bin/awk 'NR>1 && /allow/ && /(writesecurity|chown)/ && !($0 ~ /^[[:space:]]*[0-9]+: user:root allow([[:space:]]|$)/) {bad=1} NR>1 && /allow/ && /(writesecurity|chown|write|delete|append|writeattr|writeextattr|delete_child|add_file|add_subdirectory)/ && !($0 ~ /^[[:space:]]*[0-9]+: user:root allow/) {bad=1} END {exit bad}' || exit 39; fi; test "$p" = / && break; p="$(dirname "$p")"; done
fi
p="$root"
while :; do check_one "$p"; test "$p" = / && break; p="$(dirname "$p")"; done
test "$(id -u)" != 0`;
    const keyPath = path.join(root, "ownership", "public-check-app.pem");
    if (problems.length) return { ok: false, problems };
    result = runner("/bin/bash", ["-c", script], { encoding: "utf8", windowsHide: true, timeout: 30_000, maxBuffer: 16 * 1024 * 1024, env: { ...protectedChildEnvironment(), RK_OWNERSHIP_ROOT: root, RK_PUBLIC_MODE: isPublic ? "1" : "0", RK_SIGNED_KEYLOG: publicConfig?.rootFingerprint ? "1" : "0", RK_OWNERSHIP_PUBLIC_KEY: keyPath, RK_CHECKPOINTS_DIR: path.join(root, "ownership", "checkpoints"), RK_OWNERSHIP_CHECKOUT: isPublic ? path.resolve(repoRoot) : "", RK_CHECKOUT_BASE: String(publicConfig?.service?.macosCheckoutRoot ?? ""), RK_SERVICE_UID: String(publicConfig?.service?.macosUid ?? ""), RK_AGENT_UIDS: (publicConfig?.service?.agentMacosUids ?? []).join(","), RK_SERVICE_NAME: macServiceName } });
  } else return { ok: false, problems: [...problems, `protected verifier install is unsupported on ${platform}`] };
  if (result?.status !== 0) problems.push("protected install ownership, permissions, link, or process-token audit failed");
  else if (platform === "win32") {
    try { auditWindowsNative(JSON.parse(result.stdout.trim()), { problems, isPublic, root, repoRoot }); }
    catch { problems.push("protected install audit returned malformed JSON"); }
  }
  const manifest = verifyProtectedFileManifest(root);
  if (!manifest.ok) problems.push(...manifest.problems);
  return { ok: problems.length === 0, problems };
}

export function auditProtectedInstall() { return auditProtectedInstallImpl(); }
export function auditPublicProtectedInstall({ repoRoot } = {}) { return auditProtectedInstallImpl({ isPublic: true, repoRoot }); }
export function auditProtectedInstallForTest(options) { return auditProtectedInstallImpl(options); }

function auditWindowsNative(native, { problems, isPublic, root, repoRoot }) {
  if (!Array.isArray(native.items) || !/^(S-1-\d+(?:-\d+)+)$/.test(native.identitySID ?? "") || !Array.isArray(native.groupSIDs) || typeof native.elevated !== "boolean") throw new Error("missing native ACL or process identity fields");
  const data = isPublic ? native.public : undefined;
  const config = data?.config;
  const serviceSid = config?.service?.windowsSid;
  const agentSids = config?.service?.agentWindowsSids;
  if (isPublic && (!/^S-1-\d+(?:-\d+)+$/.test(serviceSid ?? "") || !Array.isArray(agentSids) || !agentSids.length || agentSids.some((sid) => !/^S-1-\d+(?:-\d+)+$/.test(sid)))) {
    problems.push("protected public service identity configuration is missing or malformed");
    return;
  }
  const key = data?.key;
  const expectedKey = path.join(root, "ownership", "public-check-app.pem");
  if (isPublic && (!key || path.resolve(key.path ?? "") !== path.resolve(expectedKey) || key.directory || /ReparsePoint/i.test(key.attributes ?? "") || !Array.isArray(key.access))) problems.push("protected public App key file is missing, linked, or malformed");
  for (const item of native.items) {
    if (isPublic && path.resolve(item.path ?? "") === path.resolve(expectedKey)) continue;
    if (isPublic && isCheckpointStatePath(item.path, root)) continue;
    if (item.elevated !== native.elevated) item.elevated = native.elevated;
    problems.push(...evaluateWindowsAcl(item, { target: item.path }).problems);
  }
  if (native.elevated) problems.push("protected verifier process must be non-elevated");
  if (!isPublic) return;
  if (serviceSid !== native.identitySID || agentSids.includes(native.identitySID) || native.groupSIDs.some((sid) => agentSids.includes(sid) || WINDOWS_SYSTEM_SIDS.has(sid))) problems.push("public verifier is not running under its exact isolated service identity");
  if (WINDOWS_SYSTEM_SIDS.has(serviceSid)) problems.push("public service identity cannot be SYSTEM, Administrators, or TrustedInstaller");
  auditWindowsCheckpointState(native.items, root, serviceSid, agentSids, problems, config?.rootFingerprint !== undefined);
  if (!key || !Array.isArray(key.access)) return;
  if (![serviceSid, ...WINDOWS_SYSTEM_SIDS].includes(key.ownerSID)) problems.push("public App key owner is outside the service/admin principals");
  for (const ace of key.access) {
    if (ace?.type !== "Allow" || (ace.propagationFlags & 2) !== 0) continue;
    if (![serviceSid, ...WINDOWS_SYSTEM_SIDS].includes(ace.identitySID)) problems.push(`public App key ACL grants access to non-service principal ${ace.identitySID}`);
    if (ace.identitySID === serviceSid && (((ace.mask ?? 0) >>> 0) & ~(0x001200a9)) !== 0) problems.push("public App service principal has write or unknown access beyond read-only key access");
  }
  if (!Array.isArray(data?.checkout) || data.checkout.length === 0) problems.push("public protected checkout ACL inventory is missing");
  else {
    const checkoutRoot = path.resolve(data.checkout[0]?.path ?? "");
    const expectedCheckout = path.resolve(repoRoot ?? "");
    const configuredBase = path.resolve(config.service.windowsCheckoutRoot ?? "");
    const relativeToBase = path.relative(configuredBase, expectedCheckout);
    if (checkoutRoot.toLowerCase() !== expectedCheckout.toLowerCase() || relativeToBase.startsWith("..") || path.isAbsolute(relativeToBase)) problems.push("public checkout is outside exact requested path or configured protected service root");
    for (const item of data.checkout) {
      const itemPath = path.resolve(item.path ?? "");
      const relative = path.relative(checkoutRoot, itemPath);
      if ((!item.ancestor && (relative.startsWith("..") || path.isAbsolute(relative))) || /ReparsePoint/i.test(item.attributes ?? "")) problems.push("public protected checkout contains an escaping or linked object");
      if (item.ancestor) {
        if (itemPath.toLowerCase() === configuredBase.toLowerCase()) {
          if (item.ownerSID !== serviceSid) problems.push("protected checkout base is not owned by exact service identity");
          let serviceWritable = false;
          for (const ace of item.access ?? []) {
            if (ace?.type !== "Allow" || (ace.propagationFlags & 2) !== 0) continue;
            const sid = ace.identitySID;
            const mask = (ace.mask ?? 0) >>> 0;
            const writeMask = 0x000d0116 | 0x40000000 | 0x10000000;
            if (sid === serviceSid && (mask & writeMask) !== 0) serviceWritable = true;
            else if (!WINDOWS_SYSTEM_SIDS.has(sid) && (mask & writeMask) !== 0) problems.push(`protected checkout base grants write authority to non-service principal ${sid}`);
          }
          if (!serviceWritable) problems.push("protected checkout base does not grant checkout management to its service identity");
        } else problems.push(...evaluateWindowsAcl(item, { target: item.path }).problems);
        continue;
      }
      if (item.ownerSID !== serviceSid) problems.push("public protected checkout object is not owned by exact service identity");
      for (const ace of item.access ?? []) {
        if (ace?.type !== "Allow" || (ace.propagationFlags & 2) !== 0) continue;
        const sid = ace.identitySID;
        const mask = (ace.mask ?? 0) >>> 0;
        const writeMask = 0x000d0116 | 0x40000000 | 0x10000000;
        if (sid !== serviceSid && !WINDOWS_SYSTEM_SIDS.has(sid) && (mask & writeMask) !== 0) problems.push(`public checkout grants write authority to non-service principal ${sid}`);
      }
    }
  }
}

function isCheckpointStatePath(itemPath, root) {
  const base = path.resolve(root, "ownership", "checkpoints").toLowerCase();
  const candidate = path.resolve(itemPath ?? "").toLowerCase();
  return candidate === base || candidate.startsWith(`${base}${path.sep}`);
}

function auditWindowsCheckpointState(items, root, serviceSid, agentSids = [], problems = [], required = true) {
  const directory = path.resolve(root, "ownership", "checkpoints");
  const directoryKey = directory.toLowerCase();
  const prefix = `${directoryKey}${path.sep}`;
  const state = items.filter((item) => {
    const itemPath = path.resolve(item.path ?? "").toLowerCase();
    return itemPath === directoryKey || itemPath.startsWith(prefix);
  });
  const rootItem = state.find((item) => path.resolve(item.path ?? "").toLowerCase() === directoryKey);
  if (!rootItem && !required) return problems;
  const adminSids = [...WINDOWS_SYSTEM_SIDS];
  const nonServiceWriteMask = (~0x001200a9) >>> 0;
  if (!rootItem || !rootItem.directory || /ReparsePoint/i.test(rootItem.attributes ?? "")) problems.push("protected checkpoint state directory is missing or linked");
  for (const item of state) {
    const itemPath = path.resolve(item.path ?? "");
    const isRoot = itemPath.toLowerCase() === directoryKey;
    const name = path.basename(itemPath);
    if (!isRoot && (!/^(?:[a-f0-9]{64}\.json|[a-f0-9]{64}\.lock|\.[a-f0-9]{64}\.\d+\.\d+\.tmp)$/.test(name) || item.directory)) problems.push("protected checkpoint subtree contains an unexpected path or directory");
    if (/ReparsePoint/i.test(item.attributes ?? "") || !Array.isArray(item.access)) problems.push("protected checkpoint subtree contains a link or incomplete ACL record");
    if (isRoot ? !adminSids.includes(item.ownerSID) : ![serviceSid, ...adminSids].includes(item.ownerSID)) problems.push("protected checkpoint owner is outside service/admin principals");
    let serviceCanCreate = false;
    let serviceCanWriteFile = false;
    for (const ace of item.access ?? []) {
      if (ace?.type !== "Allow" || (ace.propagationFlags & 2) !== 0) continue;
      const sid = ace.identitySID;
      const mask = (ace.mask ?? 0) >>> 0;
      if (agentSids.includes(sid)) problems.push(`protected checkpoint ACL includes configured agent principal ${sid}`);
      if (sid === serviceSid && isRoot && (mask & 0x00000042) === 0x00000042) serviceCanCreate = true;
      if (sid === serviceSid && !isRoot && (mask & (0x00000002 | 0x40000000)) !== 0) serviceCanWriteFile = true;
      if (![serviceSid, ...adminSids].includes(sid) && (mask & nonServiceWriteMask) !== 0) problems.push(`protected checkpoint grants write authority to non-service principal ${sid}`);
      if (sid === serviceSid && (mask & (0x00040000 | 0x00080000 | 0x10000000)) !== 0) problems.push("protected checkpoint service ACL grants DACL or ownership changes");
    }
    if (isRoot && !serviceCanCreate) problems.push("protected checkpoint directory does not grant bounded state-file creation to service identity");
    if (!isRoot && !serviceCanWriteFile) problems.push("protected checkpoint file does not grant state updates to service identity");
  }
  return problems;
}

function git(root, args, runner = spawnSync) {
  const result = runner(nativeGitExecutable(), ["-c", "core.fsmonitor=false", "-C", root, ...args], { encoding: "buffer", windowsHide: true, timeout: 15_000, env: protectedChildEnvironment() });
  if (result?.status !== 0) throw new Error(`git ${args.join(" ")} failed`);
  return Buffer.isBuffer(result.stdout) ? result.stdout : Buffer.from(result.stdout ?? "");
}

function expectedSource(root, runner, catalogPath) {
  const commit = git(root, ["rev-parse", "HEAD"], runner).toString("utf8").trim();
  const tree = git(root, ["rev-parse", `${commit}^{tree}`], runner).toString("utf8").trim();
  const origin = git(root, ["remote", "get-url", "origin"], runner).toString("utf8").trim();
  const listing = git(root, ["ls-tree", "-rz", "--full-tree", commit], runner);
  const status = git(root, ["status", "--porcelain=v1", "--untracked-files=all"], runner).toString("utf8");
  if (status.trim()) throw new Error("working tree must be clean before protected ownership verification");
  const catalog = JSON.parse(readFileSync(catalogPath, "utf8"));
  const protectedTrust = loadTrustedExternal().trust;
  const hasKeyLog = git(root, ["ls-tree", "-r", "--name-only", commit, "--", ".rightkit/ownership-keys.json"], runner).toString("utf8").trim() !== "";
  let keyLog = null;
  if (protectedTrust?.keyAuthority) {
    const authority = readCommittedKeyAuthority(protectedTrust);
    if (!authority.ok) throw new Error(`protected key authority rejected: ${authority.problems.join("; ")}`);
    keyLog = authority;
  } else if (hasKeyLog) keyLog = committedKeyLog(root, commit, (repo, args) => git(repo, args, runner), protectedTrust?.rootFingerprint, protectedTrust?.keyCheckpoint);
  return {
    commit, tree, origin, repositoryIdentity: canonicalIdentity(origin), sourceDigest: sourceTreeDigest(listing),
    scannerVersion: SCANNER_VERSION, catalogVersion: String(catalog.version ?? catalog.schema ?? "1"), catalogDigest: catalogDigest(catalog),
    ...(keyLog ? { keyLog } : {}),
  };
}

function canonicalIdentity(value) {
  const scp = value.match(/^([^@/:]+)@([^:]+):(.+)$/);
  const raw = scp ? `ssh://${scp[2]}/${scp[3]}` : value;
  const url = new URL(raw);
  if (!["https:", "ssh:"].includes(url.protocol) || url.username || url.password || url.search || url.hash) throw new Error("unsupported repository identity URL");
  let repo = url.pathname.replace(/^\/+/, "").replace(/\/+$/, "").replace(/\.git$/i, "");
  if (url.hostname.toLowerCase() === "github.com") repo = repo.toLowerCase();
  if (!repo || repo.split("/").some((part) => !part || part === "." || part === "..")) throw new Error("invalid repository identity path");
  return `https://${url.hostname.toLowerCase()}${url.port ? `:${url.port}` : ""}/${repo}`;
}

function runPrivateLocal(repoRoot, { platform = process.platform, runner = spawnSync, verifierPath = VERIFIER_PATHS[platform], runtimePath = RUNTIME_PATHS[platform], audit = auditProtectedInstall, catalogPath = path.join(INSTALL_ROOTS[platform] ?? "", "packages", "release", "rightkit-ownership.json") } = {}) {
  const root = path.resolve(repoRoot);
  if (!verifierPath || !runtimePath) return { ok: false, error: { type: "unsupported-platform", message: `protected verifier is unavailable on ${platform}` } };
  const install = audit({ platform });
  if (install?.ok !== true) return { ok: false, error: { type: "protected-install-invalid", message: (install?.problems ?? ["protected install audit failed"]).join("; ") } };
  let expected;
  try { expected = expectedSource(root, runner, catalogPath); }
  catch (error) { return { ok: false, error: { type: "source-identity-invalid", message: error.message } }; }
  let result;
  try {
    result = runner(runtimePath, [verifierPath, "--repo-root", root, "--expected-commit", expected.commit, "--expected-tree", expected.tree, "--json"], { encoding: "utf8", windowsHide: true, timeout: 120000, env: protectedChildEnvironment() });
  } catch (error) {
    if (error?.code === "ETIMEDOUT") return { ok: false, error: { type: "protected-verifier-timeout", message: "protected verifier exceeded 120 seconds" } };
    return { ok: false, error: { type: "protected-verifier-unavailable", message: error.message } };
  }
  if (result?.error?.code === "ETIMEDOUT") return { ok: false, error: { type: "protected-verifier-timeout", message: "protected verifier exceeded 120 seconds" } };
  if (result?.status !== 0) return { ok: false, error: { type: "protected-verifier-rejected", message: String(result?.stderr ?? "protected verifier exited unsuccessfully").trim() } };
  let receipt;
  try { receipt = JSON.parse(String(result.stdout ?? "")); }
  catch { return { ok: false, error: { type: "protected-receipt-malformed", message: "protected verifier did not return one JSON receipt" } }; }
  const validation = validateProtectedReceipt(receipt, expected);
  return validation.ok ? { ok: true, receipt } : { ok: false, error: { type: "protected-receipt-invalid", message: validation.problems.join("; ") } };
}

export function runPrivateLocalOwnershipAdapter(repoRoot) {
  return runPrivateLocal(repoRoot);
}

export function runPrivateLocalOwnershipAdapterForTest(repoRoot, options) {
  return runPrivateLocal(repoRoot, options);
}

export function evaluateWindowsNativeAclForTest(native, options) { return auditWindowsNative(native, options); }
export function evaluateWindowsCheckpointStateForTest(items, root, serviceSid, agentSids, problems) { return auditWindowsCheckpointState(items, root, serviceSid, agentSids, problems); }

export const __testOnly = Object.freeze({ expectedSourceForTest: expectedSource, VERIFIER_PATHS, PUBLIC_PRODUCER_PATHS, RUNTIME_PATHS, INSTALL_ROOTS, WINDOWS_AUDIT, evaluateWindowsNativeAclForTest, evaluateWindowsCheckpointStateForTest });

function main(args) {
  const index = args.indexOf("--repo-root");
  const root = index >= 0 ? args[index + 1] : process.cwd();
  const result = runPrivateLocalOwnershipAdapter(root);
  process.stdout.write(`${JSON.stringify(result)}\n`);
  process.exitCode = result.ok ? 0 : 1;
}

const realOrSelf = (file) => { try { return realpathSync(file); } catch { return path.resolve(file); } };
if (process.argv[1] && realOrSelf(process.argv[1]) === realOrSelf(fileURLToPath(import.meta.url))) main(process.argv.slice(2));
