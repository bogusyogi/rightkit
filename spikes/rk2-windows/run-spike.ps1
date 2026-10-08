# RK2 Spike B orchestrator (timeline version).
# A WebView2 target window stays alive. For each visibility state, a capture session is started on the
# target HWND (Windows.Graphics.Capture, GraphicsCaptureItem for the HWND) while the target is visible,
# the state change is applied at ChangeMs inside the capture timeline, and frames are split into
# before/after segments. OCR (Windows.Media.Ocr) reads the step id and frame counter from every frame.
# Build first: C:\Users\adrds\.dotnet\dotnet.exe build -c Release
param(
    [int]$ChangeMs = 3000,
    [int]$TotalSeconds = 8,
    [string]$Work = ''
)

$ErrorActionPreference = 'Stop'
$spike = Split-Path -Parent $MyInvocation.MyCommand.Path
$exe = Join-Path $spike 'bin\Release\net10.0-windows10.0.22621.0\rk2.exe'
$scratch = 'C:\Users\adrds\AppData\Local\Temp\claude\D--Claude\022ffa73-ccba-4bbd-9028-0cd5875863a9\scratchpad\rk2'
if (-not $Work) { $Work = Join-Path $scratch ('run-' + (Get-Date -Format 'yyyyMMdd-HHmmss')) }
New-Item -ItemType Directory -Path $Work -Force | Out-Null
$samples = Join-Path $spike 'samples'
New-Item -ItemType Directory -Path $samples -Force | Out-Null

function Start-Gui([string[]]$a) { return Start-Process -FilePath $exe -ArgumentList $a -PassThru }
# rk2.exe is a WinExe (no console). Do NOT pass -WindowStyle Hidden here: a hidden STARTUPINFO made the
# child's ShowWindow(SW_SHOW) call ineffective in testing (SW_RESTORE and direct calls were not affected).
function Run-Quiet([string[]]$a) { return (Start-Process -FilePath $exe -ArgumentList $a -Wait -PassThru).ExitCode }
function Set-Control([string]$path, [string]$cmd) { Set-Content -Path $path -Value $cmd -Encoding ASCII; Start-Sleep -Milliseconds 800 }
function Win([string]$hwnd, [string]$op, [string]$x = '', [string]$y = '') {
    $a = @('window', '--hwnd', $hwnd, '--op', $op)
    if ($x -ne '') { $a += @('--x', $x, '--y', $y) }
    if ((Run-Quiet $a) -ne 0) { throw "window op $op failed" }
    Start-Sleep -Milliseconds 800
}

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
        leakOcr = $leak; maxRed = [Math]::Round($red, 4)
        firstTMs = $(if ($n) { $sel[0].tMs } else { $null }); lastTMs = $(if ($n) { $sel[$n - 1].tMs } else { $null })
    }
}

$results = New-Object System.Collections.Generic.List[object]

function Capture-Run([string]$name, [string]$hwnd, [string]$actions, [string]$control, [switch]$Occlude, [string]$mdir) {
    $dir = Join-Path $Work $name
    New-Item -ItemType Directory -Path $dir -Force | Out-Null
    $cargs = @('capture', '--hwnd', $hwnd, '--seconds', "$TotalSeconds", '--dir', $dir, '--label', $name)
    if ($control) { $cargs += @('--control', $control) }
    if ($actions) { $cargs += @('--actions', $actions) }
    $cap = Start-Process -FilePath $exe -ArgumentList $cargs -PassThru
    $m = $null
    if ($Occlude) {
        Start-Sleep -Milliseconds $ChangeMs
        $m = Start-Gui @('marker', '--control', (Join-Path $mdir 'control.txt'), '--hwnd-file', (Join-Path $mdir 'hwnd.txt'), '--x', '80', '--y', '80', '--w', '860', '--h', '560')
    }
    $cap.WaitForExit()
    if ($m) {
        Set-Control (Join-Path $mdir 'control.txt') 'exit'
        Start-Sleep -Seconds 1
        if (-not $m.HasExited) { $m.CloseMainWindow() | Out-Null }
    }
    $null = Run-Quiet @('ocr', '--dir', $dir)
    $s = Get-Content (Join-Path $dir 'summary.json') -Raw | ConvertFrom-Json
    $pngs = @(Get-ChildItem $dir -Filter 'f*.png' | Sort-Object Name)
    if ($pngs.Count -gt 0) { Copy-Item $pngs[$pngs.Count - 1].FullName (Join-Path $samples ($name + '-last.png')) -Force }
    return $s
}

