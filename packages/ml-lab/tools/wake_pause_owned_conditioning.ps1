param(
  [string]$RepoRoot = "D:\Claude\heardright",
  [string]$PauseFile = "train\controls\pause_conditioning_restart",
  [string]$ReportJson = "train\reports\owned_conditioning_pause_latest.json",
  [switch]$Kill
)

$ErrorActionPreference = "Stop"
Set-Location $RepoRoot

$pausePath = [System.IO.Path]::GetFullPath((Join-Path $RepoRoot $PauseFile))
New-Item -ItemType Directory -Force -Path (Split-Path $pausePath) | Out-Null
@"
Paused by wake_pause_owned_conditioning.ps1 at $(Get-Date -Format o).
Do not auto-restart owned D/E conditioning lanes until this file is removed.
"@ | Set-Content -LiteralPath $pausePath -Encoding utf8

$ownedPattern = "source_truth_blockrun_20260608_0140|e_archive_stream_20260607_211948"
$owned = Get-CimInstance Win32_Process |
  Where-Object {
    $_.Name -like "python*" -and
    $_.CommandLine -and
    $_.CommandLine -match "wake_phase0_condition_audio.py" -and
    $_.CommandLine -match $ownedPattern
  }

$stopped = @()
$failed = @()
if ($Kill) {
  foreach ($proc in $owned) {
    try {
      Stop-Process -Id $proc.ProcessId -Force -ErrorAction Stop
      $stopped += [pscustomobject]@{
        pid = $proc.ProcessId
        name = $proc.Name
        command_line = $proc.CommandLine
      }
    } catch {
      $failed += [pscustomobject]@{
        pid = $proc.ProcessId
        name = $proc.Name
        error = $_.Exception.Message
        command_line = $proc.CommandLine
      }
    }
  }
}

$remainingOwned = Get-CimInstance Win32_Process |
  Where-Object {
    $_.Name -like "python*" -and
    $_.CommandLine -and
    $_.CommandLine -match "wake_phase0_condition_audio.py" -and
    $_.CommandLine -match $ownedPattern
  } |
  ForEach-Object {
    [pscustomobject]@{
      pid = $_.ProcessId
      name = $_.Name
      command_line = $_.CommandLine
    }
  }

$allPython = Get-CimInstance Win32_Process |
  Where-Object { $_.Name -match "^(python|pythonw|py)\.exe$" } |
  ForEach-Object {
    [pscustomobject]@{
      pid = $_.ProcessId
      parent_pid = $_.ParentProcessId
      name = $_.Name
      command_line = $_.CommandLine
    }
  }

$report = [ordered]@{
  created_local = (Get-Date).ToString("s")
  pause_file = $pausePath
  kill_requested = [bool]$Kill
  owned_matching_before_count = @($owned).Count
  stopped_count = @($stopped).Count
  failed_count = @($failed).Count
  remaining_owned_count = @($remainingOwned).Count
  remaining_python_count = @($allPython).Count
  stopped = $stopped
  failed = $failed
  remaining_owned = $remainingOwned
  remaining_python = $allPython
}

New-Item -ItemType Directory -Force -Path (Split-Path $ReportJson) | Out-Null
$report | ConvertTo-Json -Depth 6 | Set-Content -LiteralPath $ReportJson -Encoding utf8
$report | ConvertTo-Json -Depth 6

if (@($failed).Count -gt 0) {
  exit 2
}
exit 0
