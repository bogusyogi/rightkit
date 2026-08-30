import { copyFileSync, existsSync, mkdirSync, mkdtempSync, readdirSync, rmSync, statSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { spawnSync } from "node:child_process";

export const RIGHTRELEASE_MANIFEST_SIGNER = Object.freeze({
  id: "azure-artifact-signing-damned-ventures-v1",
  subject: "CN=Damned Ventures LLC",
  algorithm: "authenticode-catalog-sha256",
  provider: "windows-authenticode-catalog",
  providerVersion: 1,
});

const EXCLUDE_CREDENTIALS = [
  "ManagedIdentityCredential",
  "SharedTokenCacheCredential",
  "VisualStudioCredential",
  "VisualStudioCodeCredential",
  "AzurePowerShellCredential",
  "AzureDeveloperCliCredential",
  "InteractiveBrowserCredential",
];
const CERTIFICATE_SHA256 = /^[a-f0-9]{64}$/i;

/** Verify Azure-signed catalog binding exact release-manifest bytes. */
export function verifyManifestCatalogSignature({
  manifestPath,
  signaturePath,
  expectedSigner,
  commandRunner = spawnSync,
  platform = process.platform,
  powershellPath = "pwsh",
}) {
  const manifest = resolve(manifestPath);
  const signature = resolve(signaturePath);
  if (!existsSync(manifest) || !statSync(manifest).isFile()) throw new Error("manifest bytes are missing");
  if (!existsSync(signature) || !statSync(signature).isFile() || statSync(signature).size < 1) throw new Error("detached manifest catalog signature is missing");
  if (!expectedSigner || expectedSigner.id !== RIGHTRELEASE_MANIFEST_SIGNER.id || expectedSigner.subject !== RIGHTRELEASE_MANIFEST_SIGNER.subject || expectedSigner.algorithm !== RIGHTRELEASE_MANIFEST_SIGNER.algorithm || expectedSigner.provider !== RIGHTRELEASE_MANIFEST_SIGNER.provider || expectedSigner.providerVersion !== RIGHTRELEASE_MANIFEST_SIGNER.providerVersion || !CERTIFICATE_SHA256.test(expectedSigner.certificateSha256 ?? "")) {
    throw new Error("catalog verification requires exact Azure signer identity");
  }
  if (platform !== "win32" && commandRunner === spawnSync) throw new Error("RightRelease catalog verification must run on Windows");
  const inspection = inspectCatalog({ manifestPath: manifest, signaturePath: signature, commandRunner, powershellPath });
  if (inspection.verified !== true || inspection.catalogStatus !== "Valid" || inspection.signatureStatus !== "Valid") throw new Error("manifest catalog signature is invalid");
  if (normalizeSubject(inspection.subject) !== normalizeSubject(expectedSigner.subject)) throw new Error("catalog signer subject mismatch");
  const certificateSha256 = String(inspection.certificateSha256 ?? "").replaceAll(":", "").toLowerCase();
  if (!CERTIFICATE_SHA256.test(certificateSha256) || certificateSha256 !== String(expectedSigner.certificateSha256).toLowerCase()) throw new Error("catalog signer certificate fingerprint mismatch");
  return { verified: true, manifestPath: manifest, signaturePath: signature, signer: { ...expectedSigner, certificateSha256 } };
}

export function signReleaseManifestWithAzure({
  manifestPath,
  signaturePath,
  commandRunner = spawnSync,
  environment = process.env,
  signtoolPath,
  dlibPath,
  metadataPath,
  powershellPath = "pwsh",
}) {
  if (process.platform !== "win32" && commandRunner === spawnSync) throw new Error("RightRelease manifest signing must run on protected Windows release host");
  const userEnv = (name) => readUserEnvironment(name, commandRunner);
  const env = (name) => environment[name] || userEnv(name);
  const signtool = signtoolPath || firstExisting([env("AZURE_SIGNTOOL_PATH"), env("SIGNTOOL_PATH"), ...signtoolCandidates(env)]);
  if (!signtool) throw new Error("RightRelease manifest signer could not find signtool.exe");
  const dlib = dlibPath || firstExisting([
    env("AZURE_CODESIGN_DLIB_PATH"),
    env("AZURE_ARTIFACT_SIGNING_DLIB_PATH"),
    join(env("LOCALAPPDATA") || "", "AzureArtifactSigningTools", "Microsoft.ArtifactSigning.Client", "bin", "x64", "Azure.CodeSigning.Dlib.dll"),
    "C:\\Program Files\\Microsoft Azure Artifact Signing Client Tools\\x64\\Azure.CodeSigning.Dlib.dll",
    "C:\\Program Files (x86)\\Microsoft\\ArtifactSigningClientTools\\bin\\x64\\Azure.CodeSigning.Dlib.dll",
  ]);
  if (!dlib) throw new Error("RightRelease manifest signer could not find Azure Artifact Signing dlib");

  const temp = mkdtempSync(join(tmpdir(), "rightrelease-manifest-sign-"));
  const metadata = metadataPath || materializeMetadata(temp, env);
  const catalogDir = join(temp, "catalog");
  mkdirSync(catalogDir, { recursive: true });
  const catalog = join(catalogDir, "catalog.cat");
  const generator = join(temp, "create-catalog.ps1");
  writeFileSync(generator, [
    "param([string]$ManifestPath, [string]$CatalogDirectory)",
    "$ErrorActionPreference = 'Stop'",
    "Import-Module Microsoft.PowerShell.Security -ErrorAction Stop",
    "New-FileCatalog -Path $ManifestPath -CatalogFilePath $CatalogDirectory -CatalogVersion 2.0 | Out-Null",
  ].join("\r\n") + "\r\n");
  try {
    const generation = commandRunner(powershellPath, ["-NoProfile", "-File", generator, resolve(manifestPath), catalogDir], { encoding: "utf8", windowsHide: true, env: environment });
    if (generation?.status !== 0) throw new Error(`RightRelease manifest catalog generation failed: ${String(generation?.stderr ?? "").trim()}`);
    if (!existsSync(catalog) || !statSync(catalog).isFile()) throw new Error("RightRelease manifest catalog was not produced");
    const result = commandRunner(signtool, [
      "sign", "/v", "/fd", "SHA256", "/tr", "http://timestamp.acs.microsoft.com", "/td", "SHA256",
      "/dlib", dlib, "/dmdf", metadata, catalog,
    ], { encoding: "utf8", windowsHide: true, env: environment });
    if (result?.status !== 0) throw new Error(`RightRelease manifest signing failed: ${String(result?.stderr ?? "").trim()}`);
    const inspection = inspectCatalog({ manifestPath: resolve(manifestPath), signaturePath: catalog, commandRunner, powershellPath, environment });
    if (inspection.verified !== true || inspection.catalogStatus !== "Valid" || inspection.signatureStatus !== "Valid") throw new Error("RightRelease manifest catalog verification failed");
    if (normalizeSubject(inspection.subject) !== normalizeSubject(RIGHTRELEASE_MANIFEST_SIGNER.subject) || !CERTIFICATE_SHA256.test(inspection.certificateSha256 ?? "")) throw new Error("RightRelease manifest signer identity does not match protected Azure profile");
    mkdirSync(dirname(signaturePath), { recursive: true });
    copyFileSync(catalog, signaturePath);
    return { signaturePath: resolve(signaturePath), signer: { ...RIGHTRELEASE_MANIFEST_SIGNER, certificateSha256: inspection.certificateSha256.toLowerCase() } };
  } finally {
    rmSync(temp, { recursive: true, force: true });
  }
}

function inspectCatalog({ manifestPath, signaturePath, commandRunner, powershellPath, environment = process.env }) {
  const temp = mkdtempSync(join(tmpdir(), "rightrelease-catalog-verify-"));
  const verifier = join(temp, "verify-catalog.ps1");
  writeFileSync(verifier, [
    "param([string]$ManifestPath, [string]$SignaturePath)",
    "$ErrorActionPreference = 'Stop'",
    "Import-Module Microsoft.PowerShell.Security -ErrorAction Stop",
    "$Catalog = Test-FileCatalog -CatalogFilePath $SignaturePath -Path $ManifestPath -Detailed",
    "$Signature = Get-AuthenticodeSignature -FilePath $SignaturePath",
    "$Certificate = $Signature.SignerCertificate",
    "if (-not $Certificate) { throw 'Catalog signer certificate is missing' }",
    "$SimpleName = $Certificate.GetNameInfo([Security.Cryptography.X509Certificates.X509NameType]::SimpleName, $false)",
    "$Sha256 = [Security.Cryptography.SHA256]::Create()",
    "try { $Fingerprint = ([BitConverter]::ToString($Sha256.ComputeHash($Certificate.RawData))).Replace('-','').ToLowerInvariant() } finally { $Sha256.Dispose() }",
    "@{ verified=([string]$Catalog.Status -eq 'Valid' -and [string]$Signature.Status -eq 'Valid'); catalogStatus=[string]$Catalog.Status; signatureStatus=[string]$Signature.Status; subject=('CN=' + $SimpleName); certificateSha256=$Fingerprint } | ConvertTo-Json -Compress",
  ].join("\r\n") + "\r\n");
  try {
    const result = commandRunner(powershellPath, ["-NoProfile", "-File", verifier, manifestPath, signaturePath], { encoding: "utf8", windowsHide: true, env: environment });
    if (result?.status !== 0) throw new Error(`manifest catalog verification failed: ${String(result?.stderr ?? "").trim()}`);
    let inspection;
    try { inspection = JSON.parse(String(result?.stdout ?? "").trim()); } catch { throw new Error("catalog verifier returned invalid JSON"); }
    if (typeof inspection.subject !== "string" || typeof inspection.certificateSha256 !== "string") throw new Error("catalog verifier returned incomplete identity");
    return inspection;
  } finally {
    rmSync(temp, { recursive: true, force: true });
  }
}

function normalizeSubject(subject) {
  return String(subject).trim().replace(/\s*=\s*/g, "=").replace(/\s*,\s*/g, ",");
}

function materializeMetadata(dir, env) {
  const existing = env("AZURE_ARTIFACT_SIGNING_METADATA") || env("AZURE_SIGNING_METADATA");
  if (existing) {
    if (!existsSync(existing)) throw new Error(`Azure signing metadata file not found: ${existing}`);
    return existing;
  }
  const Endpoint = env("AZURE_ARTIFACT_SIGNING_ENDPOINT") || env("AZURE_SIGNING_ENDPOINT") || env("AZURE_ENDPOINT");
  const CodeSigningAccountName = env("AZURE_ARTIFACT_SIGNING_ACCOUNT") || env("AZURE_SIGNING_ACCOUNT_NAME") || env("AZURE_ACCOUNT");
  const CertificateProfileName = env("AZURE_ARTIFACT_SIGNING_PROFILE") || env("AZURE_CERTIFICATE_PROFILE_NAME") || env("AZURE_PROFILE");
  if (!Endpoint || !CodeSigningAccountName || !CertificateProfileName) throw new Error("RightRelease manifest signer requires existing Azure Artifact Signing configuration");
  const path = join(dir, "metadata.json");
  writeFileSync(path, `${JSON.stringify({ Endpoint, CodeSigningAccountName, CertificateProfileName, ExcludeCredentials: EXCLUDE_CREDENTIALS }, null, 2)}\n`);
  return path;
}

function signtoolCandidates(env) {
  const roots = [
    join(env("LOCALAPPDATA") || "", "AzureArtifactSigningTools", "Microsoft.Windows.SDK.BuildTools", "bin"),
    join(env("ProgramFiles(x86)") || "C:\\Program Files (x86)", "Windows Kits", "10", "bin"),
  ];
  return roots.flatMap((root) => existsSync(root)
    ? readdirSync(root, { withFileTypes: true }).filter((entry) => entry.isDirectory()).map((entry) => join(root, entry.name, "x64", "signtool.exe")).sort().reverse()
    : []);
}

function firstExisting(candidates) {
  return candidates.find((candidate) => candidate && existsSync(candidate));
}

function readUserEnvironment(name, commandRunner) {
  const result = commandRunner("powershell", ["-NoProfile", "-Command", `[Environment]::GetEnvironmentVariable('${name.replaceAll("'", "''")}','User')`], { encoding: "utf8", windowsHide: true });
  return result?.status === 0 ? String(result.stdout ?? "").trim() : "";
}
