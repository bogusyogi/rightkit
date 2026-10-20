# Shared by .github/actions/rk-setup and the publish.yml verify job (Windows, crates runs).
# Moved verbatim from rk-setup so qualification and shared CI locate the same native OpenSSL.
# bundled-sqlcipher needs native OpenSSL headers and an MSVC import
# library. Do not select Strawberry/MinGW .a archives or disable vendoring.
$roots = @(Get-ChildItem -LiteralPath $env:ProgramFiles -Directory -Filter 'OpenSSL*')
$selected = $null
foreach ($root in ($roots | Sort-Object FullName)) {
  $include = Join-Path $root.FullName 'include'
  $bin = Join-Path $root.FullName 'bin'
  if (-not (Test-Path -LiteralPath (Join-Path $include 'openssl/ssl.h'))) { continue }
  $library = @('lib/VC/x64/MD', 'lib/VC/x64') |
    ForEach-Object { Join-Path $root.FullName $_ } |
    Where-Object { Test-Path -LiteralPath (Join-Path $_ 'libcrypto.lib') } |
    Select-Object -First 1
  if (-not $library) { continue }
  $runtime = Get-ChildItem -LiteralPath $bin -Filter 'libcrypto*.dll' -File -ErrorAction SilentlyContinue |
    Where-Object {
      $bytes = [System.IO.File]::ReadAllBytes($_.FullName)
      if ($bytes.Length -lt 64) { return $false }
      $pe = [BitConverter]::ToInt32($bytes, 60)
      $pe -ge 0 -and $pe -le ($bytes.Length - 6) -and
        [BitConverter]::ToUInt32($bytes, $pe) -eq 0x00004550 -and
        [BitConverter]::ToUInt16($bytes, $pe + 4) -eq 0x8664
    } | Select-Object -First 1
  if (-not $runtime) { continue }
  $selected = @{ Include = $include; Library = $library; Bin = $bin; Runtime = $runtime }
  break
}
if (-not $selected) {
  Write-Warning 'Native SQLCipher system crypto unavailable: require Program Files/OpenSSL*/include/openssl/ssl.h, lib/VC/x64/MD/libcrypto.lib (or lib/VC/x64/libcrypto.lib), and matching x64 bin/libcrypto*.dll. OpenSSL paths were not exported; required SQLCipher feature checks will report missing prerequisites. Restore hosted image native OpenSSL; do not substitute MinGW libraries.'
} else {
  "OPENSSL_INCLUDE_DIR=$($selected.Include)" >> $env:GITHUB_ENV
  "OPENSSL_LIB_DIR=$($selected.Library)" >> $env:GITHUB_ENV
  $selected.Bin >> $env:GITHUB_PATH
  Write-Host "OpenSSL headers: $($selected.Include)"
  Write-Host "OpenSSL MSVC x64 import library: $($selected.Library)/libcrypto.lib"
  Write-Host "OpenSSL runtime: $($selected.Runtime.FullName) version $($selected.Runtime.VersionInfo.FileVersion)"
}
