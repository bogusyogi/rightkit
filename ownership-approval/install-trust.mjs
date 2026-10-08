#!/usr/bin/env node
// Administrator-invoked source package. Production use requires an independently
// reviewed copy plus runtime in the fixed protected installer directory.
import { randomBytes } from "node:crypto";
import { closeSync, fsyncSync, lstatSync, openSync, readFileSync, renameSync, unlinkSync, writeSync, realpathSync } from "node:fs";
import path from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { evaluateWindowsAcl, sha256, validateTrustCandidate } from "./trust-schema.mjs";

const WIN_TRUST = "C:\\ProgramData\\Orthic Labs\\RightKit\\ownership\\trust.json";
const MAC_TRUST = "/Library/Application Support/Orthic Labs/RightKit/ownership/trust.json";
const WIN_INSTALLER = "C:\\ProgramData\\Orthic Labs\\RightKit\\ownership\\installer";
const MAC_INSTALLER = "/Library/Application Support/Orthic Labs/RightKit/ownership/installer";
const HEX = /^[a-f0-9]{64}$/;

export function validatePinnedCandidate(bytes, expectedDigest) {
  if (!Buffer.isBuffer(bytes)) bytes = Buffer.from(bytes);
  if (typeof expectedDigest !== "string" || !HEX.test(expectedDigest)) throw new Error("expected digest must be lowercase SHA-256 hex");
  if (sha256(bytes) !== expectedDigest) throw new Error("candidate bytes do not match the pinned file digest");
  let trust;
  try {
    if (bytes.subarray(0, 3).equals(Buffer.from([0xef, 0xbb, 0xbf]))) throw new Error("UTF-8 BOM is not accepted");
    trust = JSON.parse(new TextDecoder("utf-8", { fatal: true }).decode(bytes));
  } catch { throw new Error("candidate is not valid UTF-8 JSON"); }
  if (!bytes.equals(Buffer.from(`${JSON.stringify(trust, null, 2)}\n`, "utf8"))) throw new Error("candidate JSON must use canonical prepared formatting without duplicate keys");
  validateTrustCandidate(trust);
  return { trust, bytes };
}

function run(file, args, env = process.env) {
  const result = spawnSync(file, args, { encoding: "utf8", windowsHide: true, env });
  if (result.status !== 0) throw new Error(`required OS protection check failed: ${result.stderr || result.error?.message || file}`);
  return result.stdout.trim();
}

function powershellInfo(target) {
  const script = "$ErrorActionPreference='Stop'; $a=Get-Acl -LiteralPath $env:RK_AUDIT_TARGET; $i=Get-Item -Force -LiteralPath $env:RK_AUDIT_TARGET; $w=[Security.Principal.WindowsIdentity]::GetCurrent(); $p=New-Object Security.Principal.WindowsPrincipal($w); [ordered]@{ownerSID=$a.GetOwner([Security.Principal.SecurityIdentifier]).Value;ownerName=$a.Owner;attributes=$i.Attributes.ToString();directory=[bool]$i.PSIsContainer;nonempty=(!$i.PSIsContainer -or [System.IO.Directory]::EnumerateFileSystemEntries($i.FullName).GetEnumerator().MoveNext());elevated=$p.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator);access=@($a.Access | ForEach-Object { [ordered]@{identitySID=$_.IdentityReference.Translate([Security.Principal.SecurityIdentifier]).Value;type=$_.AccessControlType.ToString();mask=[int64]$_.FileSystemRights.value__;propagationFlags=[int]$_.PropagationFlags} })} | ConvertTo-Json -Compress -Depth 5";
  const output = run("C:\\Windows\\System32\\WindowsPowerShell\\v1.0\\powershell.exe", ["-NoProfile", "-NonInteractive", "-Command", script], { ...process.env, RK_AUDIT_TARGET: target });
  return JSON.parse(output);
}

