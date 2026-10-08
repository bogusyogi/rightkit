# RK2 Spike B soak analysis: per-second fixture rAF rate (paint.log) and per-second capture arrival count
# (capture\arrivals.json), summarised. Writes soak-result.json into the soak work directory.
# Usage: analyze-soak.ps1 -WorkDir <soak dir> [-Seconds 180] [-StartUtc <ISO>] [-EndUtc <ISO>]
# If StartUtc/EndUtc are not given, the capture window is taken from capture\summary.json (LastWriteTime = end) minus Seconds.
param(
    [Parameter(Mandatory = $true)][string]$WorkDir,
    [int]$Seconds = 180,
    [string]$StartUtc = '',
    [string]$EndUtc = ''
)

$ErrorActionPreference = 'Stop'
$capDir = Join-Path $WorkDir 'capture'
$tdir = Join-Path $WorkDir 'target'

if ($StartUtc -and $EndUtc) {
    $startU = [DateTimeOffset]::Parse($StartUtc).UtcDateTime
    $endU = [DateTimeOffset]::Parse($EndUtc).UtcDateTime
} else {
    $endU = (Get-Item (Join-Path $capDir 'summary.json')).LastWriteTimeUtc
    $startU = $endU.AddSeconds(-$Seconds)
}
$durSec = [int][Math]::Floor(($endU - $startU).TotalSeconds)

# paint samples (UTC timestamp, rAF count)
$paint = @()
foreach ($line in (Get-Content (Join-Path $tdir 'paint.log'))) {
    $p = $line.Split(' ')
    if ($p.Count -ge 2) { $paint += [pscustomobject]@{ t = [DateTimeOffset]::Parse($p[0]).UtcDateTime; n = [double]$p[1] } }
}

# rAF rate per 1 s bucket: rAF delta / elapsed between consecutive samples, assigned to the bucket of the later sample
$rafRate = New-Object double[] ($durSec + 1)
$rafHas = New-Object bool[] ($durSec + 1)
for ($i = 1; $i -lt $paint.Count; $i++) {
    $a = $paint[$i - 1]; $b = $paint[$i]
    $dt = ($b.t - $a.t).TotalSeconds
    if ($dt -le 0) { continue }
    $bucket = [int][Math]::Floor(($b.t - $startU).TotalSeconds)
    if ($bucket -lt 0 -or $bucket -gt $durSec) { continue }
    $rafRate[$bucket] = [Math]::Round(($b.n - $a.n) / $dt, 2)
    $rafHas[$bucket] = $true
}

# arrivals (ms since StartCapture), parsed as numbers from the JSON array
$raw = Get-Content (Join-Path $capDir 'arrivals.json') -Raw
$arrMs = @([regex]::Matches($raw, '-?\d+(\.\d+)?([eE][-+]?\d+)?') | ForEach-Object { [double]$_.Value })
$arrCount = New-Object double[] ($durSec + 1)
$arrHas = New-Object bool[] ($durSec + 1)
foreach ($ms in $arrMs) {
    $bucket = [int][Math]::Floor($ms / 1000.0)
    if ($bucket -ge 0 -and $bucket -le $durSec) { $arrCount[$bucket]++ }
}
for ($i = 0; $i -le $durSec; $i++) { $arrHas[$i] = $true }

function Summarize([double[]]$series, [bool[]]$has, [string]$name) {
    $vals = @()
    for ($i = 0; $i -lt $series.Count; $i++) { if ($has[$i]) { $vals += [pscustomobject]@{ s = $i; v = $series[$i] } } }
    if ($vals.Count -eq 0) { return [pscustomobject]@{ series = $name; note = 'no samples' } }
    $below = @($vals | Where-Object { $_.v -lt 55 })
    $firstDrop = $null
    if ($below.Count) { $firstDrop = $below[0].s }
    $recovered = $false
    if ($null -ne $firstDrop) {
        $after = @($vals | Where-Object { $_.s -gt $firstDrop -and $_.v -ge 55 })
        $recovered = ($after.Count -gt 0)
    }
    $m = $vals | Measure-Object -Property v -Minimum -Maximum -Average
    return [pscustomobject]@{
        series = $name
        seconds = $vals.Count
        min = $m.Minimum
        mean = [Math]::Round($m.Average, 2)
        max = $m.Maximum
        secondsBelow55 = $below.Count
        firstSecondBelow55 = $firstDrop
        anyRecoveryAfterDrop = $recovered
    }
}

$rafSummary = Summarize $rafRate $rafHas 'fixture rAF per s (paint.log)'
$arrSummary = Summarize $arrCount $arrHas 'capture arrivals per s (no readback)'

$rafBelow = @()
for ($i = 0; $i -le $durSec; $i++) { if ($rafHas[$i] -and $rafRate[$i] -lt 55) { $rafBelow += "$i=$($rafRate[$i])" } }
$arrBelow = @()
for ($i = 0; $i -le $durSec; $i++) { if ($arrCount[$i] -lt 55) { $arrBelow += "$i=$($arrCount[$i])" } }

$result = [pscustomobject]@{
    workDir = $WorkDir
    captureWindowUtc = [pscustomobject]@{ start = $startU.ToString('o'); end = $endU.ToString('o'); seconds = $durSec }
    paintSamples = $paint.Count
    arrivals = $arrMs.Count
    browserArgs = $env:RK2_BROWSER_ARGS
    rafSummary = $rafSummary
    arrivalSummary = $arrSummary
    rafSecondsBelow55 = $rafBelow
    arrivalSecondsBelow55 = $arrBelow
    rafSeriesFirst60 = @(for ($i = 0; $i -lt [Math]::Min(60, $durSec); $i++) { if ($rafHas[$i]) { $rafRate[$i] } else { $null } })
    rafSeriesLast30 = @(for ($i = [Math]::Max(0, $durSec - 29); $i -le $durSec; $i++) { if ($rafHas[$i]) { $rafRate[$i] } else { $null } })
    arrivalSeriesFirst60 = @(for ($i = 0; $i -lt [Math]::Min(60, $durSec); $i++) { $arrCount[$i] })
    arrivalSeriesLast30 = @(for ($i = [Math]::Max(0, $durSec - 29); $i -le $durSec; $i++) { $arrCount[$i] })
}
$result | ConvertTo-Json -Depth 6 | Set-Content (Join-Path $WorkDir 'soak-result.json') -Encoding UTF8
$result | ConvertTo-Json -Depth 4
