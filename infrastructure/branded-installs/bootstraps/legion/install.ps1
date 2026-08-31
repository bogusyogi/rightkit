#requires -Version 5.1
[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
$Version = '0.3.4'
$Uri = 'https://github.com/Orthic-Labs/legion/releases/download/v0.3.4/Legion-0.3.4-windows-x86_64-setup.exe'
$ExpectedSha256 = '1972f7a1eb1ccd428f758f0e4a05d50b7aac9264f7d1689bfc6ec122a2a0925b'
$ExpectedSigner = 'CN=Damned Ventures LLC'
$Work = Join-Path ([IO.Path]::GetTempPath()) ('legion-install-' + [Guid]::NewGuid().ToString('N'))
$Installer = Join-Path $Work 'Legion-setup.exe'

try {
  New-Item -ItemType Directory -Path $Work -Force | Out-Null
  Invoke-WebRequest -UseBasicParsing -Uri $Uri -OutFile $Installer
  $Hash = (Get-FileHash -LiteralPath $Installer -Algorithm SHA256).Hash.ToLowerInvariant()
  if ($Hash -ne $ExpectedSha256) { throw 'Legion installer SHA-256 mismatch' }
  $Signature = Get-AuthenticodeSignature -LiteralPath $Installer
  if ($Signature.Status -ne 'Valid' -or -not $Signature.SignerCertificate.Subject.StartsWith($ExpectedSigner)) {
    throw 'Legion installer Authenticode identity is invalid'
  }
  $Process = Start-Process -FilePath $Installer -ArgumentList @('/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART') -Wait -PassThru -WindowStyle Hidden
  if ($Process.ExitCode -ne 0) { throw "Legion installer failed with exit code $($Process.ExitCode)" }
  $Executable = Join-Path $env:LOCALAPPDATA 'Orthic Labs\Legion\current\bin\legion.exe'
  $Ready = $false
  $Deadline = [DateTime]::UtcNow.AddSeconds(15)
  do {
    if (Test-Path -LiteralPath $Executable -PathType Leaf) {
      $InstalledVersion = (& $Executable --version | Out-String).Trim()
      if ($LASTEXITCODE -eq 0 -and $InstalledVersion -eq $Version) { $Ready = $true; break }
    }
    Start-Sleep -Milliseconds 250
  } while ([DateTime]::UtcNow -lt $Deadline)
  if (-not $Ready) { throw 'Legion installed version verification failed' }
  & $Executable doctor | Out-Null
  if ($LASTEXITCODE -ne 0) { throw 'Legion doctor failed after install' }
  Write-Output "Legion $Version installed & verified."
} finally {
  if (Test-Path -LiteralPath $Work) { Remove-Item -LiteralPath $Work -Recurse -Force }
}
