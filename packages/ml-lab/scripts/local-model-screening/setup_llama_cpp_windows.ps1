param(
  [string]$Root = "E:\models\heardright-local-screening"
)

$ErrorActionPreference = "Stop"
$Release = "b10069"
$BaseUrl = "https://github.com/ggml-org/llama.cpp/releases/download/$Release"
$DownloadDir = Join-Path $Root "downloads\llama.cpp-$Release"
$RuntimeRoot = Join-Path $Root "runtimes\llama.cpp\$Release"

$Assets = @(
  @{
    Name = "llama-b10069-bin-win-cuda-12.4-x64.zip"
    Bytes = 249038755
    Sha256 = "5933cf07087cc2515ea25654cef7913cdb5f9c29b738cb303a01a01c153014fe"
    Destination = (Join-Path $RuntimeRoot "cuda-12.4")
  },
  @{
    Name = "cudart-llama-bin-win-cuda-12.4-x64.zip"
    Bytes = 391443627
    Sha256 = "8c79a9b226de4b3cacfd1f83d24f962d0773be79f1e7b75c6af4ded7e32ae1d6"
    Destination = (Join-Path $RuntimeRoot "cuda-12.4")
  },
  @{
    Name = "llama-b10069-bin-win-vulkan-x64.zip"
    Bytes = 33271882
    Sha256 = "7705680162610e9586616c8e23f0a757957557fde033a28d2c489140b93b545a"
    Destination = (Join-Path $RuntimeRoot "vulkan")
  }
)

New-Item -ItemType Directory -Force -Path $DownloadDir, $RuntimeRoot | Out-Null

foreach ($Asset in $Assets) {
  $Archive = Join-Path $DownloadDir $Asset.Name
  $NeedsDownload = $true
  if (Test-Path -LiteralPath $Archive) {
    $CurrentBytes = (Get-Item -LiteralPath $Archive).Length
    if ($CurrentBytes -eq $Asset.Bytes) {
      $CurrentSha = (Get-FileHash -LiteralPath $Archive -Algorithm SHA256).Hash.ToLowerInvariant()
      if ($CurrentSha -eq $Asset.Sha256) {
        Write-Host "[CACHED] $($Asset.Name)"
        $NeedsDownload = $false
      } else {
        $Invalid = "$Archive.invalid-$(Get-Date -Format 'yyyyMMdd-HHmmss')"
        Move-Item -LiteralPath $Archive -Destination $Invalid
        Write-Host "[RETAINED] invalid archive -> $Invalid"
      }
    } elseif ($CurrentBytes -gt $Asset.Bytes) {
      $Invalid = "$Archive.invalid-$(Get-Date -Format 'yyyyMMdd-HHmmss')"
      Move-Item -LiteralPath $Archive -Destination $Invalid
      Write-Host "[RETAINED] oversized archive -> $Invalid"
    }
  }

  if ($NeedsDownload) {
    Write-Host "[START] $($Asset.Name)"
    & curl.exe --fail --location --retry 8 --retry-delay 2 --continue-at - `
      --output $Archive "$BaseUrl/$($Asset.Name)"
    if ($LASTEXITCODE -ne 0) {
      throw "curl failed for $($Asset.Name) with exit code $LASTEXITCODE"
    }
  }

  $DownloadedBytes = (Get-Item -LiteralPath $Archive).Length
  $DownloadedSha = (Get-FileHash -LiteralPath $Archive -Algorithm SHA256).Hash.ToLowerInvariant()
  if ($DownloadedBytes -ne $Asset.Bytes -or $DownloadedSha -ne $Asset.Sha256) {
    throw "Integrity failure for $($Asset.Name): bytes=$DownloadedBytes sha256=$DownloadedSha"
  }

  New-Item -ItemType Directory -Force -Path $Asset.Destination | Out-Null
  Expand-Archive -LiteralPath $Archive -DestinationPath $Asset.Destination -Force
  Write-Host "[OK] $($Asset.Name) -> $($Asset.Destination)"
}

$CudaCli = Get-ChildItem -LiteralPath (Join-Path $RuntimeRoot "cuda-12.4") -Recurse -File -Filter "llama-cli.exe" | Select-Object -First 1
$VulkanCli = Get-ChildItem -LiteralPath (Join-Path $RuntimeRoot "vulkan") -Recurse -File -Filter "llama-cli.exe" | Select-Object -First 1
if (-not $CudaCli -or -not $VulkanCli) {
  throw "Runtime extraction completed but llama-cli.exe was not found in both backends"
}

$Lock = [ordered]@{
  schema = "heardright.llama_cpp_runtime_lock.v1"
  release = $Release
  installed_at = (Get-Date).ToUniversalTime().ToString("o")
  cuda_cli = $CudaCli.FullName
  vulkan_cli = $VulkanCli.FullName
  assets = $Assets | ForEach-Object {
    [ordered]@{
      name = $_.Name
      bytes = $_.Bytes
      sha256 = $_.Sha256
      destination = $_.Destination
    }
  }
}
$LockPath = Join-Path $RuntimeRoot "runtime-lock.json"
$Lock | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath $LockPath -Encoding utf8

Write-Host "CUDA:   $($CudaCli.FullName)"
Write-Host "Vulkan: $($VulkanCli.FullName)"
Write-Host "Lock:   $LockPath"
& $CudaCli.FullName --version
if ($LASTEXITCODE -ne 0) {
  throw "CUDA llama-cli --version failed with exit code $LASTEXITCODE"
}
