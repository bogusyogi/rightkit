// right-release Android upload: a sealed, upload-key-signed AAB to the Google Play INTERNAL track through the Play Developer
// API (androidpublisher v3): edits.insert -> edits.bundles.upload -> edits.tracks.update (internal) -> edits.commit.
// Build-free: it reads only the sealed release that `right-release build --platform android` wrote.
//
// Config (release config, top level; the explicit provider matches the other providers):
//   distribution: {
//     provider: "google-play",
//     packageName: "com.orthiclabs.heardright",          // the production applicationId (a dev/qa/test/debug package is refused)
//     track: "internal",                                  // the only track this command can write
//     releaseStatus: "completed",                         // or "draft" (a draft release for a not-yet-published app)
//     releaseName?: "HeardRight 1.2.3",
//     serviceAccount: { credential: "play-service-account" },   // OS credential store, or { file: "~/keys/play.json" } outside the repo
//   }
//
// Secrets: the service-account JSON (which holds a private key) comes from the same stores as the signing passwords
// (android-release.mjs: Keychain `rightkit-android-<name>` on macOS, a protected token file elsewhere, or a 0600 file outside the
// repository). It is parsed in memory only. The signed JWT and the OAuth access token live in memory, are never written to disk,
// never put on argv, never logged, and never part of an error message. Any ambient environment variable that carries key
// material is refused.
//
// Dry run is the default: everything local is verified and the exact API plan is printed; nothing is read from the secret store
// and no network call is made. `--apply` performs the upload.
import { createSign } from "node:crypto";
import { existsSync, mkdirSync, writeFileSync } from "node:fs";
import path from "node:path";
import { pathToFileURL } from "node:url";
import { spawnSync } from "./background-process.mjs";
import { describeAabProblems, scanAabBuffer } from "./android-aab-scan.mjs";
import { sealedManifestAllowsContentDevtools } from "./devtools-policy.mjs";
import { AndroidReleaseError, createAndroidCredentialStore, resolveAndroidSecret, validateSecretSource } from "./android-release.mjs";
import { assertArtifactUnchanged, digestBytes, verifyPublicationRelease } from "./publication-artifacts.mjs";
import { assertPrimaryReleaseCheckout } from "./release-invocation.mjs";
import { assertCleanSource } from "./source-gate.mjs";

export const PLAY_PROVIDER = "google-play";
export const PLAY_TRACK = "internal";
export const PLAY_SCOPE = "https://www.googleapis.com/auth/androidpublisher";
export const PLAY_TOKEN_URL = "https://oauth2.googleapis.com/token";
export const PLAY_API = "https://androidpublisher.googleapis.com/androidpublisher/v3/applications";
export const PLAY_UPLOAD_API = "https://androidpublisher.googleapis.com/upload/androidpublisher/v3/applications";
export const PLAY_MARKER_NAME = "verified-play-internal.json";

const PACKAGE_NAME = /^[A-Za-z][A-Za-z0-9_]*(?:\.[A-Za-z][A-Za-z0-9_]*)+$/;
const TEST_SEGMENT = /^(?:dev|qa|test|debug)$/i;
const RELEASE_ID = /^[A-Za-z0-9][A-Za-z0-9._+-]{0,159}$/;
const DISTRIBUTION_KEYS = ["provider", "packageName", "track", "releaseStatus", "releaseName", "serviceAccount"];

export class PlayUploadError extends Error {
  constructor(code, message) { super(`[${code}] ${message}`); this.name = "PlayUploadError"; this.code = code; }
}
const refuse = (code, message) => { throw new PlayUploadError(code, message); };

