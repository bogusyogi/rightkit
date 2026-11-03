param(
  [string]$RepoRoot = "D:\Claude\heardright",
  [switch]$RestartMissing,
  [switch]$ForceSnapshot,
  [string]$PauseFile = "train\controls\pause_conditioning_restart",
  [int]$MaxRestarts = 2,
  [double]$MaxRamPctForRestart = 70.0,
  [int]$MaxPythonProcessesForRestart = 12,
  [int]$StaleManifestMinutes = 20
)

$ErrorActionPreference = "Stop"
Set-Location $RepoRoot
$pausePath = [System.IO.Path]::GetFullPath((Join-Path $RepoRoot $PauseFile))
$restartPaused = Test-Path -LiteralPath $pausePath

$python = "C:\Users\adrds\AppData\Local\Programs\Python\Python311\python.exe"
if (-not (Test-Path -LiteralPath $python)) {
  $python = "python"
}

$snapshotArgs = @(
  "scripts\wake_conditioning_throughput_snapshot.py",
  "--repo-root", $RepoRoot,
  "--sqlite-dir", "train\reports\conditioning_throughput_sqlite"
)
if ($ForceSnapshot) {
  $snapshotArgs += "--force"
}

$snapshotText = & $python @snapshotArgs
if ($LASTEXITCODE -ne 0) {
  throw "throughput snapshot failed with exit_code=$LASTEXITCODE"
}
$snapshot = $snapshotText | ConvertFrom-Json

$os = Get-CimInstance Win32_OperatingSystem
$ramUsedPct = [math]::Round((($os.TotalVisibleMemorySize - $os.FreePhysicalMemory) / $os.TotalVisibleMemorySize) * 100, 1)
$pythonProcessCount = @(
  Get-CimInstance Win32_Process |
    Where-Object { $_.Name -match "^(python|pythonw|py)\.exe$" }
).Count
$resourceRestartSuppressed = ""
if ($ramUsedPct -ge $MaxRamPctForRestart) {
  $resourceRestartSuppressed = "ram_used_pct_at_or_above_limit"
} elseif ($pythonProcessCount -ge $MaxPythonProcessesForRestart) {
  $resourceRestartSuppressed = "python_process_count_at_or_above_limit"
}

$lanes = @(
  @{
    name = "audiobooks_shard_00"
    worklist = "train\manifests\source_truth_existing_block_20260608_0138\audiobooks_shard_00_worklist.jsonl"
    manifest = "train\manifests\source_truth_blockrun_20260608_0140\audiobooks_shard_00_conditioned_manifest.jsonl"
    summary = "train\reports\source_truth_blockrun_20260608_0140_audiobooks_shard_00_conditioned_manifest_summary.json"
    deleteRoot = "train"
    logDir = "train\logs\source_truth_blockrun_20260608_0140"
  },
  @{
    name = "audiobooks_shard_01"
    worklist = "train\manifests\source_truth_existing_block_20260608_0138\audiobooks_shard_01_worklist.jsonl"
    manifest = "train\manifests\source_truth_blockrun_20260608_0140\audiobooks_shard_01_conditioned_manifest.jsonl"
    summary = "train\reports\source_truth_blockrun_20260608_0140_audiobooks_shard_01_conditioned_manifest_summary.json"
    deleteRoot = "train"
    logDir = "train\logs\source_truth_blockrun_20260608_0140"
  },
  @{
    name = "audiobooks_shard_02"
    worklist = "train\manifests\source_truth_existing_block_20260608_0138\audiobooks_shard_02_worklist.jsonl"
    manifest = "train\manifests\source_truth_blockrun_20260608_0140\audiobooks_shard_02_conditioned_manifest.jsonl"
    summary = "train\reports\source_truth_blockrun_20260608_0140_audiobooks_shard_02_conditioned_manifest_summary.json"
    deleteRoot = "train"
    logDir = "train\logs\source_truth_blockrun_20260608_0140"
  },
  @{
    name = "tv_shard_00"
    worklist = "train\manifests\source_truth_existing_block_20260608_0138\tv_shard_00_worklist.jsonl"
    manifest = "train\manifests\source_truth_blockrun_20260608_0140\tv_shard_00_conditioned_manifest.jsonl"
    summary = "train\reports\source_truth_blockrun_20260608_0140_tv_shard_00_conditioned_manifest_summary.json"
    deleteRoot = "train"
    logDir = "train\logs\source_truth_blockrun_20260608_0140"
  },
  @{
    name = "tv_shard_01"
    worklist = "train\manifests\source_truth_existing_block_20260608_0138\tv_shard_01_worklist.jsonl"
    manifest = "train\manifests\source_truth_blockrun_20260608_0140\tv_shard_01_conditioned_manifest.jsonl"
    summary = "train\reports\source_truth_blockrun_20260608_0140_tv_shard_01_conditioned_manifest_summary.json"
    deleteRoot = "train"
    logDir = "train\logs\source_truth_blockrun_20260608_0140"
  },
  @{
    name = "expansion_music_shard_00"
    worklist = "train\manifests\source_truth_existing_block_20260608_0138\expansion_music_shard_00_worklist.jsonl"
    manifest = "train\manifests\source_truth_blockrun_20260608_0140\expansion_music_shard_00_conditioned_manifest.jsonl"
    summary = "train\reports\source_truth_blockrun_20260608_0140_expansion_music_shard_00_conditioned_manifest_summary.json"
    deleteRoot = "train"
    logDir = "train\logs\source_truth_blockrun_20260608_0140"
  },
  @{
    name = "expansion_music_shard_01"
    worklist = "train\manifests\source_truth_existing_block_20260608_0138\expansion_music_shard_01_worklist.jsonl"
    manifest = "train\manifests\source_truth_blockrun_20260608_0140\expansion_music_shard_01_conditioned_manifest.jsonl"
    summary = "train\reports\source_truth_blockrun_20260608_0140_expansion_music_shard_01_conditioned_manifest_summary.json"
    deleteRoot = "train"
    logDir = "train\logs\source_truth_blockrun_20260608_0140"
  },
  @{
    name = "e_archive_stream_20260607_211948"
    worklist = "train\manifests\e_archive_stream_20260607_211948_conditioning_worklist.jsonl"
    manifest = "train\manifests\e_archive_stream_20260607_211948_conditioned_manifest.jsonl"
    summary = "train\reports\e_archive_stream_20260607_211948_conditioned_manifest_summary.json"
    deleteRoot = "E:\heardright_archive\wake_train_a5_2026_05_11"
    logDir = "train\logs"
  }
)

