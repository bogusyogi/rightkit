#requires -Version 5.1
[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
$Version = '0.1.12'
$Uri = 'https://github.com/Orthic-Labs/Membrane/releases/download/v0.1.12/Membrane.Hub_0.1.12_x64-setup.exe'
$ExpectedSha256 = '8ad86f35b403c54f5f47e3398bb48bd4f426027e91f5e3c9d126f3498761719e'
$ExpectedSigner = 'CN=Damned Ventures LLC'
$Work = Join-Path ([IO.Path]::GetTempPath()) ('membrane-install-' + [Guid]::NewGuid().ToString('N'))
$Installer = Join-Path $Work 'Membrane-Hub-setup.exe'

try {
  New-Item -ItemType Directory -Path $Work -Force | Out-Null
  Invoke-WebRequest -UseBasicParsing -Uri $Uri -OutFile $Installer
  $Hash = (Get-FileHash -LiteralPath $Installer -Algorithm SHA256).Hash.ToLowerInvariant()
  if ($Hash -ne $ExpectedSha256) { throw 'Membrane installer SHA-256 mismatch' }
  $Signature = Get-AuthenticodeSignature -LiteralPath $Installer
  if ($Signature.Status -ne 'Valid' -or -not $Signature.SignerCertificate.Subject.StartsWith($ExpectedSigner)) {
    throw 'Membrane installer Authenticode identity is invalid'
  }
  $Process = Start-Process -FilePath $Installer -ArgumentList '/S' -Wait -PassThru -WindowStyle Hidden
  if ($Process.ExitCode -ne 0) { throw "Membrane installer failed with exit code $($Process.ExitCode)" }
  $InstallRoot = Join-Path $env:LOCALAPPDATA 'Membrane Hub'
  $Executable = @('membrane-hub.exe', 'Membrane Hub.exe', 'MembraneHub.exe') |
    ForEach-Object { Join-Path $InstallRoot $_ } |
    Where-Object { Test-Path -LiteralPath $_ -PathType Leaf } |
    Select-Object -First 1
  if (-not $Executable) { throw 'Membrane Hub executable is missing after install' }
  $InstalledVersion = (Get-Item -LiteralPath $Executable).VersionInfo.ProductVersion
  if (-not $InstalledVersion.StartsWith($Version)) { throw 'Membrane installed version verification failed' }
  Write-Output "Membrane Hub $Version installed & verified."
} finally {
  if (Test-Path -LiteralPath $Work) { Remove-Item -LiteralPath $Work -Recurse -Force }
}
