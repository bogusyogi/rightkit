# RK2 Spike B follow-up: frame-rate attribution and placement evidence.
# Target fixture animates on requestAnimationFrame. Content rate 60 = repaint every rAF; content rate 30 = every
# second rAF (the capture-side MinUpdateInterval is not in the SDK metadata, so the 30 configuration is content-side).
# Each capture takes a full virtual-desktop screenshot 3 s in, with placement JSON for the fixture HWND.
param(
    [int]$Seconds = 8,
    [string]$Work = ''
)

$ErrorActionPreference = 'Stop'
$spike = Split-Path -Parent $MyInvocation.MyCommand.Path
$exe = Join-Path $spike 'bin\Release\net10.0-windows10.0.22621.0\rk2.exe'
$scratch = 'C:\Users\adrds\AppData\Local\Temp\claude\D--Claude\022ffa73-ccba-4bbd-9028-0cd5875863a9\scratchpad\rk2'
if (-not $Work) { $Work = Join-Path $scratch ('fps-' + (Get-Date -Format 'yyyyMMdd-HHmmss')) }
New-Item -ItemType Directory -Path $Work -Force | Out-Null

function Start-Gui([string[]]$a) { return Start-Process -FilePath $exe -ArgumentList $a -PassThru }
function Run-Quiet([string[]]$a) { return (Start-Process -FilePath $exe -ArgumentList $a -Wait -PassThru).ExitCode }
function Set-Control([string]$path, [string]$cmd) { Set-Content -Path $path -Value $cmd -Encoding ASCII; Start-Sleep -Milliseconds 800 }
function Win([string]$hwnd, [string]$op, [string]$x = '', [string]$y = '') {
    $a = @('window', '--hwnd', $hwnd, '--op', $op)
    if ($x -ne '') { $a += @('--x', $x, '--y', $y) }
    if ((Run-Quiet $a) -ne 0) { throw "window op $op failed" }
    Start-Sleep -Milliseconds 800
}

# ---- host record ----
$cv = Get-ItemProperty 'HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion'
$gpus = @(Get-CimInstance Win32_VideoController | ForEach-Object { [pscustomobject]@{ name = $_.Name; driver = $_.DriverVersion; curRefreshHz = $_.CurrentRefreshRate; curRes = "$($_.CurrentHorizontalResolution)x$($_.CurrentVerticalResolution)" } })
$host0 = [pscustomobject]@{
    computerName = $env:COMPUTERNAME
    productName = $cv.ProductName; displayVersion = $cv.DisplayVersion
    currentBuild = $cv.CurrentBuild; ubr = $cv.UBR; fullBuild = "$($cv.CurrentBuild).$($cv.UBR)"
    gpus = $gpus
}
$host0 | ConvertTo-Json -Depth 4 | Set-Content (Join-Path $Work 'host.json') -Encoding UTF8
Write-Output "host: $($env:COMPUTERNAME) build $($cv.CurrentBuild).$($cv.UBR)"

# ---- target ----
$tdir = Join-Path $Work 'target'
New-Item -ItemType Directory -Path $tdir -Force | Out-Null
$control = Join-Path $tdir 'control.txt'
$tproc = Start-Gui @('target', '--hwnd-file', (Join-Path $tdir 'hwnd.txt'), '--ready-file', (Join-Path $tdir 'ready.txt'), '--control', $control, '--x', '100', '--y', '100', '--w', '800', '--h', '520')
$t = 0
while (-not (Test-Path (Join-Path $tdir 'ready.txt')) -and $t -lt 120) { Start-Sleep -Milliseconds 500; $t++ }
$ready = (Get-Content (Join-Path $tdir 'ready.txt') -Raw).Trim()
$hwnd = (Get-Content (Join-Path $tdir 'hwnd.txt') -Raw).Trim()
Write-Output "target: $ready hwnd=$hwnd"
if ($ready -ne 'ready success=True') { throw "target not ready: $ready" }
Start-Sleep -Seconds 2

