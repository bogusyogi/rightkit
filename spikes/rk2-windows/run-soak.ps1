# RK2 Spike B long soak: fixture rAF rate (paint.log) and capture arrival rate per second over a long run.
# Capture: 60 fps request (pool 2, no capture-side cap), --readback none (arrival timing only, no GPU copy, no PNG).
# Mode off      = fixture moved to (-10000,-10000), outside every monitor.
# Mode occluded = fixture on-screen at (100,100), covered by a topmost animating marker at (80,80) 860x560.
# Optional env RK2_BROWSER_ARGS is passed to the fixture (WebView2 AdditionalBrowserArguments) for the throttling retry.
param(
    [ValidateSet('off', 'occluded')][string]$Mode = 'off',
    [int]$Seconds = 180,
    [string]$Tag = ''
)

$ErrorActionPreference = 'Stop'
$spike = Split-Path -Parent $MyInvocation.MyCommand.Path
$exe = Join-Path $spike 'bin\Release\net10.0-windows10.0.22621.0\rk2.exe'
$scratch = 'C:\Users\adrds\AppData\Local\Temp\claude\D--Claude\022ffa73-ccba-4bbd-9028-0cd5875863a9\scratchpad\rk2'
$work = Join-Path $scratch ("soak-$Mode-" + (Get-Date -Format 'yyyyMMdd-HHmmss') + $Tag)
New-Item -ItemType Directory -Path $work -Force | Out-Null

function Start-Gui([string[]]$a) { return Start-Process -FilePath $exe -ArgumentList $a -PassThru }
function Win([string]$hwnd, [string]$op, [string]$x = '', [string]$y = '') {
    $a = @('window', '--hwnd', $hwnd, '--op', $op)
    if ($x -ne '') { $a += @('--x', $x, '--y', $y) }
    $p = Start-Process -FilePath $exe -ArgumentList $a -Wait -PassThru
    if ($p.ExitCode -ne 0) { throw "window op $op failed" }
}

$tdir = Join-Path $work 'target'
New-Item -ItemType Directory -Path $tdir -Force | Out-Null
$control = Join-Path $tdir 'control.txt'
$tproc = Start-Gui @('target', '--hwnd-file', (Join-Path $tdir 'hwnd.txt'), '--ready-file', (Join-Path $tdir 'ready.txt'), '--control', $control, '--x', '100', '--y', '100', '--w', '800', '--h', '520')
$t = 0
while (-not (Test-Path (Join-Path $tdir 'ready.txt')) -and $t -lt 120) { Start-Sleep -Milliseconds 500; $t++ }
$ready = (Get-Content (Join-Path $tdir 'ready.txt') -Raw).Trim()
$hwnd = (Get-Content (Join-Path $tdir 'hwnd.txt') -Raw).Trim()
if ($ready -ne 'ready success=True') { throw "target not ready: $ready" }
Start-Sleep -Seconds 3

$mproc = $null
if ($Mode -eq 'off') {
    Win $hwnd 'move' '-10000' '-10000'
} else {
    $mctl = Join-Path $work 'marker-control.txt'
    $mproc = Start-Gui @('marker', '--control', $mctl, '--hwnd-file', (Join-Path $work 'marker-hwnd.txt'), '--x', '80', '--y', '80', '--w', '860', '--h', '560')
    Start-Sleep -Seconds 2
}
Start-Sleep -Seconds 1

$capDir = Join-Path $work 'capture'
New-Item -ItemType Directory -Path $capDir -Force | Out-Null
$startUtc = (Get-Date).ToUniversalTime()
$cap = Start-Process -FilePath $exe -ArgumentList @('capture', '--hwnd', $hwnd, '--seconds', "$Seconds", '--dir', $capDir, '--label', "soak-$Mode", '--fps', '60', '--readback', 'none', '--pool', '2') -Wait -PassThru
$endUtc = (Get-Date).ToUniversalTime()

# stop the occluding marker and the fixture
if ($mproc) {
    Set-Content (Join-Path $work 'marker-control.txt') 'exit' -Encoding ASCII
    Start-Sleep -Seconds 1
    if (-not $mproc.HasExited) { $mproc.CloseMainWindow() | Out-Null }
}
Set-Content $control 'exit' -Encoding ASCII
Start-Sleep -Seconds 2
if (-not $tproc.HasExited) { $tproc.CloseMainWindow() | Out-Null }

