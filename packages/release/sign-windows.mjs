#!/usr/bin/env node
// Windows Authenticode signing through Azure Artifact Signing. Two entries: the CLI (`node sign-windows.mjs [--dry-run|--verify-only] [--receipt <file>] <file>...`,
// unchanged) and the functions `signWindowsFiles` / `verifyWindowsFiles` that the gate's in-process signing steps reuse (suite-signing.mjs), so there is
// one signer and one verifier. The identity is the signer SUBJECT (WINDOWS_SIGNER_SUBJECT), never a thumbprint.
import { createHash } from "node:crypto";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, statSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "./background-process.mjs";
import { pathToFileURL } from "node:url";
import { azureCliCandidates, signingEnv } from "./signing-env.mjs";
import { WINDOWS_SIGNER_SUBJECT } from "./signing-identity.mjs";


export { WINDOWS_SIGNER_SUBJECT };

const EXCLUDE_CREDENTIALS = [
  "ManagedIdentityCredential",
  "SharedTokenCacheCredential",
  "VisualStudioCredential",
  "VisualStudioCodeCredential",
  "AzurePowerShellCredential",
  "AzureDeveloperCliCredential",
  "InteractiveBrowserCredential",
];

/** A signing or verification failure. `exitCode` is the failing tool's status when a tool failed (the CLI exits with it), `silent` that the tool already printed. */
export class WindowsSigningError extends Error {
  constructor(message, { exitCode = 1, silent = false } = {}) {
    super(message);
    this.name = "WindowsSigningError";
    this.code = "WINDOWS_SIGNING_FAILED";
    this.exitCode = exitCode;
    this.silent = silent;
  }
}
const fail = (message) => { throw new WindowsSigningError(message); };

/** The signer's Azure sign-in is not usable: a typed failure (code SIGN_WINDOWS_TOKEN_UNAVAILABLE), never a process exit. */
export const TOKEN_UNAVAILABLE_CODE = "SIGN_WINDOWS_TOKEN_UNAVAILABLE";
export class WindowsSigningTokenError extends WindowsSigningError {
  constructor(detail) {
    super(`${TOKEN_UNAVAILABLE_CODE}: the Azure signer has no silent token (${detail}). Fix: sign in to the Windows desktop session as the signing user (interactive \`az login\` there, so AzureCliCredential finds a token in the MSAL cache); \`az account show\` passing is not enough`);
    this.code = TOKEN_UNAVAILABLE_CODE;
  }
}
const CODESIGNING_RESOURCE = "https://codesigning.azure.net";
const EXPIRY = /^\d{4}-\d{2}-\d{2}[ T]\d{2}:\d{2}:\d{2}(\.\d+)?$/;

/**
 * The tools and environment one signing session uses. Everything that touches the host is injectable (run, exists, platform, the environment
 * lookup, the user-environment reader), so the sequence below runs with fakes in tests; production passes nothing.
 */
