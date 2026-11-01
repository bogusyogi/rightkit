param(
  [ValidateSet("download", "verify")]
  [string]$Mode,
  [string]$Root = "E:\models\heardright-local-screening",
  [string]$LlamaCli = "E:\models\heardright-local-screening\runtimes\llama.cpp\b10069\cuda-12.4\llama-cli.exe",
  [int]$Workers = 3,
  [string[]]$Only = @(),
  [switch]$VerifyOnly
)

$ErrorActionPreference = "Stop"
$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$LogDir = Join-Path $Root "logs"
New-Item -ItemType Directory -Force -Path $LogDir | Out-Null
$Stamp = Get-Date -Format "yyyyMMdd-HHmmss"
$Stdout = Join-Path $LogDir "$Mode-$Stamp.stdout.log"
$Stderr = Join-Path $LogDir "$Mode-$Stamp.stderr.log"
$HfCommand = Get-Command hf.exe -ErrorAction SilentlyContinue
$HfPython = if ($HfCommand) {
  Join-Path (Split-Path -Parent (Split-Path -Parent $HfCommand.Source)) "python.exe"
}
$Python = if ($HfPython -and (Test-Path -LiteralPath $HfPython)) {
  $HfPython
} else {
  (Get-Command python.exe -ErrorAction Stop).Source
}

if ($Mode -eq "download") {
  $Arguments = @(
    (Join-Path $ScriptDir "download_models.py"),
    "--root", $Root,
    "--workers", $Workers
  )
  if ($VerifyOnly) {
    $Arguments += "--verify-only"
  }
} else {
  $Arguments = @(
    (Join-Path $ScriptDir "verify_load.py"),
    "--root", $Root,
    "--llama-cli", $LlamaCli
  )
}
foreach ($Candidate in $Only) {
  foreach ($CandidateId in ($Candidate -split ",")) {
    if ($CandidateId) {
      $Arguments += @("--only", $CandidateId)
    }
  }
}

$Process = Start-Process -FilePath $Python `
  -ArgumentList $Arguments `
  -RedirectStandardOutput $Stdout `
  -RedirectStandardError $Stderr `
  -WindowStyle Hidden `
  -PassThru

$State = [ordered]@{
  schema = "heardright.detached_model_job.v1"
  mode = $Mode
  pid = $Process.Id
  started_at = (Get-Date).ToUniversalTime().ToString("o")
  stdout = $Stdout
  stderr = $Stderr
}
$StatePath = Join-Path $Root "active-$Mode-job.json"
$State | ConvertTo-Json | Set-Content -LiteralPath $StatePath -Encoding utf8

Write-Host "Started detached $Mode job PID $($Process.Id)"
Write-Host "State:  $StatePath"
Write-Host "Stdout: $Stdout"
Write-Host "Stderr: $Stderr"
