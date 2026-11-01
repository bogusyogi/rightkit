param(
  [string]$RepoRoot = "D:\Claude\heardright",
  [string]$InputList = "train\manifests\source_truth_final_manifest_inputs.txt",
  [string]$OutputJsonl = "train\manifests\source_truth_final_latest_merged.jsonl",
  [string]$SummaryJson = "train\reports\source_truth_final_latest_audit_summary.json",
  [string]$SqliteDb = "train\reports\source_truth_final_latest_audit.sqlite",
  [string]$InputAuditJson = "train\reports\source_truth_final_manifest_inputs_audit.json",
  [string]$RawReportCsv = "train\reports\source_truth_final_residual_raws.csv",
  [string]$BucketReportCsv = "train\reports\source_truth_final_bucket_report.csv",
  [string]$ExpectedBucketsCsv = "train\reports\source_truth_expected_buckets.csv",
  [string]$ConditionedRoot = "D:\Claude\heardright\train\conditioned\adaptive_hot_v2_5",
  [switch]$VerifyOutputs,
  [switch]$ScanRawRoots,
  [switch]$ValidateEvidence,
  [switch]$AllowMissingMusicSplit
)

$ErrorActionPreference = "Stop"
Set-Location $RepoRoot

$ownedPattern = "source_truth_blockrun_20260608_0140|e_archive_stream_20260607_211948"
$activeOwned = Get-CimInstance Win32_Process |
  Where-Object {
    $_.CommandLine -and
    $_.CommandLine -match "wake_phase0_condition_audio.py" -and
    $_.CommandLine -match $ownedPattern
  } |
  Select-Object ProcessId, ParentProcessId, Name, CommandLine

if ($activeOwned) {
  $activeOwned | ConvertTo-Json -Depth 3
  throw "Owned D/E conditioning lanes are still active; final audit refused to compete with them."
}

$python = "C:\Users\adrds\AppData\Local\Programs\Python\Python311\python.exe"
$argsList = @(
  "scripts\wake_source_truth_final_audit.py",
  "--sqlite-db", $SqliteDb,
  "--output-jsonl", $OutputJsonl,
  "--summary-json", $SummaryJson,
  "--bucket-report-csv", $BucketReportCsv,
  "--expected-buckets-csv", $ExpectedBucketsCsv,
  "--conditioned-root", $ConditionedRoot
)
if ($VerifyOutputs) {
  $argsList += "--verify-outputs"
}
if ($ScanRawRoots) {
  $argsList += @(
    "--raw-report-csv", $RawReportCsv,
    "--raw-root", "D:\Claude\heardright\train\audiobooks",
    "--raw-root", "D:\Claude\heardright\train\tv",
    "--raw-root", "D:\Claude\heardright\train\music",
    "--raw-root", "D:\Claude\heardright\train\music-lyric",
    "--raw-root", "D:\Claude\heardright\train\masterclass",
    "--raw-root", "D:\Claude\heardright\train\youtube",
    "--raw-root", "D:\Claude\heardright\train\positives",
    "--raw-root", "D:\Claude\heardright\train\corpus_expansion_20260607\raw",
    "--raw-root", "E:\heardright_archive\wake_train_a5_2026_05_11"
  )
}
if ($AllowMissingMusicSplit) {
  $argsList += "--allow-missing-music-split"
}

$inputs = Get-Content $InputList | Where-Object { $_ -and $_.Trim() }
$inputAuditRows = @()
$inputIndex = 0
foreach ($inputPath in $inputs) {
  if (-not (Test-Path -LiteralPath $inputPath)) {
    throw "Manifest input is missing: $inputPath"
  }
  $item = Get-Item -LiteralPath $inputPath
  $hash = Get-FileHash -LiteralPath $inputPath -Algorithm SHA256
  $inputAuditRows += [pscustomobject]@{
    order = $inputIndex
    path = $item.FullName
    bytes = $item.Length
    last_write_time = $item.LastWriteTime.ToString("o")
    sha256 = $hash.Hash.ToLowerInvariant()
  }
  $inputIndex += 1
}
if (-not (Test-Path -LiteralPath $ExpectedBucketsCsv)) {
  throw "Expected bucket contract is missing: $ExpectedBucketsCsv"
}
$argsList += $inputs

New-Item -ItemType Directory -Force -Path (Split-Path $OutputJsonl), (Split-Path $SummaryJson), (Split-Path $SqliteDb), (Split-Path $InputAuditJson), (Split-Path $RawReportCsv), (Split-Path $BucketReportCsv) | Out-Null

[ordered]@{
  created_local = (Get-Date).ToString("s")
  input_list = [System.IO.Path]::GetFullPath((Join-Path $RepoRoot $InputList))
  input_count = $inputAuditRows.Count
  merge_order = "later input rows supersede earlier rows by deterministic id"
  allow_missing_music_split = [bool]$AllowMissingMusicSplit
  verify_outputs = [bool]$VerifyOutputs
  scan_raw_roots = [bool]$ScanRawRoots
  inputs = $inputAuditRows
} | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath $InputAuditJson -Encoding utf8

& $python @argsList
$auditExit = $LASTEXITCODE
if ($auditExit -ne 0) {
  exit $auditExit
}

if ($ValidateEvidence) {
  & $python @(
    "scripts\wake_source_truth_validate_final_evidence.py",
    "--repo-root", $RepoRoot,
    "--merged-manifest", $OutputJsonl,
    "--summary-json", $SummaryJson,
    "--input-audit-json", $InputAuditJson,
    "--input-list", $InputList,
    "--residual-raws-csv", $RawReportCsv,
    "--bucket-report-csv", $BucketReportCsv,
    "--expected-buckets-csv", $ExpectedBucketsCsv
  )
  exit $LASTEXITCODE
}

exit 0