function auditWindowsPath(target, { file = false } = {}) {
  const problems = [];
  let current = path.resolve(target);
  const anchor = "C:\\ProgramData";
  if (!current.toLowerCase().startsWith(anchor.toLowerCase() + "\\") && current.toLowerCase() !== anchor.toLowerCase()) throw new Error(`protected installer path must remain below ${anchor}`);
  const chain = [];
  while (true) {
    chain.push(current);
    if (current.toLowerCase() === anchor.toLowerCase()) break;
    const parent = path.dirname(current);
    if (parent === current) throw new Error(`protected installer path escaped ${anchor}`);
    current = parent;
  }
  chain.push("C:\\");
  for (const item of chain) {
    let info;
    try {
      const stat = lstatSync(item);
      if (stat.isSymbolicLink()) problems.push(`reparse/symlink in protected path: ${item}`);
      info = powershellInfo(item);
    } catch { problems.push(`cannot inspect protected path: ${item}`); continue; }
    if (/ReparsePoint/i.test(info.attributes ?? "")) problems.push(`reparse point in protected path: ${item}`);
    const result = evaluateWindowsAcl(info, { target: item, requireNonElevated: false });
    problems.push(...result.problems.map((problem) => `${problem}: ${item}`));
    if (file && item === path.resolve(target) && info.attributes?.includes("Directory")) problems.push(`installer component must be a file: ${item}`);
  }
  if (problems.length) throw new Error(problems.join("; "));
}

function auditMacPath(target) {
  const problems = [];
  let current = path.resolve(target);
  while (true) {
    try {
      if (lstatSync(current).isSymbolicLink()) problems.push(`symlink in protected path: ${current}`);
      const info = lstatSync(current);
      if (info.uid !== 0 || (info.mode & 0o022)) problems.push(`protected path is not root-owned/private: ${current}`);
      const acl = run("/bin/ls", ["-lde", current]);
      for (const line of acl.split(/\r?\n/).slice(1)) {
        if (!/allow/i.test(line) || !/\b(?:write|delete|append|writeattr|writeextattr|chown|delete_child|add_file|add_subdirectory)\b/i.test(line)) continue;
        if (!/^\s*\d+:\s+user:root\s+allow\b/i.test(line)) problems.push(`protected ACL grants write authority to a non-root principal: ${current}`);
      }
    } catch { problems.push(`cannot inspect protected path: ${current}`); }
    const parent = path.dirname(current);
    if (parent === current) break;
    current = parent;
  }
  if (problems.length) throw new Error(problems.join("; "));
}

function fixedPaths() {
  if (process.platform === "win32") return { trust: WIN_TRUST, installer: WIN_INSTALLER, os: "windows" };
  if (process.platform === "darwin") return { trust: MAC_TRUST, installer: MAC_INSTALLER, os: "macos" };
  throw new Error(`unsupported installer platform: ${process.platform}`);
}

function auditBoundary({ trust, installer }) {
  const entry = path.resolve(fileURLToPath(import.meta.url));
  const schema = path.resolve(fileURLToPath(new URL("./trust-schema.mjs", import.meta.url)));
  const expectedEntry = path.join(installer, "install-trust.mjs");
  const expectedSchema = path.join(installer, "trust-schema.mjs");
  const runtime = path.resolve(process.execPath);
  if (process.platform === "win32") {
    const fold = (value) => value.toLowerCase();
    if (fold(entry) !== fold(expectedEntry) || fold(schema) !== fold(expectedSchema) || !fold(runtime).startsWith(fold(installer + path.sep))) {
      throw new Error("installer source, schema module & Node runtime must be provisioned inside the fixed protected installer directory");
    }
    const status = powershellInfo(expectedEntry);
    if (!status.elevated) throw new Error("launch installer directly from an administrator-approved elevated session; automatic elevation is disabled");
    auditWindowsPath(path.dirname(trust));
    const existing = lstatSync(trust, { throwIfNoEntry: false });
    if (existing) {
      if (!existing.isFile() || existing.isSymbolicLink()) throw new Error("existing trust target must be a regular non-reparse file");
      auditWindowsPath(trust, { file: true });
    }
    auditWindowsPath(expectedEntry, { file: true });
    auditWindowsPath(expectedSchema, { file: true });
    auditWindowsPath(runtime, { file: true });
  } else {
    if (entry !== expectedEntry || schema !== expectedSchema || !runtime.startsWith(installer + path.sep)) throw new Error("installer source, schema module & Node runtime must be provisioned inside the fixed protected installer directory");
    if (process.getuid?.() !== 0) throw new Error("launch installer directly from an administrator-approved root session; automatic elevation is disabled");
    auditMacPath(path.dirname(trust));
    const existing = lstatSync(trust, { throwIfNoEntry: false });
    if (existing) {
      if (!existing.isFile() || existing.isSymbolicLink()) throw new Error("existing trust target must be a regular non-symlink file");
      auditMacPath(trust);
    }
    auditMacPath(expectedEntry);
    auditMacPath(expectedSchema);
    auditMacPath(runtime);
  }
}

