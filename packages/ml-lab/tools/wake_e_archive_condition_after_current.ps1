param(
  [string]$RepoRoot = "D:\Claude\heardright",
  [string]$ArchiveRoot = "E:\heardright_archive\wake_train_a5_2026_05_11",
  [string]$Tag = "",
  [string]$WaitForCommandPattern = "remaining_20260607_1824|wake_source_truth_remaining_pass.ps1"
)

$ErrorActionPreference = "Stop"
Set-Location $RepoRoot
if (-not $Tag) {
  $Tag = "e_archive_{0}" -f (Get-Date -Format "yyyyMMdd_HHmmss")
}

New-Item -ItemType Directory -Force -Path "train\logs", "train\reports", "train\manifests", "train\conditioned\adaptive_hot_v2_5" | Out-Null

while ($true) {
  $active = Get-CimInstance Win32_Process |
    Where-Object { $_.CommandLine -match $WaitForCommandPattern -or $_.CommandLine -match "wake_phase0_condition_audio.py" } |
    Where-Object { $_.CommandLine -notmatch [regex]::Escape($Tag) }
  if (-not $active) {
    break
  }
  Start-Sleep -Seconds 60
}

$python = "C:\Users\adrds\AppData\Local\Programs\Python\Python311\python.exe"
$metadataJsonl = "train\reports\${Tag}_raw_metadata.jsonl"
$worklistJsonl = "train\manifests\${Tag}_conditioning_worklist.jsonl"
$summaryJson = "train\reports\${Tag}_worklist_summary.json"
$conditionSummary = "train\reports\${Tag}_conditioned_manifest_summary.json"

& $python scripts\wake_e_archive_worklist.py `
  --archive-root $ArchiveRoot `
  --raw-metadata-jsonl $metadataJsonl `
  --worklist-jsonl $worklistJsonl `
  --summary-json $summaryJson `
  > "train\logs\${Tag}_worklist.out.log" 2> "train\logs\${Tag}_worklist.err.log"
if ($LASTEXITCODE -ne 0) {
  throw "E archive worklist failed with exit_code=$LASTEXITCODE"
}

$args = @(
  "scripts\wake_phase0_condition_audio.py",
  "--worklist-jsonl", $worklistJsonl,
  "--output-root", "train\conditioned\adaptive_hot_v2_5",
  "--output-manifest-jsonl", "train\manifests\source_truth_conditioned_manifest.jsonl",
  "--summary-json", $conditionSummary,
  "--delete-source-on-success",
  "--delete-train-root", $ArchiveRoot
)
$out = "train\logs\${Tag}_condition_audio.child.out.log"
$err = "train\logs\${Tag}_condition_audio.child.err.log"
$proc = Start-Process -FilePath $python -ArgumentList $args -WorkingDirectory $RepoRoot -WindowStyle Hidden -RedirectStandardOutput $out -RedirectStandardError $err -PassThru
try {
  $proc.PriorityClass = "Idle"
} catch {
}
$proc.WaitForExit()
if ($proc.ExitCode -ne 0) {
  throw "E archive condition_audio failed with exit_code=$($proc.ExitCode)"
}
