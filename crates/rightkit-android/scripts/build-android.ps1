<#
.SYNOPSIS
  Builds librightkit_android.so per ABI with cargo-ndk and generates the Kotlin bindings.

.DESCRIPTION
  Writes android/src/main/jniLibs/<abi>/ and android/src/main/uniffi-generated/ (both git-ignored)
  and a BUILD-RECEIPT.txt. Run it from anywhere; it locates the workspace root. Runs cargo through
  $env:RIGHTKIT_CARGO when set (for example 'rightkit cargo'), otherwise 'cargo'. Run it through the
  owner chat only. It has not been run by its author.

  -Crate  the package to build (default rightkit-android). An app with its own cdylib that links
          rightkit-android passes its package here and -Library its library name.
#>
[CmdletBinding()]
param(
    [switch]$Debug,
    [string]$Crate = 'rightkit-android',
    [string]$Library = 'rightkit_android'
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$crateDir = Split-Path -Parent $here
$root = Resolve-Path (Join-Path $crateDir '..\..')
$pins = @{}
foreach ($line in Get-Content -LiteralPath (Join-Path $here 'toolchain.versions')) {
    if ($line -match '^\s*([A-Z_]+)=(.*)$') { $pins[$Matches[1]] = $Matches[2].Trim() }
}
$cargo = if ($env:RIGHTKIT_CARGO) { $env:RIGHTKIT_CARGO -split '\s+' } else { @('cargo') }

& (Join-Path $here 'bootstrap-android.ps1')
if ($LASTEXITCODE -ne 0) { throw 'toolchain check failed; fix it or run bootstrap-android.ps1 -Install' }

$jni = Join-Path $crateDir 'android\src\main\jniLibs'
$gen = Join-Path $crateDir 'android\src\main\uniffi-generated'
# One Cargo profile everywhere: the built-in dev profile (never --release). -Debug is accepted for compatibility and changes nothing.
$profile = @()
$targets = @()
foreach ($abi in ($pins['ABIS'] -split '\s+')) { $targets += @('-t', $abi) }

Push-Location $root
try {
    $cargoTail = if ($cargo.Count -gt 1) { $cargo[1..($cargo.Count - 1)] } else { @() }
    & $cargo[0] @cargoTail ndk @targets --platform $pins['MIN_SDK'] -o $jni build -p $Crate @profile
    if ($LASTEXITCODE -ne 0) { throw 'cargo ndk build failed' }

    $lib = Join-Path $jni "arm64-v8a\lib$Library.so"
    if (Test-Path -LiteralPath $gen) { Remove-Item -LiteralPath $gen -Recurse -Force }
    & $cargo[0] @cargoTail run -p rightkit-android --features bindgen --bin uniffi-bindgen -- `
        generate --library $lib --language kotlin --out-dir $gen --no-format
    if ($LASTEXITCODE -ne 0) { throw 'uniffi-bindgen failed' }

    $rev = (& git rev-parse --short HEAD 2>$null)
    $receipt = @(
        "built:   $((Get-Date).ToUniversalTime().ToString('o'))",
        "git:     $rev",
        "crate:   $Crate (dev)",
        "ndk:     $($pins['NDK_VERSION'])",
        "minSdk:  $($pins['MIN_SDK'])",
        "abis:    $($pins['ABIS'])"
    )
    foreach ($f in Get-ChildItem -LiteralPath $jni -Recurse -Filter '*.so') {
        $receipt += ('sha256:  {0}  {1}' -f (Get-FileHash -Algorithm SHA256 -LiteralPath $f.FullName).Hash.ToLower(), $f.FullName.Substring($jni.Length + 1))
    }
    Set-Content -LiteralPath (Join-Path $jni 'BUILD-RECEIPT.txt') -Value $receipt
    Write-Host "Done. Receipt: $(Join-Path $jni 'BUILD-RECEIPT.txt')"
} finally { Pop-Location }
