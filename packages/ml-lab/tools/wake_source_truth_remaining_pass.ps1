param(
  [string]$RepoRoot = "D:\Claude\heardright",
  [string]$Tag = "",
  [string]$ActiveTaskName = "HeardRightWakeConditionSourceTruthLow"
)

$ErrorActionPreference = "Stop"
if (Get-Variable -Name PSNativeCommandUseErrorActionPreference -ErrorAction SilentlyContinue) {
  $PSNativeCommandUseErrorActionPreference = $false
}

Set-Location $RepoRoot
if (-not $Tag) {
  $Tag = "remaining_{0}" -f (Get-Date -Format "yyyyMMdd_HHmmss")
}

New-Item -ItemType Directory -Force -Path "train\logs", "train\reports", "train\manifests", "train\conditioned\adaptive_hot_v2_5" | Out-Null

$taskQuery = schtasks.exe /Query /TN $ActiveTaskName /V /FO LIST 2>$null
if ($LASTEXITCODE -eq 0 -and ($taskQuery | Select-String -Pattern "^Status:\s+Running" -Quiet)) {
  throw "Refusing to run remaining pass while scheduled task '$ActiveTaskName' is still Running."
}

$rawManifest = "train\manifests\source_truth_${Tag}_raw_manifest.jsonl"
$rawSummary = "train\reports\source_truth_${Tag}_raw_manifest_summary.json"
$metadataJsonl = "train\reports\source_truth_${Tag}_raw_metadata.jsonl"
$metadataSummary = "train\reports\source_truth_${Tag}_raw_metadata_summary.json"
$planCsv = "train\reports\source_truth_${Tag}_window_plan_sources.csv"
$planSummary = "train\reports\source_truth_${Tag}_window_plan_summary.json"
$worklistJsonl = "train\manifests\source_truth_${Tag}_conditioning_worklist.jsonl"
$worklistSummary = "train\reports\source_truth_${Tag}_conditioning_worklist_summary.json"
$conditionSummary = "train\reports\source_truth_${Tag}_conditioned_manifest_summary.json"
$python = "C:\Users\adrds\AppData\Local\Programs\Python\Python311\python.exe"
$pyLauncher = "C:\WINDOWS\py.exe"

function Run-Step {
  param([string]$Name, [scriptblock]$Body)
  $out = "train\logs\${Tag}_$Name.out.log"
  $err = "train\logs\${Tag}_$Name.err.log"
  $status = "train\logs\${Tag}_$Name.status.json"
  $started = Get-Date
  $ok = $false
  $code = 0
  try {
    & $Body > $out 2> $err
    $code = if ($LASTEXITCODE -ne $null) { $LASTEXITCODE } else { 0 }
    $ok = ($code -eq 0)
  } catch {
    $code = -1
    $_ | Out-String | Set-Content -LiteralPath $err -Encoding utf8
  }
  [ordered]@{
    name = $Name
    tag = $Tag
    ok = $ok
    exit_code = $code
    started = $started.ToString("o")
    finished = (Get-Date).ToString("o")
  } | ConvertTo-Json | Set-Content -LiteralPath $status -Encoding utf8
  if (-not $ok) { throw "$Name failed with exit_code=$code" }
}

function Run-Py {
  param(
    [string]$Name,
    [string[]]$Arguments
  )
  $childOut = "train\logs\${Tag}_$Name.child.out.log"
  $childErr = "train\logs\${Tag}_$Name.child.err.log"
  $proc = Start-Process -FilePath $pyLauncher -ArgumentList (@("-3.11") + $Arguments) -WorkingDirectory $RepoRoot -WindowStyle Hidden -RedirectStandardOutput $childOut -RedirectStandardError $childErr -PassThru -Wait
  if ($proc.ExitCode -ne 0) {
    throw "$Name failed with exit_code=$($proc.ExitCode)"
  }
}

Run-Step "raw_manifest" {
  Run-Py "raw_manifest" @(
    "scripts\wake_train_source_truth_manifest.py",
    "--train-root", "train",
    "--output-jsonl", $rawManifest,
    "--summary-json", $rawSummary
  )
}

Run-Step "raw_metadata" {
  Run-Py "raw_metadata" @(
    "scripts\wake_audio_metadata_audit.py",
    "--metadata-jsonl", $metadataJsonl,
    "--summary-json", $metadataSummary,
    "--expansion-manifest", $rawManifest,
    "--only-expansion",
    "--workers", "1",
    "--batch-size", "16"
  )
}

Run-Step "window_plan" {
  Run-Py "window_plan" @(
    "scripts\wake_phase0_window_plan.py",
    "--metadata-jsonl", $metadataJsonl,
    "--summary-json", $planSummary,
    "--plan-csv", $planCsv
  )
}

Run-Step "materialize" {
  Run-Py "materialize" @(
    "scripts\wake_phase0_materialize_manifest.py",
    "--source-plan-csv", $planCsv,
    "--output-jsonl", $worklistJsonl,
    "--summary-json", $worklistSummary
  )
}

Run-Step "condition_audio" {
  $args = @(
    "scripts\wake_phase0_condition_audio.py",
    "--worklist-jsonl", $worklistJsonl,
    "--output-root", "train\conditioned\adaptive_hot_v2_5",
    "--output-manifest-jsonl", "train\manifests\source_truth_conditioned_manifest.jsonl",
    "--summary-json", $conditionSummary,
    "--delete-source-on-success",
    "--delete-train-root", "train"
  )
  $out = "train\logs\${Tag}_condition_audio.child.out.log"
  $err = "train\logs\${Tag}_condition_audio.child.err.log"
  $proc = Start-Process -FilePath $python -ArgumentList $args -WorkingDirectory $RepoRoot -WindowStyle Hidden -RedirectStandardOutput $out -RedirectStandardError $err -PassThru
  try {
    $proc.PriorityClass = "BelowNormal"
  } catch {
  }
  $proc.WaitForExit()
  if ($proc.ExitCode -ne 0) {
    throw "condition_audio failed with exit_code=$($proc.ExitCode)"
  }
}
