$ErrorActionPreference = "Stop"

$repo = "D:\Claude\heardright"
$python = "C:\Users\adrds\AppData\Local\Programs\Python\Python311\python.exe"
$out = Join-Path $repo "train\logs\source_truth_condition_schtask_low.out.log"
$err = Join-Path $repo "train\logs\source_truth_condition_schtask_low.err.log"

Set-Location $repo
$p = Start-Process -FilePath $python -ArgumentList @(
    "scripts\wake_phase0_condition_audio.py",
    "--worklist-jsonl", "train\manifests\source_truth_conditioning_worklist.jsonl",
    "--output-root", "train\conditioned\adaptive_hot_v2_5",
    "--output-manifest-jsonl", "train\manifests\source_truth_conditioned_manifest.jsonl",
    "--summary-json", "train\reports\source_truth_conditioned_manifest_summary.json",
    "--delete-source-on-success",
    "--delete-train-root", "train"
) -WorkingDirectory $repo -WindowStyle Hidden -RedirectStandardOutput $out -RedirectStandardError $err -PassThru

try {
    $p.PriorityClass = "Idle"
} catch {
}

$p.WaitForExit()
exit $p.ExitCode