// ---- config ------------------------------------------------------------------------------------------------------
export function validatePlayDistribution(distribution) {
  if (!distribution || typeof distribution !== "object" || Array.isArray(distribution)) refuse("PLAY_CONFIG_INVALID", "release config distribution is required");
  if (distribution.provider !== PLAY_PROVIDER) refuse("PLAY_CONFIG_INVALID", `release config distribution.provider must be ${PLAY_PROVIDER} for an Android upload`);
  const unknown = Object.keys(distribution).filter((key) => !DISTRIBUTION_KEYS.includes(key));
  if (unknown.length) refuse("PLAY_CONFIG_INVALID", `distribution has unsupported key(s) ${unknown.join(", ")}; the service account is only ever a credential name or a file path`);
  const packageName = String(distribution.packageName ?? "");
  if (!PACKAGE_NAME.test(packageName)) refuse("PLAY_CONFIG_INVALID", "distribution.packageName must be the Android applicationId");
  if (TEST_SEGMENT.test(packageName.split(".").at(-1))) refuse("PLAY_TEST_PACKAGE", `distribution.packageName ${packageName} is a dev/qa/test/debug package; those never go to Play`);
  const track = distribution.track ?? PLAY_TRACK;
  if (track !== PLAY_TRACK) refuse("PLAY_TRACK_REFUSED", `distribution.track must be ${PLAY_TRACK}; this command writes the internal track only`);
  const releaseStatus = distribution.releaseStatus ?? "completed";
  if (releaseStatus !== "completed" && releaseStatus !== "draft") refuse("PLAY_CONFIG_INVALID", "distribution.releaseStatus must be completed or draft");
  if (distribution.releaseName !== undefined && (typeof distribution.releaseName !== "string" || !distribution.releaseName.trim() || distribution.releaseName.length > 120 || /[\u0000-\u001f]/.test(distribution.releaseName))) {
    refuse("PLAY_CONFIG_INVALID", "distribution.releaseName must be a short single-line string");
  }
  let serviceAccount;
  try { serviceAccount = validateSecretSource(distribution.serviceAccount, "distribution.serviceAccount"); } catch (error) { refuse("PLAY_CONFIG_INVALID", error.message); }
  return { packageName, track, releaseStatus, releaseName: distribution.releaseName, serviceAccount };
}

// The service-account JSON never arrives through the environment: refuse key material by value and the usual names, names only.
export function playEnvSecretNames(env = process.env) {
  const names = [];
  for (const [name, value] of Object.entries(env)) {
    const text = String(value ?? "");
    if (!text.trim()) continue;
    if (/-----BEGIN [A-Z ]*PRIVATE KEY-----/.test(text) || /"private_key"\s*:/.test(text)) names.push(name);
    else if (/^(?:PLAY_|GOOGLE_PLAY_|ANDROID_PUBLISHER_)\w*(?:KEY|SECRET|TOKEN|SERVICE_?ACCOUNT)\w*$/i.test(name)) names.push(name);
  }
  return names;
}
export function refusePlayEnvSecrets(env = process.env) {
  const [name] = playEnvSecretNames(env);
  if (name) refuse("PLAY_ENV_SECRET_REFUSED", `${name} is refused: the Play service account must never be passed in the environment; keep it in the OS credential store or a 0600 file named by distribution.serviceAccount`);
}

// ---- the plan (pure data, no secrets) -----------------------------------------------------------------------------
export function playUploadPlan({ distribution, aab }) {
  const pkg = encodeURIComponent(distribution.packageName);
  const release = { ...(distribution.releaseName ? { name: distribution.releaseName } : {}), status: distribution.releaseStatus, versionCodes: ["<versionCode from bundles.upload>"] };
  return {
    packageName: distribution.packageName,
    track: distribution.track,
    aab: { name: aab.name, sha256: aab.sha256, sizeBytes: aab.sizeBytes },
    steps: [
      { id: "edits.insert", method: "POST", url: `${PLAY_API}/${pkg}/edits` },
      { id: "bundles.upload", method: "POST", url: `${PLAY_UPLOAD_API}/${pkg}/edits/<editId>/bundles?uploadType=media`, contentType: "application/octet-stream", bytes: aab.sizeBytes },
      { id: "tracks.update", method: "PUT", url: `${PLAY_API}/${pkg}/edits/<editId>/tracks/${distribution.track}`, body: { track: distribution.track, releases: [release] } },
      { id: "edits.commit", method: "POST", url: `${PLAY_API}/${pkg}/edits/<editId>:commit` },
    ],
  };
}

