# RK2 Spike B analysis: builds the per-state decision data from one orchestrator run's raw files
# (summary.json, frames.json, ocr.json per state directory). Writes results.json next to this script.
param(
    [Parameter(Mandatory = $true)][string]$RunDir,
    [int]$ChangeMs = 3000,
    [int]$RestoreMs = 5500
)

$ErrorActionPreference = 'Stop'
$spike = Split-Path -Parent $MyInvocation.MyCommand.Path

# Segment statistics over frames with tMs in [from, to).
function Get-Segment([string]$dir, [long]$from, [long]$to) {
    $frames = @(Get-Content (Join-Path $dir 'frames.json') -Raw | ConvertFrom-Json)
    $ocr = Get-Content (Join-Path $dir 'ocr.json') -Raw | ConvertFrom-Json
    $byFile = @{}
    foreach ($r in @($ocr.rows)) { $byFile[$r.file] = $r }
    $sel = @($frames | Where-Object { $_.tMs -ge $from -and $_.tMs -lt $to })
    $n = $sel.Count
    $distinct = @($sel | ForEach-Object { $_.hash } | Select-Object -Unique).Count
    $ident = 0
    for ($i = 1; $i -lt $n; $i++) { if ($sel[$i].hash -eq $sel[$i - 1].hash) { $ident++ } }
    $black = @($sel | Where-Object { $_.meanLum -lt 8 }).Count
    $step = 0; $cparsed = 0; $leak = 0; $red = 0.0
    $counters = New-Object System.Collections.Generic.List[int]
    foreach ($f in $sel) {
        $o = $byFile[$f.file]
        if ($o) {
            if ($o.step) { $step++ }
            $cv = "$($o.counter)"
            if ($cv -match '^\d+$') { $cparsed++; $counters.Add([int]$cv) }
            if ($o.leakTokens) { $leak++ }
        }
        $red = [Math]::Max([double]$red, [double]$f.redFraction)
    }
    $incr = 0
    for ($i = 1; $i -lt $counters.Count; $i++) { if ($counters[$i] -gt $counters[$i - 1]) { $incr++ } }
    $fps = 0.0
    if ($n -gt 1 -and ($sel[$n - 1].tMs - $sel[0].tMs) -gt 0) { $fps = [Math]::Round(($n - 1) / (($sel[$n - 1].tMs - $sel[0].tMs) / 1000.0), 1) }
    $range = ''
    if ($counters.Count -gt 0) { $range = "$($counters[0])..$($counters[$counters.Count - 1])" }
    return [pscustomobject]@{
        frames = $n; fps = $fps; distinct = $distinct; identicalPairs = $ident; black = $black
        stepReadable = "$step/$n"; counterParsed = "$cparsed/$n"
        counterIncr = "$incr/$([Math]::Max($counters.Count - 1, 0))"; counterRange = $range
        leakOcr = $leak; maxRed = [Math]::Round([double]$red, 4)
        firstTMs = $(if ($n) { $sel[0].tMs } else { $null }); lastTMs = $(if ($n) { $sel[$n - 1].tMs } else { $null })
    }
}

$rows = New-Object System.Collections.Generic.List[object]
$states = @(Get-ChildItem $RunDir -Directory | Where-Object { $_.Name -match '^[A-J]-' } | Sort-Object Name)
foreach ($st in $states) {
    $s = Get-Content (Join-Path $st.FullName 'summary.json') -Raw | ConvertFrom-Json
    $name = $st.Name
    $segs = @()
    if ($name -like 'G-*') {
        $segs += @{ n = 'before-minimize'; f = 0; t = $ChangeMs }
        $segs += @{ n = 'minimized'; f = ($ChangeMs + 700); t = $RestoreMs }
        $segs += @{ n = 'after-restore'; f = ($RestoreMs + 1000); t = 100000 }
    }
    elseif ($name -like 'A-*' -or $name -like 'H-*' -or $name -like 'I-*' -or $name -like 'J-*') {
        $segs += @{ n = 'all'; f = 0; t = 100000 }
    }
    else {
        $segs += @{ n = 'before-change'; f = 0; t = $ChangeMs }
        $segs += @{ n = 'after-change'; f = ($ChangeMs + 700); t = 100000 }
    }
    $actionsOk = @($s.actionLog | ForEach-Object { $_.ok }) -join ','
    foreach ($g in $segs) {
        $seg = Get-Segment $st.FullName $g.f $g.t
        $rows.Add([pscustomobject]@{
            state = $name; segment = $g.n
            frames = $seg.frames; fps = $seg.fps; distinct = $seg.distinct; identical = $seg.identicalPairs
            black = $seg.black; step = $seg.stepReadable; counter = $seg.counterParsed; incr = $seg.counterIncr
            counterRange = $seg.counterRange; leakOcr = $seg.leakOcr; maxRed = $seg.maxRed
            firstTMs = $seg.firstTMs; lastTMs = $seg.lastTMs
            windowVisibleBefore = $s.isWindowVisibleBefore; windowVisibleEnd = $s.isWindowVisibleEnd
            iconicEnd = $s.isIconicEnd; rectEnd = $s.windowRectEnd; onMonitorBefore = $s.windowIntersectsAnyMonitorBefore
            itemSize = "$($s.itemWidth)x$($s.itemHeight)"; actionsOk = $actionsOk
            borderFalse = $s.isBorderRequiredSetFalse; error = $s.error; nativeStep = $s.nativeStep; handlerError = $s.lastHandlerError
        })
    }
}

$out = [pscustomobject]@{
    windowsBuild = (Get-CimInstance Win32_OperatingSystem).BuildNumber
    runDir = $RunDir
    changeMs = $ChangeMs; restoreMs = $RestoreMs
    rows = $rows
}
$outPath = Join-Path $spike 'results.json'
$out | ConvertTo-Json -Depth 6 | Set-Content -Path $outPath -Encoding UTF8
$rows | Format-Table state, segment, frames, fps, distinct, identical, black, step, counter, incr, leakOcr, maxRed, firstTMs, lastTMs -AutoSize | Out-String -Width 260
$rows | Where-Object { $_.error } | Select-Object -Unique state, error, nativeStep | Format-Table -AutoSize | Out-String -Width 260
"wrote $outPath"
