param(
  [string]$RepoRoot = "D:\Claude\heardright"
)

$ErrorActionPreference = "Stop"
if (Get-Variable -Name PSNativeCommandUseErrorActionPreference -ErrorAction SilentlyContinue) {
  $PSNativeCommandUseErrorActionPreference = $false
}

Set-Location $RepoRoot
New-Item -ItemType Directory -Force -Path "train\logs", "train\reports", "train\manifests", "train\conditioned\adaptive_hot_v2_5" | Out-Null

$metadataSummary = "train\reports\source_truth_raw_metadata_summary.json"
$metadataJsonl = "train\reports\source_truth_raw_metadata.jsonl"
$deadline = (Get-Date).AddHours(12)
while (-not (Test-Path -LiteralPath $metadataSummary)) {
  if ((Get-Date) -gt $deadline) {
    throw "Timed out waiting for $metadataSummary"
  }
  Start-Sleep -Seconds 30
}

function Run-Step {
  param([string]$Name, [scriptblock]$Body)
  $out = "train\logs\$Name.out.log"
  $err = "train\logs\$Name.err.log"
  $status = "train\logs\$Name.status.json"
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
    ok = $ok
    exit_code = $code
    started = $started.ToString("o")
    finished = (Get-Date).ToString("o")
  } | ConvertTo-Json | Set-Content -LiteralPath $status -Encoding utf8
  if (-not $ok) { throw "$Name failed with exit_code=$code" }
}

Run-Step "source_truth_window_plan" {
  C:\WINDOWS\py.exe -3.11 scripts\wake_phase0_window_plan.py `
    --metadata-jsonl train\reports\source_truth_raw_metadata.jsonl `
    --summary-json train\reports\source_truth_window_plan_summary.json `
    --plan-csv train\reports\source_truth_window_plan_sources.csv
}

Run-Step "source_truth_materialize" {
  C:\WINDOWS\py.exe -3.11 scripts\wake_phase0_materialize_manifest.py `
    --source-plan-csv train\reports\source_truth_window_plan_sources.csv `
    --output-jsonl train\manifests\source_truth_conditioning_worklist.jsonl `
    --summary-json train\reports\source_truth_conditioning_worklist_summary.json
}

Run-Step "source_truth_condition_audio" {
  C:\WINDOWS\py.exe -3.11 scripts\wake_phase0_condition_audio.py `
    --worklist-jsonl train\manifests\source_truth_conditioning_worklist.jsonl `
    --output-root train\conditioned\adaptive_hot_v2_5 `
    --output-manifest-jsonl train\manifests\source_truth_conditioned_manifest.jsonl `
    --summary-json train\reports\source_truth_conditioned_manifest_summary.json `
    --delete-source-on-success `
    --delete-train-root train
}
