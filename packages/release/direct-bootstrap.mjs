import { createHash } from "node:crypto";
import { mkdirSync, readFileSync, realpathSync, statSync, writeFileSync } from "node:fs";
import { basename, dirname, isAbsolute, join, resolve } from "node:path";
import { spawnSync } from "node:child_process";
import { RIGHTRELEASE_MANIFEST_SIGNER, signReleaseManifestWithAzure } from "./sign-release-manifest.mjs";
import { validateCycloneDxSbom, validateInTotoSlsaProvenance } from "./supply-chain-evidence.mjs";

const SHA256 = /^[a-f0-9]{64}$/;
const VERSION = /^(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)$/;
const MAX_BOOTSTRAP_BYTES = 256 * 1024;
const TARGETS = new Set(["windows-x86_64", "windows-arm64", "macos-x86_64", "macos-arm64"]);

export function canonicalJson(value) {
  if (Array.isArray(value)) return `[${value.map(canonicalJson).join(",")}]`;
  if (value && typeof value === "object") {
    return `{${Object.keys(value).sort().map((key) => `${JSON.stringify(key)}:${canonicalJson(value[key])}`).join(",")}}`;
  }
  return JSON.stringify(value);
}

export function sha256File(path) {
  return createHash("sha256").update(readFileSync(path)).digest("hex");
}

export function collectReleaseAsset({
  target,
  name,
  url,
  archivePath,
  executablePath,
  nativeSignaturePolicy,
  provenancePath,
  sbomPath,
}) {
  const archive = statSync(archivePath);
  if (!archive.isFile()) throw new Error("release archive is missing");
  if (!statSync(provenancePath).isFile() || !statSync(sbomPath).isFile()) throw new Error("provenance/SBOM evidence is missing");
  const archiveSha256 = sha256File(archivePath);
  validateInTotoSlsaProvenance(provenancePath, { expectedSubject: { name, sha256: archiveSha256 } });
  validateCycloneDxSbom(sbomPath, { expectedFile: { name, sha256: archiveSha256 } });
  const releaseBase = url.slice(0, url.lastIndexOf("/") + 1);
  const provenanceName = provenancePath.split(/[\\/]/).at(-1);
  const sbomName = sbomPath.split(/[\\/]/).at(-1);
  return {
    target,
    name,
    url,
    size: archive.size,
    sha256: archiveSha256,
    executablePath,
    nativeSignaturePolicy,
    provenanceName,
    provenanceUrl: releaseBase + provenanceName,
    provenanceSha256: sha256File(provenancePath),
    sbomName,
    sbomUrl: releaseBase + sbomName,
    sbomSha256: sha256File(sbomPath),
  };
}

function assertRelativeFile(path, label) {
  if (typeof path !== "string" || !path || isAbsolute(path) || path.split(/[\\/]/).some((part) => !part || part === "." || part === "..")) {
    throw new Error(`${label} must be a contained relative file path`);
  }
}

function assertGitHubAssetUrl(url) {
  const parsed = new URL(url);
  if (parsed.protocol !== "https:" || parsed.hostname !== "github.com" || !/\/releases\/download\//.test(parsed.pathname)) {
    throw new Error(`release payload must use immutable GitHub Release URL: ${url}`);
  }
}

export function createPortableArchive({ sourceDir, outputPath, commandRunner = spawnSync }) {
  const source = realpathSync(sourceDir);
  const outputDir = resolve(dirname(outputPath));
  mkdirSync(outputDir, { recursive: true });
  // On Windows, resolve the System32 bsdtar explicitly: PATH often finds GNU
  // tar (Git Bash) first, which reads a colon in -f as a remote host spec and
  // cannot write zip at all — its -a silently emits plain tar bytes for a
  // .zip suffix. bsdtar writes a real zip. Run from the output directory with
  // a bare filename so the invocation stays safe under either tar.
  const tarProgram = process.platform === "win32"
    ? join(process.env.SystemRoot ?? "C:\\Windows", "System32", "tar.exe")
    : "tar";
  const result = commandRunner(tarProgram, ["-a", "-c", "-f", basename(outputPath), "-C", source, "."], {
    cwd: outputDir,
    encoding: "utf8",
    windowsHide: true,
  });
  if (result?.status !== 0) throw new Error(`portable archive failed: ${String(result?.stderr ?? "").trim()}`);
  if (!statSync(outputPath).isFile()) throw new Error(`portable archive missing: ${outputPath}`);
  return { path: resolve(outputPath), size: statSync(outputPath).size, sha256: sha256File(outputPath) };
}

export function buildChecksums(assets) {
  const checksums = {};
  for (const asset of assets) {
    const files = [
      { name: asset.name, sha256: asset.sha256 },
      ...(asset.provenanceName ? [{ name: asset.provenanceName, sha256: asset.provenanceSha256 }] : []),
      ...(asset.sbomName ? [{ name: asset.sbomName, sha256: asset.sbomSha256 }] : []),
    ];
    for (const file of files) {
      assertRelativeFile(file.name, "asset name");
      if (!SHA256.test(file.sha256)) throw new Error(`invalid SHA-256 for ${file.name}`);
      if (checksums[file.name]) throw new Error(`duplicate release asset: ${file.name}`);
      checksums[file.name] = file.sha256;
    }
  }
  return { schemaVersion: 1, algorithm: "sha256", assets: checksums };
}

