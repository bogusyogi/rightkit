# RightKit MSIX repair guard (canonical source: rightkit/packages/release/msix-repair-guard.ps1). ASCII only; Windows PowerShell 5.1 compatible.
#
# Why this exists: cancelling an MSIX update while Windows registers it can leave the OLD package
# "Modified, NeedsRemediation" (event 751: "Windows failed to undo changes ... The package needs to be reinstalled").
# A package in that state cannot be launched, so nothing inside the package can repair it. This script lives OUTSIDE the
# package (the app writes it to %LOCALAPPDATA%\<AppName>\repair\ on every normal launch) and is run by a per-user scheduled
# task `<PFN>-Repair` (no admin) on event 751 in Microsoft-Windows-AppXDeploymentServer/Operational, at logon, and at a
# one-shot retry time after a failed repair.
#
#   msix-repair-guard.ps1 -Install -Pfn <family> [-Feed <.appinstaller uri>]   register/refresh the task and write guard.json
#   msix-repair-guard.ps1                                                      (task action) repair if needed, clean up if uninstalled
#
# Repair order: `Add-AppxPackage -AppInstallerFile <feed>` (known-good), then `Add-AppxPackage -Path <msix>` (the msix is downloaded
# first and its manifest identity checked). Every decision is logged to repair.log next to this script; every run also writes a
# machine-readable last-result.json (status, code, time, detail), because the task runs under `conhost --headless` and hides exit codes.
#
# Exit codes (also in last-result.json): 0 healthy / repaired / cleaned up / another run active, 1 repair failed (a retry is scheduled),
# 2 guard misconfigured or refused (guard.json or an install input failed validation), 3 no install source known, 4 unhandled error
# (logged; a retry is scheduled).
#
# Retry: after a failed repair (exit 1 or 4) the script gives the task a one-shot time trigger at +2, +5, +15, +60 minutes and then
# hourly, for at most 24 hours from the first failure; the trigger is removed on success, when the package is healthy, or when the
# app is uninstalled. State: retry.json. A later logon or event 751 run still starts a repair at any time.
#
# Uninstall cleanup: a run that finds the package absent (twice, 10 s apart) removes this folder and the task. It runs at the next
# logon, the next event 751, or the next retry tick; Windows has no "package removed" trigger for a per-user task.
#
# Hardening: feed and msix URIs must be https, or http to a loopback host (a local test feed). A feed or msix whose identity (Name,
# Publisher) differs from the installed package is refused and never installed. guard.json must name this folder's task.
#
# Requirement: the app's data directory must be physical (MSIX fileSystemWrite virtualization disabled), otherwise the packaged app
# writes this folder into its private LocalCache and the task cannot see it.
[CmdletBinding()]
param([switch]$Install, [string]$Pfn = '', [string]$Feed = '')
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'   # Windows PowerShell 5.1 renders a progress bar per downloaded chunk: a 30 MB Invoke-WebRequest -OutFile then takes minutes
$GuardVersion = 2
$Dir = Split-Path -Parent $PSCommandPath
$LogPath = Join-Path $Dir 'repair.log'
$ConfigPath = Join-Path $Dir 'guard.json'
$RetryPath = Join-Path $Dir 'retry.json'
$AttemptPath = Join-Path $Dir 'install-attempt.json'
$ResultPath = Join-Path $Dir $(if ($Install) { 'install-result.json' } else { 'last-result.json' })
$Mode = $(if ($Install) { 'install' } else { 'repair' })
$RetryDelaysMinutes = @(2, 5, 15, 60)   # then hourly
$RetryWindowHours = 24
$PfnPattern = '^[A-Za-z0-9][A-Za-z0-9.\-]*_[a-z0-9]{13}$'
$script:Ctx = ''

