param(
  [string]$TrainRoot = "D:\Claude\heardright\train",
  [string]$RetiredRoot = "D:\Claude\heardright\train\_retired_noncanonical_20260609",
  [string]$ReportJson = "D:\Claude\heardright\train\reports\zephyr_quarantine_noncanonical_20260609.json",
  [switch]$Execute
)

$ErrorActionPreference = "Stop"

function Resolve-ExistingOrParent {
  param([string]$Path)
  if (Test-Path -LiteralPath $Path) {
    return (Resolve-Path -LiteralPath $Path).Path
  }
  $parent = Split-Path -Parent $Path
  if (-not (Test-Path -LiteralPath $parent)) {
    throw "Parent path does not exist: $parent"
  }
  $parentResolved = (Resolve-Path -LiteralPath $parent).Path
  return [System.IO.Path]::GetFullPath((Join-Path $parentResolved (Split-Path -Leaf $Path)))
}

$trainFull = (Resolve-Path -LiteralPath $TrainRoot).Path
$picoFull = (Resolve-Path -LiteralPath (Join-Path $TrainRoot "pico")).Path
$retiredFull = Resolve-ExistingOrParent $RetiredRoot

if (-not $retiredFull.StartsWith($trainFull, [System.StringComparison]::OrdinalIgnoreCase)) {
  throw "Retired root must stay under train root: $retiredFull"
}

$targets = @(
  [ordered]@{
    name = "unified_candidate"
    source = Join-Path $TrainRoot "conditioned\adaptive_hot_v2_5\unified_candidate"
    destination = Join-Path $RetiredRoot "conditioned_adaptive_hot_v2_5_unified_candidate"
  },
  [ordered]@{
    name = "legacy_archive"
    source = Join-Path $TrainRoot "conditioned\adaptive_hot_v2_5\legacy_archive"
    destination = Join-Path $RetiredRoot "conditioned_adaptive_hot_v2_5_legacy_archive"
  },
  [ordered]@{
    name = "corpus_expansion_conditioned"
    source = Join-Path $TrainRoot "corpus_expansion_20260607\conditioned"
    destination = Join-Path $RetiredRoot "corpus_expansion_20260607_conditioned"
  }
)

$results = @()
foreach ($target in $targets) {
  $source = $target.source
  $destination = $target.destination
  $row = [ordered]@{
    name = $target.name
    source = $source
    destination = $destination
    source_exists_before = Test-Path -LiteralPath $source
    destination_exists_before = Test-Path -LiteralPath $destination
    action = if ($Execute) { "move" } else { "dry_run" }
    ok = $false
    error = ""
  }

  try {
    if (-not $row.source_exists_before) {
      $row.action = "skip_missing_source"
      $row.ok = $true
      $results += [pscustomobject]$row
      continue
    }

    $sourceFull = (Resolve-Path -LiteralPath $source).Path
    if (-not $sourceFull.StartsWith($trainFull, [System.StringComparison]::OrdinalIgnoreCase)) {
      throw "Source outside train root: $sourceFull"
    }
    if ($sourceFull.StartsWith($picoFull, [System.StringComparison]::OrdinalIgnoreCase)) {
      throw "Refusing to touch pico path: $sourceFull"
    }
    if (Test-Path -LiteralPath $destination) {
      throw "Destination already exists: $destination"
    }

    if ($Execute) {
      New-Item -ItemType Directory -Force -Path $RetiredRoot | Out-Null
      Move-Item -LiteralPath $sourceFull -Destination $destination -ErrorAction Stop
      $row.source_exists_after = Test-Path -LiteralPath $source
      $row.destination_exists_after = Test-Path -LiteralPath $destination
    } else {
      $row.source_exists_after = $row.source_exists_before
      $row.destination_exists_after = $row.destination_exists_before
    }
    $row.ok = $true
  } catch {
    $row.error = $_.Exception.Message
  }
  $results += [pscustomobject]$row
}

$summary = [ordered]@{
  generated_at = (Get-Date).ToString("s")
  execute = [bool]$Execute
  train_root = $trainFull
  pico_root = $picoFull
  retired_root = $retiredFull
  results = $results
}

New-Item -ItemType Directory -Force -Path (Split-Path -Parent $ReportJson) | Out-Null
$summary | ConvertTo-Json -Depth 6 | Set-Content -LiteralPath $ReportJson -Encoding UTF8
$summary | ConvertTo-Json -Depth 6