$mdir = Join-Path $Work 'marker'
New-Item -ItemType Directory -Path $mdir -Force | Out-Null

# One capture run. $state: visible | occluded | offscreen. $readback: full | map | none. $content: 30 | 60.
function Run-Cap([string]$name, [string]$state, [string]$readback, [int]$content) {
    Set-Control $control ('content' + $content)
    $dir = Join-Path $Work $name
    New-Item -ItemType Directory -Path $dir -Force | Out-Null
    $startUtc = (Get-Date).ToUniversalTime().ToString('o')
    $mp = $null
    if ($state -eq 'occluded') {
        $mctl = Join-Path $mdir ("control-$name.txt")
        $mp = Start-Gui @('marker', '--control', $mctl, '--hwnd-file', (Join-Path $mdir ("hwnd-$name.txt")), '--x', '80', '--y', '80', '--w', '860', '--h', '560')
        Start-Sleep -Seconds 2
    }
    if ($state -eq 'offscreen') { Win $hwnd 'move' '-10000' '-10000' }
    $cargs = @('capture', '--hwnd', $hwnd, '--seconds', "$Seconds", '--dir', $dir, '--label', $name, '--fps', "$content", '--readback', $readback, '--pool', '2')
    $cap = Start-Process -FilePath $exe -ArgumentList $cargs -PassThru
    # placement: full virtual-desktop screenshot at 3 s, placement JSON for the fixture HWND
    Start-Sleep -Seconds 3
    $shotRc = Run-Quiet @('shot', '--out', (Join-Path $dir 'screen.png'), '--hwnd', $hwnd)
    # full virtual-desktop frame via WGC CreateForMonitor (GDI grabs are black on this host); one frame saved
    $monRc = Run-Quiet @('capture', '--hwnd', $hwnd, '--monitor', '1', '--seconds', '2', '--dir', (Join-Path $dir 'screen-wgc'), '--label', 'screen', '--readback', 'full', '--maxsave', '1', '--pool', '2')
    $cap.WaitForExit()
    $endUtc = (Get-Date).ToUniversalTime().ToString('o')
    if ($mp) {
        Set-Control $mctl 'exit'
        Start-Sleep -Seconds 1
        if (-not $mp.HasExited) { $mp.CloseMainWindow() | Out-Null }
    }
    if ($state -eq 'offscreen') { Win $hwnd 'move' '100' '100' }
    if ($readback -eq 'full') { $null = Run-Quiet @('ocr', '--dir', $dir) }
    $meta = [pscustomobject]@{ name = $name; state = $state; readback = $readback; content = $content; startUtc = $startUtc; endUtc = $endUtc; shotExit = $shotRc; monitorCaptureExit = $monRc; captureExit = $cap.ExitCode }
    $meta | ConvertTo-Json | Set-Content (Join-Path $dir 'run.json') -Encoding UTF8
    Write-Output "[$name] state=$state readback=$readback content=$content captureExit=$($cap.ExitCode) shotExit=$shotRc"
}

# ---- stage runs first (visible), then the state runs at both content rates ----
Run-Cap 'S30-visible-none' 'visible' 'none' 30
Run-Cap 'S60-visible-none' 'visible' 'none' 60
Run-Cap 'S30-visible-map' 'visible' 'map' 30
Run-Cap 'S60-visible-map' 'visible' 'map' 60
Run-Cap 'R30-visible-full' 'visible' 'full' 30
Run-Cap 'R60-visible-full' 'visible' 'full' 60
Run-Cap 'R30-occluded-full' 'occluded' 'full' 30
Run-Cap 'R60-occluded-full' 'occluded' 'full' 60
Run-Cap 'R30-offscreen-full' 'offscreen' 'full' 30
Run-Cap 'R60-offscreen-full' 'offscreen' 'full' 60

Set-Control $control 'content60'
Set-Control $control 'exit'
Start-Sleep -Seconds 2
if (-not $tproc.HasExited) { $tproc.CloseMainWindow() | Out-Null }
Write-Output "DONE $Work"