// ---- authentication (service account -> short-lived access token) -------------------------------------------------
export function parseServiceAccount(text) {
  let value;
  try { value = JSON.parse(text); } catch { refuse("PLAY_SERVICE_ACCOUNT_INVALID", "the Play service account secret is not valid JSON"); }
  if (!value || value.type !== "service_account" || typeof value.client_email !== "string" || !value.client_email.includes("@")
    || typeof value.private_key !== "string" || !/-----BEGIN [A-Z ]*PRIVATE KEY-----/.test(value.private_key)) {
    refuse("PLAY_SERVICE_ACCOUNT_INVALID", "the Play service account secret must be a service_account key file (client_email, private_key)");
  }
  // A tampered secret must not redirect the signed assertion to another host.
  if (value.token_uri !== undefined && value.token_uri !== PLAY_TOKEN_URL) refuse("PLAY_SERVICE_ACCOUNT_INVALID", `the Play service account token_uri must be ${PLAY_TOKEN_URL}`);
  return { clientEmail: value.client_email, privateKey: value.private_key };
}

const base64url = (input) => Buffer.from(input).toString("base64url");
export function signServiceAccountAssertion(account, { nowSeconds = Math.floor(Date.now() / 1000), sign = (data, key) => createSign("RSA-SHA256").update(data).sign(key) } = {}) {
  const head = base64url(JSON.stringify({ alg: "RS256", typ: "JWT" }));
  const claims = base64url(JSON.stringify({ iss: account.clientEmail, scope: PLAY_SCOPE, aud: PLAY_TOKEN_URL, iat: nowSeconds, exp: nowSeconds + 3600 }));
  const data = `${head}.${claims}`;
  return `${data}.${base64url(sign(data, account.privateKey))}`;
}

// Credentials never survive into a message: bearer tokens, Google access tokens and anything JWT-shaped are masked.
const redact = (text) => text.replace(/Bearer\s+\S+/gi, "Bearer [redacted]").replace(/ya29\.[\w.-]+/g, "[redacted]").replace(/[\w-]{16,}\.[\w-]{16,}\.[\w-]{8,}/g, "[redacted]");

// The Google error message only: never a request body, header or token.
async function failure(response, step) {
  let detail = "";
  try { const body = await response.json(); detail = redact(String(body?.error?.message ?? body?.error_description ?? body?.error ?? "")).replace(/\s+/g, " ").slice(0, 300); } catch { /* no body */ }
  return new PlayUploadError("PLAY_API_FAILED", `${step} failed with HTTP ${response.status}${detail ? `: ${detail}` : ""}`);
}

export async function fetchAccessToken(account, { fetchImpl = fetch, nowSeconds, sign } = {}) {
  const assertion = signServiceAccountAssertion(account, { nowSeconds, sign });
  const response = await fetchImpl(PLAY_TOKEN_URL, {
    method: "POST",
    headers: { "content-type": "application/x-www-form-urlencoded" },
    body: new URLSearchParams({ grant_type: "urn:ietf:params:oauth:grant-type:jwt-bearer", assertion }).toString(),
    signal: AbortSignal.timeout(60_000),
  });
  if (!response.ok) throw await failure(response, "Play OAuth token exchange");
  const body = await response.json();
  if (typeof body?.access_token !== "string" || !body.access_token) refuse("PLAY_API_FAILED", "Play OAuth token exchange returned no access token");
  return body.access_token;
}

