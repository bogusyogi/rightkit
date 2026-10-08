# RK2 Spike B follow-up analysis: per-run delivered-frame intervals, dropped-counter gaps, fixture paint rate,
# and placement evidence. Reads a run-fps.ps1 work directory; writes fps-results.json next to this script.
param([Parameter(Mandatory = $true)][string]$WorkDir)

$ErrorActionPreference = 'Stop'
$spike = Split-Path -Parent $MyInvocation.MyCommand.Path

# fixture paint samples (UTC, rAF count), one per second
$paintFile = Join-Path $WorkDir 'target\paint.log'
$paint = @()
if (Test-Path $paintFile) {
    foreach ($line in (Get-Content $paintFile)) {
        $p = $line.Split(' ')
        if ($p.Count -ge 2) { $paint += [pscustomobject]@{ t = [DateTimeOffset]::Parse($p[0]).UtcDateTime; n = [double]$p[1] } }
    }
}

# fixture paint rate (rAF delta per second) between two UTC instants
function Get-PaintRate([DateTime]$from, [DateTime]$to) {
    $inside = @($paint | Where-Object { $_.t -ge $from -and $_.t -le $to })
    if ($inside.Count -lt 2) { return $null }
    $dn = $inside[$inside.Count - 1].n - $inside[0].n
    $dt = ($inside[$inside.Count - 1].t - $inside[0].t).TotalSeconds
    if ($dt -le 0) { return $null }
    return [Math]::Round($dn / $dt, 2)
}

$rows = New-Object System.Collections.Generic.List[object]
foreach ($dir in (Get-ChildItem $WorkDir -Directory | Where-Object { $_.Name -match '^(S|R)\d\d-' } | Sort-Object Name)) {
    $sum = Get-Content (Join-Path $dir.FullName 'summary.json') -Raw | ConvertFrom-Json
    $run = Get-Content (Join-Path $dir.FullName 'run.json') -Raw | ConvertFrom-Json
    $runRaw = Get-Content (Join-Path $dir.FullName 'run.json') -Raw
    $startUtc = [DateTimeOffset]::Parse([regex]::Match($runRaw, '"startUtc":\s*"([^"]+)"').Groups[1].Value).UtcDateTime
    $endUtc = [DateTimeOffset]::Parse([regex]::Match($runRaw, '"endUtc":\s*"([^"]+)"').Groups[1].Value).UtcDateTime
    $step = if ($run.content -eq 30) { 2 } else { 1 }

    # dropped-counter gaps from OCR counters of delivered frames (full readback only)
    $gaps = @(); $missed = 0; $ocrCount = 0
    $ocrFile = Join-Path $dir.FullName 'ocr.json'
    if (Test-Path $ocrFile) {
        $ocr = Get-Content $ocrFile -Raw | ConvertFrom-Json
        $vals = @()
        foreach ($r in @($ocr.rows)) { if ("$($r.counter)" -match '^\d+$') { $vals += [int]$r.counter } }
        $ocrCount = $vals.Count
        for ($i = 1; $i -lt $vals.Count; $i++) {
            $d = $vals[$i] - $vals[$i - 1]
            $expected = $step
            $m = [Math]::Max(0, [int]([Math]::Round($d / $expected)) - 1)
            if ($m -gt 0) { $gaps += $m; $missed += $m }
        }
    }

    $arr = $sum.PSObject.Properties | Where-Object { $_.Name -like 'arrival.*' }
    $a = @{}; foreach ($p in $arr) { $a[$p.Name.Substring(8)] = $p.Value }

    $shotPath = Join-Path $dir.FullName 'screen.json'
    $placement = $null
    if (Test-Path $shotPath) { $placement = Get-Content $shotPath -Raw | ConvertFrom-Json }
    $screenWgc = $null
    $sfPath = Join-Path $dir.FullName 'screen-wgc\frames.json'
    if (Test-Path $sfPath) {
        $sj = @(Get-Content $sfPath -Raw | ConvertFrom-Json)
        if ($sj.Count -gt 0) { $screenWgc = [pscustomobject]@{ w = $sj[0].W; h = $sj[0].H; meanLum = $sj[0].meanLum; file = "screen-wgc\$($sj[0].file)" } }
    }

    $rows.Add([pscustomobject]@{
        run = $dir.Name
        state = $run.state
        readback = $run.readback
        contentRateHz = $run.content
        windowsBuild = $sum.windowsBuild
        arrivals = $a['count']
        arrivalIntervals = $a['intervalCount']
        meanMs = $a['meanMs']; p50Ms = $a['p50Ms']; p95Ms = $a['p95Ms']; maxMs = $a['maxMs']; minMs = $a['minMs']
        gapsOver1p5xMedian = $a['gapsOver1.5xMedian']
        deliveredFps = $a['deliveredFps']
        framesReadBack = $sum.framesReadBack
        ocrCounterFrames = $ocrCount
        droppedCounterGapEvents = $gaps.Count
        droppedCounterMissedPaints = $missed
        droppedCounterMaxGap = $(if ($gaps.Count) { ($gaps | Measure-Object -Maximum).Maximum } else { 0 })
        fixturePaintRateHz = (Get-PaintRate $startUtc $endUtc)
        placement = $(if ($placement) { [pscustomobject]@{
            fixtureRect = $placement.fixtureRect
            fixtureIsWindowVisible = $placement.fixtureIsWindowVisible
            fixtureIsIconic = $placement.fixtureIsIconic
            fixtureIntersectsAnyMonitor = $placement.fixtureIntersectsAnyMonitor
            fixtureIntersectsVirtualScreen = $placement.fixtureIntersectsVirtualScreen
            virtualScreen = $placement.virtualScreen
            screenMeanLuminance = $placement.screenMeanLuminance
            screenshotError = $placement.screenshotError
        } } else { $null })
        screenWgc = $screenWgc
        error = $sum.error
        handlerError = $sum.lastHandlerError
    })
}

$hostInfo = $null
if (Test-Path (Join-Path $WorkDir 'host.json')) { $hostInfo = Get-Content (Join-Path $WorkDir 'host.json') -Raw | ConvertFrom-Json }
$out = [pscustomobject]@{ workDir = $WorkDir; host = $hostInfo; paintSamples = $paint.Count; runs = $rows }
$outPath = Join-Path $spike 'fps-results.json'
$out | ConvertTo-Json -Depth 8 | Set-Content -Path $outPath -Encoding UTF8
$rows | Format-Table run, readback, contentRateHz, arrivals, deliveredFps, meanMs, p50Ms, p95Ms, maxMs, gapsOver1p5xMedian, fixturePaintRateHz, droppedCounterMissedPaints -AutoSize | Out-String -Width 300
$rows | ForEach-Object { if ($_.placement) { "$($_.run): rect=$($_.placement.fixtureRect) vis=$($_.placement.fixtureIsWindowVisible) iconic=$($_.placement.fixtureIsIconic) onMonitor=$($_.placement.fixtureIntersectsAnyMonitor) screenLum=$($_.placement.screenMeanLuminance) shotErr=$($_.placement.screenshotError)" } } | Out-String -Width 300
"wrote $outPath"
