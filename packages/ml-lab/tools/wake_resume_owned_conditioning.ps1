param(
  [string]$RepoRoot = "D:\Claude\heardright",
  [string]$PauseFile = "train\controls\pause_conditioning_restart",
  [string]$ReportJson = "train\reports\owned_conditioning_resume_latest.json",
  [int]$MaxRestarts = 2,
  [double]$MaxRamPctForRestart = 70.0,
  [int]$MaxPythonProcessesForRestart = 12,
  [switch]$ForceSnapshot
)

$ErrorActionPreference = "Stop"
Set-Location $RepoRoot

$pausePath = [System.IO.Path]::GetFullPath((Join-Path $RepoRoot $PauseFile))
$pauseExisted = Test-Path -LiteralPath $pausePath
if ($pauseExisted) {
  Remove-Item -LiteralPath $pausePath -Force
}

$stewardArgs = @(
  "-NoProfile",
  "-ExecutionPolicy", "Bypass",
  "-File", "scripts\wake_owned_conditioning_steward.ps1",
  "-RestartMissing",
  "-MaxRestarts", "$MaxRestarts",
  "-MaxRamPctForRestart", "$MaxRamPctForRestart",
  "-MaxPythonProcessesForRestart", "$MaxPythonProcessesForRestart"
)
if ($ForceSnapshot) {
  $stewardArgs += "-ForceSnapshot"
}

$stewardText = powershell @stewardArgs
$stewardExit = $LASTEXITCODE
$steward = $null
try {
  $steward = $stewardText | ConvertFrom-Json
} catch {
}

$report = [ordered]@{
  created_local = (Get-Date).ToString("s")
  pause_file = $pausePath
  pause_file_removed = [bool]$pauseExisted
  max_restarts = $MaxRestarts
  max_ram_pct_for_restart = $MaxRamPctForRestart
  max_python_processes_for_restart = $MaxPythonProcessesForRestart
  force_snapshot = [bool]$ForceSnapshot
  steward_exit_code = $stewardExit
  steward_report = "train\reports\owned_conditioning_steward_latest.json"
  steward_summary = if ($steward) {
    [ordered]@{
      restart_paused = $steward.restart_paused
      restarted_count = $steward.restarted_count
      lane_state_counts = $steward.lane_state_counts
      python_owned_lane_processes = $steward.python_owned_lane_processes
      total_remaining_rows_estimate = $steward.total_remaining_rows_estimate
      ram_used_pct = $steward.ram_used_pct
    }
  } else {
    $null
  }
}

New-Item -ItemType Directory -Force -Path (Split-Path $ReportJson) | Out-Null
$report | ConvertTo-Json -Depth 6 | Set-Content -LiteralPath $ReportJson -Encoding utf8
$report | ConvertTo-Json -Depth 6

exit $stewardExit