function Log([string]$m) {
    try {
        if ((Test-Path -LiteralPath $LogPath) -and ((Get-Item -LiteralPath $LogPath).Length -gt 262144)) { Move-Item -LiteralPath $LogPath -Destination ($LogPath + '.old') -Force }
        Add-Content -LiteralPath $LogPath -Value ((Get-Date).ToString('yyyy-MM-ddTHH:mm:ss.fffzzz') + ' [' + $Mode + '] ' + $m)
    } catch { }
}
function Write-Json([string]$path, $obj) {
    try {
        $tmp = $path + '.tmp'
        [IO.File]::WriteAllText($tmp, ($obj | ConvertTo-Json), (New-Object Text.UTF8Encoding($false)))
        Move-Item -LiteralPath $tmp -Destination $path -Force
    } catch { Log ('could not write ' + $path + ': ' + $_.Exception.Message) }
}
function Write-Result([string]$status, [int]$code, [string]$detail) {
    $r = [ordered]@{ status = $status; code = $code; mode = $Mode; timeUtc = (Get-Date).ToUniversalTime().ToString('o'); guardVersion = $GuardVersion; detail = $detail }
    $st = Read-Json $RetryPath
    if ($st -and $st.nextEpoch) { $r.nextRetryUtc = ([datetime]'1970-01-01T00:00:00Z').ToUniversalTime().AddSeconds([double]$st.nextEpoch).ToString('o'); $r.retryAttempt = [int]$st.attempt }
    Write-Json $ResultPath $r
}
function Finish([string]$status, [int]$code, [string]$detail) {
    Log ($status.ToUpper() + ' (code ' + $code + '): ' + $detail)
    Write-Result $status $code $detail
    exit $code
}
function Read-Json([string]$path) { try { return (Get-Content -LiteralPath $path -Raw -ErrorAction Stop | ConvertFrom-Json) } catch { return $null } }
function Read-Config { return (Read-Json $ConfigPath) }

# ------------------------------------------------------------------ scheduled task (shared by install and the retry logic)
function Get-TaskXml([string]$family, $retryLocal) {
    $id = [Security.Principal.WindowsIdentity]::GetCurrent()
    $esc = { param($s) [Security.SecurityElement]::Escape([string]$s) }
    $sub = '<QueryList><Query Id="0" Path="Microsoft-Windows-AppXDeploymentServer/Operational"><Select Path="Microsoft-Windows-AppXDeploymentServer/Operational">*[System[(EventID=751)]]</Select></Query></QueryList>'
    $taskArgs = '--headless powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -WindowStyle Hidden -File "' + $PSCommandPath + '"'
    $retryXml = ''
    if ($retryLocal) { $retryXml = '<TimeTrigger><StartBoundary>' + ([datetime]$retryLocal).ToString('yyyy-MM-ddTHH:mm:ss') + '</StartBoundary><Enabled>true</Enabled></TimeTrigger>' }
    return @"
<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.4" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo><Description>Repair guard for $(& $esc $family): re-adds the app from its App Installer feed when an interrupted update leaves it NeedsRemediation; retries after a failure. Removes itself when the app is uninstalled.</Description></RegistrationInfo>
  <Triggers>
    <EventTrigger><Enabled>true</Enabled><Subscription>$(& $esc $sub)</Subscription></EventTrigger>
    <LogonTrigger><Enabled>true</Enabled><UserId>$(& $esc $id.Name)</UserId></LogonTrigger>
    $retryXml
  </Triggers>
  <Principals><Principal id="Author"><UserId>$(& $esc $id.User.Value)</UserId><LogonType>InteractiveToken</LogonType><RunLevel>LeastPrivilege</RunLevel></Principal></Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <AllowHardTerminate>true</AllowHardTerminate>
    <StartWhenAvailable>true</StartWhenAvailable>
    <AllowStartOnDemand>true</AllowStartOnDemand>
    <Enabled>true</Enabled>
    <Hidden>true</Hidden>
    <ExecutionTimeLimit>PT30M</ExecutionTimeLimit>
  </Settings>
  <Actions Context="Author"><Exec><Command>%SystemRoot%\System32\conhost.exe</Command><Arguments>$(& $esc $taskArgs)</Arguments></Exec></Actions>
</Task>
"@
}
function Register-GuardTask([string]$family, $retryLocal) {
    [void](Register-ScheduledTask -TaskName ($family + '-Repair') -Xml (Get-TaskXml $family $retryLocal) -Force)
}
function Get-RetryLocal {
    $st = Read-Json $RetryPath
    if ($st -and $st.nextEpoch) { return ([datetime]'1970-01-01T00:00:00Z').ToUniversalTime().AddSeconds([double]$st.nextEpoch).ToLocalTime() }
    return $null
}

