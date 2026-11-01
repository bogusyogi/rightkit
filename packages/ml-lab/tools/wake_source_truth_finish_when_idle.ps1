param(
  [string]$RepoRoot = "D:\Claude\heardright",
  [string]$FinishReportJson = "train\reports\source_truth_finish_when_idle_latest.json",
  [switch]$ForceSnapshot
)

$ErrorActionPreference = "Stop"
Set-Location $RepoRoot

$stewardArgs = @(
  "-NoProfile",
  "-ExecutionPolicy", "Bypass",
  "-File", "scripts\wake_owned_conditioning_steward.ps1",
  "-RestartMissing"
)
if ($ForceSnapshot) {
  $stewardArgs += "-ForceSnapshot"
}

$stewardText = powershell @stewardArgs
if ($LASTEXITCODE -ne 0) {
  throw "Owned conditioning steward failed with exit_code=$LASTEXITCODE"
}
$steward = $stewardText | ConvertFrom-Json

function Get-StateCount {
  param(
    [object]$Counts,
    [string]$Name
  )
  if ($null -eq $Counts) {
    return 0
  }
  $property = $Counts.PSObject.Properties[$Name]
  if ($null -eq $property -or $null -eq $property.Value) {
    return 0
  }
  return [int]$property.Value
}

$stateCounts = $steward.lane_state_counts
$completeCount = Get-StateCount $stateCounts "complete"
$runningCount = Get-StateCount $stateCounts "running_active"
$missingCount = Get-StateCount $stateCounts "missing_with_remaining"
$staleCount = Get-StateCount $stateCounts "running_stale_manifest"
$unknownCount = Get-StateCount $stateCounts "unknown"
$laneCount = @($steward.lanes).Count

function Write-FinishReport {
  param([object]$Report)
  $Report | ConvertTo-Json -Depth 6 | Set-Content -LiteralPath $FinishReportJson -Encoding utf8
}

$action = "wait"
$reason = "owned lanes are not all complete"
$finalAuditExitCode = $null
if ($missingCount -gt 0 -or $staleCount -gt 0 -or $unknownCount -gt 0) {
  $action = "attention_needed"
  $reason = "owned lanes include missing, stale, or unknown states"
} elseif ($laneCount -gt 0 -and $completeCount -eq $laneCount -and $runningCount -eq 0) {
  $action = "run_final_audit"
  $reason = "all owned lanes are complete"
}

$report = [ordered]@{
  created_local = (Get-Date).ToString("s")
  action = $action
  reason = $reason
  final_audit_exit_code = $finalAuditExitCode
  steward_report = "train\reports\owned_conditioning_steward_latest.json"
  lane_state_counts = $stateCounts
  total_remaining_rows_estimate = $steward.total_remaining_rows_estimate
  total_ok_true_rows = $steward.total_ok_true_rows
  eta_hours_at_recent_throughput = $steward.eta_hours_at_recent_throughput
  ram_used_pct = $steward.ram_used_pct
}

New-Item -ItemType Directory -Force -Path (Split-Path $FinishReportJson) | Out-Null

if ($action -eq "run_final_audit") {
  Write-FinishReport $report
  $auditArgs = @(
    "-NoProfile",
    "-ExecutionPolicy", "Bypass",
    "-File", "scripts\wake_source_truth_final_audit_when_idle.ps1",
    "-VerifyOutputs",
    "-ScanRawRoots",
    "-ValidateEvidence"
  )
  powershell @auditArgs
  $finalAuditExitCode = $LASTEXITCODE
  $report["final_audit_exit_code"] = $finalAuditExitCode
  if ($finalAuditExitCode -eq 0) {
    $report["action"] = "final_audit_passed"
    $report["reason"] = "all owned lanes complete and final evidence validator passed"
  } else {
    $report["action"] = "final_audit_failed"
    $report["reason"] = "final audit or final evidence validator returned nonzero"
  }
}

Write-FinishReport $report
$report | ConvertTo-Json -Depth 6

if ($report["action"] -eq "attention_needed" -or $report["action"] -eq "final_audit_failed") {
  exit 2
}
exit 0
