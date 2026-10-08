import { createHash, randomUUID } from 'node:crypto';
import { spawn } from 'node:child_process';
import { lstat, open, realpath } from 'node:fs/promises';
import { dirname, join, relative, resolve, win32 } from 'node:path';

/** Class-1 real-copy ceiling: 2 GiB & 20,000 files. Proposed Windows cost bound;
 * BOTH leads must approve before trusted broker config sets copyCostApproval.
 * Never infer approval from caller environment or silently raise either bound.
 */
export const WINDOWS_COPY_COST_BOUND_BYTES = 2 * 1024 ** 3;
export const WINDOWS_COPY_COST_BOUND_FILES = 20_000;
function refuse(code, message) { throw Object.assign(new Error(`${code}: ${message}`), { code }); }
function inside(root, file, api = win32) { const rel = api.relative(root, file); return rel && rel !== '..' && !rel.startsWith(`..${api.sep}`) && !api.isAbsolute(rel); }
function localPath(value, label) {
  if (typeof value !== 'string' || !/^[A-Za-z]:[\\/]/.test(value) || /[\x00-\x1f<>:"|?*]/.test(value.slice(2)) || value.split(/[\\/]/).some((part) => part === '..' || /[. ]$/.test(part) || /^(con|prn|aux|nul|com[1-9]|lpt[1-9])(?:\.|$)/i.test(part))) refuse('RK_CLASS1_INPUT', `${label} requires canonical local Windows path`);
  const normalized = win32.normalize(value).replace(/\\+$/, '');
  if (normalized.length <= 2) refuse('RK_CLASS1_INPUT', `${label} must be below volume root`);
  return normalized;
}

export function enforceWindowsCopyCostBound(cost = {}) {
  if (!cost || typeof cost !== 'object' || Array.isArray(cost) || Object.keys(cost).some((key) => !['bytes', 'fileCount', 'blockClone'].includes(key))) refuse('RK_CLASS1_INPUT', 'invalid closure cost fields');
  const { bytes, fileCount, blockClone } = cost;
  if (!Number.isSafeInteger(bytes) || bytes < 0 || !Number.isSafeInteger(fileCount) || fileCount < 1 || typeof blockClone !== 'boolean') refuse('RK_CLASS1_INPUT', 'invalid closure cost');
  if (!blockClone && (bytes > WINDOWS_COPY_COST_BOUND_BYTES || fileCount > WINDOWS_COPY_COST_BOUND_FILES)) refuse('RK_CLASS1_COPY_COST_BOUND', `real copy exceeds ${WINDOWS_COPY_COST_BOUND_BYTES} bytes or ${WINDOWS_COPY_COST_BOUND_FILES} files`);
  return blockClone ? 'block-clone' : 'copy';
}

/** Cheap preflight only; native helper repeats this with pinned no-follow handles.
 * Explicit repositoryRoot is supplied by authenticated registration, never Git discovery.
 */
export async function validateWindowsInputs(inputs, repositoryRoot) {
  if (!Array.isArray(inputs) || !inputs.length || typeof repositoryRoot !== 'string' || !repositoryRoot || !(/^[A-Za-z]:[\\/]/.test(repositoryRoot) || repositoryRoot.startsWith('/'))) refuse('RK_CLASS1_INPUT', 'nonempty inputs & absolute declared repositoryRoot required');
  const inputIO = (error) => refuse('RK_CLASS1_INPUT', `source closure unavailable: ${error.message}`);
  const base = resolve(repositoryRoot); const canonical = await realpath(base).catch(inputIO);
  const files = []; const seen = new Set();
  for (const input of inputs) {
    if (typeof input !== 'string' || !input || !(/^[A-Za-z]:[\\/]/.test(input) || input.startsWith('/'))) refuse('RK_CLASS1_INPUT', 'absolute source file required');
    const file = resolve(input); const rel = relative(base, file);
    if (!rel || rel === '..' || rel.startsWith(`..${process.platform === 'win32' ? '\\' : '/'}`) || resolve(base, rel) !== file) refuse('RK_CLASS1_INPUT_ESCAPE', file);
    for (let cursor = file;; cursor = dirname(cursor)) {
      const info = await lstat(cursor).catch(inputIO);
      if (info.isSymbolicLink()) refuse('RK_CLASS1_REPARSE_POINT', cursor);
      if (cursor === dirname(cursor)) break;
    }
    const resolved = await realpath(file).catch(inputIO); const finalRel = relative(canonical, resolved);
    if (!finalRel || finalRel === '..' || finalRel.startsWith('..\\') || finalRel.startsWith('../') || /^[A-Za-z]:|^\//.test(finalRel)) refuse('RK_CLASS1_INPUT_ESCAPE', file);
    const info = await lstat(file).catch(inputIO);
    if (!info.isFile()) refuse('RK_CLASS1_INPUT', `source is not regular file: ${file}`);
    const key = process.platform === 'win32' ? file.toLowerCase() : file;
    if (seen.has(key)) refuse('RK_CLASS1_INPUT', `duplicate source: ${file}`);
    seen.add(key); files.push({ source: file, path: rel.replaceAll('\\', '/'), size: info.size });
  }
  return files;
}

/** Only hash already-protected cloned files. No source reads here. gitIncluded is
 * repeated in entries so serialized manifest explicitly records .git policy.
 */
export async function hashWindowsManifest(buildRoot, files, { includeGit = false } = {}) {
  if (!Array.isArray(files) || !files.length || typeof includeGit !== 'boolean') refuse('RK_CLASS1_MANIFEST', 'invalid manifest descriptors');
  const manifest = []; const seen = new Set();
  const manifestIO = (error) => refuse('RK_CLASS1_MANIFEST', `protected clone unavailable: ${error.message}`);
  for (const file of files) {
    if (!file || typeof file.path !== 'string' || !file.path || file.path.includes('\\') || file.path.startsWith('/') || file.path.split('/').some((part) => !part || part === '.' || part === '..' || part.includes(':')) || !Number.isSafeInteger(file.size) || file.size < 0) refuse('RK_CLASS1_MANIFEST', 'invalid cloned path/size');
    const key = file.path.toLowerCase();
    if (seen.has(key) || (!includeGit && file.path.split('/').some((part) => part.toLowerCase() === '.git'))) refuse('RK_CLASS1_MANIFEST', 'duplicate path or .git policy mismatch');
    seen.add(key);
    const cloned = join(buildRoot, file.path); const info = await lstat(cloned).catch(manifestIO);
    if (!info.isFile() || info.isSymbolicLink() || info.size !== file.size) refuse('RK_CLASS1_MANIFEST', `invalid cloned file: ${file.path}`);
    const handle = await open(cloned, 'r').catch(manifestIO); const hash = createHash('sha256');
    try { for await (const chunk of handle.createReadStream({ autoClose: false })) hash.update(chunk); }
    catch (error) { manifestIO(error); }
    finally { await handle.close(); }
    manifest.push({ path: file.path, sha256: hash.digest('hex'), size: file.size, gitIncluded: includeGit });
  }
  return manifest.sort((a, b) => a.path < b.path ? -1 : a.path > b.path ? 1 : 0);
}

// Inline PowerShell/.NET helper avoids dependencies/native builds. Called only
// by dedicated service identity. File & EVERY ancestor handle rejects reparses,
// pins replacement (no FILE_SHARE_DELETE); sources also deny FILE_SHARE_WRITE.
// FSCTL docs: https://learn.microsoft.com/en-us/windows/win32/fileio/block-cloning
export const WINDOWS_BUILD_ROOT_HELPER = String.raw`
$ErrorActionPreference = 'Stop'
try {
Add-Type -TypeDefinition @'
using System;
using System.IO;
using System.Text;
using System.Collections.Generic;
using System.ComponentModel;
using System.Runtime.InteropServices;
using System.Security.AccessControl;
using System.Security.Principal;
using Microsoft.Win32.SafeHandles;
public static class RKClass1 {
  [StructLayout(LayoutKind.Sequential)] public struct Info { public uint Attributes; public System.Runtime.InteropServices.ComTypes.FILETIME Created, Accessed, Written; public uint Volume, SizeHigh, SizeLow, Links, IndexHigh, IndexLow; }
  [StructLayout(LayoutKind.Sequential)] public struct Extents { public IntPtr Source; public long SourceOffset, TargetOffset, Length; }
  [DllImport("kernel32.dll", CharSet=CharSet.Unicode, SetLastError=true)] static extern SafeFileHandle CreateFileW(string p, uint access, uint share, IntPtr security, uint create, uint flags, IntPtr template);
  [DllImport("kernel32.dll", SetLastError=true)] static extern bool GetFileInformationByHandle(SafeFileHandle h, out Info i);
  [DllImport("kernel32.dll", CharSet=CharSet.Unicode, SetLastError=true)] static extern uint GetFinalPathNameByHandleW(SafeFileHandle h, StringBuilder p, uint size, uint flags);
  [DllImport("kernel32.dll", CharSet=CharSet.Unicode, SetLastError=true)] static extern bool GetVolumeInformationW(string root, StringBuilder name, uint n, out uint serial, out uint max, out uint flags, StringBuilder fs, uint f);
  [DllImport("kernel32.dll", CharSet=CharSet.Unicode, SetLastError=true)] static extern bool GetDiskFreeSpaceW(string root, out uint sectors, out uint bytes, out uint free, out uint total);
  [DllImport("kernel32.dll", SetLastError=true)] static extern bool DeviceIoControl(SafeFileHandle h, uint code, ref Extents data, int size, IntPtr output, int outputSize, out uint returned, IntPtr overlap);
  static void Check(bool ok, string code) { if (!ok) throw new IOException(code + ": " + new Win32Exception(Marshal.GetLastWin32Error()).Message); }
  public static SafeFileHandle Pin(string p, bool directory) {
    var h = CreateFileW(p, directory ? 0x80u : 0x80000000u, 1u, IntPtr.Zero, 3, 0x00200000u | (directory ? 0x02000000u : 0u), IntPtr.Zero);
    try { Check(!h.IsInvalid, "RK_CLASS1_NOFOLLOW_OPEN"); Info i; Check(GetFileInformationByHandle(h, out i), "RK_CLASS1_HANDLE_INFO");
      if ((i.Attributes & 0x400u) != 0) throw new IOException("RK_CLASS1_REPARSE_POINT: " + p);
      if (((i.Attributes & 0x10u) != 0) != directory) throw new IOException("RK_CLASS1_INPUT: wrong file type " + p);
      var final = new StringBuilder(32768); uint count = GetFinalPathNameByHandleW(h, final, 32768, 0);
      Check(count > 0 && count < 32768, "RK_CLASS1_CANONICAL_PATH");
      if (!String.Equals(final.ToString(), @"\\?\" + Path.GetFullPath(p), StringComparison.OrdinalIgnoreCase)) throw new IOException("RK_CLASS1_INPUT_ESCAPE: " + p);
      return h;
    } catch { h.Dispose(); throw; }
  }
  public static void PinParents(string p, List<SafeFileHandle> pins, HashSet<string> seen) {
    var chain = new List<string>(); for (var d = Path.GetDirectoryName(p); !String.IsNullOrEmpty(d); d = Path.GetDirectoryName(d)) chain.Add(d);
    chain.Reverse(); foreach (var d in chain) if (seen.Add(d)) pins.Add(Pin(d, true));
  }
  public static long Size(SafeFileHandle h) { Info i; Check(GetFileInformationByHandle(h, out i), "RK_CLASS1_HANDLE_INFO"); return ((long)i.SizeHigh << 32) | i.SizeLow; }
  public static bool CloneSupported(string root) {
    uint serial, max, flags; var fs = new StringBuilder(64);
    Check(GetVolumeInformationW(root, null, 0, out serial, out max, out flags, fs, 64), "RK_CLASS1_VOLUME_INFO");
    return String.Equals(fs.ToString(), "ReFS", StringComparison.OrdinalIgnoreCase) && (flags & 0x08000000u) != 0;
  }
  public static long ClusterSize(string root) {
    uint sectors, bytes, free, total; Check(GetDiskFreeSpaceW(root, out sectors, out bytes, out free, out total), "RK_CLASS1_CLUSTER_SIZE");
    long size = (long)sectors * bytes; if (size <= 0 || size > 2147483648L) throw new IOException("RK_CLASS1_CLUSTER_SIZE: unsupported cluster"); return size;
  }
  public static void VerifyVolumeParent(string p, string service, string agent) {
    var acl = Directory.GetAccessControl(p);
    if (acl.GetOwner(typeof(SecurityIdentifier)).Value == agent) throw new IOException("RK_CLASS1_ROOT_PROTECTION: agent owns volume root");
    foreach (FileSystemAccessRule ace in acl.GetAccessRules(true, true, typeof(SecurityIdentifier))) {
      string id = ace.IdentityReference.Value;
      if (ace.AccessControlType == AccessControlType.Allow && (ace.FileSystemRights & FileSystemRights.DeleteSubdirectoriesAndFiles) != 0 && id != service && id != "S-1-5-18" && id != "S-1-5-32-544") throw new IOException("RK_CLASS1_ROOT_PROTECTION: volume permits ancestor deletion");
    }
  }
  public static void SecureDirectory(string p, string service, string agent) {
    var acl = new DirectorySecurity(); acl.SetAccessRuleProtection(true, false);
    var sid = new SecurityIdentifier(service); acl.SetOwner(sid);
    acl.AddAccessRule(new FileSystemAccessRule(sid, FileSystemRights.FullControl, InheritanceFlags.ContainerInherit | InheritanceFlags.ObjectInherit, PropagationFlags.None, AccessControlType.Allow));
    acl.AddAccessRule(new FileSystemAccessRule(new SecurityIdentifier(agent), FileSystemRights.ReadAndExecute, InheritanceFlags.ContainerInherit | InheritanceFlags.ObjectInherit, PropagationFlags.None, AccessControlType.Allow));
    Directory.CreateDirectory(p, acl); Directory.SetAccessControl(p, acl);
  }
  public static void VerifyParent(string p, string service, string agent) {
    var acl = Directory.GetAccessControl(p);
    if (acl.GetOwner(typeof(SecurityIdentifier)).Value != service || !acl.AreAccessRulesProtected) throw new IOException("RK_CLASS1_ROOT_PROTECTION: owner/inheritance " + p);
    int write = (int)(FileSystemRights.Write | FileSystemRights.Delete | FileSystemRights.DeleteSubdirectoriesAndFiles | FileSystemRights.ChangePermissions | FileSystemRights.TakeOwnership);
    bool full = false, read = false;
    foreach (FileSystemAccessRule ace in acl.GetAccessRules(true, true, typeof(SecurityIdentifier))) {
      string id = ace.IdentityReference.Value;
      if (ace.AccessControlType == AccessControlType.Deny) throw new IOException("RK_CLASS1_ROOT_PROTECTION: unexpected deny ACE");
      if (id == service && (ace.FileSystemRights & FileSystemRights.FullControl) == FileSystemRights.FullControl) full = true;
      if (id == agent && (ace.FileSystemRights & FileSystemRights.ReadAndExecute) == FileSystemRights.ReadAndExecute) read = true;
      if (((int)ace.FileSystemRights & write) != 0 && id != service) throw new IOException("RK_CLASS1_ROOT_PROTECTION: other writer " + p);
    }
    if (!full || !read) throw new IOException("RK_CLASS1_ROOT_PROTECTION: missing service/agent grant");
  }
  public static void Copy(SafeFileHandle source, string destination, bool clone, long size) {
    var target = CreateFileW(destination, 0xC0000000u, 0, IntPtr.Zero, 1, 0x00200000u, IntPtr.Zero);
    Check(!target.IsInvalid, "RK_CLASS1_DESTINATION");
    using (target) using (var output = new FileStream(target, FileAccess.ReadWrite)) {
      output.SetLength(size);
      if (clone && size != 0) {
        long cluster = ClusterSize(Path.GetPathRoot(destination));
        long aligned = size - size % cluster; uint returned;
        // Both ends must be cluster-aligned; bound each ioctl to <4 GiB.
        for (long offset = 0; offset < aligned;) {
          long count = Math.Min(aligned - offset, (2147483648L / cluster) * cluster);
          var data = new Extents { Source = source.DangerousGetHandle(), SourceOffset = offset, TargetOffset = offset, Length = count };
          Check(DeviceIoControl(target, 0x00098344u, ref data, Marshal.SizeOf(typeof(Extents)), IntPtr.Zero, 0, out returned, IntPtr.Zero), "RK_CLASS1_BLOCK_CLONE"); offset += count;
        }
        // Tail bytes copied from same pinned handle; clone method reports block-clone + bounded tail.
        if (aligned != size) { using (var borrowed = new SafeFileHandle(source.DangerousGetHandle(), false)) using (var input = new FileStream(borrowed, FileAccess.Read)) { input.Position = aligned; output.Position = aligned; input.CopyTo(output); } }
      } else if (!clone) { using (var borrowed = new SafeFileHandle(source.DangerousGetHandle(), false)) using (var input = new FileStream(borrowed, FileAccess.Read)) input.CopyTo(output); }
      output.Flush(true);
    }
  }
}
'@
$q = [Console]::In.ReadToEnd() | ConvertFrom-Json
if ([Security.Principal.WindowsIdentity]::GetCurrent().User.Value -ne $q.serviceSid) { throw 'RK_CLASS1_SERVICE_IDENTITY: dedicated service account required' }
$pins = New-Object 'System.Collections.Generic.List[Microsoft.Win32.SafeHandles.SafeFileHandle]'
$seen = New-Object 'System.Collections.Generic.HashSet[string]' ([StringComparer]::OrdinalIgnoreCase)
$sources = @(); $bytes = [long]0
try {
  [RKClass1]::PinParents(($q.root + '\placeholder'), $pins, $seen)
  [RKClass1]::VerifyVolumeParent([IO.Path]::GetPathRoot($q.root), $q.serviceSid, $q.agentSid)
  # All managed ancestors must be service-owned, so root cannot be replaced after helper exits.
  for ($p = $q.root; $p -and $p -ne [IO.Path]::GetPathRoot($p); $p = [IO.Path]::GetDirectoryName($p)) { [RKClass1]::VerifyParent($p, $q.serviceSid, $q.agentSid) }
  [RKClass1]::PinParents(($q.repositoryRoot + '\placeholder'), $pins, $seen)
  foreach ($file in $q.files) {
    [RKClass1]::PinParents($file.source, $pins, $seen)
    $handle = [RKClass1]::Pin($file.source, $false); $pins.Add($handle)
    $size = [RKClass1]::Size($handle); if ($size -ne $file.size) { throw 'RK_CLASS1_INPUT_CHANGED: source size changed during admission' }
    if ($bytes -gt [long]9007199254740991 - $size) { throw 'RK_CLASS1_INPUT: closure size overflow' }; $bytes += $size
    $sources += @{ file = $file; handle = $handle }
  }
  $clone = [RKClass1]::CloneSupported([IO.Path]::GetPathRoot($q.root))
  foreach ($entry in $sources) { if ([IO.Path]::GetPathRoot($entry.file.source) -ine [IO.Path]::GetPathRoot($q.root)) { $clone = $false } }
  $copyBytes = $bytes; $copyFiles = $sources.Count
  if ($clone) {
    $cluster = [RKClass1]::ClusterSize([IO.Path]::GetPathRoot($q.root)); $copyBytes = [long]0; $copyFiles = 0
    foreach ($entry in $sources) { $tail = $entry.file.size % $cluster; $copyBytes += $tail; if ($tail -gt 0) { $copyFiles++ } }
  }
  if ($copyFiles -gt 0) {
    if ($copyBytes -gt $q.copyBytes -or $copyFiles -gt $q.copyFiles) { throw 'RK_CLASS1_COPY_COST_BOUND: real copy/tails exceed approved byte/file ceiling' }
    if (-not ($q.copyCostApproval.windowsLead -eq $true -and $q.copyCostApproval.macLead -eq $true)) { throw 'RK_CLASS1_COPY_COST_UNAPPROVED: both leads must approve copy bound' }
  }
  $build = [IO.Path]::Combine($q.root, $q.name)
  if ([IO.Directory]::Exists($build) -or [IO.File]::Exists($build)) { throw 'RK_CLASS1_DESTINATION: build root already exists' }
  [RKClass1]::SecureDirectory($build, $q.serviceSid, $q.agentSid)
  foreach ($entry in $sources) {
    $destination = [IO.Path]::Combine($build, $entry.file.path.Replace('/', '\'))
    $directory = [IO.Path]::GetDirectoryName($destination)
    [IO.Directory]::CreateDirectory($directory) | Out-Null
    [RKClass1]::Copy($entry.handle, $destination, $clone, $entry.file.size)
    # Inherited DACL is service-only write; explicitly set every clone owner to service SID.
    $acl = [IO.File]::GetAccessControl($destination); $acl.SetOwner([Security.Principal.SecurityIdentifier]::new($q.serviceSid)); [IO.File]::SetAccessControl($destination, $acl)
  }
  [RKClass1]::VerifyParent($build, $q.serviceSid, $q.agentSid)
  @{ buildRoot = $build; method = $(if ($clone) { 'block-clone' } else { 'copy' }); bytes = $bytes } | ConvertTo-Json -Compress
} finally { foreach ($pin in $pins) { $pin.Dispose() } }
} catch { [Console]::Error.WriteLine($_.Exception.Message); exit 1 }
`;

function runHelper(request) {
  return new Promise((accept, reject) => {
    const executable = win32.join(process.env.SystemRoot ?? 'C:\\Windows', 'System32', 'WindowsPowerShell', 'v1.0', 'powershell.exe');
    const child = spawn(executable, ['-NoLogo', '-NoProfile', '-NonInteractive', '-Command', WINDOWS_BUILD_ROOT_HELPER], { windowsHide: true, stdio: ['pipe', 'pipe', 'pipe'] });
    let stdout = ''; let stderr = ''; let overflow = false;
    const timer = setTimeout(() => { child.kill(); reject(Object.assign(new Error('RK_CLASS1_HELPER_TIMEOUT: snapshot helper exceeded 120s'), { code: 'RK_CLASS1_HELPER_TIMEOUT' })); }, 120_000);
    const failed = (error) => { clearTimeout(timer); child.kill(); reject(Object.assign(new Error(`RK_CLASS1_HELPER_FAILED: ${error.message}`), { code: 'RK_CLASS1_HELPER_FAILED' })); };
    child.on('error', failed); child.stdin.on('error', failed);
    const collect = (channel) => (chunk) => { if (stdout.length + stderr.length + chunk.length > 256 * 1024) { overflow = true; child.kill(); } else if (channel === 'out') stdout += chunk; else stderr += chunk; };
    child.stdout.on('data', collect('out')); child.stderr.on('data', collect('err'));
    child.on('close', (code) => {
      clearTimeout(timer);
      if (overflow) { reject(Object.assign(new Error('RK_CLASS1_HELPER_OUTPUT_BOUND'), { code: 'RK_CLASS1_HELPER_OUTPUT_BOUND' })); return; }
      if (code !== 0) { const errorCode = stderr.match(/RK_CLASS1_[A-Z_]+/)?.[0] ?? 'RK_CLASS1_HELPER_FAILED'; reject(Object.assign(new Error(`${errorCode}: ${stderr.trim()}`), { code: errorCode })); return; }
      try { accept(JSON.parse(stdout.trim())); } catch { reject(Object.assign(new Error('RK_CLASS1_HELPER_REPORT: invalid helper output'), { code: 'RK_CLASS1_HELPER_REPORT' })); }
    });
    child.stdin.end(JSON.stringify(request));
  });
}

/** Caller retains root until all sandboxed descendants exit. .git excluded by
 * default & listed in git.excludedInputs; includeGit:true copies it as ordinary
 * no-follow files. One declared repository preserves relative build paths.
 * Runtime & helper must be installed/protected; no elevation from workspace code.
 * copyCostApproval is trusted broker configuration, never request-supplied evidence.
 */
export async function createProtectedBuildRoot(options = {}) {
  const allowed = new Set(['inputs', 'root', 'serviceSid', 'repositoryRoot', 'agentSid', 'includeGit', 'copyCostApproval']);
  if (!options || typeof options !== 'object' || Array.isArray(options) || Object.keys(options).some((key) => !allowed.has(key))) refuse('RK_CLASS1_INPUT', 'unknown snapshot option');
  const { inputs, serviceSid, agentSid, includeGit = false, copyCostApproval = { windowsLead: false, macLead: false } } = options;
  const root = localPath(options.root, 'root'); const repositoryRoot = localPath(options.repositoryRoot, 'repositoryRoot');
  if (!/^S-1-5-80-(?:\d+-){4}\d+$/.test(serviceSid ?? '') || !/^S-1-5-21-(?:\d+-){3}\d+$/.test(agentSid ?? '') || serviceSid === agentSid || typeof includeGit !== 'boolean') refuse('RK_CLASS1_INPUT', 'dedicated service SID, agent-user SID & boolean .git policy required');
  if (!copyCostApproval || Object.keys(copyCostApproval).sort().join(',') !== 'macLead,windowsLead' || typeof copyCostApproval.windowsLead !== 'boolean' || typeof copyCostApproval.macLead !== 'boolean') refuse('RK_CLASS1_INPUT', 'explicit both-lead copy approval required');
  if (inside(repositoryRoot, root) || inside(root, repositoryRoot) || root.toLowerCase() === repositoryRoot.toLowerCase()) refuse('RK_CLASS1_INPUT_ESCAPE', 'build parent & repository must be disjoint');
  if (process.platform !== 'win32') refuse('RK_CLASS1_PLATFORM', 'Windows service required');
  let files;
  try { files = await validateWindowsInputs(inputs, repositoryRoot); } catch (error) { if (error.code?.startsWith('RK_CLASS1_')) throw error; refuse('RK_CLASS1_INPUT', error.message); }
  const isGit = (file) => file.path.split('/').some((part) => part.toLowerCase() === '.git');
  const excludedInputs = includeGit ? [] : files.filter(isGit).map((file) => file.path);
  files = files.filter((file) => includeGit || !isGit(file));
  if (!files.length) refuse('RK_CLASS1_INPUT', 'input closure empty after .git exclusion');
  for (const file of files) localPath(file.source, 'source');
  const name = `class1-${randomUUID()}`;
  const result = await runHelper({ root, repositoryRoot, serviceSid, agentSid, files, name, copyBytes: WINDOWS_COPY_COST_BOUND_BYTES, copyFiles: WINDOWS_COPY_COST_BOUND_FILES, copyCostApproval });
  const bytes = files.reduce((sum, file) => sum + file.size, 0);
  if (result?.buildRoot !== win32.join(root, name) || !['copy', 'block-clone'].includes(result.method) || result.bytes !== bytes) refuse('RK_CLASS1_HELPER_REPORT', 'snapshot report mismatch');
  enforceWindowsCopyCostBound({ bytes, fileCount: files.length, blockClone: result.method === 'block-clone' });
  return { ...result, manifest: await hashWindowsManifest(result.buildRoot, files, { includeGit }), git: { included: includeGit, excludedInputs } };
}