# ------------------------------------------------------------------ retry state (repair mode)
function Clear-Retry([string]$family) {
    if (-not (Test-Path -LiteralPath $RetryPath)) { return }
    try { Remove-Item -LiteralPath $RetryPath -Force } catch { }
    try { Register-GuardTask $family $null; Log 'retry cleared (task has no one-shot trigger)' } catch { Log ('could not clear the retry trigger: ' + $_.Exception.Message) }
}
function Set-Retry([string]$family, [string]$why) {
    $epoch0 = [datetime]'1970-01-01T00:00:00Z'
    $now = [datetime]::UtcNow
    $nowEpoch = [math]::Floor(($now - $epoch0.ToUniversalTime()).TotalSeconds)
    $st = Read-Json $RetryPath
    $attempt = 1; $first = $nowEpoch
    if ($st -and $st.firstFailureEpoch -and (($nowEpoch - [double]$st.firstFailureEpoch) -lt ($RetryWindowHours * 3600))) { $attempt = [int]$st.attempt + 1; $first = [double]$st.firstFailureEpoch }
    $delay = $(if ($attempt -le $RetryDelaysMinutes.Count) { $RetryDelaysMinutes[$attempt - 1] } else { 60 })
    $next = $nowEpoch + ($delay * 60)
    if (($next - $first) -gt ($RetryWindowHours * 3600)) {
        Write-Json $RetryPath ([ordered]@{ attempt = $attempt; firstFailureEpoch = $first; gaveUp = $true; reason = $why })
        try { Register-GuardTask $family $null } catch { }
        Log ('retry window of ' + $RetryWindowHours + ' h used up after ' + ($attempt - 1) + ' retries; no further automatic retry (next logon / event 751 starts again)')
        return
    }
    Write-Json $RetryPath ([ordered]@{ attempt = $attempt; firstFailureEpoch = $first; nextEpoch = $next; delayMinutes = $delay; reason = $why })
    $local = $epoch0.ToUniversalTime().AddSeconds($next).ToLocalTime()
    try { Register-GuardTask $family $local; Log ('retry ' + $attempt + ' scheduled in ' + $delay + ' min at ' + $local.ToString('s')) } catch { Log ('could not schedule the retry: ' + $_.Exception.Message) }
}

