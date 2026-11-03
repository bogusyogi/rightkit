param(
  [string]$TrainRoot = "D:\Claude\heardright\train",
  [string]$QueryFile = "D:\Claude\heardright\train\youtube\zephyr_title_queries.txt",
  [string]$OutSubdir = "youtube\zephyr_title_under5m",
  [int]$PerQuery = 125,
  [string]$MatchTitle = "(?i)zephyr",
  [string]$MatchFilter = "duration <= 300",
  [string]$CookiesFromBrowser = "",
  [switch]$NoDurationFilter
)

$ErrorActionPreference = "Stop"
if (Get-Variable -Name PSNativeCommandUseErrorActionPreference -ErrorAction SilentlyContinue) {
  $PSNativeCommandUseErrorActionPreference = $false
}

if (-not (Get-Command yt-dlp -ErrorAction SilentlyContinue)) {
  throw "yt-dlp is not available on PATH"
}

$outRoot = Join-Path $TrainRoot $OutSubdir
$archive = Join-Path $TrainRoot "youtube\youtube_download_archive.txt"
$log = Join-Path $TrainRoot ("youtube\logs\" + (($OutSubdir -replace '[\\/]+','_') + ".log"))
New-Item -ItemType Directory -Force -Path $outRoot, (Split-Path $archive), (Split-Path $log) | Out-Null

$queries = Get-Content -LiteralPath $QueryFile | Where-Object { $_.Trim() -and -not $_.Trim().StartsWith("#") }
foreach ($query in $queries) {
  $safe = ($query -replace '[^a-zA-Z0-9]+','_').Trim('_').ToLowerInvariant()
  $target = Join-Path $outRoot $safe
  New-Item -ItemType Directory -Force -Path $target | Out-Null
  $search = "ytsearch$($PerQuery):$query"
  "[$(Get-Date -Format o)] $search title=$MatchTitle filter=$MatchFilter" | Tee-Object -FilePath $log -Append

  $args = @(
    "--ignore-errors",
    "--no-playlist",
    "--download-archive", $archive,
    "--write-info-json",
    "--match-title", $MatchTitle,
    "--extract-audio",
    "--audio-format", "m4a",
    "--audio-quality", "0",
    "--paths", $target,
    "--output", "%(id)s.%(ext)s",
    $search
  )
  if (-not $NoDurationFilter) {
    $args = @("--match-filter", $MatchFilter) + $args
  }
  if ($CookiesFromBrowser) {
    $args = @("--cookies-from-browser", $CookiesFromBrowser) + $args
  }

  $previousErrorActionPreference = $ErrorActionPreference
  $ErrorActionPreference = "Continue"
  try {
    yt-dlp @args 2>&1 | Tee-Object -FilePath $log -Append
  } finally {
    $ErrorActionPreference = $previousErrorActionPreference
  }
}