# ---- per-second series ----
$paintFile = Join-Path $tdir 'paint.log'
$paint = @()
foreach ($line in (Get-Content $paintFile)) {
    $p = $line.Split(' ')
    if ($p.Count -ge 2) { $paint += [pscustomobject]@{ t = [DateTimeOffset]::Parse($p[0]).UtcDateTime; n = [double]$p[1] } }
}
$durSec = [int][Math]::Floor(($endUtc - $startUtc).TotalSeconds)
# rAF rate per second bucket: delta rAF count between consecutive paint samples inside each 1 s bucket
$rafRate = New-Object double[] ($durSec + 1)
$rafHas = New-Object bool[] ($durSec + 1)
for ($i = 1; $i -lt $paint.Count; $i++) {
    $a = $paint[$i - 1]; $b = $paint[$i]
    $dt = ($b.t - $a.t).TotalSeconds
    if ($dt -le 0) { continue }
    $bucket = [int][Math]::Floor(($b.t - $startUtc).TotalSeconds)
    if ($bucket -lt 0 -or $bucket -gt $durSec) { continue }
    $rafRate[$bucket] = [Math]::Round(($b.n - $a.n) / $dt, 2)
    $rafHas[$bucket] = $true
}
# arrival count per second from the capture's arrivals.json (ms since StartCapture)
$arr = @(Get-Content (Join-Path $capDir 'arrivals.json') -Raw | ConvertFrom-Json)
$arrCount = New-Object int[] ($durSec + 1)
foreach ($ms in $arr) {
    $bucket = [int][Math]::Floor($ms / 1000.0)
    if ($bucket -ge 0 -and $bucket -le $durSec) { $arrCount[$bucket]++ }
}

function Summarize([double[]]$series, [bool[]]$has, [string]$name) {
    $vals = @()
    for ($i = 0; $i -lt $series.Count; $i++) { if ($has[$i]) { $vals += [pscustomobject]@{ s = $i; v = $series[$i] } } }
    if ($vals.Count -eq 0) { return [pscustomobject]@{ series = $name; note = 'no samples' } }
    $below = @($vals | Where-Object { $_.v -lt 55 })
    $firstDrop = $(if ($below.Count) { $below[0].s } else { $null })
    $recovered = $false
    if ($firstDrop -ne $null) {
        $after = @($vals | Where-Object { $_.s -gt $firstDrop -and $_.v -ge 55 })
        $recovered = ($after.Count -gt 0)
    }
    return [pscustomobject]@{
        series = $name
        seconds = $vals.Count
        min = ($vals | Measure-Object v -Minimum).Minimum
        mean = [Math]::Round(($vals | Measure-Object v -Average).Average, 2)
        max = ($vals | Measure-Object v -Maximum).Maximum
        secondsBelow55 = $below.Count
        firstSecondBelow55 = $firstDrop
        anyRecoveryAfterDrop = $recovered
    }
}

$rafSeries = Summarize $rafRate $rafHas 'fixture rAF per s (paint.log)'
$arrHas = New-Object bool[] ($durSec + 1)
for ($i = 0; $i -le $durSec; $i++) { $arrHas[$i] = $true }
$arrDouble = New-Object double[] ($durSec + 1)
for ($i = 0; $i -le $durSec; $i++) { $arrDouble[$i] = $arrCount[$i] }
$arrSeries = Summarize $arrDouble $arrHas 'capture arrivals per s (no readback)'

$rafBelow = @(for ($i = 0; $i -le $durSec; $i++) { if ($rafHas[$i] -and $rafRate[$i] -lt 55) { "$i=$($rafRate[$i])" } })
$result = [pscustomobject]@{
    mode = $Mode; seconds = $Seconds; tag = $Tag; workDir = $work
    startUtc = $startUtc.ToString('o'); endUtc = $endUtc.ToString('o')
    captureExit = $cap.ExitCode
    browserArgs = $env:RK2_BROWSER_ARGS
    paintSamples = $paint.Count
    arrivals = $arr.Count
    rafSummary = $rafSeries
    arrivalSummary = $arrSeries
    rafSecondsBelow55 = $rafBelow
    rafSeriesFirst60 = @(for ($i = 0; $i -lt [Math]::Min(60, $durSec); $i++) { if ($rafHas[$i]) { $rafRate[$i] } else { $null } })
    rafSeriesLast30 = @(for ($i = [Math]::Max(0, $durSec - 29); $i -le $durSec; $i++) { if ($rafHas[$i]) { $rafRate[$i] } else { $null } })
}
$result | ConvertTo-Json -Depth 6 | Set-Content (Join-Path $work 'soak-result.json') -Encoding UTF8
Write-Output "DONE $work"
