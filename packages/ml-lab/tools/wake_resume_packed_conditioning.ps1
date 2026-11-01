param(
  [string]$RepoRoot = "D:\Claude\heardright",
  [string]$PauseFile = "train\controls\pause_conditioning_restart",
  [string]$RunTag = "source_truth_packed_20260608",
  [int]$MaxLanes = 2,
  [int64]$MaxShardBytes = 1073741824,
  [switch]$RemovePause
)

$ErrorActionPreference = "Stop"
Set-Location $RepoRoot

if ($RemovePause) {
  $pausePath = [System.IO.Path]::GetFullPath((Join-Path $RepoRoot $PauseFile))
  if (Test-Path -LiteralPath $pausePath) {
    Remove-Item -LiteralPath $pausePath -Force
  }
}

$pauseStillExists = Test-Path -LiteralPath (Join-Path $RepoRoot $PauseFile)
if ($pauseStillExists) {
  throw "Pause file exists. Re-run with -RemovePause when ready to start packed lanes."
}

$python = "C:\Users\adrds\AppData\Local\Programs\Python\Python311\python.exe"
if (-not (Test-Path -LiteralPath $python)) {
  $python = "python"
}

$laneSpecs = @(
  @{
    name = "audiobooks_shard_00"
    worklist = "train\manifests\source_truth_existing_block_20260608_0138\audiobooks_shard_00_worklist.jsonl"
    deleteRoot = "train"
    doneManifests = @(
      "train\manifests\source_truth_blockrun_20260608_0118\audiobooks_shard_00_conditioned_manifest.jsonl",
      "train\manifests\source_truth_blockrun_20260608_0140\audiobooks_shard_00_conditioned_manifest.jsonl"
    )
  },
  @{
    name = "audiobooks_shard_01"
    worklist = "train\manifests\source_truth_existing_block_20260608_0138\audiobooks_shard_01_worklist.jsonl"
    deleteRoot = "train"
    doneManifests = @(
      "train\manifests\source_truth_blockrun_20260608_0118\audiobooks_shard_01_conditioned_manifest.jsonl",
      "train\manifests\source_truth_blockrun_20260608_0140\audiobooks_shard_01_conditioned_manifest.jsonl"
    )
  },
  @{
    name = "audiobooks_shard_02"
    worklist = "train\manifests\source_truth_existing_block_20260608_0138\audiobooks_shard_02_worklist.jsonl"
    deleteRoot = "train"
    doneManifests = @(
      "train\manifests\source_truth_blockrun_20260608_0118\audiobooks_shard_02_conditioned_manifest.jsonl",
      "train\manifests\source_truth_blockrun_20260608_0140\audiobooks_shard_02_conditioned_manifest.jsonl"
    )
  },
  @{
    name = "expansion_music_shard_00"
    worklist = "train\manifests\source_truth_existing_block_20260608_0138\expansion_music_shard_00_worklist.jsonl"
    deleteRoot = "train"
    doneManifests = @(
      "train\manifests\source_truth_blockrun_20260608_0118\expansion_music_shard_00_conditioned_manifest.jsonl",
      "train\manifests\source_truth_blockrun_20260608_0140\expansion_music_shard_00_conditioned_manifest.jsonl"
    )
  },
  @{
    name = "expansion_music_shard_01"
    worklist = "train\manifests\source_truth_existing_block_20260608_0138\expansion_music_shard_01_worklist.jsonl"
    deleteRoot = "train"
    doneManifests = @(
      "train\manifests\source_truth_blockrun_20260608_0118\expansion_music_shard_01_conditioned_manifest.jsonl",
      "train\manifests\source_truth_blockrun_20260608_0140\expansion_music_shard_01_conditioned_manifest.jsonl"
    )
  },
  @{
    name = "e_archive_stream_20260607_211948"
    worklist = "train\manifests\e_archive_stream_20260607_211948_conditioning_worklist.jsonl"
    deleteRoot = "E:\heardright_archive\wake_train_a5_2026_05_11"
    doneManifests = @(
      "train\manifests\e_archive_stream_20260607_211948_conditioned_manifest.jsonl"
    )
  }
)

$manifestDir = "train\manifests\$RunTag"
$reportDir = "train\reports\$RunTag"
$logDir = "train\logs\$RunTag"
$outputRoot = "train\conditioned\adaptive_hot_v2_5\_packed_shards"
New-Item -ItemType Directory -Force -Path $manifestDir, $reportDir, $logDir, $outputRoot | Out-Null

