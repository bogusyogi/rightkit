param(
  [switch]$Execute
)

$ErrorActionPreference = "Stop"

$trainRoot = "D:\Claude\heardright\train"
$retiredRoot = Join-Path $trainRoot "_retired_noncanonical_20260609"
$reportPath = Join-Path $trainRoot "reports\delete_retired_noncanonical_20260609.json"

$targets = @(
  "conditioned_adaptive_hot_v2_5_unified_candidate",
  "conditioned_adaptive_hot_v2_5_legacy_archive",
  "corpus_expansion_20260607_conditioned"
) | ForEach-Object { Join-Path $retiredRoot $_ }

function Resolve-ExistingOrParent {
  param([string]$Path)
  if (Test-Path -LiteralPath $Path) {
    return (Resolve-Path -LiteralPath $Path).Path
  }
  $parent = Split-Path -Parent $Path
  if (-not (Test-Path -LiteralPath $parent)) {
    throw "Parent does not exist: $parent"
  }
  return (Join-Path (Resolve-Path -LiteralPath $parent).Path (Split-Path -Leaf $Path))
}

$resolvedRoot = (Resolve-Path -LiteralPath $retiredRoot).Path.TrimEnd("\")
$resolvedTargets = foreach ($target in $targets) {
  $resolved = (Resolve-ExistingOrParent -Path $target).TrimEnd("\")
  if (-not ($resolved.StartsWith($resolvedRoot + "\", [System.StringComparison]::OrdinalIgnoreCase))) {
    throw "Refusing target outside retired root: $resolved"
  }
  if ($resolved -match "(^|\\)pico(\\|$)") {
    throw "Refusing target that includes pico: $resolved"
  }
  $resolved
}

$events = New-Object System.Collections.Generic.List[object]

function Write-Report {
  param([string]$State)
  $report = [pscustomobject]@{
    generated_at = (Get-Date).ToString("s")
    state = $State
    execute = [bool]$Execute
    retired_root = $resolvedRoot
    targets = $resolvedTargets
    events = $events
  }
  $report | ConvertTo-Json -Depth 6 | Set-Content -LiteralPath $reportPath -Encoding UTF8
}

Write-Report -State "started"

if ($Execute) {
  foreach ($target in $resolvedTargets) {
    $existsBefore = Test-Path -LiteralPath $target
    $events.Add([pscustomobject]@{
      time = (Get-Date).ToString("s")
      target = $target
      status = if ($existsBefore) { "delete_started" } else { "already_absent" }
    })
    Write-Report -State "running"
    if ($existsBefore) {
      [System.IO.Directory]::Delete($target, $true)
    }
    $existsAfter = Test-Path -LiteralPath $target
    $events.Add([pscustomobject]@{
      time = (Get-Date).ToString("s")
      target = $target
      status = if ($existsAfter) { "delete_incomplete" } else { "deleted" }
    })
    Write-Report -State "running"
  }
}

Write-Report -State "complete"
Get-Content -LiteralPath $reportPath -Raw
