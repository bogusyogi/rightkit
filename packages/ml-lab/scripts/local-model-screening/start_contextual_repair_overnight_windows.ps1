param(
  [string]$RunRoot = "$env:LOCALAPPDATA\HeardRightNext\bakeoffs\l1-local-contextual-repair-v1",
  [string]$ApprovalLedger = "",
  [string]$ModelRoot = "E:\models\heardright-local-screening",
  [string]$LlamaServer = "E:\models\heardright-local-screening\runtimes\llama.cpp\b10069\cuda-12.4\llama-server.exe",
  [switch]$DryRun
)

$ErrorActionPreference = "Stop"
$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$Supervisor = Join-Path $ScriptDir "run_contextual_repair_overnight.mjs"
$Node = (Get-Command node.exe -ErrorAction Stop).Source
$RunRoot = [IO.Path]::GetFullPath($RunRoot)
if (-not $ApprovalLedger) {
  $ApprovalLedger = Join-Path $RunRoot "private\approval-ledger.json"
}
$ApprovalLedger = [IO.Path]::GetFullPath($ApprovalLedger)
$ModelRoot = [IO.Path]::GetFullPath($ModelRoot)
$LlamaServer = [IO.Path]::GetFullPath($LlamaServer)

foreach ($Required in @($Supervisor, $ModelRoot, $LlamaServer)) {
  if (-not (Test-Path -LiteralPath $Required)) {
    throw "Required overnight input is missing: $Required"
  }
}

$Arguments = @(
  $Supervisor,
  "--live",
  "--run-root", $RunRoot,
  "--approval-ledger", $ApprovalLedger,
  "--model-root", $ModelRoot,
  "--llama-server", $LlamaServer
)

if ($DryRun) {
  [pscustomobject]@{
    status = "detached_launcher_dry_run_no_generation"
    node = $Node
    supervisor = $Supervisor
    run_root = $RunRoot
    gpu_concurrency = 1
    window_style = "Hidden"
  } | ConvertTo-Json -Compress
  exit 0
}

if (-not (Test-Path -LiteralPath $ApprovalLedger)) {
  throw "Approved ledger is missing: $ApprovalLedger"
}

$LogDir = Join-Path $RunRoot "logs"
New-Item -ItemType Directory -Force -Path $LogDir | Out-Null
$LaunchState = Join-Path $LogDir "overnight-launch.json"
if (Test-Path -LiteralPath $LaunchState) {
  try {
    $Existing = Get-Content -LiteralPath $LaunchState -Raw | ConvertFrom-Json
    if ($Existing.pid -and (Get-Process -Id $Existing.pid -ErrorAction SilentlyContinue)) {
      throw "Overnight supervisor already active as PID $($Existing.pid)"
    }
  } catch {
    if ($_.Exception.Message -like "Overnight supervisor already active*") { throw }
  }
}

$Stamp = Get-Date -Format "yyyyMMdd-HHmmss"
$Stdout = Join-Path $LogDir "overnight-$Stamp.stdout.log"
$Stderr = Join-Path $LogDir "overnight-$Stamp.stderr.log"
$Process = Start-Process -FilePath $Node `
  -ArgumentList $Arguments `
  -RedirectStandardOutput $Stdout `
  -RedirectStandardError $Stderr `
  -WindowStyle Hidden `
  -PassThru

[pscustomobject]@{
  schema = "heardright.contextual_repair_overnight_launch.v1"
  pid = $Process.Id
  started_at = (Get-Date).ToUniversalTime().ToString("o")
  run_root = $RunRoot
  stdout = $Stdout
  stderr = $Stderr
  event_log = (Join-Path $LogDir "overnight.events.jsonl")
  completion_report = (Join-Path $RunRoot "reports\overnight.complete.json")
  gpu_concurrency = 1
} | ConvertTo-Json | Set-Content -LiteralPath $LaunchState -Encoding utf8

Get-Content -LiteralPath $LaunchState -Raw
