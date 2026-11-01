param(
  [string]$RepoRoot = "D:\Claude\heardright",
  [ValidateSet("condition_retry_m4a_ffmpeg", "near_phonetic_tts", "near_phonetic_inventory", "mic_soak_musan_aug")]
  [string]$Job
)

$ErrorActionPreference = "Stop"
if (Get-Variable -Name PSNativeCommandUseErrorActionPreference -ErrorAction SilentlyContinue) {
  $PSNativeCommandUseErrorActionPreference = $false
}
$logDir = Join-Path $RepoRoot "train\corpus_expansion_20260607\logs"
New-Item -ItemType Directory -Force -Path $logDir | Out-Null
Set-Location $RepoRoot

function Run-Step {
  param(
    [string]$Name,
    [scriptblock]$Body
  )
  $out = Join-Path $logDir "$Name.out.log"
  $err = Join-Path $logDir "$Name.err.log"
  $status = Join-Path $logDir "$Name.status.json"
  $started = Get-Date
  try {
    & $Body > $out 2> $err
    $code = if ($LASTEXITCODE -ne $null) { $LASTEXITCODE } else { 0 }
    [ordered]@{
      name = $Name
      ok = ($code -eq 0)
      exit_code = $code
      started = $started.ToString("o")
      finished = (Get-Date).ToString("o")
    } | ConvertTo-Json | Set-Content -LiteralPath $status -Encoding utf8
  } catch {
    $_ | Out-String | Set-Content -LiteralPath $err -Encoding utf8
    [ordered]@{
      name = $Name
      ok = $false
      exit_code = -1
      error = $_.Exception.Message
      started = $started.ToString("o")
      finished = (Get-Date).ToString("o")
    } | ConvertTo-Json | Set-Content -LiteralPath $status -Encoding utf8
  }
}

if ($Job -eq "condition_retry_m4a_ffmpeg") {
Run-Step "condition_retry_m4a_ffmpeg" {
  C:\WINDOWS\py.exe -3.11 scripts\wake_phase0_condition_audio.py `
    --worklist-jsonl train\corpus_expansion_20260607\manifests\raw_expansion_conditioning_retry_failed_m4a_worklist.jsonl `
    --output-root train\corpus_expansion_20260607\conditioned\adaptive_hot_v2_5 `
    --output-manifest-jsonl train\corpus_expansion_20260607\manifests\raw_expansion_conditioned_retry_m4a_manifest.jsonl `
    --summary-json train\corpus_expansion_20260607\reports\raw_expansion_conditioned_retry_m4a_summary.json
}
}

if ($Job -eq "near_phonetic_tts") {
Run-Step "near_phonetic_tts" {
  powershell.exe -NoProfile -ExecutionPolicy Bypass -File scripts\wake_near_phonetic_tts.ps1 `
    -ExpansionRoot "D:\Claude\heardright\train\corpus_expansion_20260607"
}
}

if ($Job -eq "near_phonetic_inventory") {
Run-Step "near_phonetic_inventory" {
  C:\WINDOWS\py.exe -3.11 scripts\wake_near_phonetic_inventory.py --cap-per-term 50
}
}

if ($Job -eq "mic_soak_musan_aug") {
Run-Step "mic_soak_musan_aug" {
  cmd.exe /c "C:\WINDOWS\py.exe -3.11 scripts\wake_musan_augment_soak.py --augments-per-soak 6"
}
}