function Resolve-RepoPath {
  param([string]$PathText)
  $path = [System.IO.Path]::GetFullPath((Join-Path $RepoRoot $PathText))
  return $path
}

function Get-LaneSnapshot {
  param([string]$ManifestPath)
  $full = Resolve-RepoPath $ManifestPath
  foreach ($lane in @($snapshot.lanes)) {
    if ([System.IO.Path]::GetFullPath($lane.manifest) -ieq $full) {
      return $lane
    }
  }
  return $null
}

function Get-LaneProcess {
  param([hashtable]$Lane)
  $manifestFull = Resolve-RepoPath $Lane.manifest
  $worklistFull = Resolve-RepoPath $Lane.worklist
  $manifestLeaf = Split-Path -Leaf $manifestFull
  $worklistLeaf = Split-Path -Leaf $worklistFull
  Get-CimInstance Win32_Process |
    Where-Object {
      $_.Name -like "python*" -and
      $_.CommandLine -and
      $_.CommandLine -like "*wake_phase0_condition_audio.py*" -and
      (
        $_.CommandLine -like "*$manifestFull*" -or
        $_.CommandLine -like "*$worklistFull*" -or
        $_.CommandLine -like "*$manifestLeaf*" -or
        $_.CommandLine -like "*$worklistLeaf*"
      )
    }
}

function Start-Lane {
  param([hashtable]$Lane)
  $stamp = Get-Date -Format "yyyyMMdd_HHmmss"
  $logDir = Resolve-RepoPath $Lane.logDir
  New-Item -ItemType Directory -Force -Path $logDir | Out-Null
  $args = @(
    "scripts\wake_phase0_condition_audio.py",
    "--worklist-jsonl", $Lane.worklist,
    "--output-root", "train\conditioned\adaptive_hot_v2_5",
    "--output-manifest-jsonl", $Lane.manifest,
    "--summary-json", $Lane.summary,
    "--delete-source-on-success",
    "--delete-train-root", $Lane.deleteRoot
  )
  $out = Join-Path $logDir ("{0}.steward_{1}.out.log" -f $Lane.name, $stamp)
  $err = Join-Path $logDir ("{0}.steward_{1}.err.log" -f $Lane.name, $stamp)
  $proc = Start-Process -FilePath $python -ArgumentList $args -WorkingDirectory $RepoRoot -WindowStyle Hidden -RedirectStandardOutput $out -RedirectStandardError $err -PassThru
  try {
    $proc.PriorityClass = "BelowNormal"
  } catch {
  }
  return @{
    pid = $proc.Id
    out_log = $out
    err_log = $err
  }
}

