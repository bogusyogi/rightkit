param(
  [switch]$Execute
)

$ErrorActionPreference = "Stop"

$trainRoot = "D:\Claude\heardright\train"
$reportPath = Join-Path $trainRoot "reports\delete_raw_shell_roots_20260609.json"
$targets = @(
  (Join-Path $trainRoot "audiobooks"),
  (Join-Path $trainRoot "corpus_expansion_20260607\raw")
)

$trainResolved = (Resolve-Path -LiteralPath $trainRoot).Path.TrimEnd("\")
$events = New-Object System.Collections.Generic.List[object]

foreach ($target in $targets) {
  if (-not (Test-Path -LiteralPath $target)) {
    $events.Add([pscustomobject]@{ path = $target; status = "already_absent"; files = 0; bytes = 0 })
    continue
  }
  $resolved = (Resolve-Path -LiteralPath $target).Path.TrimEnd("\")
  if (-not ($resolved.StartsWith($trainResolved + "\", [System.StringComparison]::OrdinalIgnoreCase))) {
    throw "Refusing target outside train root: $resolved"
  }
  if ($resolved -match "(^|\\)pico(\\|$)") {
    throw "Refusing target under pico: $resolved"
  }

  $files = @(Get-ChildItem -LiteralPath $resolved -File -Recurse -Force -ErrorAction SilentlyContinue)
  $bytes = [int64](($files | Measure-Object Length -Sum).Sum)
  if ($Execute) {
    [System.IO.Directory]::Delete($resolved, $true)
    $status = if (Test-Path -LiteralPath $resolved) { "delete_incomplete" } else { "deleted" }
  } else {
    $status = "dry_run"
  }
  $events.Add([pscustomobject]@{ path = $resolved; status = $status; files = $files.Count; bytes = $bytes })
}

$report = [pscustomobject]@{
  generated_at = (Get-Date).ToString("s")
  execute = [bool]$Execute
  events = $events
}
$report | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath $reportPath -Encoding UTF8
$report | ConvertTo-Json -Depth 5
