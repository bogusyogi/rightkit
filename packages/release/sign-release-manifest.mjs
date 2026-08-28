import { existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { basename, dirname, join, resolve } from "node:path";
import { spawnSync } from "node:child_process";

export const RIGHTRELEASE_MANIFEST_SIGNER = Object.freeze({
  id: "azure-artifact-signing-damned-ventures-v1",
  subject: "CN=Damned Ventures LLC",
  algorithm: "cms-sha256",
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

export function signReleaseManifestWithAzure({
  manifestPath,
  signaturePath,
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

  const temp = mkdtempSync(join(tmpdir(), "rightrelease-manifest-sign-"));
  const metadata = metadataPath || materializeMetadata(temp, env);
  const outputDir = join(temp, "signature");
  mkdirSync(outputDir, { recursive: true });
  try {
    const result = commandRunner(signtool, [
      "sign", "/v", "/fd", "SHA256", "/tr", "http://timestamp.acs.microsoft.com", "/td", "SHA256",
      "/dlib", dlib, "/dmdf", metadata,
      "/p7", outputDir, "/p7ce", "DetachedSignedData", "/p7co", "1.2.840.113549.1.7.1",
      resolve(manifestPath),
    ], { encoding: "utf8", windowsHide: true, env: environment });
    if (result?.status !== 0) throw new Error(`RightRelease manifest signing failed: ${String(result?.stderr ?? "").trim()}`);
    const generated = join(outputDir, `${basename(manifestPath)}.p7`);
    if (!existsSync(generated)) throw new Error("RightRelease manifest signer did not produce detached CMS signature");
    const inspector = join(temp, "inspect-signer.ps1");
    writeFileSync(inspector, [
      "param([string]$ManifestPath, [string]$SignaturePath)",
      "Add-Type -AssemblyName System.Security",
      "$Content = New-Object Security.Cryptography.Pkcs.ContentInfo(,[IO.File]::ReadAllBytes($ManifestPath))",
      "$Cms = New-Object Security.Cryptography.Pkcs.SignedCms($Content, $true)",
      "$Cms.Decode([IO.File]::ReadAllBytes($SignaturePath))",
      "$Cms.CheckSignature($true)",
      "if ($Cms.SignerInfos.Count -ne 1) { throw 'Expected exactly one manifest signer' }",
      "$Certificate = $Cms.SignerInfos[0].Certificate",
      "$Sha256 = [Security.Cryptography.SHA256]::Create()",
      "try { $Fingerprint = ([BitConverter]::ToString($Sha256.ComputeHash($Certificate.RawData))).Replace('-','').ToLowerInvariant() } finally { $Sha256.Dispose() }",
      "@{ subject=$Certificate.Subject; certificateSha256=$Fingerprint } | ConvertTo-Json -Compress",
    ].join("\r\n") + "\r\n");
    const inspection = commandRunner("powershell", ["-NoProfile", "-File", inspector, resolve(manifestPath), generated], { encoding: "utf8", windowsHide: true, env: environment });
    if (inspection?.status !== 0) throw new Error(`RightRelease could not inspect manifest signer: ${String(inspection?.stderr ?? "").trim()}`);
    let certificate;
    try { certificate = JSON.parse(String(inspection.stdout ?? "").trim()); } catch { throw new Error("RightRelease manifest signer inspection returned invalid JSON"); }
    if (certificate.subject !== RIGHTRELEASE_MANIFEST_SIGNER.subject || !/^[a-f0-9]{64}$/.test(certificate.certificateSha256 ?? "")) {
      throw new Error("RightRelease manifest signer identity does not match protected Azure profile");
    }
    mkdirSync(dirname(signaturePath), { recursive: true });
    writeFileSync(signaturePath, readFileSync(generated));
    return { signaturePath: resolve(signaturePath), signer: { ...RIGHTRELEASE_MANIFEST_SIGNER, certificateSha256: certificate.certificateSha256 } };
  } finally {
    rmSync(temp, { recursive: true, force: true });
  }
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