$laneReports = @()
$restarted = @()
$restartSlotsRemaining = [Math]::Max(0, $MaxRestarts)
foreach ($lane in $lanes) {
  $laneSnapshot = Get-LaneSnapshot $lane.manifest
  $remaining = if ($laneSnapshot) { [int64]$laneSnapshot.remaining_rows_estimate } else { $null }
  $processes = @(Get-LaneProcess $lane)
  $started = $null
  $restartSuppressed = ""
  if ($RestartMissing -and -not $restartPaused -and $resourceRestartSuppressed -and $remaining -ne $null -and $remaining -gt 0 -and $processes.Count -eq 0) {
    $restartSuppressed = $resourceRestartSuppressed
  } elseif ($RestartMissing -and -not $restartPaused -and $remaining -ne $null -and $remaining -gt 0 -and $processes.Count -eq 0 -and $restartSlotsRemaining -le 0) {
    $restartSuppressed = "max_restarts_reached"
  }
  if ($RestartMissing -and -not $restartPaused -and -not $resourceRestartSuppressed -and $remaining -ne $null -and $remaining -gt 0 -and $processes.Count -eq 0 -and $restartSlotsRemaining -gt 0) {
    $started = Start-Lane $lane
    $restartSlotsRemaining -= 1
    $restarted += [pscustomobject]@{
      lane = $lane.name
      pid = $started.pid
      remaining_rows_estimate = $remaining
      err_log = $started.err_log
    }
    Start-Sleep -Milliseconds 200
    $processes = @(Get-LaneProcess $lane)
  }
  $procReports = @()
  foreach ($proc in $processes) {
    $p = Get-Process -Id $proc.ProcessId -ErrorAction SilentlyContinue
    $procReports += [pscustomobject]@{
      pid = $proc.ProcessId
      cpu_seconds = if ($p) { [math]::Round($p.CPU, 3) } else { $null }
      working_set_mb = if ($p) { [math]::Round($p.WorkingSet64 / 1MB, 1) } else { $null }
    }
  }
  $manifestPath = Resolve-RepoPath $lane.manifest
  $manifestItem = Get-Item -LiteralPath $manifestPath -ErrorAction SilentlyContinue
  $manifestAgeMinutes = if ($manifestItem) { [math]::Round(((Get-Date) - $manifestItem.LastWriteTime).TotalMinutes, 2) } else { $null }
  $laneState = "unknown"
  if ($remaining -ne $null -and $remaining -le 0) {
    $laneState = "complete"
  } elseif ($processes.Count -eq 0 -and $restartPaused) {
    $laneState = "restart_paused_with_remaining"
  } elseif ($processes.Count -eq 0 -and $restartSuppressed) {
    $laneState = "restart_deferred_with_remaining"
  } elseif ($processes.Count -eq 0) {
    $laneState = "missing_with_remaining"
  } elseif ($manifestAgeMinutes -ne $null -and $manifestAgeMinutes -gt $StaleManifestMinutes) {
    $laneState = "running_stale_manifest"
  } else {
    $laneState = "running_active"
  }
  $laneReports += [pscustomobject]@{
    lane = $lane.name
    state = $laneState
    process_count = $processes.Count
    processes = $procReports
    remaining_rows_estimate = $remaining
    ok_true_rows = if ($laneSnapshot) { [int64]$laneSnapshot.ok_true_rows } else { $null }
    ok_false_rows_observed = if ($laneSnapshot) { [int64]$laneSnapshot.ok_false_rows_observed } else { $null }
    worklist_rows = if ($laneSnapshot) { [int64]$laneSnapshot.worklist_rows } else { $null }
    manifest_last_write = if ($manifestItem) { $manifestItem.LastWriteTime.ToString("o") } else { $null }
    manifest_age_minutes = $manifestAgeMinutes
    restarted = $started -ne $null
    restart_suppressed = $restartSuppressed
  }
}

$stateCounts = @{}
foreach ($group in ($laneReports | Group-Object -Property state)) {
  $stateCounts[$group.Name] = $group.Count
}

$drives = Get-PSDrive -PSProvider FileSystem |
  Where-Object { $_.Name -in @("D", "E") } |
  ForEach-Object {
    [pscustomobject]@{
      name = $_.Name
      free_gb = [math]::Round($_.Free / 1GB, 1)
      used_gb = [math]::Round($_.Used / 1GB, 1)
    }
  }

$report = [ordered]@{
  created_local = (Get-Date).ToString("s")
  restart_missing = [bool]$RestartMissing
  restart_paused = [bool]$restartPaused
  pause_file = $pausePath
  max_restarts = $MaxRestarts
  restart_slots_remaining = $restartSlotsRemaining
  max_ram_pct_for_restart = $MaxRamPctForRestart
  max_python_processes_for_restart = $MaxPythonProcessesForRestart
  python_process_count = $pythonProcessCount
  resource_restart_suppressed = $resourceRestartSuppressed
  force_snapshot = [bool]$ForceSnapshot
  stale_manifest_minutes = $StaleManifestMinutes
  python_owned_lane_processes = [int](($laneReports | Measure-Object -Property process_count -Sum).Sum)
  lane_state_counts = $stateCounts
  restarted_count = $restarted.Count
  restarted = $restarted
  ram_used_pct = $ramUsedPct
  drives = $drives
  total_remaining_rows_estimate = [int64]$snapshot.total_remaining_rows_estimate
  total_ok_true_rows = [int64]$snapshot.total_ok_true_rows
  throughput_rows_per_hour_since_previous = $snapshot.throughput_rows_per_hour_since_previous
  eta_hours_at_recent_throughput = $snapshot.eta_hours_at_recent_throughput
  skipped_due_to_min_interval = [bool]$snapshot.skipped_due_to_min_interval
  lanes = $laneReports
}

$reportPath = Join-Path $RepoRoot "train\reports\owned_conditioning_steward_latest.json"
$report | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $reportPath -Encoding utf8
$report | ConvertTo-Json -Depth 8