# ------------------------------------------------------------------ package state
function Get-Family([string]$family) {
    $name = $family.Substring(0, $family.LastIndexOf('_'))
    return @(Get-AppxPackage -Name $name -PackageTypeFilter Main -ErrorAction SilentlyContinue | Where-Object { [string]$_.PackageFamilyName -eq $family })
}
function Get-StateText([string]$family) {
    $p = @(Get-Family $family)
    if ($p.Count -eq 0) { return 'absent' }
    return (($p | ForEach-Object { [string]$_.Version + ':' + [string]$_.Status }) -join ',')
}
function Test-Healthy([string]$family) {
    $p = @(Get-Family $family)
    return ($p.Count -gt 0 -and @($p | Where-Object { [string]$_.Status -ne 'Ok' }).Count -eq 0)
}
function Test-AllowedUri([string]$uri) {
    try {
        $u = [Uri]$uri
        if (-not $u.IsAbsoluteUri -or $u.UserInfo) { return $false }
        if ($u.Scheme -eq 'https') { return $true }
        if ($u.Scheme -eq 'http' -and $u.IsLoopback) { return $true }
    } catch { }
    return $false
}
function Same-Text([string]$a, [string]$b) { return ([string]::Equals($a.Trim(), $b.Trim(), [StringComparison]::OrdinalIgnoreCase)) }
function Test-Identity($expected, [string]$name, [string]$publisher) {
    if (-not $expected.name -or -not $expected.publisher) { return 'no expected identity recorded' }
    if (-not (Same-Text $expected.name $name)) { return ('Name ' + $name + ' differs from ' + $expected.name) }
    if (-not (Same-Text $expected.publisher $publisher)) { return ('Publisher ' + $publisher + ' differs from ' + $expected.publisher) }
    return ''
}
function Get-FeedInfo([string]$feedUri) {
    $r = Invoke-WebRequest -Uri $feedUri -UseBasicParsing -TimeoutSec 20 -MaximumRedirection 3
    if (-not (Test-AllowedUri ([string]$r.BaseResponse.ResponseUri))) { throw ('feed redirected to a disallowed uri ' + $r.BaseResponse.ResponseUri) }
    $text = $r.Content
    if ($text -is [byte[]]) { $text = [Text.Encoding]::UTF8.GetString($text) }   # a non-text content type returns bytes
    $x = [xml]($text.TrimStart([char]0xFEFF))
    $n = $x.GetElementsByTagName('MainPackage') | Select-Object -First 1
    if (-not $n) { $n = $x.GetElementsByTagName('MainBundle') | Select-Object -First 1 }
    if (-not $n) { throw 'feed has no MainPackage or MainBundle' }
    return [pscustomobject]@{ uri = [string]$n.Uri; name = [string]$n.Name; publisher = [string]$n.Publisher; version = [string]$n.Version }
}
function Save-Msix([string]$uri, [string]$dest) {
    $r = Invoke-WebRequest -Uri $uri -UseBasicParsing -TimeoutSec 900 -MaximumRedirection 3 -OutFile $dest -PassThru
    if (-not (Test-AllowedUri ([string]$r.BaseResponse.ResponseUri))) { throw ('msix redirected to a disallowed uri ' + $r.BaseResponse.ResponseUri) }
}
function Get-MsixIdentity([string]$file) {
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $zip = [IO.Compression.ZipFile]::OpenRead($file)
    try {
        $entry = $zip.GetEntry('AppxManifest.xml')
        $kind = 'Package'
        if (-not $entry) { $entry = $zip.GetEntry('AppxMetadata/AppxBundleManifest.xml'); $kind = 'Bundle' }
        if (-not $entry) { throw 'no AppxManifest.xml or AppxBundleManifest.xml in the archive' }
        $sr = New-Object IO.StreamReader($entry.Open())
        try { $x = [xml]($sr.ReadToEnd().TrimStart([char]0xFEFF)) } finally { $sr.Dispose() }
        $idn = $x.GetElementsByTagName('Identity') | Select-Object -First 1
        if (-not $idn) { throw 'manifest has no Identity' }
        return [pscustomobject]@{ name = [string]$idn.Name; publisher = [string]$idn.Publisher; version = [string]$idn.Version; kind = $kind }
    } finally { $zip.Dispose() }
}

