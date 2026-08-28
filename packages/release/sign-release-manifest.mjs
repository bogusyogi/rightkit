import { copyFileSync, existsSync, mkdirSync, mkdtempSync, readdirSync, rmSync, statSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";

export const RIGHTRELEASE_MANIFEST_SIGNER = Object.freeze({
  id: "azure-artifact-signing-damned-ventures-v1",
  subject: "CN=Damned Ventures LLC, O=Damned Ventures LLC, L=new york, S=New York, C=US",
  algorithm: "authenticode-catalog-sha256",
  provider: "windows-authenticode-catalog",
  providerVersion: 1,
});

const EXCLUDE_CREDENTIALS = [
  "ManagedIdentityCredential",
  "WorkloadIdentityCredential",
  "SharedTokenCacheCredential",
  "VisualStudioCredential",
  "VisualStudioCodeCredential",
  "AzurePowerShellCredential",
  "AzureDeveloperCliCredential",
  "InteractiveBrowserCredential",
];

const CERTIFICATE_SHA256 = /^[a-f0-9]{64}$/i;

export function verifyReleaseManifestCatalog({
  manifestPath,
  catalogPath,
  expectedSigner,
  commandRunner = spawnSync,
  platform = process.platform,
  powershellPath = "powershell",
}) {
  const manifest = resolve(manifestPath);
  const catalog = resolve(catalogPath);
  if (platform !== "win32") throw new Error("Authenticode catalog verification requires Windows");
  if (!existsSync(manifest) || !statSync(manifest).isFile()) throw new Error("catalog manifest bytes are missing");
  if (!existsSync(catalog) || !statSync(catalog).isFile() || statSync(catalog).size < 1) throw new Error("release manifest catalog is missing");
  if (!expectedSigner || expectedSigner.id !== RIGHTRELEASE_MANIFEST_SIGNER.id || expectedSigner.subject !== RIGHTRELEASE_MANIFEST_SIGNER.subject || expectedSigner.algorithm !== RIGHTRELEASE_MANIFEST_SIGNER.algorithm || !CERTIFICATE_SHA256.test(expectedSigner.certificateSha256 ?? "")) {
    throw new Error("catalog verification requires exact Azure signer identity");
  }
  const inspection = verifyCatalogWithPowerShell({ manifestPath: manifest, catalogPath: catalog, commandRunner, powershellPath });
  if (!inspection.verified) throw new Error("release manifest catalog is invalid");
  if (normalizeSubject(inspection.subject) !== normalizeSubject(expectedSigner.subject)) throw new Error("catalog signer subject mismatch");
  const certificateSha256 = String(inspection.certificateSha256 ?? "").replaceAll(":", "").toLowerCase();
  if (!CERTIFICATE_SHA256.test(certificateSha256) || certificateSha256 !== String(expectedSigner.certificateSha256).toLowerCase()) throw new Error("catalog signer certificate fingerprint mismatch");
  return { verified: true, manifestPath: manifest, catalogPath: catalog, signer: { ...expectedSigner, certificateSha256 } };
}

export function signReleaseManifestCatalogWithAzure({
  manifestPath,
  catalogPath,
  commandRunner = spawnSync,
  environment = process.env,
  signtoolPath,
  dlibPath,
  metadataPath,
}) {
  if (process.platform !== "win32" && commandRunner === spawnSync) {
    throw new Error("RightRelease manifest signing must run on protected Windows release host");
  }
  const userEnv = (name) => readUserEnvironment(name, commandRunner);
  const env = (name) => environment[name] || userEnv(name);
  const signtool = signtoolPath || firstExisting([
    env("AZURE_SIGNTOOL_PATH"),
    env("SIGNTOOL_PATH"),
    ...signtoolCandidates(env),
  ]);
  if (!signtool) throw new Error("RightRelease manifest signer could not find signtool.exe");
  const dlib = dlibPath || firstExisting([
    env("AZURE_CODESIGN_DLIB_PATH"),
    env("AZURE_ARTIFACT_SIGNING_DLIB_PATH"),
    join(env("LOCALAPPDATA") || "", "AzureArtifactSigningTools", "Microsoft.ArtifactSigning.Client", "bin", "x64", "Azure.CodeSigning.Dlib.dll"),
    "C:\\Program Files\\Microsoft Azure Artifact Signing Client Tools\\x64\\Azure.CodeSigning.Dlib.dll",
    "C:\\Program Files (x86)\\Microsoft\\ArtifactSigningClientTools\\bin\\x64\\Azure.CodeSigning.Dlib.dll",
  ]);
  if (!dlib) throw new Error("RightRelease manifest signer could not find Azure Artifact Signing dlib");

  const temp = mkdtempSync(join(tmpdir(), "rightrelease-manifest-catalog-"));
  const powershellEnvironment = windowsPowerShellEnvironment(environment);
  const metadata = metadataPath || materializeMetadata(temp, env);
  const catalogSource = join(temp, "catalog-source");
  const unsignedCatalog = join(temp, "release-manifest.cat");
  mkdirSync(catalogSource, { recursive: true });
  copyFileSync(resolve(manifestPath), join(catalogSource, "release-manifest.json"));
  try {
    const materializer = join(temp, "new-catalog.ps1");
    writeFileSync(materializer, [
      "param([string]$SourcePath, [string]$CatalogPath)",
      "Import-Module Microsoft.PowerShell.Security -ErrorAction Stop",
      "$Result = New-FileCatalog -Path $SourcePath -CatalogFilePath $CatalogPath -CatalogVersion 2.0",
      "if (-not (Test-Path $Result)) { throw 'Manifest catalog was not created' }",
    ].join("\r\n") + "\r\n");
    const created = commandRunner("powershell", ["-NoProfile", "-File", materializer, catalogSource, unsignedCatalog], { encoding: "utf8", windowsHide: true, env: powershellEnvironment });
    if (created?.status !== 0 || !existsSync(unsignedCatalog)) throw new Error(`RightRelease manifest catalog creation failed: ${String(created?.stderr ?? "").trim()}`);
    const result = commandRunner(signtool, [
      "sign", "/v", "/fd", "SHA256", "/tr", "http://timestamp.acs.microsoft.com", "/td", "SHA256",
      "/dlib", dlib, "/dmdf", metadata,
      unsignedCatalog,
    ], { encoding: "utf8", windowsHide: true, env: environment });
    if (result?.status !== 0) throw new Error(`RightRelease manifest catalog signing failed: ${String(result?.stderr ?? "").trim()}`);
    const inspection = verifyCatalogWithPowerShell({ manifestPath: resolve(manifestPath), catalogPath: unsignedCatalog, commandRunner, powershellPath: "powershell", environment: powershellEnvironment });
    if (normalizeSubject(inspection.subject) !== normalizeSubject(RIGHTRELEASE_MANIFEST_SIGNER.subject) || !CERTIFICATE_SHA256.test(inspection.certificateSha256 ?? "")) throw new Error(`RightRelease catalog signer identity does not match protected Azure profile: ${inspection.subject}`);
    mkdirSync(dirname(catalogPath), { recursive: true });
    copyFileSync(unsignedCatalog, catalogPath);
    return { catalogPath: resolve(catalogPath), signer: { ...RIGHTRELEASE_MANIFEST_SIGNER, certificateSha256: inspection.certificateSha256.toLowerCase() } };
  } finally {
    rmSync(temp, { recursive: true, force: true });
  }
}

function verifyCatalogWithPowerShell({ manifestPath, catalogPath, commandRunner, powershellPath, environment = process.env }) {
  const temp = mkdtempSync(join(tmpdir(), "rightrelease-catalog-verify-"));
  const source = join(temp, "catalog-source");
  mkdirSync(source, { recursive: true });
  copyFileSync(manifestPath, join(source, "release-manifest.json"));
  const verifier = join(temp, "verify-catalog.ps1");
  writeFileSync(verifier, [
    "param([string]$SourcePath, [string]$CatalogPath)",
    "Import-Module Microsoft.PowerShell.Security -ErrorAction Stop",
    "$Signature = Get-AuthenticodeSignature -FilePath $CatalogPath",
    "if ($Signature.Status -ne 'Valid' -or $null -eq $Signature.SignerCertificate) { throw 'Catalog Authenticode signature is invalid' }",
    "$Validation = Test-FileCatalog -Path $SourcePath -CatalogFilePath $CatalogPath",
    "if ([string]$Validation -ne 'Valid') { throw 'Catalog does not bind exact manifest bytes' }",
    "$Certificate = $Signature.SignerCertificate",
    "$Sha256 = [Security.Cryptography.SHA256]::Create()",
    "try { $Fingerprint = ([BitConverter]::ToString($Sha256.ComputeHash($Certificate.RawData))).Replace('-','').ToLowerInvariant() } finally { $Sha256.Dispose() }",
    "@{ verified=$true; subject=$Certificate.Subject; certificateSha256=$Fingerprint } | ConvertTo-Json -Compress",
  ].join("\r\n") + "\r\n");
  try {
    const result = commandRunner(powershellPath, ["-NoProfile", "-File", verifier, source, catalogPath], { encoding: "utf8", windowsHide: true, env: windowsPowerShellEnvironment(environment) });
    if (result?.status !== 0) throw new Error(`catalog verification failed: ${String(result?.stderr ?? "").trim()}`);
    return parseCatalogInspection(result?.stdout);
  } finally {
    rmSync(temp, { recursive: true, force: true });
  }
}

function parseCatalogInspection(output) {
  let inspection;
  try { inspection = JSON.parse(String(output ?? "").trim()); } catch { throw new Error("catalog verifier returned invalid JSON"); }
  if (inspection.verified !== true || typeof inspection.subject !== "string" || typeof inspection.certificateSha256 !== "string") throw new Error("catalog verifier returned incomplete identity");
  return inspection;
}

function normalizeSubject(subject) {
  return String(subject).trim().replace(/\s*=\s*/g, "=").replace(/\s*,\s*/g, ",");
}

function windowsPowerShellEnvironment(environment) {
  const sanitized = { ...environment };
  for (const key of Object.keys(sanitized)) {
    if (key.toLowerCase() === "psmodulepath") delete sanitized[key];
  }
  return sanitized;
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
  if (!Endpoint || !CodeSigningAccountName || !CertificateProfileName) {
    throw new Error("RightRelease manifest signer requires existing Azure Artifact Signing configuration");
  }
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
  const result = commandRunner("powershell", ["-NoProfile", "-Command", `[Environment]::GetEnvironmentVariable('${name.replaceAll("'", "''")}','User')`], {
    encoding: "utf8",
    windowsHide: true,
  });
  return result?.status === 0 ? String(result.stdout ?? "").trim() : "";
}