// ---- the edit transaction -----------------------------------------------------------------------------------------
// An edit that fails after edits.insert is deleted so a half-built edit never lingers; the commit is the only publishing step.
export async function uploadToPlay({ distribution, aabBytes, accessToken, fetchImpl = fetch, log = console.log }) {
  const pkg = encodeURIComponent(distribution.packageName);
  const auth = { authorization: `Bearer ${accessToken}` };
  const call = async (step, url, init, timeoutMs = 120_000) => {
    const response = await fetchImpl(url, { ...init, headers: { ...auth, ...(init.headers ?? {}) }, signal: AbortSignal.timeout(timeoutMs) });
    if (!response.ok) throw await failure(response, step);
    return response.json();
  };
  const edit = await call("edits.insert", `${PLAY_API}/${pkg}/edits`, { method: "POST", headers: { "content-type": "application/json" }, body: "{}" });
  const editId = String(edit?.id ?? "");
  if (!/^[A-Za-z0-9_-]+$/.test(editId)) refuse("PLAY_API_FAILED", "edits.insert returned no edit id");
  log(`right-release: play edit ${editId} opened for ${distribution.packageName}`);
  try {
    const bundle = await call("bundles.upload", `${PLAY_UPLOAD_API}/${pkg}/edits/${editId}/bundles?uploadType=media`,
      { method: "POST", headers: { "content-type": "application/octet-stream" }, body: aabBytes }, 30 * 60_000);
    const versionCode = Number(bundle?.versionCode);
    if (!Number.isInteger(versionCode) || versionCode <= 0) refuse("PLAY_API_FAILED", "bundles.upload returned no versionCode");
    log(`right-release: play bundle uploaded, versionCode ${versionCode}`);
    const release = { ...(distribution.releaseName ? { name: distribution.releaseName } : {}), status: distribution.releaseStatus, versionCodes: [String(versionCode)] };
    await call("tracks.update", `${PLAY_API}/${pkg}/edits/${editId}/tracks/${distribution.track}`,
      { method: "PUT", headers: { "content-type": "application/json" }, body: JSON.stringify({ track: distribution.track, releases: [release] }) });
    await call("edits.commit", `${PLAY_API}/${pkg}/edits/${editId}:commit`, { method: "POST" });
    log(`right-release: play edit ${editId} committed to the ${distribution.track} track`);
    return { editId, versionCode };
  } catch (error) {
    try { await fetchImpl(`${PLAY_API}/${pkg}/edits/${editId}`, { method: "DELETE", headers: auth, signal: AbortSignal.timeout(30_000) }); } catch { /* best effort */ }
    throw error;
  }
}

// ---- authorisation and the command --------------------------------------------------------------------------------
const git = (cwd, args) => {
  const result = spawnSync("git", args, { cwd, encoding: "utf8", windowsHide: true });
  if (result.status !== 0) throw new Error(`git ${args.join(" ")} failed: ${result.stderr}`);
  return result.stdout.trim();
};

// Local checks only; no network, no secret. The sealed release is the only input: build-free by construction.
export async function authorizePlayUpload({ releaseId, configName = "right-release.config.mjs", cwd = process.cwd() }) {
  if (!RELEASE_ID.test(releaseId ?? "")) refuse("PLAY_RELEASE_REQUIRED", "--release <sealed-id> is required");
  const repoRoot = git(cwd, ["rev-parse", "--show-toplevel"]);
  assertPrimaryReleaseCheckout(repoRoot);
  const commit = git(repoRoot, ["rev-parse", "HEAD"]);
  assertCleanSource({ status: git(repoRoot, ["status", "--porcelain=v1", "-z", "--untracked-files=all"]), commandId: "right-release upload" });
  const configPath = path.resolve(cwd, configName);
  if (!existsSync(configPath)) refuse("PLAY_CONFIG_INVALID", `release config is required: ${configName}`);
  // The config is a module that runs on import: loaded only after the tree is proven clean.
  const config = (await import(`${pathToFileURL(configPath).href}?upload=${Date.now()}`)).default;
  const distribution = validatePlayDistribution(config?.distribution);
  const sealedDir = path.join(repoRoot, ".right-release", "sealed", releaseId, "android");
  const sealed = verifyPublicationRelease({ sealedDir, repoRoot, appRoot: path.dirname(configPath), commit, app: config.app, version: config.version, platform: "android", configPath, releaseId });
  const entries = sealed.manifest.files.filter((entry) => entry.role === "aab");
  if (entries.length !== 1) refuse("PLAY_AAB_MISSING", "the sealed release must hold exactly one aab entry");
  const aab = sealed.artifacts.get(entries[0].name);
  // A debug-signed or unpinned bundle is refused here again, from the sealed bytes, whatever the receipts say.
  const scan = scanAabBuffer(aab.bytes, { allowContentOnlyDevtools: sealedManifestAllowsContentDevtools(sealed.manifest), label: entries[0].name });
  assertUploadableScan(scan, sealed.manifest.signing?.uploadCertSha256);
  return { repoRoot, sealed, sealedDir, configPath, config, distribution, aab, entry: entries[0] };
}