# ================================================================== install: register / refresh the task
if ($Install) {
    trap { Log ('UNHANDLED ERROR: ' + $_.Exception.Message + ' at line ' + $_.InvocationInfo.ScriptLineNumber); Write-Result 'error' 4 $_.Exception.Message; exit 4 }
    if (-not $Pfn) { Finish 'misconfigured' 2 'install refused: no -Pfn' }
    if ($Pfn -notmatch $PfnPattern) { Finish 'refused' 2 ('install refused: -Pfn is not a package family name: ' + $Pfn) }
    $prev = Read-Config
    $feedUri = $Feed
    try {
        [void][Windows.Management.Deployment.PackageManager, Windows.Management.Deployment, ContentType = WindowsRuntime]
        $pm = New-Object Windows.Management.Deployment.PackageManager
        foreach ($x in @($pm.FindPackagesForUser('', $Pfn))) {
            $ai = $x.GetAppInstallerInfo()
            if ($ai -and $ai.Uri) { $feedUri = [string]$ai.Uri; break }
        }
    } catch { Log ('App Installer info unreadable: ' + $_.Exception.Message) }
    if (-not $feedUri -and $prev -and $prev.feed) { $feedUri = [string]$prev.feed; Log 'package has no App Installer association; keeping the recorded feed' }
    $msix = $(if ($prev -and $prev.msix) { [string]$prev.msix } else { '' })
    if ($feedUri -and -not (Test-AllowedUri $feedUri)) { Finish 'refused' 2 ('install refused: feed uri is not https (or http to loopback): ' + $feedUri) }
    if ($feedUri) {
        try {
            $fi = Get-FeedInfo $feedUri
            if ($fi.uri) { if (Test-AllowedUri $fi.uri) { $msix = $fi.uri } else { Log ('feed msix uri not recorded (not https): ' + $fi.uri) } }
        } catch { Log ('feed not readable at install time (' + $feedUri + '): ' + $_.Exception.Message) }
    } else { Log 'no App Installer feed known: repair can only use a recorded msix uri' }
    if ($msix -and -not (Test-AllowedUri $msix)) { Finish 'refused' 2 ('install refused: msix uri is not https (or http to loopback): ' + $msix) }

    # expected identity: what is installed now (this run is started by the packaged app), else what was recorded
    $name = $(if ($prev -and $prev.name) { [string]$prev.name } else { '' })
    $publisher = $(if ($prev -and $prev.publisher) { [string]$prev.publisher } else { '' })
    $pk = @(Get-Family $Pfn) | Select-Object -First 1
    if ($pk) { $name = [string]$pk.Name; $publisher = [string]$pk.Publisher }
    if (-not $name -or -not $publisher) { Log 'WARNING: package identity (Name, Publisher) unknown; the repair will refuse every source until a launch of the installed app records it' }

    $taskName = $Pfn + '-Repair'
    try { Register-GuardTask $Pfn (Get-RetryLocal) } catch { Finish 'misconfigured' 2 ('task registration failed: ' + $_.Exception.Message) }
    $cfg = [ordered]@{ guardVersion = $GuardVersion; pfn = $Pfn; name = $name; publisher = $publisher; feed = $feedUri; msix = $msix; task = $taskName; dir = $Dir; installedUtc = (Get-Date).ToUniversalTime().ToString('o') }
    [IO.File]::WriteAllText($ConfigPath, ($cfg | ConvertTo-Json), (New-Object Text.UTF8Encoding($false)))
    try { Remove-Item -LiteralPath $AttemptPath -Force -ErrorAction SilentlyContinue } catch { }
    Log ('task ' + $taskName + ' registered; identity=' + $name + ' / ' + $publisher + '; feed=' + $(if ($feedUri) { $feedUri } else { '(none)' }) + ' msix=' + $(if ($msix) { $msix } else { '(none)' }))
    Write-Result 'installed' 0 ('task ' + $taskName + ' registered')
    exit 0
}

