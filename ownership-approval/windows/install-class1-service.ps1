# Operator-only class-1 migration. Existing bootstrap registers scheduled task
# RightKitManagedAgentBroker, NOT an SCM service. Supply an installed SCM-aware
# service host executable (node.exe itself does not implement ServiceMain).
# No downloads, credential reads, password arguments, or automatic service start.
[CmdletBinding(SupportsShouldProcess = $true, ConfirmImpact = 'Medium')]
param(
    [string]$ServiceExecutable,
    [string]$ServiceArguments = '',
    [string]$AgentUser = "$env:USERDOMAIN\$env:USERNAME",
    # Signed rightkit-sandbox-exec.exe built from the published crate; its committed receipt pins sha256 & signer.
    [string]$SandboxExecSource,
    [string]$SandboxExecReceipt = (Join-Path $PSScriptRoot 'sandbox-exec-0.3.1.receipt.json'),
    [switch]$DryRunOutput
)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$serviceName = 'RightKitBuildControl'
$serviceAccount = 'NT SERVICE\RightKitBuildControl'
$legacyTask = 'RightKitManagedAgentBroker'
$dataRoot = 'D:\.rightkit-managed'
$brokerRoot = Join-Path $dataRoot 'rightkit-build-control'
$cargoHome = Join-Path $brokerRoot 'global\cargo-home'
$gitDb = Join-Path $cargoHome 'git\db'
$cache = Join-Path $brokerRoot 'global\compiler-cache'
$buildRoots = Join-Path $brokerRoot 'build-roots'
$binRoot = Join-Path $brokerRoot 'bin'
$sandboxExec = Join-Path $binRoot 'rightkit-sandbox-exec.exe'
$parents = @($dataRoot, $brokerRoot, (Join-Path $brokerRoot 'global'))
$trees = @($cargoHome, $gitDb, $cache, $buildRoots, $binRoot)
if ([string]::IsNullOrWhiteSpace($AgentUser) -or $AgentUser -eq '\' -or $AgentUser -match '[\x00-\x1f]') { throw 'RK_CLASS1_AGENT_IDENTITY: explicit agent account required' }
if ($ServiceArguments -match '[\x00-\x1f]') { throw 'RK_CLASS1_SERVICE_COMMAND: invalid service arguments' }
if ($SandboxExecSource) {
    if ($SandboxExecSource -notmatch '^[A-Za-z]:\\') { throw 'RK_CLASS1_SANDBOX_SOURCE: absolute -SandboxExecSource required' }
    $sandboxReceipt = Get-Content -Raw -LiteralPath $SandboxExecReceipt | ConvertFrom-Json
    $SandboxExecSha256 = [string]$sandboxReceipt.binarySha256; $sandboxSigner = [string]$sandboxReceipt.signerThumbprint
    if ($SandboxExecSha256 -notmatch '^[0-9a-fA-F]{64}$' -or $sandboxSigner -notmatch '^[0-9a-fA-F]{40}$') { throw 'RK_CLASS1_SANDBOX_RECEIPT: receipt needs binarySha256 & signerThumbprint' }
}

# This path does not inspect live roots/service, require elevation, compile, or mutate.
if ($DryRunOutput -or $WhatIfPreference) {
    Write-Output "PLAN service: create/configure $serviceName as $serviceAccount (SCM-aware ServiceExecutable required if missing)"
    Write-Output "PLAN service SID: sc.exe sidtype $serviceName unrestricted; startup manual; no start"
    Write-Output "PLAN legacy task: refuse running jobs; disable $legacyTask after migration"
    foreach ($root in $parents) { Write-Output "PLAN parent: $root; owner=$serviceAccount; inheritance=removed; service=FullControl; agent=$AgentUser ReadAndExecute" }
    foreach ($root in $trees) { Write-Output "PLAN tree: $root; reject reparse points; recursive owner=$serviceAccount; inheritance=removed; service=FullControl; agent=$AgentUser ReadAndExecute" }
    Write-Output "PLAN sandbox launcher: $(if ($SandboxExecSource) { $SandboxExecSource } else { '<none; existing copy kept>' }) -> $sandboxExec; verify receipt SHA-256 & Authenticode signer before copy, SHA-256 after; owner=$serviceAccount"
    Write-Output 'PLAN credentials: untouched'
    return
}

$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$principal = [Security.Principal.WindowsPrincipal]::new($identity)
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) { throw 'RK_CLASS1_ELEVATION_REQUIRED: run operator script elevated' }
$agentSid = ([Security.Principal.NTAccount]::new($AgentUser)).Translate([Security.Principal.SecurityIdentifier])
if ($agentSid.Value -notmatch '^S-1-5-21-(\d+-){3}\d+$') { throw 'RK_CLASS1_AGENT_IDENTITY: normal agent-user SID required' }
# Privilege-backed handle ACLs permit reruns after Administrators ACEs are removed.
# Every existing path component is pinned before enumeration or security changes.
Add-Type -TypeDefinition @'
using System;
using System.IO;
using System.ComponentModel;
using System.Runtime.InteropServices;
using System.Security.AccessControl;
using Microsoft.Win32.SafeHandles;
public static class RKClass1Install {
  [StructLayout(LayoutKind.Sequential)] struct Privilege { public uint Count, Low; public int High; public uint Attributes; }
  [StructLayout(LayoutKind.Sequential)] struct Tag { public uint Attributes, ReparseTag; }
  [DllImport("kernel32.dll")] static extern IntPtr GetCurrentProcess();
  [DllImport("kernel32.dll")] static extern bool CloseHandle(IntPtr h);
  [DllImport("kernel32.dll")] static extern IntPtr LocalFree(IntPtr p);
  [DllImport("advapi32.dll", SetLastError=true)] static extern bool OpenProcessToken(IntPtr p, uint access, out IntPtr token);
  [DllImport("advapi32.dll", CharSet=CharSet.Unicode, SetLastError=true)] static extern bool LookupPrivilegeValueW(string system, string name, out long value);
  [DllImport("advapi32.dll", SetLastError=true)] static extern bool AdjustTokenPrivileges(IntPtr token, bool disable, ref Privilege state, uint size, IntPtr previous, IntPtr length);
  [DllImport("kernel32.dll", CharSet=CharSet.Unicode, SetLastError=true)] static extern SafeFileHandle CreateFileW(string p, uint access, uint share, IntPtr security, uint create, uint flags, IntPtr template);
  [DllImport("kernel32.dll", SetLastError=true)] static extern bool GetFileInformationByHandleEx(SafeFileHandle h, int type, out Tag info, uint size);
  [DllImport("advapi32.dll")] static extern uint GetSecurityInfo(SafeFileHandle h, int type, uint flags, out IntPtr owner, out IntPtr group, out IntPtr dacl, out IntPtr sacl, out IntPtr descriptor);
  [DllImport("advapi32.dll")] static extern uint GetSecurityDescriptorLength(IntPtr sd);
  [DllImport("advapi32.dll")] static extern bool GetSecurityDescriptorOwner(IntPtr sd, out IntPtr owner, out bool defaulted);
  [DllImport("advapi32.dll")] static extern bool GetSecurityDescriptorDacl(IntPtr sd, out bool present, out IntPtr dacl, out bool defaulted);
  [DllImport("advapi32.dll")] static extern uint SetSecurityInfo(SafeFileHandle h, int type, uint flags, IntPtr owner, IntPtr group, IntPtr dacl, IntPtr sacl);
  static void Check(bool ok) { if (!ok) throw new IOException("RK_CLASS1_INSTALL_NATIVE: " + new Win32Exception(Marshal.GetLastWin32Error()).Message); }
  public static void EnablePrivileges() {
    IntPtr token; Check(OpenProcessToken(GetCurrentProcess(), 0x28, out token));
    try { foreach (string name in new [] { "SeBackupPrivilege", "SeRestorePrivilege" }) { long value; Check(LookupPrivilegeValueW(null, name, out value)); var p = new Privilege { Count=1, Low=(uint)value, High=(int)(value >> 32), Attributes=2 }; Check(AdjustTokenPrivileges(token, false, ref p, 0, IntPtr.Zero, IntPtr.Zero)); Check(Marshal.GetLastWin32Error() == 0); } } finally { CloseHandle(token); }
  }
  public static SafeFileHandle Pin(string path) {
    var h = CreateFileW(path, 0xE0080, 1, IntPtr.Zero, 3, 0x02200000, IntPtr.Zero);
    try { Check(!h.IsInvalid); Tag info; Check(GetFileInformationByHandleEx(h, 9, out info, 8)); if ((info.Attributes & 0x400) != 0) throw new IOException("RK_CLASS1_REPARSE_POINT: " + path); return h; } catch { h.Dispose(); throw; }
  }
  public static string Read(SafeFileHandle h) {
    IntPtr owner, group, dacl, sacl, sd; uint status = GetSecurityInfo(h, 1, 5, out owner, out group, out dacl, out sacl, out sd); if (status != 0) throw new IOException("RK_CLASS1_INSTALL_ACL: " + status);
    try { var bytes = new byte[GetSecurityDescriptorLength(sd)]; Marshal.Copy(sd, bytes, 0, bytes.Length); return new RawSecurityDescriptor(bytes, 0).GetSddlForm(AccessControlSections.Owner | AccessControlSections.Access); } finally { LocalFree(sd); }
  }
  public static void Write(SafeFileHandle h, byte[] bytes) {
    var pinned = GCHandle.Alloc(bytes, GCHandleType.Pinned);
    try { IntPtr owner, dacl; bool defaulted, present; Check(GetSecurityDescriptorOwner(pinned.AddrOfPinnedObject(), out owner, out defaulted)); Check(GetSecurityDescriptorDacl(pinned.AddrOfPinnedObject(), out present, out dacl, out defaulted)); if (!present) throw new IOException("RK_CLASS1_INSTALL_ACL: missing DACL"); uint status = SetSecurityInfo(h, 1, 0x80000005, owner, IntPtr.Zero, dacl, IntPtr.Zero); if (status != 0) throw new IOException("RK_CLASS1_INSTALL_ACL: " + status); } finally { pinned.Free(); }
  }
}
'@
[RKClass1Install]::EnablePrivileges()
$handles = [Collections.Generic.Dictionary[string,Microsoft.Win32.SafeHandles.SafeFileHandle]]::new([StringComparer]::OrdinalIgnoreCase)
function Get-Pinned([string]$Path) {
    $full = [IO.Path]::GetFullPath($Path)
    if (-not $handles.ContainsKey($full)) {
        $parent = [IO.Path]::GetDirectoryName($full)
        if ($parent) { $null = Get-Pinned $parent }
        $handles.Add($full, [RKClass1Install]::Pin($full))
    }
    return $handles[$full]
}
try {
$service = Get-CimInstance Win32_Service -Filter "Name='$serviceName'"
$task = Get-ScheduledTask -TaskName $legacyTask -ErrorAction SilentlyContinue
if ($null -ne $task -and $task.State -eq 'Running') { throw 'RK_CLASS1_SERVICE_BUSY: drain legacy broker jobs before migration' }
if ($null -ne $service -and $service.State -ne 'Stopped') { throw 'RK_CLASS1_SERVICE_BUSY: service must be stopped before identity/ACL changes' }
if (-not $ServiceExecutable -and $null -eq $service) { throw 'RK_CLASS1_SERVICE_HOST_REQUIRED: supply protected SCM-aware -ServiceExecutable; scheduled node task cannot serve SCM' }

function Assert-NoReparse([string]$Path) {
    $resolvedPath = [IO.Path]::GetFullPath($Path)
    for ($cursor = $resolvedPath; $cursor; $cursor = [IO.Path]::GetDirectoryName($cursor)) {
        if (Test-Path -LiteralPath $cursor) {
            $null = Get-Pinned $cursor
            $item = Get-Item -LiteralPath $cursor -Force
            if (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) { throw "RK_CLASS1_REPARSE_POINT: $cursor" }
        }
    }
}
function Read-Tree([string]$Path) {
    if (-not (Test-Path -LiteralPath $Path)) { return }
    $item = Get-Item -LiteralPath $Path -Force
    if (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) { throw "RK_CLASS1_REPARSE_POINT: $Path" }
    if (-not $item.PSIsContainer) { throw "RK_CLASS1_DATA_ROOT: directory required $Path" }
    $queue = [Collections.Generic.Queue[string]]::new(); $queue.Enqueue($Path)
    while ($queue.Count -gt 0) {
        $directory = $queue.Dequeue()
        foreach ($entry in Get-ChildItem -LiteralPath $directory -Force) {
            $null = Get-Pinned $entry.FullName
            if (($entry.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) { throw "RK_CLASS1_REPARSE_POINT: $($entry.FullName)" }
            Write-Output $entry
            if ($entry.PSIsContainer) { $queue.Enqueue($entry.FullName) }
        }
    }
}

# Inspect entire selected closure before effects; never follow junctions recursively.
$inventory = @()
foreach ($root in @($parents + $trees | Select-Object -Unique)) {
    Assert-NoReparse $root
    if (Test-Path -LiteralPath $root) {
        if (-not (Get-Item -LiteralPath $root -Force).PSIsContainer) { throw "RK_CLASS1_DATA_ROOT: directory required $root" }
    }
}
foreach ($root in $trees) { $inventory += @(Read-Tree $root) }

$binaryPath = $null
if ($ServiceExecutable) {
    if ($ServiceExecutable -notmatch '^[A-Za-z]:\\' -or $ServiceExecutable.Contains('"')) { throw 'RK_CLASS1_SERVICE_COMMAND: absolute executable path required' }
    Assert-NoReparse $ServiceExecutable
    $exe = Get-Item -LiteralPath $ServiceExecutable -Force
    if ($exe.PSIsContainer -or $exe.Extension -ne '.exe' -or $exe.Name -match '^(node|powershell|pwsh|cmd)\.exe$') { throw 'RK_CLASS1_SERVICE_HOST_REQUIRED: SCM-aware executable required' }
    # Refuse mutable elevated service code; validate every ancestor including drive.
    $mutating = [int]([Security.AccessControl.FileSystemRights]::Write -bor [Security.AccessControl.FileSystemRights]::Delete -bor [Security.AccessControl.FileSystemRights]::DeleteSubdirectoriesAndFiles -bor [Security.AccessControl.FileSystemRights]::ChangePermissions -bor [Security.AccessControl.FileSystemRights]::TakeOwnership)
    for ($cursor = $exe.FullName; $cursor; $cursor = [IO.Path]::GetDirectoryName($cursor)) {
        $acl = Get-Acl -LiteralPath $cursor
        $owner = $acl.GetOwner([Security.Principal.SecurityIdentifier]).Value
        if ($owner -eq $agentSid.Value) { throw "RK_CLASS1_SERVICE_HOST_MUTABLE: agent owns $cursor" }
        foreach ($ace in $acl.GetAccessRules($true, $true, [Security.Principal.SecurityIdentifier])) {
            if ($ace.AccessControlType -eq 'Allow' -and ([int]$ace.FileSystemRights -band $mutating) -ne 0 -and $ace.IdentityReference.Value -notin @('S-1-5-18', 'S-1-5-32-544', 'S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464')) { throw "RK_CLASS1_SERVICE_HOST_MUTABLE: untrusted writer on $cursor" }
        }
    }
    $binaryPath = '"' + $exe.FullName + '"'
    if ($ServiceArguments) { $binaryPath += ' ' + $ServiceArguments }
}
elseif ($null -ne $service) {
    # Existing host must also pass executable protection checks on rerun.
    throw 'RK_CLASS1_SERVICE_HOST_REQUIRED: supply -ServiceExecutable to authenticate existing host on rerun'
}

function Invoke-ServiceControl([string[]]$Arguments) {
    Write-Output ('CHANGE sc.exe ' + ($Arguments -join ' '))
    & "$env:SystemRoot\System32\sc.exe" @Arguments | Write-Output
    if ($LASTEXITCODE -ne 0) { throw "RK_CLASS1_SERVICE_CONTROL: sc.exe failed ($LASTEXITCODE)" }
}
if ($PSCmdlet.ShouldProcess($serviceName, 'Configure virtual service identity & manual startup')) {
    if ($null -eq $service) { Invoke-ServiceControl @('create', $serviceName, 'binPath=', $binaryPath, 'obj=', $serviceAccount, 'start=', 'demand', 'DisplayName=', 'RightKit Build Control') }
    elseif ($service.StartName -ne $serviceAccount -or $service.PathName -ne $binaryPath -or $service.StartMode -ne 'Manual') { Invoke-ServiceControl @('config', $serviceName, 'binPath=', $binaryPath, 'obj=', $serviceAccount, 'start=', 'demand') }
    else { Write-Output "UNCHANGED service $serviceName" }
    Invoke-ServiceControl @('sidtype', $serviceName, 'unrestricted')
}
$serviceSid = ([Security.Principal.NTAccount]::new($serviceAccount)).Translate([Security.Principal.SecurityIdentifier])

function Protect-Item([string]$Path) {
    Assert-NoReparse $Path
    if (-not (Test-Path -LiteralPath $Path)) {
        if ($PSCmdlet.ShouldProcess($Path, 'Create managed directory')) { New-Item -ItemType Directory -Path $Path | Out-Null; Write-Output "CHANGE created $Path" }
    }
    $item = Get-Item -LiteralPath $Path -Force
    $handle = Get-Pinned $Path
    if ($item.PSIsContainer) { $desired = [Security.AccessControl.DirectorySecurity]::new(); $inherit = [Security.AccessControl.InheritanceFlags]::ContainerInherit -bor [Security.AccessControl.InheritanceFlags]::ObjectInherit }
    else { $desired = [Security.AccessControl.FileSecurity]::new(); $inherit = [Security.AccessControl.InheritanceFlags]::None }
    $desired.SetAccessRuleProtection($true, $false); $desired.SetOwner($serviceSid)
    $desired.AddAccessRule([Security.AccessControl.FileSystemAccessRule]::new($serviceSid, [Security.AccessControl.FileSystemRights]::FullControl, $inherit, [Security.AccessControl.PropagationFlags]::None, [Security.AccessControl.AccessControlType]::Allow))
    $desired.AddAccessRule([Security.AccessControl.FileSystemAccessRule]::new($agentSid, [Security.AccessControl.FileSystemRights]::ReadAndExecute, $inherit, [Security.AccessControl.PropagationFlags]::None, [Security.AccessControl.AccessControlType]::Allow))
    $sections = [Security.AccessControl.AccessControlSections]::Owner -bor [Security.AccessControl.AccessControlSections]::Access
    $before = [RKClass1Install]::Read($handle)
    $after = $desired.GetSecurityDescriptorSddlForm($sections)
    if ($before -eq $after) { Write-Output "UNCHANGED protected $Path"; return }
    if ($PSCmdlet.ShouldProcess($Path, "Set owner=$serviceAccount; explicit DACL service=FullControl agent=ReadAndExecute; remove inheritance")) {
        [RKClass1Install]::Write($handle, $desired.GetSecurityDescriptorBinaryForm())
        if ([RKClass1Install]::Read($handle) -ne $after) { throw "RK_CLASS1_ROOT_PROTECTION: ACL readback mismatch $Path" }
        Write-Output "CHANGE protected $Path"
    }
}
# Agent cannot create/replace descendants once protected parents are in place.
foreach ($root in @($parents + $trees | Select-Object -Unique)) { Protect-Item $root }
# Enumerate again after securing roots. Protect each child directory before
# descending, closing late-create gaps in preflight inventory.
foreach ($root in $trees) {
    $queue = [Collections.Generic.Queue[string]]::new(); $queue.Enqueue($root)
    while ($queue.Count -gt 0) {
        foreach ($item in Get-ChildItem -LiteralPath $queue.Dequeue() -Force) {
            Protect-Item $item.FullName
            if ($item.PSIsContainer) { $queue.Enqueue($item.FullName) }
        }
    }
}
# Install the launcher only after bin is service-owned, then verify the installed bytes.
if ($SandboxExecSource -and $PSCmdlet.ShouldProcess($sandboxExec, 'Install rightkit-sandbox-exec.exe')) {
    Assert-NoReparse $SandboxExecSource
    if ((Get-FileHash -Algorithm SHA256 -LiteralPath $SandboxExecSource).Hash -ne $SandboxExecSha256.ToUpperInvariant()) { throw 'RK_CLASS1_SANDBOX_DIGEST: source does not match receipt binarySha256' }
    $signature = Get-AuthenticodeSignature -LiteralPath $SandboxExecSource
    if ($signature.Status -ne 'Valid' -or $signature.SignerCertificate.Thumbprint -ne $sandboxSigner.ToUpperInvariant()) { throw "RK_CLASS1_SANDBOX_SIGNER: Authenticode $($signature.Status); signer must match receipt signerThumbprint" }
    Copy-Item -LiteralPath $SandboxExecSource -Destination $sandboxExec -Force
    Protect-Item $sandboxExec
    if ((Get-FileHash -Algorithm SHA256 -LiteralPath $sandboxExec).Hash -ne $SandboxExecSha256.ToUpperInvariant()) { throw 'RK_CLASS1_SANDBOX_DIGEST: installed launcher readback mismatch' }
    Write-Output "CHANGE installed $sandboxExec"
}
if ($null -ne $task -and $task.State -ne 'Disabled' -and $PSCmdlet.ShouldProcess($legacyTask, 'Disable legacy same-user scheduled broker')) { Disable-ScheduledTask -TaskName $legacyTask | Out-Null; Write-Output "CHANGE disabled $legacyTask" }
Write-Output "CLASS1 service configured; SID=$($serviceSid.Value); agentSID=$($agentSid.Value); buildRoots=$buildRoots; service remains stopped; credentials untouched"
Write-Output ('CLASS1 broker config: ' + (@{ protectedRoot = $brokerRoot; buildRootParent = $buildRoots; windowsSandboxExec = $sandboxExec; serviceSid = $serviceSid.Value; agentSid = $agentSid.Value } | ConvertTo-Json -Compress))
} finally { foreach ($handle in $handles.Values) { $handle.Dispose() } }
