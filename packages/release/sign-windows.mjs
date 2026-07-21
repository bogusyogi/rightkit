#!/usr/bin/env node
import { existsSync, mkdtempSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";

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
const userEnvCache = new Map();

const args = process.argv.slice(2);
const dryRun = args.includes("--dry-run");
const verifyOnly = args.includes("--verify-only");
const files = args.filter((arg) => arg !== "--dry-run" && arg !== "--verify-only").map((arg) => path.resolve(arg));

if (!files.length) fail("usage: node sign-windows.mjs [--dry-run|--verify-only] <file>...");
if (dryRun) {
  console.log(`dry-run: ${verifyOnly ? "Authenticode verification" : "Azure Artifact Signing"} ${files.join(", ")}`);
  process.exit(0);
}
if (process.platform !== "win32") fail("Windows signing must run on Windows");
for (const file of files) if (!existsSync(file)) fail(`missing file: ${file}`);

const childEnv = signingEnv();
const signtool = findFirst([env("AZURE_SIGNTOOL_PATH"), env("SIGNTOOL_PATH"), ...signtoolCandidates()]);
if (!signtool) fail("signtool.exe not found; set AZURE_SIGNTOOL_PATH or install Windows SDK Build Tools");
if (verifyOnly) {
  for (const file of files) run(signtool, ["verify", "/pa", "/v", file]);
  process.exit(0);
}
const dlib = findFirst([
  env("AZURE_CODESIGN_DLIB_PATH"),
  env("AZURE_ARTIFACT_SIGNING_DLIB_PATH"),
  path.join(env("LOCALAPPDATA") || "", "AzureArtifactSigningTools", "Microsoft.ArtifactSigning.Client", "bin", "x64", "Azure.CodeSigning.Dlib.dll"),
  "C:\\Program Files\\Microsoft Azure Artifact Signing Client Tools\\x64\\Azure.CodeSigning.Dlib.dll",
  "C:\\Program Files (x86)\\Microsoft\\ArtifactSigningClientTools\\bin\\x64\\Azure.CodeSigning.Dlib.dll",
]);
if (!dlib) fail("Azure.CodeSigning.Dlib.dll not found; set AZURE_CODESIGN_DLIB_PATH or install Azure Artifact Signing Client Tools");

const tempDir = mkdtempSync(path.join(os.tmpdir(), "right-sign-"));
const metadata = metadataPath(tempDir);
try {
  for (const file of files) {
    run(signtool, ["sign", "/v", "/fd", "SHA256", "/tr", "http://timestamp.acs.microsoft.com", "/td", "SHA256", "/dlib", dlib, "/dmdf", metadata, file]);
    run(signtool, ["verify", "/pa", "/v", file]);
  }
} finally {
  rmSync(tempDir, { recursive: true, force: true });
}

function metadataPath(dir) {
  const existing = env("AZURE_ARTIFACT_SIGNING_METADATA") || env("AZURE_SIGNING_METADATA");
  if (existing) {
    if (!existsSync(existing)) fail(`metadata file not found: ${existing}`);
    return existing;
  }
  const Endpoint = env("AZURE_ARTIFACT_SIGNING_ENDPOINT") || env("AZURE_SIGNING_ENDPOINT") || env("AZURE_ENDPOINT");
  const CodeSigningAccountName = env("AZURE_ARTIFACT_SIGNING_ACCOUNT") || env("AZURE_SIGNING_ACCOUNT_NAME") || env("AZURE_ACCOUNT");
  const CertificateProfileName = env("AZURE_ARTIFACT_SIGNING_PROFILE") || env("AZURE_CERTIFICATE_PROFILE_NAME") || env("AZURE_PROFILE");
  if (!Endpoint || !CodeSigningAccountName || !CertificateProfileName) {
    fail("set AZURE_ARTIFACT_SIGNING_ENDPOINT, AZURE_ARTIFACT_SIGNING_ACCOUNT, and AZURE_ARTIFACT_SIGNING_PROFILE");
  }
  const file = path.join(dir, "metadata.json");
  writeFileSync(file, `${JSON.stringify({ Endpoint, CodeSigningAccountName, CertificateProfileName, ExcludeCredentials: EXCLUDE_CREDENTIALS }, null, 2)}\n`);
  return file;
}

function signingEnv() {
  const paths = [path.dirname(findFirst(azureCliCandidates()) ?? ""), env("PATH")].filter(Boolean);
  return { ...process.env, PATH: paths.join(path.delimiter) };
}

function azureCliCandidates() {
  return [
    path.join(env("LOCALAPPDATA") || "", "AzureCLI", "bin", "az.cmd"),
    path.join(env("ProgramFiles(x86)") || "C:\\Program Files (x86)", "Microsoft SDKs", "Azure", "CLI2", "wbin", "az.cmd"),
    path.join(env("ProgramFiles") || "C:\\Program Files", "Microsoft SDKs", "Azure", "CLI2", "wbin", "az.cmd"),
  ];
}

function signtoolCandidates() {
  return [
    ...sdkBuildToolsCandidates(),
    ...windowsKitCandidates(path.join(env("ProgramFiles(x86)") || "C:\\Program Files (x86)", "Windows Kits", "10", "bin")),
  ];
}

function sdkBuildToolsCandidates() {
  const bin = path.join(env("LOCALAPPDATA") || "", "AzureArtifactSigningTools", "Microsoft.Windows.SDK.BuildTools", "bin");
  return windowsKitCandidates(bin);
}

function windowsKitCandidates(bin) {
  if (!existsSync(bin)) return [];
  return readdirSync(bin, { withFileTypes: true })
    .filter((entry) => entry.isDirectory())
    .map((entry) => path.join(bin, entry.name, "x64", "signtool.exe"))
    .sort()
    .reverse();
}

function findFirst(candidates) {
  return candidates.find((candidate) => candidate && existsSync(candidate));
}

function env(name) {
  return process.env[name] || userEnv(name);
}

function userEnv(name) {
  if (userEnvCache.has(name)) return userEnvCache.get(name);
  const result = spawnSync("powershell", [
    "-NoProfile",
    "-Command",
    `[Environment]::GetEnvironmentVariable('${name.replace(/'/g, "''")}','User')`,
  ], { encoding: "utf8", windowsHide: true });
  const value = result.status === 0 ? result.stdout.trim() : "";
  userEnvCache.set(name, value);
  return value;
}

function run(cmd, runArgs) {
  const result = spawnSync(cmd, runArgs, { env: childEnv, stdio: "inherit" });
  if (result.status !== 0) process.exit(result.status ?? 1);
}

function fail(message) {
  console.error(`right-sign-windows: ${message}`);
  process.exit(1);
}