# ================================================================== repair (task action)
trap {
    Log ('UNHANDLED ERROR: ' + $_.Exception.Message + ' at line ' + $_.InvocationInfo.ScriptLineNumber)
    Write-Result 'error' 4 $_.Exception.Message
    if ($script:Ctx) { try { Set-Retry $script:Ctx 'unhandled error' } catch { } }
    exit 4
}
$cfg = Read-Config
if (-not $cfg -or -not $cfg.pfn) { Finish 'misconfigured' 2 'guard.json missing or unreadable; nothing to do' }
$family = [string]$cfg.pfn
$feedUri = [string]$cfg.feed
$msix = [string]$cfg.msix
if ($family -notmatch $PfnPattern) { Finish 'refused' 2 ('guard.json pfn is not a package family name: ' + $family) }
if ([string]$cfg.task -ne ($family + '-Repair')) { Finish 'refused' 2 ('guard.json task ' + [string]$cfg.task + ' is not ' + $family + '-Repair') }
if ((Split-Path -Leaf $Dir) -ne 'repair') { Finish 'refused' 2 ('script folder is not named repair: ' + $Dir) }
if ($feedUri -and -not (Test-AllowedUri $feedUri)) { Finish 'refused' 2 ('guard.json feed uri is not https (or http to loopback): ' + $feedUri) }
if ($msix -and -not (Test-AllowedUri $msix)) { Finish 'refused' 2 ('guard.json msix uri is not https (or http to loopback): ' + $msix) }
$script:Ctx = $family
$mutex = [Threading.Mutex]::new($false, ('Local\' + $family + '-Repair'))
if (-not $mutex.WaitOne(0)) { Log 'another repair run is active; exiting (last-result.json left to that run)'; exit 0 }
$tmpMsix = ''
try {
    $state = Get-StateText $family
    if ($state -eq 'absent') {
        Start-Sleep -Seconds 10   # an update or reinstall can briefly show no package; require absence twice
        $state = Get-StateText $family
    }
    if ($state -eq 'absent') {
        # uninstalled: remove the repair folder (never its parent: that holds user data), then the task itself (last: it ends this run)
        $note = Join-Path $env:TEMP ($family + '-repair-cleanup.log')
        try { Add-Content -LiteralPath $note -Value ((Get-Date).ToString('o') + ' package ' + $family + ' absent; removing task ' + $cfg.task + ' and ' + $Dir) } catch { }
        try { Remove-Item -LiteralPath $Dir -Recurse -Force -ErrorAction Stop } catch { try { Add-Content -LiteralPath $note -Value ('folder removal failed: ' + $_.Exception.Message) } catch { } }
        try { Unregister-ScheduledTask -TaskName ([string]$cfg.task) -Confirm:$false -ErrorAction Stop; Add-Content -LiteralPath $note -Value 'task unregistered' } catch { try { Add-Content -LiteralPath $note -Value ('task removal failed: ' + $_.Exception.Message) } catch { } }
        exit 0
    }
    if (Test-Healthy $family) { Clear-Retry $family; Finish 'healthy' 0 ('package healthy: ' + $state) }
    Log ('package NOT healthy: ' + $state + '; letting the deployment service settle')
    Start-Sleep -Seconds 8
    if (Test-Healthy $family) { Clear-Retry $family; Finish 'healthy' 0 ('package recovered by itself: ' + (Get-StateText $family)) }

    if (-not $feedUri -and -not $msix) { Finish 'no-source' 3 'FAILED: no App Installer feed and no msix uri recorded; cannot repair. Reinstall from the original source or use Settings > Apps > Advanced options > Repair.' }
    # expected identity: recorded at install; fall back to the (broken) installed registration, which still reports Name and Publisher
    $expect = [pscustomobject]@{ name = [string]$cfg.name; publisher = [string]$cfg.publisher }
    if (-not $expect.name -or -not $expect.publisher) {
        $pk = @(Get-Family $family) | Select-Object -First 1
        if ($pk) { $expect.name = [string]$pk.Name; $expect.publisher = [string]$pk.Publisher }
    }
    if (-not $expect.name -or -not $expect.publisher) {
        Log 'FAILED: expected package identity unknown; refusing to install anything'
        Set-Retry $family 'identity unknown'
        Write-Result 'failed' 1 'expected package identity unknown'
        exit 1
    }
    $why = @()
    $feedOk = $false
    if ($feedUri) {
        $fi = $null
        try { $fi = Get-FeedInfo $feedUri } catch { Log ('feed unreachable, skipping -AppInstallerFile: ' + $feedUri + ' : ' + $_.Exception.Message); $why += ('feed unreachable: ' + $_.Exception.Message) }
        if ($fi) {
            $bad = Test-Identity $expect $fi.name $fi.publisher
            if (-not $bad -and $fi.uri -and -not (Test-AllowedUri $fi.uri)) { $bad = 'feed msix uri is not https (or http to loopback): ' + $fi.uri }
            if ($bad) { Log ('REFUSED feed ' + $feedUri + ': ' + $bad); $why += ('feed refused: ' + $bad) }
            else {
                $feedOk = $true
                if ($fi.uri) { $msix = $fi.uri }   # newer than the one recorded
                Log ('Add-AppxPackage -AppInstallerFile ' + $feedUri + ' (declares ' + $fi.version + ')')
                try { Add-AppxPackage -AppInstallerFile $feedUri -ErrorAction Stop } catch { Log ('-AppInstallerFile failed: ' + $_.Exception.Message + ' | ' + $_.FullyQualifiedErrorId); $why += ('-AppInstallerFile failed: ' + $_.Exception.Message) }
                if (Test-Healthy $family) {
                    $now = @(Get-Family $family) | Select-Object -First 1
                    $bad = Test-Identity $expect ([string]$now.Name) ([string]$now.Publisher)
                    if ($bad) { Log ('FAILED: identity mismatch after repair via feed: ' + $bad); $why += ('identity after repair: ' + $bad) }
                    else { Clear-Retry $family; Finish 'repaired' 0 ('REPAIRED via feed: ' + (Get-StateText $family)) }
                }
            }
        }
    }
    if ($msix) {
        $tmpMsix = Join-Path $env:TEMP ($family + '-repair-' + [guid]::NewGuid().ToString('N') + '.msix')
        $got = $false
        try { Log ('downloading ' + $msix); Save-Msix $msix $tmpMsix; $got = $true } catch { Log ('msix unreachable, skipping -Path: ' + $msix + ' : ' + $_.Exception.Message); $why += ('msix unreachable: ' + $_.Exception.Message) }
        if ($got) {
            $mi = $null
            try { $mi = Get-MsixIdentity $tmpMsix } catch { Log ('REFUSED msix: corrupt or unreadable package: ' + $_.Exception.Message); $why += ('msix corrupt: ' + $_.Exception.Message) }
            if ($mi) {
                $bad = Test-Identity $expect $mi.name $mi.publisher
                if ($bad) { Log ('REFUSED msix ' + $msix + ': ' + $bad); $why += ('msix refused: ' + $bad) }
                else {
                    Log ('Add-AppxPackage -Path ' + $msix + ' (' + $mi.kind + ' ' + $mi.version + ')')
                    try { Add-AppxPackage -Path $tmpMsix -ErrorAction Stop } catch { Log ('-Path failed: ' + $_.Exception.Message + ' | ' + $_.FullyQualifiedErrorId); $why += ('-Path failed: ' + $_.Exception.Message) }
                    if (Test-Healthy $family) {
                        $now = @(Get-Family $family) | Select-Object -First 1
                        $bad = Test-Identity $expect ([string]$now.Name) ([string]$now.Publisher)
                        if ($bad) { Log ('FAILED: identity mismatch after repair via msix: ' + $bad); $why += ('identity after repair: ' + $bad) }
                        else { Clear-Retry $family; Finish 'repaired' 0 ('REPAIRED via msix uri: ' + (Get-StateText $family)) }
                    }
                }
            }
        }
    } else { $why += 'no msix uri recorded' }
    $detail = 'FAILED: package still ' + (Get-StateText $family) + '. Reasons: ' + ($why -join ' | ')
    Set-Retry $family 'repair failed'
    Write-Result 'failed' 1 $detail
    Log ($detail + '. A retry is scheduled (see retry.json); manual: Settings > Apps > Advanced options > Repair.')
    exit 1
} finally {
    if ($tmpMsix) { try { Remove-Item -LiteralPath $tmpMsix -Force -ErrorAction SilentlyContinue } catch { } }
    try { $mutex.ReleaseMutex() } catch { }
}