$activePythonCommands = @()
try {
  $activePythonCommands = @(Get-CimInstance Win32_Process -Filter "Name = 'python.exe'" | ForEach-Object { $_.CommandLine })
} catch {
  $activePythonCommands = @()
}

$started = @()
$skippedActive = @()
$skippedComplete = @()
foreach ($lane in $laneSpecs) {
  if (($started.Count + $skippedActive.Count) -ge $MaxLanes) {
    break
  }
  if (-not (Test-Path -LiteralPath $lane.worklist)) {
    throw "Missing worklist: $($lane.worklist)"
  }
  $manifest = Join-Path $manifestDir ($lane.name + "_packed_manifest.jsonl")
  $index = Join-Path $manifestDir ($lane.name + "_packed_index.jsonl")
  $summary = Join-Path $reportDir ($lane.name + "_packed_summary.json")
  $progress = Join-Path $reportDir ($lane.name + "_packed_progress.jsonl")
  $outLog = Join-Path $logDir ($lane.name + ".packed.out.log")
  $errLog = Join-Path $logDir ($lane.name + ".packed.err.log")
  $isActive = $false
  foreach ($command in $activePythonCommands) {
    if ($command -and $command.Contains($manifest)) {
      $isActive = $true
      break
    }
  }
  if ($isActive) {
    $skippedActive += [pscustomobject]@{
      lane = $lane.name
      manifest = $manifest
      reason = "already_running"
    }
    continue
  }
  if (Test-Path -LiteralPath $summary) {
    try {
      $summaryData = Get-Content -LiteralPath $summary -Raw | ConvertFrom-Json
      $counters = $summaryData.counters
      $pendingRows = [int64]($counters.pending_rows_considered)
      $finishedRows = [int64]($counters.written) + [int64]($counters.errors)
      if ($pendingRows -gt 0 -and $finishedRows -ge $pendingRows) {
        $skippedComplete += [pscustomobject]@{
          lane = $lane.name
          manifest = $manifest
          summary = $summary
          reason = "already_complete"
          pending_rows = $pendingRows
          finished_rows = $finishedRows
        }
        continue
      }
    } catch {
    }
  }
  $args = @(
    "scripts\wake_phase0_condition_packed_shards.py",
    "--worklist-jsonl", $lane.worklist,
    "--output-root", $outputRoot,
    "--output-manifest-jsonl", $manifest,
    "--output-index-jsonl", $index,
    "--summary-json", $summary,
    "--progress-jsonl", $progress,
    "--shard-prefix", $lane.name,
    "--max-shard-bytes", "$MaxShardBytes",
    "--delete-source-on-success",
    "--delete-train-root", $lane.deleteRoot
  )
  $existingDoneManifests = @()
  foreach ($doneManifest in $lane.doneManifests) {
    if (Test-Path -LiteralPath $doneManifest) {
      $args += @("--done-manifest-jsonl", $doneManifest)
      $existingDoneManifests += $doneManifest
    }
  }
  $proc = Start-Process -FilePath $python -ArgumentList $args -WorkingDirectory $RepoRoot -WindowStyle Hidden -RedirectStandardOutput $outLog -RedirectStandardError $errLog -PassThru
  try {
    $proc.PriorityClass = "BelowNormal"
  } catch {
  }
  $started += [pscustomobject]@{
    lane = $lane.name
    pid = $proc.Id
    manifest = $manifest
    index = $index
    summary = $summary
    progress = $progress
    out_log = $outLog
    err_log = $errLog
    done_manifests = $existingDoneManifests
  }
}

$report = [ordered]@{
  created_local = (Get-Date).ToString("s")
  run_tag = $RunTag
  max_lanes = $MaxLanes
  max_shard_bytes = $MaxShardBytes
  output_root = $outputRoot
  started_count = $started.Count
  skipped_active_count = $skippedActive.Count
  skipped_active = $skippedActive
  skipped_complete_count = $skippedComplete.Count
  skipped_complete = $skippedComplete
  started = $started
}
$reportPath = "train\reports\${RunTag}_packed_resume_latest.json"
$report | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath $reportPath -Encoding utf8
$report | ConvertTo-Json -Depth 5