export function windowsSigningContext({ processEnv = process.env, run = spawnSync, exists = existsSync, platform = process.platform, userEnvReader, quiet = false } = {}) {
  const userEnvCache = new Map();
  const readUserEnv = userEnvReader ?? ((name) => {
    const quoted = name.replace(/'/g, "''");
    const result = run("powershell", ["-NoProfile", "-Command", `[Environment]::GetEnvironmentVariable('${quoted}','User')`], { encoding: "utf8", windowsHide: true });
    return result.status === 0 ? String(result.stdout ?? "").trim() : "";
  });
  const userEnv = (name) => {
    if (!userEnvCache.has(name)) userEnvCache.set(name, readUserEnv(name));
    return userEnvCache.get(name);
  };
  const env = (name) => processEnv[name] || userEnv(name);
  const findFirst = (candidates) => candidates.find((candidate) => candidate && exists(candidate));
  const windowsKitCandidates = (bin) => {
    if (!exists(bin)) return [];
    return readdirSync(bin, { withFileTypes: true }).filter((entry) => entry.isDirectory()).map((entry) => path.join(bin, entry.name, "x64", "signtool.exe")).sort().reverse();
  };
  const signtoolCandidates = () => [
    ...windowsKitCandidates(path.join(env("LOCALAPPDATA") || "", "AzureArtifactSigningTools", "Microsoft.Windows.SDK.BuildTools", "bin")),
    ...windowsKitCandidates(path.join(env("ProgramFiles(x86)") || "C:\\Program Files (x86)", "Windows Kits", "10", "bin")),
  ];
  const childEnv = signingEnv(processEnv, env);
  const runTool = (cmd, runArgs) => {
    const result = run(cmd, runArgs, { env: childEnv, encoding: "utf8", windowsHide: true });
    if (!quiet) {
      if (result.stdout) process.stdout.write(result.stdout);
      if (result.stderr) process.stderr.write(result.stderr);
    }
    if (result.status !== 0) throw new WindowsSigningError(`${path.basename(cmd)} ${runArgs[0]} failed (exit ${result.status ?? "unavailable"})${quiet ? `: ${String(result.stderr || result.stdout || result.error?.message || "").trim()}` : ""}`, { exitCode: result.status ?? 1, silent: !quiet });
    return `${result.stdout ?? ""}\n${result.stderr ?? ""}`;
  };
  // The token preflight asks `az` the way the signer's AzureCliCredential does (cmd.exe /d /c az, the signer env) and keeps only the expiry: az is asked for
  // `expiresOn` alone, so no token is printed, and anything token-shaped in an error is redacted. Never throws a process exit; returns { ok, detail }.
  const checkToken = ({ now = Date.now(), minMinutes = 10, timeoutMs = 120000 } = {}) => {
    const command = `az account get-access-token --resource ${CODESIGNING_RESOURCE} --query expiresOn -o tsv`;
    const result = run(process.env.ComSpec || "cmd.exe", ["/d", "/c", command], { env: childEnv, encoding: "utf8", windowsHide: true, timeout: timeoutMs, cwd: env("SystemRoot") ? path.win32.join(env("SystemRoot"), "System32") : undefined });
    const redact = (text) => String(text ?? "").replace(/(eyJ[A-Za-z0-9_-]+\.)+[A-Za-z0-9_-]*/g, "<redacted>").trim();
    if (result?.error) return { ok: false, detail: `az could not be run: ${redact(result.error.message)}` };
    if (result?.status !== 0) return { ok: false, detail: `az account get-access-token exit ${result?.status ?? "unavailable"}: ${redact(result?.stderr) || "no output"}` };
    const stdout = String(result.stdout ?? "").trim();
    if (!EXPIRY.test(stdout)) return { ok: false, detail: `az printed something other than an expiry (not shown, ${stdout.length} chars)` };
    const expires = new Date(stdout.replace(" ", "T")).getTime();
    if (!Number.isFinite(expires) || expires < now + minMinutes * 60000) return { ok: false, detail: `the token expires at ${stdout} (local), under ${minMinutes} minutes from now` };
    return { ok: true, detail: stdout };
  };
  return { env, platform, exists, findFirst, signtoolCandidates, run: runTool, checkToken };
}

/** Before any copy, temp file or output: the signer's Azure CLI sign-in must yield a silent token. */
export function requireSignerToken(ctx = windowsSigningContext({ quiet: true })) {
  const state = ctx.checkToken();
  if (!state.ok) throw new WindowsSigningTokenError(state.detail);
}

/**
 * Check one `signtool verify /pa /v` report: valid chain, signer subject, timestamp. Throws through `fail`; returns the evidence fields.
 * Self-contained on purpose (release-cli-contract.test.mjs evaluates this function alone): the subject is WINDOWS_SIGNER_SUBJECT, spelled out.
 */
function assertVerification(file, output) {
  if (!/successfully verified/i.test(output)) fail(`Authenticode verification did not report Valid for ${file}`);
  if (!/^[ \t]*Issued to:[ \t]*Damned Ventures LLC[ \t]*$/m.test(output)) fail(`Authenticode subject must be Damned Ventures LLC for ${file}`);
  if (!/(?:timestamp|time stamped|timestamped)/i.test(output)) fail(`Authenticode timestamp is missing for ${file}`);
  return { authenticode: "Valid", subject: "CN=Damned Ventures LLC", timestampPresent: true };
}

function fileEvidence(file) {
  const bytes = readFileSync(file);
  return { sha256: createHash("sha256").update(bytes).digest("hex"), sizeBytes: statSync(file).size };
}

export const assertWindowsVerification = assertVerification;

function requireSigntool(ctx) {
  const signtool = ctx.findFirst([ctx.env("AZURE_SIGNTOOL_PATH"), ctx.env("SIGNTOOL_PATH"), ...ctx.signtoolCandidates()]);
  if (!signtool) fail("signtool.exe not found; set AZURE_SIGNTOOL_PATH or install Windows SDK Build Tools");
  return signtool;
}

function requireWindowsHost(ctx, files) {
  if (ctx.platform !== "win32") fail("Windows signing must run on Windows");
  for (const file of files) if (!ctx.exists(file)) fail(`missing file: ${file}`);
}

/** Verify Authenticode on each file (signtool verify /pa /v) against the signer subject. Returns one evidence record per file. */
export function verifyWindowsFiles(files, { ctx = windowsSigningContext() } = {}) {
  const absolute = files.map((file) => path.resolve(file));
  requireWindowsHost(ctx, absolute);
  const signtool = requireSigntool(ctx);
  return absolute.map((file) => ({ file, ...assertVerification(file, ctx.run(signtool, ["verify", "/pa", "/v", file])) }));
}

/**
 * Sign each file with Azure Artifact Signing, verify it, and return one evidence record per file `{ file, before, after, authenticode, subject,
 * timestampPresent }`. `receiptPath` also writes the receipt the release path reads (`{ schema: 1, files }`).
 */
export function signWindowsFiles(files, { ctx = windowsSigningContext(), receiptPath = null } = {}) {
  const absolute = files.map((file) => path.resolve(file));
  requireWindowsHost(ctx, absolute);
  requireSignerToken(ctx);
  const signtool = requireSigntool(ctx);
  const dlib = ctx.findFirst([
    ctx.env("AZURE_CODESIGN_DLIB_PATH"),
    ctx.env("AZURE_ARTIFACT_SIGNING_DLIB_PATH"),
    path.join(ctx.env("LOCALAPPDATA") || "", "AzureArtifactSigningTools", "Microsoft.ArtifactSigning.Client", "bin", "x64", "Azure.CodeSigning.Dlib.dll"),
    "C:\\Program Files\\Microsoft Azure Artifact Signing Client Tools\\x64\\Azure.CodeSigning.Dlib.dll",
    "C:\\Program Files (x86)\\Microsoft\\ArtifactSigningClientTools\\bin\\x64\\Azure.CodeSigning.Dlib.dll",
  ]);
  if (!dlib) fail("Azure.CodeSigning.Dlib.dll not found; set AZURE_CODESIGN_DLIB_PATH or install Azure Artifact Signing Client Tools");
  const tempDir = mkdtempSync(path.join(os.tmpdir(), "right-sign-"));
  const evidence = [];
  try {
    const metadata = metadataPath(ctx, tempDir);
    for (const file of absolute) {
      const before = fileEvidence(file);
      ctx.run(signtool, ["sign", "/v", "/fd", "SHA256", "/tr", "http://timestamp.acs.microsoft.com", "/td", "SHA256", "/dlib", dlib, "/dmdf", metadata, file]);
      const verification = assertVerification(file, ctx.run(signtool, ["verify", "/pa", "/v", file]));
      evidence.push({ file, before, after: fileEvidence(file), ...verification });
    }
  } finally {
    rmSync(tempDir, { recursive: true, force: true });
  }
  if (receiptPath) {
    const absoluteReceipt = path.resolve(receiptPath);
    mkdirSync(path.dirname(absoluteReceipt), { recursive: true });
    writeFileSync(absoluteReceipt, `${JSON.stringify({ schema: 1, files: evidence }, null, 2)}\n`);
  }
  return evidence;
}

function metadataPath(ctx, dir) {
  const existing = ctx.env("AZURE_ARTIFACT_SIGNING_METADATA") || ctx.env("AZURE_SIGNING_METADATA");
  if (existing) {
    if (!ctx.exists(existing)) fail(`metadata file not found: ${existing}`);
    return existing;
  }
  const Endpoint = ctx.env("AZURE_ARTIFACT_SIGNING_ENDPOINT") || ctx.env("AZURE_SIGNING_ENDPOINT") || ctx.env("AZURE_ENDPOINT");
  const CodeSigningAccountName = ctx.env("AZURE_ARTIFACT_SIGNING_ACCOUNT") || ctx.env("AZURE_SIGNING_ACCOUNT_NAME") || ctx.env("AZURE_ACCOUNT");
  const CertificateProfileName = ctx.env("AZURE_ARTIFACT_SIGNING_PROFILE") || ctx.env("AZURE_CERTIFICATE_PROFILE_NAME") || ctx.env("AZURE_PROFILE");
  if (!Endpoint || !CodeSigningAccountName || !CertificateProfileName) {
    fail("set AZURE_ARTIFACT_SIGNING_ENDPOINT, AZURE_ARTIFACT_SIGNING_ACCOUNT, and AZURE_ARTIFACT_SIGNING_PROFILE");
  }
  const file = path.join(dir, "metadata.json");
  writeFileSync(file, `${JSON.stringify({ Endpoint, CodeSigningAccountName, CertificateProfileName, ExcludeCredentials: EXCLUDE_CREDENTIALS }, null, 2)}\n`);
  return file;
}

/** The CLI. Returns the exit code; prints what the script always printed. It never exits itself: only the entry below does, after this returned (its temp directory is removed in signWindowsFiles' finally). */
export function main(args = process.argv.slice(2), options = {}) {
  const dryRun = args.includes("--dry-run");
  const verifyOnly = args.includes("--verify-only");
  const receiptIndex = args.indexOf("--receipt");
  const receiptPath = receiptIndex >= 0 ? args[receiptIndex + 1] : null;
  const files = args
    .filter((arg, index) => arg !== "--dry-run" && arg !== "--verify-only" && arg !== "--receipt" && (receiptIndex < 0 || index !== receiptIndex + 1))
    .map((arg) => path.resolve(arg));
  try {
    if (!files.length) fail("usage: node sign-windows.mjs [--dry-run|--verify-only] <file>...");
    if (receiptIndex >= 0 && !receiptPath) fail("--receipt requires a path");
    if (dryRun) {
      console.log(`dry-run: ${verifyOnly ? "Authenticode verification" : "Azure Artifact Signing"} ${files.join(", ")}`);
      return 0;
    }
    const ctx = windowsSigningContext(options);
    if (verifyOnly) verifyWindowsFiles(files, { ctx });
    else signWindowsFiles(files, { ctx, receiptPath });
    return 0;
  } catch (error) {
    if (!(error instanceof WindowsSigningError)) throw error;
    if (!error.silent) console.error(`right-sign-windows: ${error.message}`);
    return error.exitCode;
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) process.exit(main());