function readCandidateOnce(file) {
  const st = lstatSync(file);
  if (!st.isFile() || st.isSymbolicLink()) throw new Error("candidate must be a regular non-symlink file");
  if (st.size > 32 * 1024 * 1024) throw new Error("candidate exceeds 32 MiB limit");
  if (process.platform === "win32" && (st.mode & 0o170000) === 0o120000) throw new Error("candidate may not be a reparse point");
  const descriptor = openSync(file, "r");
  try { return readFileSync(descriptor); } finally { closeSync(descriptor); }
}

function setWindowsFileProtection(file) {
  const script = "$ErrorActionPreference='Stop'; $path=$env:RK_INSTALL_FILE; $acl=New-Object Security.AccessControl.FileSecurity; $acl.SetAccessRuleProtection($true,$false); $admin=New-Object Security.Principal.NTAccount('BUILTIN','Administrators'); $system=New-Object Security.Principal.NTAccount('NT AUTHORITY','SYSTEM'); $users=New-Object Security.Principal.NTAccount('BUILTIN','Users'); $acl.SetOwner($admin); foreach($pair in @(@($admin,'FullControl'),@($system,'FullControl'),@($users,'Read'))){ $rule=New-Object Security.AccessControl.FileSystemAccessRule($pair[0],$pair[1],'Allow'); [void]$acl.AddAccessRule($rule) }; Set-Acl -LiteralPath $path -AclObject $acl";
  run("C:\\Windows\\System32\\WindowsPowerShell\\v1.0\\powershell.exe", ["-NoProfile", "-NonInteractive", "-Command", script], { ...process.env, RK_INSTALL_FILE: file });
}

function secureTemp(file) {
  if (process.platform === "win32") setWindowsFileProtection(file);
  else run("/bin/chmod", ["644", file]);
}

function installBytes(target, bytes) {
  const temp = path.join(path.dirname(target), `.trust-${process.pid}-${randomBytes(8).toString("hex")}.tmp`);
  let fd;
  try {
    fd = openSync(temp, "wx", 0o600);
    let offset = 0;
    while (offset < bytes.length) offset += writeSync(fd, bytes, offset, bytes.length - offset);
    fsyncSync(fd);
    closeSync(fd); fd = undefined;
    secureTemp(temp);
    renameSync(temp, target);
    if (process.platform !== "win32") {
      const dir = openSync(path.dirname(target), "r");
      try { fsyncSync(dir); } finally { closeSync(dir); }
    }
  } catch (error) {
    if (fd !== undefined) closeSync(fd);
    try { unlinkSync(temp); } catch {}
    throw error;
  }
}

function main(args) {
  if (args.length !== 4 || args[0] !== "--candidate" || args[2] !== "--sha256") throw new Error("usage: node install-trust.mjs --candidate <candidate.json> --sha256 <pinned-file-sha256>");
  const [, candidatePath, , pinnedDigest] = args;
  const paths = fixedPaths();
  auditBoundary(paths);
  const bytes = readCandidateOnce(candidatePath);
  validatePinnedCandidate(bytes, pinnedDigest);
  installBytes(paths.trust, bytes);
  auditBoundary(paths);
  const installed = readFileSync(paths.trust);
  if (sha256(installed) !== pinnedDigest) throw new Error("installed trust bytes do not match the pinned candidate digest");
  process.stdout.write(`Installed protected ${paths.os} ownership trust at ${paths.trust}\nSHA-256: ${pinnedDigest}\n`);
}

const realOrSelf = (file) => { try { return realpathSync(file); } catch { return path.resolve(file); } };
if (process.argv[1] && realOrSelf(process.argv[1]) === realOrSelf(fileURLToPath(import.meta.url))) {
  try { main(process.argv.slice(2)); }
  catch (error) { process.stderr.write(`Protected trust install refused: ${error.message}\n`); process.exitCode = 1; }
}