// A scanned bundle may be uploaded only when it is clean, signed, not debug-signed, and signed by exactly the pinned upload certificate.
export function assertUploadableScan(scan, pinnedCertSha256) {
  const problems = describeAabProblems(scan);
  if (problems.length) refuse("PLAY_AAB_REFUSED", `the sealed AAB is refused:\n${problems.join("\n")}`);
  if (!scan.signature) refuse("PLAY_AAB_UNSIGNED", "the sealed AAB has no readable upload-key signature");
  if (scan.signature.debugCertificate) refuse("PLAY_AAB_DEBUG_SIGNED", "the sealed AAB is signed with the Android debug certificate; a debug-signed build is never uploaded");
  if (!/^[0-9a-f]{64}$/.test(pinnedCertSha256 ?? "") || scan.signature.certSha256 !== pinnedCertSha256) refuse("PLAY_AAB_SIGNER_MISMATCH", "the sealed AAB signer is not the pinned upload certificate");
}

export async function runPlayUpload({
  releaseId, configName, cwd = process.cwd(), apply = false, env = process.env, platform = process.platform, log = console.log,
  fetchImpl = fetch, credentialStore, sign, nowSeconds, authorize = authorizePlayUpload,
}) {
  refusePlayEnvSecrets(env);
  const auth = await authorize({ releaseId, configName, cwd });
  const aabEvidence = { name: auth.entry.name, sha256: auth.entry.sha256, sizeBytes: auth.entry.sizeBytes };
  const plan = playUploadPlan({ distribution: auth.distribution, aab: aabEvidence });
  const stateRoot = path.join(auth.repoRoot, ".right-release", "state", releaseId, "android");
  const marker = path.join(stateRoot, PLAY_MARKER_NAME);
  if (!apply) {
    log(`right-release upload: dry-run (default; pass --apply to upload) ${releaseId} -> Play ${plan.track} track of ${plan.packageName}`);
    log(JSON.stringify(plan, null, 2));
    return { applied: false, plan };
  }
  if (existsSync(marker)) { log(`right-release upload: already uploaded ${releaseId} to the Play ${plan.track} track`); return { applied: false, alreadyUploaded: true, plan }; }
  const store = credentialStore ?? createAndroidCredentialStore({ env, platform });
  const account = parseServiceAccount(resolveAndroidSecret(auth.distribution.serviceAccount, { root: path.dirname(auth.configPath), store, label: "distribution.serviceAccount" }));
  const bytes = assertArtifactUnchanged(auth.aab);
  if (digestBytes(bytes) !== auth.entry.sha256) refuse("PLAY_AAB_REFUSED", "the sealed AAB bytes changed after verification");
  const accessToken = await fetchAccessToken(account, { fetchImpl, nowSeconds, sign });
  const result = await uploadToPlay({ distribution: auth.distribution, aabBytes: bytes, accessToken, fetchImpl, log });
  mkdirSync(stateRoot, { recursive: true });
  writeFileSync(marker, `${JSON.stringify({ schema: 1, releaseId, platform: "android", provider: PLAY_PROVIDER, packageName: plan.packageName, track: plan.track, versionCode: result.versionCode, editId: result.editId, aabSha256: aabEvidence.sha256, uploadedAt: new Date().toISOString() }, null, 2)}\n`);
  log(`right-release upload: uploaded ${releaseId} versionCode ${result.versionCode} to the Play ${plan.track} track of ${plan.packageName}`);
  return { applied: true, plan, ...result };
}

// Entry used by upload-release.mjs for --platform android. Exit code 0 on success, 1 on any refusal.
export async function main({ releaseId, configName, apply, dryRun }) {
  if (apply && dryRun) { console.error("right-release upload: --apply and --dry-run are mutually exclusive"); return 2; }
  try {
    await runPlayUpload({ releaseId, configName, apply: apply === true });
    return 0;
  } catch (error) {
    if (error instanceof PlayUploadError || error instanceof AndroidReleaseError) { console.error(`right-release upload: ${error.message}`); return 1; }
    console.error(`right-release upload: ${error.message}`);
    return 1;
  }
}
