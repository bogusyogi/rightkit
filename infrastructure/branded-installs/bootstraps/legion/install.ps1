#requires -Version 5.1
[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
$Version = '0.3.12'
$Uri = 'https://github.com/Orthic-Labs/legion/releases/download/v0.3.12/Legion-0.3.12-windows-x86_64-setup.exe'
$ExpectedSha256 = 'd9a8b90c58d1758363eb0ad94559dc4e0629a7c5966ad2b2b7b5cec35c333d3a'
$ExpectedSigner = 'CN=Damned Ventures LLC'
$CommandTimeoutSeconds = 300
$Work = Join-Path ([IO.Path]::GetTempPath()) ('legion-install-' + [Guid]::NewGuid().ToString('N'))
$Installer = Join-Path $Work 'Legion-setup.exe'

function Write-BootstrapLifecycle([string]$Phase, [string]$State, [string]$Code, [string]$Detail = '') {
  $Event = [ordered]@{
    schema = 'rightkit.branded-bootstrap.lifecycle.v1'
    product = 'legion'
    phase = $Phase
    state = $State
    code = $Code
    detail = $Detail
  }
  [Console]::Out.WriteLine(($Event | ConvertTo-Json -Compress))
}

function Invoke-BoundedCommand([string]$Executable, [string[]]$Arguments, [string]$Phase, [string]$Code) {
  $StdoutPath = Join-Path $Work ($Phase + '-stdout.txt')
  $StderrPath = Join-Path $Work ($Phase + '-stderr.txt')
  Write-BootstrapLifecycle $Phase 'started' ($Code + '_STARTED')
  $Process = Start-Process -FilePath $Executable -ArgumentList $Arguments -PassThru -WindowStyle Hidden -RedirectStandardOutput $StdoutPath -RedirectStandardError $StderrPath
  # Cache the process handle so ExitCode is populated after exit (Windows PowerShell 5.1 returns $null otherwise).
  $null = $Process.Handle
  if (-not $Process.WaitForExit($CommandTimeoutSeconds * 1000)) {
    try { $Process.Kill() } catch {}
    Write-BootstrapLifecycle $Phase 'failed' ($Code + '_TIMEOUT') "command exceeded $CommandTimeoutSeconds seconds"
    throw "[$($Code)_TIMEOUT] $Phase exceeded $CommandTimeoutSeconds seconds"
  }
  $Process.WaitForExit()
  $Stdout = if (Test-Path -LiteralPath $StdoutPath) { [IO.File]::ReadAllText($StdoutPath).Trim() } else { '' }
  $Stderr = if (Test-Path -LiteralPath $StderrPath) { [IO.File]::ReadAllText($StderrPath).Trim() } else { '' }
  if ($Process.ExitCode -ne 0) {
    Write-BootstrapLifecycle $Phase 'failed' ($Code + '_FAILED') $Stderr
    throw "[$($Code)_FAILED] $Phase exited $($Process.ExitCode): $Stderr"
  }
  Write-BootstrapLifecycle $Phase 'succeeded' ($Code + '_SUCCEEDED')
  return [pscustomobject]@{ Stdout = $Stdout; Stderr = $Stderr; ExitCode = $Process.ExitCode }
}

function Test-SamePath([string]$Left, [string]$Right) {
  if ([string]::IsNullOrWhiteSpace($Left) -or [string]::IsNullOrWhiteSpace($Right)) { return $false }
  return [string]::Equals([IO.Path]::GetFullPath($Left).TrimEnd('\'), [IO.Path]::GetFullPath($Right).TrimEnd('\'), [StringComparison]::OrdinalIgnoreCase)
}

function Assert-LegionSetup([string]$Json, [string]$ExpectedKind, [string]$Executable, [string]$Code) {
  try { $Status = $Json | ConvertFrom-Json } catch { throw "[$($Code)_INVALID] Legion setup output was not JSON" }
  if ($Status.kind -ne $ExpectedKind -or $Status.status -ne 'complete' -or $Status.origin -ne 'installed' -or $Status.stableCurrent -ne $true) {
    throw "[$($Code)_INVALID] Legion setup did not report complete installed activation"
  }
  if (-not (Test-SamePath ([string]$Status.executable) $Executable)) { throw "[$($Code)_BINDING_INVALID] Legion setup escaped stable current executable" }
  $Clients = if ($ExpectedKind -eq 'legion-setup-execution') { @($Status.execution.clients) } else { @($Status.clients) }
  foreach ($ClientId in @('claude-code', 'codex')) {
    $Client = @($Clients | Where-Object { $_.clientId -eq $ClientId -or $_.client_id -eq $ClientId }) | Select-Object -First 1
    if (-not $Client -or $Client.installed -ne $true -or $Client.fidelity -ne 'Full') {
      throw "[$($Code)_CLIENT_INVALID] Legion setup did not structurally activate $ClientId"
    }
  }
  if ($Status.liveIdentity.projections.claudePlugin.state -ne 'current' -or $Status.liveIdentity.projections.codexPlugin.state -ne 'current') {
    throw "[$($Code)_PROJECTION_INVALID] Legion setup did not report current Claude & Codex projections"
  }
}

try {
  Write-BootstrapLifecycle 'bootstrap' 'started' 'LEGION_BOOTSTRAP_STARTED'
  New-Item -ItemType Directory -Path $Work -Force | Out-Null
  Invoke-WebRequest -UseBasicParsing -Uri $Uri -OutFile $Installer
  $Hash = (Get-FileHash -LiteralPath $Installer -Algorithm SHA256).Hash.ToLowerInvariant()
  if ($Hash -ne $ExpectedSha256) { throw '[LEGION_INSTALLER_HASH_INVALID] Legion installer SHA-256 mismatch' }
  $Signature = Get-AuthenticodeSignature -LiteralPath $Installer
  if ($Signature.Status -ne 'Valid' -or -not $Signature.SignerCertificate.Subject.StartsWith($ExpectedSigner)) {
    throw '[LEGION_INSTALLER_SIGNATURE_INVALID] Legion installer Authenticode identity is invalid'
  }
  $Process = Start-Process -FilePath $Installer -ArgumentList @('/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART') -Wait -PassThru -WindowStyle Hidden
  if ($Process.ExitCode -ne 0) { throw "[LEGION_INSTALL_FAILED] Legion installer failed with exit code $($Process.ExitCode)" }

  $InstallRoot = Join-Path $env:LOCALAPPDATA 'Orthic Labs\Legion\current'
  $Executable = Join-Path $InstallRoot 'bin\legion.exe'
  if (-not (Test-Path -LiteralPath $Executable -PathType Leaf)) { throw '[LEGION_EXECUTABLE_MISSING] Stable Legion executable is missing after install' }

  $VersionResult = Invoke-BoundedCommand $Executable @('--version') 'version' 'LEGION_VERSION'
  if ($VersionResult.Stdout -ne $Version) { throw "[LEGION_VERSION_INVALID] Expected $Version, received $($VersionResult.Stdout)" }

  $RepairResult = Invoke-BoundedCommand $Executable @('--json', 'setup', 'repair', '--confirm') 'repair' 'LEGION_SETUP_REPAIR'
  Assert-LegionSetup $RepairResult.Stdout 'legion-setup-execution' $Executable 'LEGION_SETUP_REPAIR'
  $StatusResult = Invoke-BoundedCommand $Executable @('--json', 'setup', 'status') 'status' 'LEGION_SETUP_STATUS'
  Assert-LegionSetup $StatusResult.Stdout 'legion-setup-status' $Executable 'LEGION_SETUP_STATUS'
  [void](Invoke-BoundedCommand $Executable @('doctor') 'doctor' 'LEGION_DOCTOR')
  Write-BootstrapLifecycle 'bootstrap' 'succeeded' 'LEGION_BOOTSTRAP_SUCCEEDED' "version=$Version"
} catch {
  Write-BootstrapLifecycle 'bootstrap' 'failed' 'LEGION_BOOTSTRAP_FAILED' $_.Exception.Message
  throw
} finally {
  if (Test-Path -LiteralPath $Work) { Remove-Item -LiteralPath $Work -Recurse -Force }
}
