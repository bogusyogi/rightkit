param(
  [switch]$Execute
)

$ErrorActionPreference = "Stop"

$repoRoot = "D:\Claude\heardright"
$trainRoot = Join-Path $repoRoot "train"
$reports = Join-Path $trainRoot "reports"
$failedSources = Join-Path $reports "source_truth_packed_current_20260609_failed_sources.jsonl"
$decisionPath = Join-Path $reports "source_truth_packed_current_20260609_failed_exclusion_decision.json"
$deleteReportPath = Join-Path $reports "delete_safe_remaining_raws_20260609.json"

function Resolve-WithinTrain {
  param([string]$RawPath)
  $path = $RawPath
  if (-not [System.IO.Path]::IsPathRooted($path)) {
    $path = Join-Path $repoRoot $path
  }
  if (Test-Path -LiteralPath $path) {
    $resolved = (Resolve-Path -LiteralPath $path).Path
  } else {
    $parent = Split-Path -Parent $path
    if (Test-Path -LiteralPath $parent) {
      $resolved = Join-Path (Resolve-Path -LiteralPath $parent).Path (Split-Path -Leaf $path)
    } else {
      $resolved = $path
    }
  }
  $trainResolved = (Resolve-Path -LiteralPath $trainRoot).Path.TrimEnd("\")
  if (-not ($resolved.StartsWith($trainResolved + "\", [System.StringComparison]::OrdinalIgnoreCase))) {
    throw "Refusing path outside train root: $resolved"
  }
  if ($resolved -match "(^|\\)pico(\\|$)") {
    throw "Refusing path under pico: $resolved"
  }
  return $resolved
}

function Measure-ExistingFile {
  param([string]$Path)
  if (Test-Path -LiteralPath $Path -PathType Leaf) {
    $item = Get-Item -LiteralPath $Path
    return [pscustomobject]@{ path = $Path; exists = $true; bytes = $item.Length }
  }
  return [pscustomobject]@{ path = $Path; exists = $false; bytes = 0 }
}

$failedRows = @()
if (Test-Path -LiteralPath $failedSources) {
  Get-Content -LiteralPath $failedSources | Where-Object { $_.Trim() } | ForEach-Object {
    $failedRows += ($_ | ConvertFrom-Json)
  }
}

$badSemantics = @($failedRows | Where-Object { $_.semantic_bucket -notin @("music_lyrics", "read_speech") })
$badFamilies = @($failedRows | Where-Object { -not $_.error_families.decode_timeout })
if ($badSemantics.Count -gt 0 -or $badFamilies.Count -gt 0) {
  throw "Failed rows include unexpected semantics or non-timeout families; refusing automatic raw delete."
}

$failedSourcePaths = @($failedRows | ForEach-Object { Resolve-WithinTrain -RawPath ([string]$_.source_path) } | Sort-Object -Unique)
$knownRawRoots = @(
  (Join-Path $trainRoot "audiobooks"),
  (Join-Path $trainRoot "corpus_expansion_20260607\raw")
) | ForEach-Object { Resolve-WithinTrain -RawPath $_ }

$decision = [pscustomobject]@{
  generated_at = (Get-Date).ToString("s")
  decision = "exclude_failed_decode_timeout_rows"
  reason = "All failed packed rows are optional negative/read-speech or music decode timeouts; no positives or near negatives."
  failed_rows = ($failedRows | Measure-Object failed_rows -Sum).Sum
  failed_sources = $failedSourcePaths.Count
  allowed_semantic_buckets = @("music_lyrics", "read_speech")
  delete_failed_sources = $failedSourcePaths
  delete_raw_roots_after_failed_source_decision = $knownRawRoots
}
$decision | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath $decisionPath -Encoding UTF8

$beforeFiles = @($failedSourcePaths | ForEach-Object { Measure-ExistingFile -Path $_ })
$events = New-Object System.Collections.Generic.List[object]

function Add-Event {
  param([string]$Path, [string]$Status, [long]$Bytes = 0)
  $events.Add([pscustomobject]@{
    time = (Get-Date).ToString("s")
    path = $Path
    status = $Status
    bytes = $Bytes
  })
}

if ($Execute) {
  foreach ($path in $failedSourcePaths) {
    if (Test-Path -LiteralPath $path -PathType Leaf) {
      $bytes = (Get-Item -LiteralPath $path).Length
      Remove-Item -LiteralPath $path -Force
      Add-Event -Path $path -Status "deleted_failed_source" -Bytes $bytes
    } else {
      Add-Event -Path $path -Status "already_absent"
    }
  }

  foreach ($root in $knownRawRoots) {
    if (Test-Path -LiteralPath $root) {
      $remainingAudio = @(Get-ChildItem -LiteralPath $root -File -Recurse -Include *.wav,*.flac,*.mp3,*.m4a,*.ogg,*.opus,*.aac,*.wma,*.webm -ErrorAction SilentlyContinue)
      if ($remainingAudio.Count -eq 0) {
        [System.IO.Directory]::Delete($root, $true)
        Add-Event -Path $root -Status "deleted_empty_raw_root"
      } else {
        Add-Event -Path $root -Status "raw_root_not_empty_after_failed_source_delete" -Bytes $remainingAudio.Count
      }
    } else {
      Add-Event -Path $root -Status "raw_root_absent"
    }
  }
}

$afterFiles = @($failedSourcePaths | ForEach-Object { Measure-ExistingFile -Path $_ })
$report = [pscustomobject]@{
  generated_at = (Get-Date).ToString("s")
  execute = [bool]$Execute
  decision_report = $decisionPath
  before_failed_sources = $beforeFiles
  after_failed_sources = $afterFiles
  events = $events
}
$report | ConvertTo-Json -Depth 6 | Set-Content -LiteralPath $deleteReportPath -Encoding UTF8
$report | ConvertTo-Json -Depth 6
