<#
.SYNOPSIS
  Checks (default) or installs (-Install) the pinned Android toolchain for RightKit builds.

.DESCRIPTION
  Idempotent. Reads the pins from toolchain.versions next to this script. Check mode changes
  nothing and exits 1 when something is missing. -Install adds what it safely can:
    * rustup targets (aarch64-linux-android, x86_64-linux-android)
    * cargo-ndk at the pinned version
    * SDK packages (platform, build-tools, platform-tools, NDK) through an EXISTING sdkmanager
  It never accepts SDK licences, never downloads or runs an unpinned installer, and never edits
  environment variables, the registry or system settings; it prints the variables to set.
  Android Studio (JBR, cmdline-tools) is installed by a person.

  Run it through the owner chat only (rightkit#148). It has not been run by its author.

.PARAMETER Install
  Install what is missing and can be installed safely.
.PARAMETER SdkRoot
  Android SDK root. Default: $env:ANDROID_HOME, $env:ANDROID_SDK_ROOT, then
  %LOCALAPPDATA%\Android\Sdk.
#>
[CmdletBinding()]
param(
    [switch]$Install,
    [string]$SdkRoot
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$pins = @{}
foreach ($line in Get-Content -LiteralPath (Join-Path $here 'toolchain.versions')) {
    if ($line -match '^\s*([A-Z_]+)=(.*)$') { $pins[$Matches[1]] = $Matches[2].Trim() }
}
foreach ($k in 'NDK_VERSION', 'CARGO_NDK_VERSION', 'PLATFORM_API', 'BUILD_TOOLS_VERSION', 'RUST_TARGETS', 'JDK_MAJOR') {
    if (-not $pins.ContainsKey($k)) { throw "toolchain.versions has no $k" }
}

$script:missing = New-Object System.Collections.Generic.List[string]
function Report-Ok([string]$m) { Write-Host ("OK       " + $m) }
function Report-Missing([string]$m) { Write-Host ("MISSING  " + $m); $script:missing.Add($m) }
function Report-Did([string]$m) { Write-Host ("INSTALLED " + $m) }

# ---- SDK root -------------------------------------------------------------------------------
if (-not $SdkRoot) { $SdkRoot = $env:ANDROID_HOME }
if (-not $SdkRoot) { $SdkRoot = $env:ANDROID_SDK_ROOT }
if (-not $SdkRoot) { $SdkRoot = Join-Path $env:LOCALAPPDATA 'Android\Sdk' }
Write-Host "SDK root: $SdkRoot"

# ---- JDK (Android Studio JBR) -----------------------------------------------------------------
$jdk = $env:JAVA_HOME
if (-not $jdk) {
    foreach ($c in @((Join-Path $env:ProgramFiles 'Android\Android Studio\jbr'),
                     (Join-Path $env:LOCALAPPDATA 'Programs\Android Studio\jbr'))) {
        if (Test-Path -LiteralPath (Join-Path $c 'bin\java.exe')) { $jdk = $c; break }
    }
}
if ($jdk -and (Test-Path -LiteralPath (Join-Path $jdk 'bin\java.exe'))) {
    $rel = Join-Path $jdk 'release'
    $major = $null
    if (Test-Path -LiteralPath $rel) {
        $m = Select-String -LiteralPath $rel -Pattern '^JAVA_VERSION="?(\d+)' | Select-Object -First 1
        if ($m) { $major = [int]$m.Matches[0].Groups[1].Value }
    }
    if ($major -and $major -ge [int]$pins['JDK_MAJOR']) { Report-Ok "JDK $major at $jdk" }
    else { Report-Missing "JDK $($pins['JDK_MAJOR'])+ (found: $jdk, major: $major). Install Android Studio and use its JBR" }
} else {
    Report-Missing "JDK $($pins['JDK_MAJOR'])+ (set JAVA_HOME to Android Studio's jbr directory)"
}

# ---- sdkmanager and licences -----------------------------------------------------------------
$sdkmanager = $null
foreach ($d in @(Get-ChildItem -LiteralPath (Join-Path $SdkRoot 'cmdline-tools') -Directory -ErrorAction SilentlyContinue |
                 Sort-Object Name -Descending)) {
    $p = Join-Path $d.FullName 'bin\sdkmanager.bat'
    if (Test-Path -LiteralPath $p) { $sdkmanager = $p; break }
}
if ($sdkmanager) { Report-Ok "sdkmanager $sdkmanager" }
else { Report-Missing "sdkmanager (install 'Android SDK Command-line Tools' from Android Studio > SDK Manager)" }

if (Test-Path -LiteralPath (Join-Path $SdkRoot 'licenses\android-sdk-license')) { Report-Ok 'SDK licences accepted' }
else { Report-Missing "SDK licences not accepted: run `"$sdkmanager`" --sdk_root=`"$SdkRoot`" --licenses yourself and read them" }

# ---- SDK packages ----------------------------------------------------------------------------
$ndk = $pins['NDK_VERSION']
$wanted = @(
    @{ Id = "platforms;android-$($pins['PLATFORM_API'])"; Dir = "platforms\android-$($pins['PLATFORM_API'])" },
    @{ Id = "build-tools;$($pins['BUILD_TOOLS_VERSION'])"; Dir = "build-tools\$($pins['BUILD_TOOLS_VERSION'])" },
    @{ Id = 'platform-tools'; Dir = 'platform-tools' },
    @{ Id = "ndk;$ndk"; Dir = "ndk\$ndk" }
)
$toInstall = @()
foreach ($w in $wanted) {
    if (Test-Path -LiteralPath (Join-Path $SdkRoot $w.Dir)) { Report-Ok $w.Id }
    else { Report-Missing $w.Id; $toInstall += $w.Id }
}
$ndkProps = Join-Path $SdkRoot "ndk\$ndk\source.properties"
if (Test-Path -LiteralPath $ndkProps) {
    if (-not (Select-String -LiteralPath $ndkProps -SimpleMatch "Pkg.Revision = $ndk" -Quiet)) {
        Report-Missing "NDK at ndk\$ndk does not report revision $ndk (reinstall it)"
    }
}
if ($Install -and $toInstall.Count -gt 0) {
    $licOk = Test-Path -LiteralPath (Join-Path $SdkRoot 'licenses\android-sdk-license')
    if ($sdkmanager -and $licOk) {
        & $sdkmanager "--sdk_root=$SdkRoot" @toInstall
        if ($LASTEXITCODE -ne 0) { throw "sdkmanager failed ($LASTEXITCODE)" }
        foreach ($id in $toInstall) { Report-Did $id }
        # Drop the entries just installed from the missing list.
        foreach ($id in $toInstall) { [void]$script:missing.Remove($id) }
    } else {
        Write-Host 'SKIPPED  SDK package install: needs sdkmanager and accepted licences (see above)'
    }
}

# ---- rustup targets ----------------------------------------------------------------------------
if (Get-Command rustup -ErrorAction SilentlyContinue) {
    $installed = @(& rustup target list --installed)
    foreach ($t in ($pins['RUST_TARGETS'] -split '\s+')) {
        if ($installed -contains $t) { Report-Ok "rustup target $t" }
        elseif ($Install) {
            & rustup target add $t
            if ($LASTEXITCODE -ne 0) { throw "rustup target add $t failed" }
            Report-Did "rustup target $t"
        } else { Report-Missing "rustup target $t" }
    }
} else {
    Report-Missing 'rustup (install Rust from rustup.rs; the workspace pins rust 1.98.0)'
}

# ---- cargo-ndk ---------------------------------------------------------------------------------
$want = $pins['CARGO_NDK_VERSION']
$have = $null
if (Get-Command cargo-ndk -ErrorAction SilentlyContinue) {
    $have = (& cargo-ndk --version) -join ' '
}
if ($have -and $have -match [regex]::Escape($want)) { Report-Ok "cargo-ndk $want" }
elseif ($Install -and (Get-Command cargo -ErrorAction SilentlyContinue)) {
    & cargo install cargo-ndk --version $want --locked
    if ($LASTEXITCODE -ne 0) { throw "cargo install cargo-ndk failed" }
    Report-Did "cargo-ndk $want"
} else {
    Report-Missing "cargo-ndk $want (found: $have)"
}

# ---- environment to set ----------------------------------------------------------------------
Write-Host ''
Write-Host 'Set these in the shell that builds (this script does not change your environment):'
Write-Host "  `$env:ANDROID_HOME     = '$SdkRoot'"
Write-Host "  `$env:ANDROID_NDK_HOME = '$(Join-Path $SdkRoot "ndk\$ndk")'"
Write-Host "  `$env:NDK_HOME         = `$env:ANDROID_NDK_HOME"
if ($jdk) { Write-Host "  `$env:JAVA_HOME        = '$jdk'" }

if ($script:missing.Count -gt 0) {
    Write-Host ''
    Write-Host ("{0} item(s) missing." -f $script:missing.Count)
    exit 1
}
Write-Host 'Android toolchain is complete.'
exit 0
