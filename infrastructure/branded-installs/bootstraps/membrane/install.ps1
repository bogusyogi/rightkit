#requires -Version 5.1
[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
$Version = '0.1.12'
$Uri = 'https://github.com/Orthic-Labs/Membrane/releases/download/v0.1.12/Membrane.Hub_0.1.12_x64-setup.exe'
$ExpectedSha256 = '8ad86f35b403c54f5f47e3398bb48bd4f426027e91f5e3c9d126f3498761719e'
$ExpectedSigner = 'CN=Damned Ventures LLC'
$CommandTimeoutSeconds = 300
$Work = Join-Path ([IO.Path]::GetTempPath()) ('membrane-install-' + [Guid]::NewGuid().ToString('N'))
$Installer = Join-Path $Work 'Membrane-Hub-setup.exe'

function Write-BootstrapLifecycle([string]$Phase, [string]$State, [string]$Code, [string]$Detail = '') {
  $Event = [ordered]@{
    schema = 'rightkit.branded-bootstrap.lifecycle.v1'
    product = 'membrane'
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

try {
  Write-BootstrapLifecycle 'bootstrap' 'started' 'MEMBRANE_BOOTSTRAP_STARTED'
  New-Item -ItemType Directory -Path $Work -Force | Out-Null
  Invoke-WebRequest -UseBasicParsing -Uri $Uri -OutFile $Installer
  $Hash = (Get-FileHash -LiteralPath $Installer -Algorithm SHA256).Hash.ToLowerInvariant()
  if ($Hash -ne $ExpectedSha256) { throw '[MEMBRANE_INSTALLER_HASH_INVALID] Membrane installer SHA-256 mismatch' }
  $Signature = Get-AuthenticodeSignature -LiteralPath $Installer
  if ($Signature.Status -ne 'Valid' -or -not $Signature.SignerCertificate.Subject.StartsWith($ExpectedSigner)) {
    throw '[MEMBRANE_INSTALLER_SIGNATURE_INVALID] Membrane installer Authenticode identity is invalid'
  }
  $Process = Start-Process -FilePath $Installer -ArgumentList '/S' -Wait -PassThru -WindowStyle Hidden
  if ($Process.ExitCode -ne 0) { throw "[MEMBRANE_INSTALL_FAILED] Membrane installer failed with exit code $($Process.ExitCode)" }

  $InstallRoot = Join-Path $env:LOCALAPPDATA 'Orthic Labs\Membrane\current'
  $Executable = Join-Path $InstallRoot 'membrane.exe'
  if (-not (Test-Path -LiteralPath $Executable -PathType Leaf)) { throw '[MEMBRANE_EXECUTABLE_MISSING] Stable Membrane executable is missing after install' }

  $VersionResult = Invoke-BoundedCommand $Executable @('--version') 'version' 'MEMBRANE_VERSION'
  if ($VersionResult.Stdout -ne "membrane $Version") { throw "[MEMBRANE_VERSION_INVALID] Expected membrane $Version, received $($VersionResult.Stdout)" }

  $QuotedInstallRoot = '"' + $InstallRoot + '"'
  [void](Invoke-BoundedCommand $Executable @('activate', '--install-root', $QuotedInstallRoot) 'activate' 'MEMBRANE_ACTIVATE')
  $StatusResult = Invoke-BoundedCommand $Executable @('activate', '--install-root', $QuotedInstallRoot, '--dry-run') 'status' 'MEMBRANE_STATUS'
  try { $Status = $StatusResult.Stdout | ConvertFrom-Json } catch { throw '[MEMBRANE_STATUS_INVALID] Membrane status did not emit JSON' }
  if ($Status.schemaVersion -ne 1 -or $Status.dryRun -ne $true -or $Status.runtimeOrigin -ne 'installed') {
    throw '[MEMBRANE_STATUS_INVALID] Membrane status did not report installed dry-run health'
  }
  if (-not (Test-SamePath ([string]$Status.installRoot) $InstallRoot) -or -not (Test-SamePath ([string]$Status.membraneExecutable) $Executable)) {
    throw '[MEMBRANE_STATUS_BINDING_INVALID] Membrane status escaped stable current executable'
  }
  if ([string]::IsNullOrWhiteSpace([string]$Status.versionRoot) -or [string]::IsNullOrWhiteSpace([string]$Status.trayExecutable)) {
    throw '[MEMBRANE_STATUS_BINDING_INVALID] Membrane status omitted version or tray executable identity'
  }
  if ($Status.service.serviceId -ne 'membrane-hub' -or [string]::IsNullOrWhiteSpace([string]$Status.service.releaseGeneration) -or @($Status.clients).Count -lt 2) {
    throw '[MEMBRANE_STATUS_HEALTH_INVALID] Membrane status omitted resident service or client health'
  }
  Write-BootstrapLifecycle 'bootstrap' 'succeeded' 'MEMBRANE_BOOTSTRAP_SUCCEEDED' "version=$Version"
} catch {
  Write-BootstrapLifecycle 'bootstrap' 'failed' 'MEMBRANE_BOOTSTRAP_FAILED' $_.Exception.Message
  throw
} finally {
  if (Test-Path -LiteralPath $Work) { Remove-Item -LiteralPath $Work -Recurse -Force }
}
