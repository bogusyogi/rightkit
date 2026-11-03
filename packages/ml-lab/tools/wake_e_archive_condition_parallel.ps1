param(
  [string]$RepoRoot = "D:\Claude\heardright",
  [string]$ArchiveRoot = "E:\heardright_archive\wake_train_a5_2026_05_11",
  [string]$Tag = ""
)

$ErrorActionPreference = "Stop"
Set-Location $RepoRoot
if (-not $Tag) {
  $Tag = "e_archive_parallel_{0}" -f (Get-Date -Format "yyyyMMdd_HHmmss")
}

New-Item -ItemType Directory -Force -Path "train\logs", "train\reports", "train\manifests", "train\conditioned\adaptive_hot_v2_5" | Out-Null

$python = "C:\Users\adrds\AppData\Local\Programs\Python\Python311\python.exe"
$metadataJsonl = "train\reports\${Tag}_raw_metadata.jsonl"
$worklistJsonl = "train\manifests\${Tag}_conditioning_worklist.jsonl"
$summaryJson = "train\reports\${Tag}_worklist_summary.json"
$conditionManifest = "train\manifests\${Tag}_conditioned_manifest.jsonl"
$conditionSummary = "train\reports\${Tag}_conditioned_manifest_summary.json"

& $python scripts\wake_e_archive_worklist.py `
  --archive-root $ArchiveRoot `
  --raw-metadata-jsonl $metadataJsonl `
  --worklist-jsonl $worklistJsonl `
  --summary-json $summaryJson `
  > "train\logs\${Tag}_worklist.out.log" 2> "train\logs\${Tag}_worklist.err.log"
$worklistExit = $LASTEXITCODE
if (($worklistExit -ne 0) -and (-not (Test-Path $worklistJsonl))) {
  throw "E archive worklist failed before writing worklist, exit_code=$worklistExit"
}

$args = @(
  "scripts\wake_phase0_condition_audio.py",
  "--worklist-jsonl", $worklistJsonl,
  "--output-root", "train\conditioned\adaptive_hot_v2_5",
  "--output-manifest-jsonl", $conditionManifest,
  "--summary-json", $conditionSummary,
  "--delete-source-on-success",
  "--delete-train-root", $ArchiveRoot
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
  throw "E archive condition_audio failed with exit_code=$($proc.ExitCode)"
}