export function buildReleaseManifest({
  product,
  version,
  tag = `v${version}`,
  sourceCommit,
  minimumBootstrapVersion,
  assets,
  checksumsDigest,
}) {
  if (!/^[a-z][a-z0-9-]+$/.test(product)) throw new Error("invalid product");
  if (!VERSION.test(version) || tag !== `v${version}`) throw new Error("version/tag mismatch");
  if (!/^[a-f0-9]{40,64}$/.test(sourceCommit)) throw new Error("invalid source commit");
  if (!VERSION.test(minimumBootstrapVersion)) throw new Error("invalid minimum bootstrap version");
  if (!SHA256.test(checksumsDigest)) throw new Error("invalid checksums digest");
  const seen = new Set();
  const normalizedAssets = assets.map((asset) => {
    if (!TARGETS.has(asset.target)) throw new Error(`unsupported release target: ${asset.target}`);
    if (seen.has(asset.target)) throw new Error(`duplicate release target: ${asset.target}`);
    seen.add(asset.target);
    assertRelativeFile(asset.name, "asset name");
    assertGitHubAssetUrl(asset.url);
    if (!Number.isSafeInteger(asset.size) || asset.size < 1 || !SHA256.test(asset.sha256)) throw new Error(`invalid asset evidence: ${asset.name}`);
    if (!SHA256.test(asset.provenanceSha256) || !SHA256.test(asset.sbomSha256)) throw new Error(`invalid bound evidence: ${asset.name}`);
    assertRelativeFile(asset.provenanceName, "provenance name");
    assertRelativeFile(asset.sbomName, "SBOM name");
    assertGitHubAssetUrl(asset.provenanceUrl);
    assertGitHubAssetUrl(asset.sbomUrl);
    const releaseBase = asset.url.slice(0, asset.url.lastIndexOf("/") + 1);
    if (asset.provenanceUrl !== releaseBase + asset.provenanceName || asset.sbomUrl !== releaseBase + asset.sbomName) throw new Error(`bound evidence URL mismatch: ${asset.name}`);
    if (asset.target.startsWith("windows-") && asset.nativeSignaturePolicy !== "authenticode-valid") throw new Error(`Windows asset must require Authenticode: ${asset.name}`);
    if (asset.target.startsWith("macos-") && asset.nativeSignaturePolicy !== "developer-id-notarized") throw new Error(`macOS asset must require Developer ID notarization: ${asset.name}`);
    assertRelativeFile(asset.executablePath, "executable path");
    return { ...asset };
  }).sort((left, right) => left.target.localeCompare(right.target));
  return {
    schemaVersion: 1,
    kind: "rightkit-direct-release-manifest",
    product,
    version,
    tag,
    sourceCommit,
    minimumBootstrapVersion,
    signingKeyId: RIGHTRELEASE_MANIFEST_SIGNER.id,
    signatureAlgorithm: RIGHTRELEASE_MANIFEST_SIGNER.algorithm,
    signatureProvider: RIGHTRELEASE_MANIFEST_SIGNER.provider,
    signatureProviderVersion: RIGHTRELEASE_MANIFEST_SIGNER.providerVersion,
    checksumsSha256: checksumsDigest,
    assets: normalizedAssets,
  };
}

export function materializeDirectRelease({ outputDir, manifestInput }) {
  mkdirSync(outputDir, { recursive: true });
  const checksums = buildChecksums(manifestInput.assets);
  const checksumsText = `${canonicalJson(checksums)}\n`;
  const checksumsPath = join(outputDir, "checksums.json");
  writeFileSync(checksumsPath, checksumsText);
  const manifest = buildReleaseManifest({
    ...manifestInput,
    checksumsDigest: createHash("sha256").update(checksumsText).digest("hex"),
  });
  const manifestText = `${canonicalJson(manifest)}\n`;
  const manifestPath = join(outputDir, "release-manifest.json");
  const signaturePath = join(outputDir, "release-manifest.cat");
  writeFileSync(manifestPath, manifestText);
  const signing = signReleaseManifestWithAzure({ manifestPath, signaturePath });
  if (!statSync(signaturePath).isFile() || statSync(signaturePath).size < 1) throw new Error("detached release manifest signature is missing");
  if (signing?.signer?.id !== RIGHTRELEASE_MANIFEST_SIGNER.id || signing.signer.subject !== RIGHTRELEASE_MANIFEST_SIGNER.subject || !/^[a-f0-9]{64}$/.test(signing.signer.certificateSha256 ?? "")) {
    throw new Error("detached release manifest signer is not RightRelease Azure authority");
  }
  return { manifest, manifestPath, signaturePath, signing, checksums };
}

function psQuote(value) {
  return "'" + String(value).replaceAll("'", "''") + "'";
}

function psArray(values) {
  return "@(" + values.map(psQuote).join(",") + ")";
}