function Add-Row([string]$name, $s, [long]$preTo, [long]$postFrom, [long]$tailFrom, [string]$notes) {
    $dir = Join-Path $Work $name
    if ($preTo -lt 0) { $preTo = 100000 }
    $pre = Get-Segment $dir 0 $preTo
    $post = $null; $tail = $null
    if ($postFrom -ge 0) { $post = Get-Segment $dir $postFrom 100000 }
    if ($tailFrom -ge 0) { $tail = Get-Segment $dir $tailFrom 100000 }
    $row = [pscustomobject]@{
        state = $name; notes = $notes
        windowRectBefore = $s.windowRectBefore; visibleBefore = $s.isWindowVisibleBefore; onMonitorBefore = $s.windowIntersectsAnyMonitorBefore
        visibleEnd = $s.isWindowVisibleEnd; iconicEnd = $s.isIconicEnd
        itemSize = "$($s.itemWidth)x$($s.itemHeight)"
        borderOffOk = $s.isBorderRequiredSetFalse
        actionLog = $s.actionLog
        error = $s.error; nativeStep = $s.nativeStep; handlerError = $s.lastHandlerError
        pre = $pre; post = $post; tail = $tail
    }
    $results.Add($row)
    $line = "[{0}] pre: frames={1} fps={2} step={3} counter={4} incr={5} red={6} | post: frames={7} step={8} counter={9} incr={10} black={11} identical={12} distinct={13} | tail: frames={14} step={15} counter={16}" -f `
        $name, $pre.frames, $pre.fps, $pre.stepReadable, $pre.counterParsed, $pre.counterIncr, $pre.maxRed,
        $(if ($post) { $post.frames } else { '-' }), $(if ($post) { $post.stepReadable } else { '-' }), $(if ($post) { $post.counterParsed } else { '-' }), $(if ($post) { $post.counterIncr } else { '-' }), $(if ($post) { $post.black } else { '-' }), $(if ($post) { $post.identicalPairs } else { '-' }), $(if ($post) { $post.distinct } else { '-' }),
        $(if ($tail) { $tail.frames } else { '-' }), $(if ($tail) { $tail.stepReadable } else { '-' }), $(if ($tail) { $tail.counterParsed } else { '-' })
    Write-Output $line
    if ($s.error) { Write-Output "    error: $($s.error) native=$($s.nativeStep) handler=$($s.lastHandlerError)" }
}

# ---- header ----
$build = (Get-CimInstance Win32_OperatingSystem).BuildNumber
Write-Output "Windows build: $build (OSVersion $([Environment]::OSVersion.Version))"
Write-Output "Timeline: change at $ChangeMs ms, capture $TotalSeconds s. Work: $Work"

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

# A. visible baseline, no change
$s = Capture-Run 'A-visible-baseline' $hwnd '' $control
Add-Row 'A-visible-baseline' $s -1 -1 -1 'no state change'

# B. occluded: distinctive animating topmost marker appears over the target at ChangeMs
$s = Capture-Run 'B-occluded-by-marker' $hwnd '' $control -Occlude -mdir $mdir
Add-Row 'B-occluded-by-marker' $s $ChangeMs ($ChangeMs + 700) -1 'marker covers target 80..940 x 80..640; marker appears at change'

# C. off-screen: move outside every monitor at ChangeMs
$s = Capture-Run 'C-off-screen' $hwnd "move:-10000:-10000@$ChangeMs" $control
Add-Row 'C-off-screen' $s $ChangeMs ($ChangeMs + 700) -1 'SetWindowPos(-10000,-10000) at change'
Win $hwnd 'move' '100' '100'

# D. app-hidden: ShowWindow SW_HIDE at ChangeMs
$s = Capture-Run 'D-app-hidden-SW_HIDE' $hwnd "hide@$ChangeMs" $control
Add-Row 'D-app-hidden-SW_HIDE' $s $ChangeMs ($ChangeMs + 700) -1 'ShowWindow SW_HIDE at change'
Win $hwnd 'show'

# E. WebView2 IsVisible=false (WinForms control Visible=false) at ChangeMs
$s = Capture-Run 'E-app-hidden-webview2' $hwnd "webview-hide@$ChangeMs" $control
Add-Row 'E-app-hidden-webview2' $s $ChangeMs ($ChangeMs + 700) -1 'WebView2 control Visible=false at change'
Set-Control $control 'webview-show'

# F. minimized at ChangeMs
$s = Capture-Run 'F-minimized' $hwnd "minimize@$ChangeMs" $control
Add-Row 'F-minimized' $s $ChangeMs ($ChangeMs + 700) -1 'ShowWindow SW_MINIMIZE at change'
Win $hwnd 'restore'
Start-Sleep -Seconds 2

# G. recovery: minimize at ChangeMs, restore at ChangeMs+2500; tail = after restore
$restoreAt = $ChangeMs + 2500
$s = Capture-Run 'G-minimize-then-restore' $hwnd "minimize@$ChangeMs;restore@$restoreAt" $control
Add-Row 'G-minimize-then-restore' $s $ChangeMs ($ChangeMs + 700) ($restoreAt + 1000) 'minimize then restore; tail = after restore'

# H. fresh capture (state applied before the session starts): app-hidden SW_HIDE
Win $hwnd 'hide'
$s = Capture-Run 'H-fresh-app-hidden-SW_HIDE' $hwnd '' $control
Add-Row 'H-fresh-app-hidden-SW_HIDE' $s -1 -1 -1 'session created after SW_HIDE'
Win $hwnd 'show'

# I. fresh capture: minimized before session starts
Win $hwnd 'minimize'
$s = Capture-Run 'I-fresh-minimized' $hwnd '' $control
Add-Row 'I-fresh-minimized' $s -1 -1 -1 'session created after minimize'
Win $hwnd 'restore'
Start-Sleep -Seconds 2

# J. positive control for the leak detector: capture the marker window itself (should show red + marker text)
$cdir = Join-Path $Work 'J-control-marker-self'
New-Item -ItemType Directory -Path $cdir -Force | Out-Null
$jctl = Join-Path $mdir 'control-J.txt'
$jhwnd = Join-Path $mdir 'hwnd-J.txt'
$mp = Start-Gui @('marker', '--control', $jctl, '--hwnd-file', $jhwnd, '--x', '80', '--y', '80', '--w', '860', '--h', '560')
$t = 0
while (-not (Test-Path $jhwnd) -and $t -lt 60) { Start-Sleep -Milliseconds 250; $t++ }
Start-Sleep -Seconds 2
$mhwnd = (Get-Content $jhwnd -Raw).Trim()
$s = Capture-Run 'J-control-marker-self' $mhwnd '' $null
Add-Row 'J-control-marker-self' $s -1 -1 -1 'positive control: marker HWND captured directly'
Set-Control $jctl 'exit'
Start-Sleep -Seconds 1
if (-not $mp.HasExited) { $mp.CloseMainWindow() | Out-Null }

# shutdown target
Set-Control $control 'exit'
Start-Sleep -Seconds 2
if (-not $tproc.HasExited) { $tproc.CloseMainWindow() | Out-Null }

$results | ConvertTo-Json -Depth 6 | Set-Content -Path (Join-Path $Work 'results.json') -Encoding UTF8
Copy-Item (Join-Path $Work 'results.json') (Join-Path $spike 'results.json') -Force
Write-Output "DONE results: $(Join-Path $Work 'results.json')"
