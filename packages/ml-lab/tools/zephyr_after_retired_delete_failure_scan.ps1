param(
  [int]$DeletePid = 0
)

$ErrorActionPreference = "Stop"

$repoRoot = "D:\Claude\heardright"
$trainRoot = Join-Path $repoRoot "train"
$reports = Join-Path $trainRoot "reports"
$logs = Join-Path $reports "logs"
$deleteReport = Join-Path $reports "delete_retired_noncanonical_20260609.json"
$deleteErr = Join-Path $logs "delete_retired_noncanonical_20260609.err.log"
$stateReport = Join-Path $reports "after_retired_delete_failure_scan_20260609.json"

New-Item -ItemType Directory -Force -Path $reports, $logs | Out-Null

function Write-State {
  param(
    [string]$State,
    [string]$Message = ""
  )
  [pscustomobject]@{
    generated_at = (Get-Date).ToString("s")
    state = $State
    message = $Message
    delete_pid = $DeletePid
    delete_report = $deleteReport
  } | ConvertTo-Json -Depth 4 | Set-Content -LiteralPath $stateReport -Encoding UTF8
}

Write-State -State "waiting_for_delete" -Message "Waiting for retired conditioned duplicate delete to exit."

if ($DeletePid -gt 0) {
  while (Get-Process -Id $DeletePid -ErrorAction SilentlyContinue) {
    Start-Sleep -Seconds 20
  }
}

if (-not (Test-Path -LiteralPath $deleteReport)) {
  Write-State -State "blocked" -Message "Delete report missing; not starting failure scan."
  exit 2
}

$deleteState = Get-Content -LiteralPath $deleteReport -Raw | ConvertFrom-Json
$stderrLength = if (Test-Path -LiteralPath $deleteErr) { (Get-Item -LiteralPath $deleteErr).Length } else { 0 }
if ($deleteState.state -ne "complete" -or $stderrLength -gt 0) {
  Write-State -State "blocked" -Message "Delete did not complete cleanly; not starting failure scan."
  exit 3
}

Write-State -State "running_failure_scan" -Message "Scanning packed manifest for failed rows."

$manifest = Join-Path $trainRoot "manifests\source_truth_packed_current_20260609_merged.jsonl"
$failed = Join-Path $reports "source_truth_packed_current_20260609_failed_rows.jsonl"
$sources = Join-Path $reports "source_truth_packed_current_20260609_failed_sources.jsonl"
$summary = Join-Path $reports "source_truth_packed_current_20260609_failed_summary.json"
$progress = Join-Path $reports "source_truth_packed_current_20260609_failed_progress.json"

Push-Location $repoRoot
try {
  python scripts\zephyr_stream_packed_failures.py `
    --manifest $manifest `
    --failed-jsonl $failed `
    --source-summary-jsonl $sources `
    --summary-json $summary `
    --progress-json $progress `
    --progress-every-lines 100000
} finally {
  Pop-Location
}

Write-State -State "complete" -Message "Failure scan complete."