export function renderPowerShellBootstrap({
  product,
  repository,
  bootstrapVersion,
  acceptedManifestSigners,
  installRootSubdir,
  executablePath,
  activationArgs,
  statusArgs,
  expectedStatusKind,
  expectedStatus = "complete",
  healthAssertions = null,
  preflightArgs = [],
  allowLocalReleaseRoot = false,
}) {
  if (!/^[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+$/.test(repository)) throw new Error("invalid GitHub repository");
  if (!VERSION.test(bootstrapVersion)) throw new Error("invalid bootstrap version");
  if (typeof allowLocalReleaseRoot !== "boolean") throw new Error("allowLocalReleaseRoot must be boolean");
  assertRelativeFile(executablePath, "executable path");
  if (!Array.isArray(acceptedManifestSigners) || !acceptedManifestSigners.length) throw new Error("accepted manifest signers are required");
  const signers = {};
  for (const signer of acceptedManifestSigners) {
    if (typeof signer.id !== "string" || !signer.id || typeof signer.subject !== "string" || !/^CN=/.test(signer.subject) || !/^[a-f0-9]{64}$/.test(signer.certificateSha256 ?? "")) {
      throw new Error("accepted manifest signer requires id, certificate subject, and SHA-256 fingerprint");
    }
    signers[signer.id] = { subject: signer.subject, certificateSha256: signer.certificateSha256 };
  }
  if (healthAssertions === null && (typeof expectedStatusKind !== "string" || !expectedStatusKind)) {
    throw new Error("expectedStatusKind or product-specific healthAssertions is required");
  }
  const assertions = healthAssertions ?? [
    { path: "kind", equals: expectedStatusKind },
    { path: "status", equals: expectedStatus },
  ];
  if (!Array.isArray(assertions) || !assertions.length) throw new Error("health assertions are required");
  for (const assertion of assertions) {
    if (!assertion || typeof assertion.path !== "string" || !/^[A-Za-z_][A-Za-z0-9_-]*(?:\.(?:[A-Za-z_][A-Za-z0-9_-]*|\d+))*$/.test(assertion.path)) {
      throw new Error("health assertion requires safe JSON path");
    }
    const predicates = [Object.hasOwn(assertion, "equals"), assertion.nonempty === true, Object.hasOwn(assertion, "minCount")].filter(Boolean).length;
    if (predicates !== 1) throw new Error("health assertion requires exactly one equals, nonempty, or minCount predicate");
    if (Object.hasOwn(assertion, "minCount") && (!Number.isSafeInteger(assertion.minCount) || assertion.minCount < 0)) throw new Error("health minCount must be a nonnegative integer");
  }
  const contract = {
    schemaVersion: 1,
    generator: "rightkit-direct-bootstrap",
    product,
    repository,
    bootstrapVersion,
    installRootSubdir,
    acceptedManifestSigners: signers,
    executablePath: executablePath.replaceAll("\\", "/"),
    activationArgs,
    statusArgs,
    healthAssertions: assertions,
    preflightArgs,
    ...(allowLocalReleaseRoot ? { allowLocalReleaseRoot: true } : {}),
  };
  // Bind generated contract metadata so a caller cannot pass a marker plus an
  // arbitrary base64 payload and have it treated as a RightRelease bootstrap.
  // This is not a replacement for manifest catalog signing; it is a structural
  // integrity fence for the executable bootstrap source itself.
  contract.contractSha256 = createHash("sha256").update(canonicalJson(contract)).digest("hex");
  const contractB64 = Buffer.from(JSON.stringify(contract)).toString("base64");
  const lines = [
    "# rightkit-direct-bootstrap:v1",
    "# generated contract: generator=rightkit-direct-bootstrap contractSha256-bound",
    allowLocalReleaseRoot
      ? "[CmdletBinding()] param([string]$Version, [switch]$AllowDowngrade, [string]$ReleaseRoot)"
      : "[CmdletBinding()] param([string]$Version, [switch]$AllowDowngrade)",
    "$ErrorActionPreference = 'Stop'",
    "$ProgressPreference = 'SilentlyContinue'",
    "$Contract = [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String(" + psQuote(contractB64) + ")) | ConvertFrom-Json",
    "$AcceptedSigners = @{}; $Contract.acceptedManifestSigners.PSObject.Properties | ForEach-Object { $AcceptedSigners[$_.Name] = $_.Value }",
    "$InstallRoot = Join-Path $env:LOCALAPPDATA " + psQuote(installRootSubdir),
    "$VersionsRoot = Join-Path $InstallRoot 'versions'",
    "$Current = Join-Path $InstallRoot 'current'",
    "$PreviousLink = Join-Path $InstallRoot '.current-previous'",
    "$NextLink = Join-Path $InstallRoot '.current-next'",
    "$Journal = Join-Path $InstallRoot 'integration-journal.json'",
    "function Expand-BootstrapArgs($Values, $ActiveVersionRoot, $ActiveVersion) { foreach ($Value in $Values) { ([string]$Value).Replace('{current}', $Current).Replace('{installRoot}', $InstallRoot).Replace('{versionRoot}', $ActiveVersionRoot).Replace('{version}', $ActiveVersion) } }",
    "function Resolve-HealthPath($Value, [string]$JsonPath) { $CurrentValue = $Value; foreach ($Segment in $JsonPath.Split('.')) { if ($Segment -match '^\\d+$') { $Index = [int]$Segment; if ($null -eq $CurrentValue -or $Index -ge @($CurrentValue).Count) { throw ('Health path is missing: ' + $JsonPath) }; $CurrentValue = @($CurrentValue)[$Index] } else { $Property = $CurrentValue.PSObject.Properties[$Segment]; if ($null -eq $Property) { throw ('Health path is missing: ' + $JsonPath) }; $CurrentValue = $Property.Value } }; return $CurrentValue }",
    "function Assert-Health($Health, [string]$ActiveVersion) { foreach ($Assertion in $Contract.healthAssertions) { $Actual = Resolve-HealthPath $Health ([string]$Assertion.path); if ($Assertion.PSObject.Properties.Name -contains 'equals') { $Expected = $Assertion.equals; if ($Expected -is [string]) { $Expected = $Expected.Replace('{version}', $ActiveVersion).Replace('{current}', $Current).Replace('{installRoot}', $InstallRoot) }; if ($Actual -ne $Expected) { throw ('Health assertion failed: ' + $Assertion.path) } } elseif ($Assertion.nonempty -eq $true) { if ($null -eq $Actual -or ([string]$Actual).Trim().Length -eq 0) { throw ('Health assertion failed: ' + $Assertion.path) } } elseif (@($Actual).Count -lt [int]$Assertion.minCount) { throw ('Health assertion failed: ' + $Assertion.path) } } }",
    ...(allowLocalReleaseRoot ? [
      "New-Item -ItemType Directory -Force -Path $VersionsRoot | Out-Null",
      "$LocalReleaseRoot = $null; if ($ReleaseRoot) { $LocalReleaseRoot = [IO.Path]::GetFullPath($ReleaseRoot); if (-not (Test-Path -LiteralPath $LocalReleaseRoot -PathType Container)) { throw 'Local release root is missing' } }",
      "if (-not $Version -and -not $LocalReleaseRoot) { $Version = ((Invoke-RestMethod " + psQuote("https://api.github.com/repos/" + repository + "/releases/latest") + ").tag_name -replace '^v','') }",
      "if ($Version -and $Version -notmatch '^(?:0|[1-9]\\d*)\\.(?:0|[1-9]\\d*)\\.(?:0|[1-9]\\d*)$') { throw 'Invalid stable release version' }",
      "$ReleaseBase = if ($Version) { " + psQuote("https://github.com/" + repository + "/releases/download/v") + " + $Version } else { $null }",
      "$Work = Join-Path ([IO.Path]::GetTempPath()) (" + psQuote(product + "-install-") + " + [guid]::NewGuid())",
      "New-Item -ItemType Directory -Force -Path $Work | Out-Null",
      "$ManifestPath = Join-Path $Work 'release-manifest.json'; $SignaturePath = Join-Path $Work 'release-manifest.cat'; $ChecksumsPath = Join-Path $Work 'checksums.json'",
      "function Receive-ReleaseFile([string]$Name, [string]$Destination) { if ($LocalReleaseRoot) { $RootPrefix = $LocalReleaseRoot.TrimEnd([IO.Path]::DirectorySeparatorChar, [IO.Path]::AltDirectorySeparatorChar) + [IO.Path]::DirectorySeparatorChar; $Source = [IO.Path]::GetFullPath((Join-Path $LocalReleaseRoot $Name)); if (-not $Source.StartsWith($RootPrefix, [StringComparison]::OrdinalIgnoreCase) -or -not (Test-Path -LiteralPath $Source -PathType Leaf)) { throw ('Local release asset is missing or escapes root: ' + $Name) }; Copy-Item -LiteralPath $Source -Destination $Destination } else { Invoke-WebRequest ($ReleaseBase + '/' + $Name) -OutFile $Destination } }",
      "Receive-ReleaseFile 'release-manifest.json' $ManifestPath",
      "Receive-ReleaseFile 'release-manifest.cat' $SignaturePath",
    ] : [
      "New-Item -ItemType Directory -Force -Path $VersionsRoot | Out-Null",
      "if (-not $Version) { $Version = ((Invoke-RestMethod " + psQuote("https://api.github.com/repos/" + repository + "/releases/latest") + ").tag_name -replace '^v','') }",
      "if ($Version -notmatch '^(?:0|[1-9]\\d*)\\.(?:0|[1-9]\\d*)\\.(?:0|[1-9]\\d*)$') { throw 'Invalid stable release version' }",
      "$PriorTarget = $null; if (Test-Path $Current) { $PriorTarget = (Get-Item $Current).Target }",
      "$PriorVersion = if ($PriorTarget) { ((Split-Path ([string]$PriorTarget) -Leaf) -split '-')[0] } else { $null }",
      "if ($PriorVersion -and -not $AllowDowngrade -and ([version]$Version -lt [version]$PriorVersion)) { throw 'Downgrade requires -AllowDowngrade' }",
      "$ReleaseBase = " + psQuote("https://github.com/" + repository + "/releases/download/v") + " + $Version",
      "$Work = Join-Path ([IO.Path]::GetTempPath()) (" + psQuote(product + "-install-") + " + [guid]::NewGuid())",
      "New-Item -ItemType Directory -Force -Path $Work | Out-Null",
      "$ManifestPath = Join-Path $Work 'release-manifest.json'; $SignaturePath = Join-Path $Work 'release-manifest.cat'; $ChecksumsPath = Join-Path $Work 'checksums.json'",
      "Invoke-WebRequest ($ReleaseBase + '/release-manifest.json') -OutFile $ManifestPath",
      "Invoke-WebRequest ($ReleaseBase + '/release-manifest.cat') -OutFile $SignaturePath",
    ]),
    "$ManifestBytes = [IO.File]::ReadAllBytes($ManifestPath); Import-Module Microsoft.PowerShell.Security -ErrorAction Stop",
    "$Catalog = Test-FileCatalog -CatalogFilePath $SignaturePath -Path $ManifestPath -Detailed; if ([string]$Catalog.Status -ne 'Valid') { throw 'Release manifest catalog binding is invalid' }",
    "$CatalogSignature = Get-AuthenticodeSignature -FilePath $SignaturePath; if ([string]$CatalogSignature.Status -ne 'Valid' -or -not $CatalogSignature.SignerCertificate) { throw 'Release manifest signature is invalid' }; $SignerCertificate = $CatalogSignature.SignerCertificate",
    "$SignerSubject = 'CN=' + $SignerCertificate.GetNameInfo([Security.Cryptography.X509Certificates.X509NameType]::SimpleName, $false)",
    "$Sha256 = [Security.Cryptography.SHA256]::Create(); try { $SignerFingerprint = ([BitConverter]::ToString($Sha256.ComputeHash($SignerCertificate.RawData))).Replace('-','').ToLowerInvariant() } finally { $Sha256.Dispose() }",
    "$VerifiedKeyId = $null; foreach ($KeyId in $AcceptedSigners.Keys) { $Accepted = $AcceptedSigners[$KeyId]; if ($SignerSubject -eq [string]$Accepted.subject -and $SignerFingerprint -eq [string]$Accepted.certificateSha256) { $VerifiedKeyId = $KeyId; break } }",
    "if (-not $VerifiedKeyId) { throw 'Release manifest signer is not accepted' }",
    "$Manifest = [Text.Encoding]::UTF8.GetString($ManifestBytes) | ConvertFrom-Json",
    ...(allowLocalReleaseRoot ? [
      "if (-not $Version) { $Version = [string]$Manifest.version }",
      "if ($Version -notmatch '^(?:0|[1-9]\\d*)\\.(?:0|[1-9]\\d*)\\.(?:0|[1-9]\\d*)$') { throw 'Invalid stable release version' }",
      "$ReleaseBase = " + psQuote("https://github.com/" + repository + "/releases/download/v") + " + $Version",
      "$PriorTarget = $null; if (Test-Path $Current) { $PriorTarget = (Get-Item $Current).Target }",
      "$PriorVersion = if ($PriorTarget) { ((Split-Path ([string]$PriorTarget) -Leaf) -split '-')[0] } else { $null }",
      "if ($PriorVersion -and -not $AllowDowngrade -and ([version]$Version -lt [version]$PriorVersion)) { throw 'Downgrade requires -AllowDowngrade' }",
    ] : []),
    "if ($Manifest.signingKeyId -ne $VerifiedKeyId -or $Manifest.signatureAlgorithm -ne 'authenticode-catalog-sha256' -or $Manifest.signatureProvider -ne 'windows-authenticode-catalog' -or $Manifest.signatureProviderVersion -ne 1) { throw 'Release manifest signer identity mismatch' }",
    "if ($Manifest.product -ne " + psQuote(product) + " -or $Manifest.version -ne $Version -or $Manifest.tag -ne ('v' + $Version)) { throw 'Release identity mismatch' }",
    "if ([version]$Contract.bootstrapVersion -lt [version]$Manifest.minimumBootstrapVersion) { throw 'Bootstrap update required' }",
    "$Machine = [Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString(); $Target = if ($Machine -eq 'Arm64') { 'windows-arm64' } elseif ($Machine -eq 'X64') { 'windows-x86_64' } else { throw ('Unsupported architecture: ' + $Machine) }",
    "$Matches = @($Manifest.assets | Where-Object { $_.target -eq $Target }); if ($Matches.Count -ne 1) { throw 'Release has no unique exact target' }; $Asset = $Matches[0]",
    "if ($Asset.url -ne ($ReleaseBase + '/' + $Asset.name)) { throw 'Asset URL is not exact immutable GitHub release URL' }",
    allowLocalReleaseRoot
      ? "Receive-ReleaseFile 'checksums.json' $ChecksumsPath"
      : "Invoke-WebRequest ($ReleaseBase + '/checksums.json') -OutFile $ChecksumsPath",
    "$ChecksumHash = (Get-FileHash -Algorithm SHA256 $ChecksumsPath).Hash.ToLowerInvariant(); if ($ChecksumHash -ne $Manifest.checksumsSha256) { throw 'checksums.json is not manifest-bound' }",
    "$Checksums = Get-Content -Raw $ChecksumsPath | ConvertFrom-Json; if ($Checksums.assets.($Asset.name) -ne $Asset.sha256) { throw 'Asset checksum differs from bound checksums' }",
    allowLocalReleaseRoot
      ? "$Archive = Join-Path $Work $Asset.name; Receive-ReleaseFile ([string]$Asset.name) $Archive"
      : "$Archive = Join-Path $Work $Asset.name; Invoke-WebRequest $Asset.url -OutFile $Archive",
    "if ((Get-FileHash -Algorithm SHA256 $Archive).Hash.ToLowerInvariant() -ne $Asset.sha256) { throw 'Archive checksum mismatch' }",
    "if ($Checksums.assets.($Asset.provenanceName) -ne $Asset.provenanceSha256 -or $Checksums.assets.($Asset.sbomName) -ne $Asset.sbomSha256) { throw 'Bound provenance/SBOM checksums are missing' }",
    "$ProvenancePath = Join-Path $Work $Asset.provenanceName; $SbomPath = Join-Path $Work $Asset.sbomName",
    allowLocalReleaseRoot
      ? "Receive-ReleaseFile ([string]$Asset.provenanceName) $ProvenancePath; Receive-ReleaseFile ([string]$Asset.sbomName) $SbomPath"
      : "Invoke-WebRequest $Asset.provenanceUrl -OutFile $ProvenancePath; Invoke-WebRequest $Asset.sbomUrl -OutFile $SbomPath",
    "if ((Get-FileHash -Algorithm SHA256 $ProvenancePath).Hash.ToLowerInvariant() -ne $Asset.provenanceSha256) { throw 'Provenance checksum mismatch' }",
    "if ((Get-FileHash -Algorithm SHA256 $SbomPath).Hash.ToLowerInvariant() -ne $Asset.sbomSha256) { throw 'SBOM checksum mismatch' }",
    "$Stage = Join-Path $Work 'stage'; New-Item -ItemType Directory -Force -Path $Stage | Out-Null",
    "Add-Type -AssemblyName System.IO.Compression.FileSystem; $Zip = [IO.Compression.ZipFile]::OpenRead($Archive); try { foreach ($Entry in $Zip.Entries) { $Dest = [IO.Path]::GetFullPath((Join-Path $Stage $Entry.FullName)); if (-not $Dest.StartsWith(([IO.Path]::GetFullPath($Stage) + [IO.Path]::DirectorySeparatorChar), [StringComparison]::OrdinalIgnoreCase)) { throw 'Archive path escapes staging root' } } } finally { $Zip.Dispose() }",
    "Expand-Archive -Path $Archive -DestinationPath $Stage -Force",
    "$VersionRoot = Join-Path $VersionsRoot ($Version + '-' + $Asset.sha256.Substring(0,12) + '-' + [guid]::NewGuid()); Move-Item $Stage $VersionRoot",
    "$VersionExe = Join-Path $VersionRoot ($Asset.executablePath -replace '/', [IO.Path]::DirectorySeparatorChar); if (-not (Test-Path $VersionExe)) { throw 'Release executable missing' }",
    "$Authenticode = Get-AuthenticodeSignature $VersionExe; if ($Asset.nativeSignaturePolicy -eq 'authenticode-valid' -and $Authenticode.Status -ne 'Valid') { throw 'Native signature invalid' }",
    "$ActivationArgsTemplate = " + psArray(activationArgs),
    "$StatusArgsTemplate = " + psArray(statusArgs),
    "$PreflightArgsTemplate = " + psArray(preflightArgs),
    "$PreflightArgs = @(Expand-BootstrapArgs $PreflightArgsTemplate $VersionRoot $Version)",
    "$PriorStatus = $null; if ($PriorTarget) { $PriorExe = Join-Path $Current " + psQuote(executablePath.replaceAll("/", "\\")) + "; $PriorStatusArgs = @(Expand-BootstrapArgs $StatusArgsTemplate ([string]$PriorTarget) $PriorVersion); try { $PriorStatus = (& $PriorExe @PriorStatusArgs | Out-String).Trim() } catch {} }",
    "@{ schemaVersion=1; priorTarget=$PriorTarget; priorVersion=$PriorVersion; targetVersion=$Version; priorStatus=$PriorStatus } | ConvertTo-Json -Depth 8 | Set-Content -Encoding UTF8 $Journal",
    "if ($PreflightArgs.Count) { & $VersionExe @PreflightArgs; if ($LASTEXITCODE -ne 0) { throw 'Activation preflight failed' } }",
    "if (Test-Path $NextLink) { Remove-Item -Force $NextLink }; if (Test-Path $PreviousLink) { Remove-Item -Force $PreviousLink }",
    "New-Item -ItemType Junction -Path $NextLink -Target $VersionRoot | Out-Null",
    "$Switched = $false; $PriorDetached = $false",
    "try {",
    "  if (Test-Path $Current) { [IO.Directory]::Move($Current, $PreviousLink); $PriorDetached = $true }; [IO.Directory]::Move($NextLink, $Current); $Switched = $true",
    "  $StableExe = Join-Path $Current " + psQuote(executablePath.replaceAll("/", "\\")),
    "  $ActivationArgs = @(Expand-BootstrapArgs $ActivationArgsTemplate $VersionRoot $Version); $StatusArgs = @(Expand-BootstrapArgs $StatusArgsTemplate $VersionRoot $Version)",
    "  & $StableExe @ActivationArgs; if ($LASTEXITCODE -ne 0) { throw 'Activation failed' }",
    "  $HealthText = (& $StableExe @StatusArgs | Out-String).Trim(); if ($LASTEXITCODE -ne 0) { throw 'Health command failed' }; $Health = $HealthText | ConvertFrom-Json",
    "  Assert-Health $Health $Version",
    "  $StableBin = Split-Path $StableExe -Parent; $UserPath = [Environment]::GetEnvironmentVariable('Path','User'); if (-not (($UserPath -split ';') -contains $StableBin)) { [Environment]::SetEnvironmentVariable('Path', (($UserPath.TrimEnd(';') + ';' + $StableBin).Trim(';')), 'User') }",
    // Journal cleanup must succeed before discarding the prior pointer. If
    // cleanup fails, catch still has a rollback anchor to restore.
    "  Remove-Item -Force $Journal; if (Test-Path $PreviousLink) { Remove-Item -Force $PreviousLink }; Write-Output (" + psQuote(product + " ") + " + $Version + ' installed')",
    "} catch {",
    "  if (($Switched -or $PriorDetached) -and (Test-Path $Current)) { Remove-Item -Force $Current }; if ($PriorDetached -and (Test-Path $PreviousLink)) { [IO.Directory]::Move($PreviousLink, $Current) }",
    "  if ($PriorTarget -and (Test-Path $Current)) { $PriorExe = Join-Path $Current " + psQuote(executablePath.replaceAll("/", "\\")) + "; $PriorActivationArgs = @(Expand-BootstrapArgs $ActivationArgsTemplate ([string]$PriorTarget) $PriorVersion); $PriorStatusArgs = @(Expand-BootstrapArgs $StatusArgsTemplate ([string]$PriorTarget) $PriorVersion); & $PriorExe @PriorActivationArgs; if ($LASTEXITCODE -ne 0) { throw 'Rollback activation failed' }; $PriorHealthText = (& $PriorExe @PriorStatusArgs | Out-String).Trim(); if ($LASTEXITCODE -ne 0) { throw 'Rollback health failed' }; $PriorHealth = $PriorHealthText | ConvertFrom-Json; try { Assert-Health $PriorHealth $PriorVersion } catch { throw 'Prior health was not restored' } }",
    "  if (Test-Path $VersionRoot) { Remove-Item -Recurse -Force $VersionRoot }; throw",
    "} finally { if (Test-Path $Work) { Remove-Item -Recurse -Force $Work } }",
  ];
  const body = lines.join("\r\n") + "\r\n";
  const sourceSha256 = createHash("sha256").update(body).digest("hex");
  return `${body}# source-sha256:${sourceSha256}\r\n`;
}

export function validatePowerShellBootstrap(script, expected = {}) {
  const required = [
    "# rightkit-direct-bootstrap:v1",
    "Release manifest signature is invalid",
    "certificateSha256",
    "Test-FileCatalog",
    "Get-AuthenticodeSignature",
    "checksums.json is not manifest-bound",
    "Provenance checksum mismatch",
    "SBOM checksum mismatch",
    "New-Item -ItemType Junction",
    "integration-journal.json",
    "Rollback activation failed",
    "Prior health was not restored",
    "Downgrade requires -AllowDowngrade",
    "Get-AuthenticodeSignature",
    "generator",
    "contractSha256",
    "source-sha256:",
  ];
  const errors = required.filter((marker) => !script.includes(marker)).map((marker) => "missing bootstrap contract marker: " + marker);
  const encoded = script.match(/FromBase64String\('([^']+)'\)/)?.[1];
  let contract = {};
  try { contract = encoded ? JSON.parse(Buffer.from(encoded, "base64").toString()) : {}; } catch { errors.push("bootstrap contract is not valid encoded JSON"); }
  const { contractSha256, ...unsignedContract } = contract;
  if (contract.schemaVersion !== 1 || contract.generator !== "rightkit-direct-bootstrap" || contract.bootstrapVersion == null || contract.repository == null || !/^[a-f0-9]{64}$/.test(contractSha256 ?? "") || contractSha256 !== createHash("sha256").update(canonicalJson(unsignedContract)).digest("hex")) errors.push("bootstrap encoded contract is incomplete or tampered");
  try {
    const acceptedManifestSigners = Object.entries(contract.acceptedManifestSigners ?? {}).map(([id, signer]) => ({ id, ...signer }));
    const regenerated = renderPowerShellBootstrap({
      product: contract.product,
      repository: contract.repository,
      bootstrapVersion: contract.bootstrapVersion,
      acceptedManifestSigners,
      installRootSubdir: contract.installRootSubdir,
      executablePath: contract.executablePath,
      activationArgs: contract.activationArgs,
      statusArgs: contract.statusArgs,
      healthAssertions: contract.healthAssertions,
      preflightArgs: contract.preflightArgs,
      allowLocalReleaseRoot: contract.allowLocalReleaseRoot === true,
    });
    if (regenerated !== script) errors.push("bootstrap source does not match canonical generated output");
  } catch {
    errors.push("bootstrap contract cannot be regenerated");
  }
  const sourceMarker = script.match(/# source-sha256:([a-f0-9]{64})\r?\n?$/);
  const markerOffset = script.lastIndexOf("# source-sha256:");
  if (!sourceMarker || markerOffset < 0 || createHash("sha256").update(script.slice(0, markerOffset)).digest("hex") !== sourceMarker[1]) errors.push("bootstrap source digest is missing or tampered");
  if (expected.product && contract.product !== expected.product) errors.push("bootstrap product identity missing");
  if (expected.allowLocalReleaseRoot === true && contract.allowLocalReleaseRoot !== true) errors.push("local release root support missing");
  if (expected.acceptedSignerIds) for (const id of expected.acceptedSignerIds) if (!Object.hasOwn(contract.acceptedManifestSigners ?? {}, id)) errors.push("accepted manifest signer missing: " + id);
  return { valid: errors.length === 0, errors };
}

export function planBootstrapPublication({ product, bootstrapVersion, scriptPath, stableKey = `${product}/install.ps1` }) {
  if (!/^[a-z][a-z0-9-]+$/.test(product ?? "")) throw new Error("invalid bootstrap product");
  if (!VERSION.test(bootstrapVersion)) throw new Error("invalid bootstrap version");
  if (stableKey !== `${product}/install.ps1`) throw new Error("bootstrap stable key must be product/install.ps1");
  const script = statSync(scriptPath);
  if (!script.isFile() || script.size < 1 || script.size > MAX_BOOTSTRAP_BYTES || scriptPath.split(/[\\/]/).at(-1) !== "install.ps1") throw new Error("bootstrap must be one small install.ps1 file");
  const validation = validatePowerShellBootstrap(readFileSync(scriptPath, "utf8"), { product });
  if (!validation.valid) throw new Error("bootstrap script violates generated RightRelease contract: " + validation.errors.join("; "));
  const versionedKey = `${product}/versions/${bootstrapVersion}/install.ps1`;
  return {
    product,
    bootstrapVersion,
    bucket: "rightapps-downloads",
    stableKey,
    versionedKey,
    scriptPath: resolve(scriptPath),
    sha256: sha256File(scriptPath),
  };
}

export function createWranglerR2Client({ commandRunner = spawnSync, wranglerRunner = ["pnpm", ["dlx", "wrangler@4"]] } = {}) {
  const run = (args) => {
    const [command, prefix] = wranglerRunner;
    const result = commandRunner(command, [...prefix, "r2", "object", ...args, "--remote"], { encoding: "utf8", windowsHide: true });
    if (result?.status !== 0) throw new Error(`R2 operation failed: ${String(result?.stderr ?? "").trim()}`);
  };
  return {
    put(bucket, key, path) { run(["put", `${bucket}/${key}`, "--file", path]); },
    get(bucket, key, path) { run(["get", `${bucket}/${key}`, "--file", path]); },
  };
}

export function publishBootstrapPlan(plan, r2, { verificationPath }) {
  validateBootstrapPublicationPlan(plan);
  r2.put(plan.bucket, plan.versionedKey, plan.scriptPath);
  r2.get(plan.bucket, plan.versionedKey, verificationPath);
  if (sha256File(verificationPath) !== plan.sha256) throw new Error("immutable bootstrap remote verification failed");
  r2.put(plan.bucket, plan.stableKey, plan.scriptPath);
  r2.get(plan.bucket, plan.stableKey, verificationPath);
  if (sha256File(verificationPath) !== plan.sha256) throw new Error("stable bootstrap remote verification failed");
  return { ...plan, verified: true };
}

export function validateBootstrapPublicationPlan(plan) {
  if (!plan || !/^[a-z][a-z0-9-]+$/.test(plan.product ?? "")) throw new Error("bootstrap publication product is invalid");
  if (!VERSION.test(plan.bootstrapVersion ?? "")) throw new Error("bootstrap publication version is invalid");
  if (plan.bucket !== "rightapps-downloads") throw new Error("bootstrap publication bucket is not branded public R2");
  if (plan.stableKey !== `${plan.product}/install.ps1`) throw new Error("bootstrap stable key is outside product boundary");
  if (plan.versionedKey !== `${plan.product}/versions/${plan.bootstrapVersion}/install.ps1`) throw new Error("bootstrap versioned key is outside product boundary");
  if (String(plan.stableKey).includes("..") || String(plan.versionedKey).includes("..")) throw new Error("bootstrap key traversal is forbidden");
  return plan;
}

export async function runInstallTransaction({
  targetVersion,
  currentVersion,
  allowDowngrade = false,
  stage,
  verify,
  preflight = async () => {},
  snapshotIntegrations,
  switchCurrent,
  activate,
  verifyHealth,
  restoreCurrent = async () => {},
  restoreIntegrations = async () => {},
  cleanup = async () => {},
}) {
  if (!VERSION.test(targetVersion)) throw new Error("invalid target version");
  if (currentVersion && !allowDowngrade && targetVersion.localeCompare(currentVersion, undefined, { numeric: true }) < 0) {
    throw new Error("downgrade requires explicit allowDowngrade");
  }
  const integrationJournal = await snapshotIntegrations();
  let staged;
  let switched = false;
  try {
    staged = await stage();
    await verify(staged);
    await preflight(staged);
    // switchCurrent is a compound operation (detach prior, install next). It
    // may throw after changing state, so recovery must run even when its
    // promise rejects before returning.
    switched = true;
    await switchCurrent(staged);
    await activate(staged);
    const health = await verifyHealth(staged);
    if (!health || health.status !== "complete") throw new Error("installed product health is incomplete");
    return { status: "complete", targetVersion, health };
  } catch (error) {
    try {
      if (switched) await restoreCurrent();
    } finally {
      await restoreIntegrations(integrationJournal);
    }
    await cleanup(staged);
    throw error;
  }
}
