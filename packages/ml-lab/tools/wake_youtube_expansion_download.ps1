param(
  [string]$ExpansionRoot = "D:\Claude\heardright\train\corpus_expansion_20260607",
  [string]$QueryFile = "",
  [int]$PerQuery = 3
)

$ErrorActionPreference = "Stop"
if (Get-Variable -Name PSNativeCommandUseErrorActionPreference -ErrorAction SilentlyContinue) {
  $PSNativeCommandUseErrorActionPreference = $false
}
if (-not $QueryFile) {
  $QueryFile = Join-Path $ExpansionRoot "youtube_queries.txt"
}
$outDir = Join-Path $ExpansionRoot "raw\youtube\fresh_scrape"
$archive = Join-Path $ExpansionRoot "manifests\youtube_download_archive.txt"
$log = Join-Path $ExpansionRoot "logs\youtube_fresh_scrape.log"
New-Item -ItemType Directory -Force -Path $outDir, (Split-Path $archive), (Split-Path $log) | Out-Null

if (-not (Get-Command yt-dlp -ErrorAction SilentlyContinue)) {
  throw "yt-dlp is not available on PATH"
}

$queries = Get-Content -LiteralPath $QueryFile | Where-Object { $_.Trim() -and -not $_.Trim().StartsWith("#") }
foreach ($query in $queries) {
  $safe = ($query -replace '[^a-zA-Z0-9]+','_').Trim('_').ToLowerInvariant()
  $target = Join-Path $outDir $safe
  New-Item -ItemType Directory -Force -Path $target | Out-Null
  $search = "ytsearch$($PerQuery):$query"
  "[$(Get-Date -Format o)] $search" | Tee-Object -FilePath $log -Append
  $previousErrorActionPreference = $ErrorActionPreference
  $ErrorActionPreference = "Continue"
  try {
    yt-dlp `
      --ignore-errors `
      --no-playlist `
      --download-archive $archive `
      --write-info-json `
      --extract-audio `
      --audio-format m4a `
      --audio-quality 0 `
      --paths $target `
      --output "%(id)s.%(ext)s" `
      $search 2>&1 | Tee-Object -FilePath $log -Append
  } finally {
    $ErrorActionPreference = $previousErrorActionPreference
  }
}
