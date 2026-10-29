// right-release iOS upload (bogusyogi/rightkit#349): a sealed, signed IPA to TestFlight, then the exact build into beta groups.
// Build-free: it reads only the sealed release that `right-release build --platform ios` wrote. Dry run is the default (as
// for Android): everything local is verified and the exact plan is printed; no Keychain item is read and nothing leaves the
// machine. `--apply` uploads.
//
// Config (release config, top level):
//   distribution: {
//     provider: "app-store-connect",
//     appId: "6806854037",                 // optional: the App Store Connect app id; when absent it is looked up from bundleId at --apply
//     bundleId: "app.heardright.ios.native",
//     track: "testflight",                 // the only track; there is no App Store submission
//     groups: "all" | ["Internal Dogfood"], // REQUIRED: no implicit policy
//     submitExternalBetaReview: false,      // default false; true creates the TestFlight beta app review submission for external groups
//     waitTimeoutSeconds: 1800, pollIntervalSeconds: 20,
//     credentials: { keyService?, keyIdService?, issuerService?, account? },   // Keychain service NAMES only
//   }
//
// Credentials (settled decision D4: no credential sits in a file): the .p8, key id and issuer id come from the macOS login
// Keychain (ios-asc-credentials.mjs) and live only in memory. A key path, key id, issuer or key in the config or the environment is
// refused. The IPA is uploaded in process through the App Store Connect build-upload resources (ios-build-upload.mjs), the
// TestFlight steps use the same in-memory ES256 JWT. No file, temp or not, ever holds the key; no altool, no Transporter.
import { existsSync, mkdirSync, readFileSync, renameSync, writeFileSync } from "node:fs";
import path from "node:path";
import { spawnSync } from "./background-process.mjs";
import { defaultReleaseConfigFile, importReleaseConfig } from "./release-config.mjs";
import { assertGuiSigningSession, assertIosHost, IosHostError } from "./ios-host.mjs";
import { acquireFileLock } from "./ios-lock.mjs";
import {
  IosCredentialsError, readAscCredentials, refuseIosAscEnv, resolveAscCredentialSource,
} from "./ios-asc-credentials.mjs";
import { buildUploadPlan, fileSource, uploadIpaToAppStoreConnect } from "./ios-build-upload.mjs";
import { IOS_SIGNING_CONTRACT } from "./ios-package-recipe.mjs";
import { ASC_API, createAscClient, createTokenProvider, distributeToTestFlight, normalizeGroupSelection, TestFlightError, testflightPlan } from "./ios-testflight.mjs";
import { iosOwnershipScope } from "./ownership-scope.mjs";
import { assertArtifactUnchanged, digestBytes, verifyPublicationRelease } from "./publication-artifacts.mjs";
import { assertBindsSource, assertInsideSource, assertReleaseSource, gitRootOf, isSnapshotSource, resolveReleaseSource } from "./release-source-identity.mjs";
import { verifyOwnershipEvidence } from "./ownership-evidence.mjs";
import { targetDistribution } from "./target-distribution.mjs";

export const IOS_PROVIDER = "app-store-connect";
export const IOS_TRACK = "testflight";
export const IOS_UPLOADED_MARKER = "verified-ios-upload.json";
export const IOS_DISTRIBUTED_MARKER = "verified-ios-testflight.json";
export const IOS_EVIDENCE_NAME = "ios-testflight.json";

const RELEASE_ID = /^[A-Za-z0-9][A-Za-z0-9._+-]{0,159}$/;
const TEST_SEGMENT = /^(?:dev|qa|test|debug)$/i;
const DISTRIBUTION_KEYS = ["provider", "appId", "bundleId", "track", "groups", "submitExternalBetaReview", "waitTimeoutSeconds", "pollIntervalSeconds", "credentials"];
const CREDENTIAL_LIKE = /key|issuer|p8|pem|secret|token|password|file|path|env/i;

export class IosUploadError extends Error {
  constructor(code, message) { super(`[${code}] ${message}`); this.name = "IosUploadError"; this.code = code; }
}
const refuse = (code, message) => { throw new IosUploadError(code, message); };

// ---- config ------------------------------------------------------------------------------------------------------
export function validateIosDistribution(distribution) {
  if (!distribution || typeof distribution !== "object" || Array.isArray(distribution)) refuse("IOS_UPLOAD_CONFIG_INVALID", "release config distribution is required");
  if (distribution.provider !== IOS_PROVIDER) refuse("IOS_UPLOAD_CONFIG_INVALID", `release config distribution.provider must be ${IOS_PROVIDER} for an iOS upload`);
  for (const key of Object.keys(distribution)) {
    if (DISTRIBUTION_KEYS.includes(key)) continue;
    if (CREDENTIAL_LIKE.test(key)) refuse("IOS_ASC_CREDENTIAL_SOURCE_REFUSED", `distribution.${key} is refused: App Store Connect credentials are read only from the macOS login Keychain (distribution.credentials names the Keychain services), never from the config, a file or the environment`);
    refuse("IOS_UPLOAD_CONFIG_INVALID", `distribution has unsupported key ${key} (allowed: ${DISTRIBUTION_KEYS.join(", ")})`);
  }
  // appId is optional: absent, --apply resolves it from bundleId through the App Store Connect API (resolveAscAppId).
  const appId = distribution.appId === undefined ? null : String(distribution.appId);
  if (appId !== null && !/^\d{5,20}$/.test(appId)) refuse("IOS_UPLOAD_CONFIG_INVALID", "distribution.appId must be the numeric App Store Connect app id (or omit it to resolve it from bundleId)");
  const bundleId = String(distribution.bundleId ?? "");
  if (!/^[A-Za-z0-9-]+(?:\.[A-Za-z0-9-]+)+$/.test(bundleId)) refuse("IOS_UPLOAD_CONFIG_INVALID", "distribution.bundleId must be the reverse-DNS app bundle id");
  if (TEST_SEGMENT.test(bundleId.split(".").at(-1))) refuse("IOS_UPLOAD_TEST_BUNDLE", `distribution.bundleId ${bundleId} is a dev/qa/test/debug id; those never go to TestFlight`);
  const track = distribution.track ?? IOS_TRACK;
  if (track !== IOS_TRACK) refuse("IOS_UPLOAD_TRACK_REFUSED", `distribution.track must be ${IOS_TRACK}; this command never submits to the App Store`);
  let groups;
  if (distribution.groups === undefined) refuse("IOS_UPLOAD_CONFIG_INVALID", 'distribution.groups is required: "all" or a non-empty list of beta group names (there is no implicit default)');
  try { groups = normalizeGroupSelection(distribution.groups); } catch (error) { refuse("IOS_UPLOAD_CONFIG_INVALID", `distribution.${error.message}`); }
  if (distribution.submitExternalBetaReview !== undefined && typeof distribution.submitExternalBetaReview !== "boolean") refuse("IOS_UPLOAD_CONFIG_INVALID", "distribution.submitExternalBetaReview must be a boolean");
  const int = (value, label, min, max, fallback) => {
    if (value === undefined) return fallback;
    if (!Number.isInteger(value) || value < min || value > max) refuse("IOS_UPLOAD_CONFIG_INVALID", `distribution.${label} must be an integer ${min}..${max}`);
    return value;
  };
  let credentials;
  try { credentials = resolveAscCredentialSource(distribution.credentials, { label: "distribution.credentials" }); } catch (error) {
    if (error instanceof IosCredentialsError) refuse(error.code, error.message);
    throw error;
  }
  return {
    appId, bundleId, track, groups,
    submitExternalBetaReview: distribution.submitExternalBetaReview ?? false,
    waitTimeoutSeconds: int(distribution.waitTimeoutSeconds, "waitTimeoutSeconds", 60, 7200, 1800),
    pollIntervalSeconds: int(distribution.pollIntervalSeconds, "pollIntervalSeconds", 5, 300, 20),
    credentials: distribution.credentials,
    credentialServices: credentials,
  };
}

const APP_ID_PENDING = "{appId from bundleId}";

/**
 * The App Store Connect app id for a bundle id: exactly one app whose bundleId equals it (the filter is not trusted to be exact).
 * Read with the same in-memory JWT as the upload, so the config needs only the bundle id.
 */
export async function resolveAscAppId({ client, bundleId }) {
  const apps = await client.getAll(`/v1/apps?${new URLSearchParams({ "filter[bundleId]": bundleId, limit: "200" })}`);
  const exact = apps.filter((app) => String(app?.attributes?.bundleId ?? "") === bundleId);
  if (exact.length !== 1) refuse("IOS_ASC_APP_NOT_FOUND", exact.length ? `${exact.length} App Store Connect apps have bundle id ${bundleId}; set distribution.appId` : `no App Store Connect app has bundle id ${bundleId}; create the app record in App Store Connect first`);
  const id = String(exact[0].id ?? "");
  if (!/^\d{5,20}$/.test(id)) refuse("IOS_ASC_APP_NOT_FOUND", `App Store Connect returned a non-numeric app id for ${bundleId}`);
  return id;
}

/** The plan printed by a dry run: pure data, no secret, no Keychain read. */
export function iosUploadPlan({ distribution, ipa, receipt }) {
  const appId = distribution.appId ?? APP_ID_PENDING;
  return {
    appId,
    ...(distribution.appId ? {} : { appIdSource: `resolved at --apply: GET ${ASC_API}/v1/apps?filter[bundleId]=${distribution.bundleId} (exactly one exact match)` }),
    bundleId: distribution.bundleId,
    track: distribution.track,
    ipa: { name: ipa.name, sha256: ipa.sha256, sizeBytes: ipa.sizeBytes },
    marketingVersion: receipt.marketingVersion,
    buildNumber: receipt.buildNumber,
    credentials: { source: "macOS login Keychain", services: [distribution.credentialServices.keyService, distribution.credentialServices.keyIdService, distribution.credentialServices.issuerService], account: distribution.credentialServices.account },
    steps: [
      ...buildUploadPlan({ appId, marketingVersion: receipt.marketingVersion, buildNumber: receipt.buildNumber, ipa }),
      ...testflightPlan({ appId, marketingVersion: receipt.marketingVersion, buildNumber: receipt.buildNumber, groups: distribution.groups, submitExternalBetaReview: distribution.submitExternalBetaReview }),
    ],
  };
}

// ---- authorisation -----------------------------------------------------------------------------------------------

/** Local checks only; no network, no secret. The sealed release is the only input: build-free by construction. */
export async function authorizeIosUpload({ releaseId, cwd = process.cwd(), configName = defaultReleaseConfigFile(cwd), env = process.env }) {
  if (!RELEASE_ID.test(releaseId ?? "")) refuse("IOS_UPLOAD_RELEASE_REQUIRED", "--release <sealed-id> is required");
  // Source identity (release-source-identity.mjs, item 7 row 7.04). Outside a door run: the checkout's HEAD, primary worktree and a clean
  // status, as always. Inside one: the selected commit and its verified snapshot, with HEAD, status and branch of the checkout unread.
  const release = resolveReleaseSource({ cwd, env });
  assertReleaseSource(release, { commandId: "right-release upload", env });
  const split = isSnapshotSource(release);
  const repoRoot = gitRootOf(release);
  const commit = release.commit;
  const configPath = path.resolve(cwd, configName);
  assertInsideSource(release, configPath, "the release config");
  if (!existsSync(configPath)) refuse("IOS_UPLOAD_CONFIG_INVALID", `release config is required: ${configName}`);
  // A legacy config is a module that runs on import (a JSON config is data): loaded only after the tree is proven clean.
  const config = await importReleaseConfig(configPath, { query: `upload=${Date.now()}` });
  const distribution = validateIosDistribution(targetDistribution(config, "ios"));
  const sealedDir = path.join(repoRoot, ".right-release", "sealed", releaseId, "ios");
  // The sealed release, its signing receipts and the state live where the build wrote them (the checkout's `.right-release`); the scope of
  // the ownership scan is read from the snapshot's own config and project, which are the commit's.
  const appRoot = split ? release.checkoutAppRoot : path.dirname(configPath);
  const scopeRepoRoot = split ? release.snapshot : repoRoot;
  const scopeAppRoot = path.dirname(configPath);
  // The IPA is verified by a streamed hash and never held in memory (streamRoles); the parts are read as byte ranges at upload time.
  const sealed = verifyPublicationRelease({ sealedDir, repoRoot, appRoot, commit, app: config.app, version: config.version, platform: "ios", configPath, releaseId, streamRoles: ["ipa"], ownershipScope: iosOwnershipScope({ repoRoot: scopeRepoRoot, appRoot: scopeAppRoot, target: config.targets?.ios, nativeAssembly: config.nativeAssembly }) },
    split ? { verifyOwnership: (expected, root, scanRoot, scope) => verifyOwnershipEvidence(expected, root, scanRoot, scope, release) } : {});
  if (split) assertBindsSource(release, sealed.manifest, "the sealed release");
  const entries = sealed.manifest.files.filter((entry) => entry.role === "ipa");
  if (entries.length !== 1) refuse("IOS_IPA_MISSING", "the sealed release must hold exactly one ipa entry");
  const ipa = sealed.artifacts.get(entries[0].name);
  // The build receipt rides in the seal; it names the version, build number, team and bundle id of exactly this IPA.
  const receiptEntry = (sealed.manifest.signing?.receipts ?? []).map((item) => ({ item, value: tryParse(path.resolve(appRoot, item.file), item.sha256) })).find((candidate) => candidate.value?.contract === IOS_SIGNING_CONTRACT);
  if (!receiptEntry) refuse("IOS_RECEIPT_MISSING", "the sealed release has no iOS build receipt");
  const receipt = receiptEntry.value;
  if (receipt.file?.sha256 !== entries[0].sha256) refuse("IOS_RECEIPT_MISMATCH", "the iOS build receipt describes a different IPA than the sealed ipa entry");
  if (receipt.bundleId !== distribution.bundleId) refuse("IOS_RECEIPT_MISMATCH", `distribution.bundleId ${distribution.bundleId} is not the IPA's bundle id ${receipt.bundleId}`);
  if (receipt.teamId !== sealed.manifest.signing?.teamId) refuse("IOS_RECEIPT_MISMATCH", "the iOS build receipt team id is not the team pinned in the sealed signing policy");
  return { repoRoot, commit, sealed, sealedDir, configPath, config, distribution, ipa, entry: entries[0], receipt };
}

function tryParse(file, expectedSha256) {
  try {
    const bytes = readFileSync(file);
    if (digestBytes(bytes) !== expectedSha256) return undefined;
    return JSON.parse(bytes.toString("utf8"));
  } catch { return undefined; }
}

// ---- the command -------------------------------------------------------------------------------------------------
const writeJsonAtomic = (file, value) => {
  mkdirSync(path.dirname(file), { recursive: true });
  const temporary = `${file}.tmp-${process.pid}`;
  writeFileSync(temporary, `${JSON.stringify(value, null, 2)}\n`);
  renameSync(temporary, file);
};

/**
 * The journal of build uploads this vault created: {buildUploadId, buildUploadFileId, appId, version, build, ipaSha256}. It is written
 * (atomically) right after each create POST, and an App Store Connect record is adopted only if it is listed here for the same IPA.
 * A missing file is an empty journal; an unreadable or malformed one is an error, never an empty journal.
 */
export function buildUploadJournal(dir, appId) {
  const file = path.join(dir, `ios-build-uploads.${appId}.json`);
  const read = () => {
    let raw;
    try { raw = readFileSync(file, "utf8"); } catch (error) {
      if (error?.code === "ENOENT") return [];
      refuse("IOS_UPLOAD_JOURNAL_INVALID", `the build-upload journal ${file} is unreadable (${error?.code ?? "error"}); fix or remove it only after checking App Store Connect`);
    }
    let parsed;
    try { parsed = JSON.parse(raw); } catch { parsed = undefined; }
    if (!parsed || !Array.isArray(parsed.entries) || parsed.entries.some((entry) => !entry || typeof entry.buildUploadId !== "string" || !/^[a-f0-9]{64}$/.test(String(entry.ipaSha256)))) refuse("IOS_UPLOAD_JOURNAL_INVALID", `the build-upload journal ${file} is malformed; fix or remove it only after checking App Store Connect`);
    return parsed.entries;
  };
  const record = (entry) => {
    const entries = read().filter((item) => item.buildUploadId !== entry.buildUploadId);
    writeJsonAtomic(file, { schema: 1, entries: [...entries, entry] });
  };
  return { file, read, record };
}

/** A marker may only be trusted for the very IPA, app and build number it was written for. */
function readMarker(file, expected, label) {
  let marker;
  try { marker = JSON.parse(readFileSync(file, "utf8")); } catch { refuse("IOS_UPLOAD_MARKER_INVALID", `${label} marker ${file} is unreadable; inspect App Store Connect and remove it only if the build was not uploaded`); }
  for (const [key, value] of Object.entries(expected)) {
    if (marker?.[key] !== value) refuse("IOS_UPLOAD_MARKER_MISMATCH", `${label} marker ${file} was written for a different ${key} (${String(marker?.[key])}, now ${String(value)}); refusing to skip the upload on it`);
  }
  return marker;
}

export async function runIosUpload({
  releaseId, configName, cwd = process.cwd(), apply = false, env = process.env, platform = process.platform, log = console.log,
  fetchImpl = globalThis.fetch, authorize = authorizeIosUpload, readCredentials = readAscCredentials, run = spawnSync, detectSession,
  acquireLock = acquireFileLock, journalDir, sleep, monotonic, now = () => new Date(),
}) {
  refuseIosAscEnv(env);
  const auth = await authorize({ releaseId, configName, cwd, env });
  const ipaEvidence = { name: auth.entry.name, sha256: auth.entry.sha256, sizeBytes: auth.entry.sizeBytes };
  const plan = iosUploadPlan({ distribution: auth.distribution, ipa: ipaEvidence, receipt: auth.receipt });
  const stateRoot = path.join(auth.repoRoot, ".right-release", "state", releaseId, "ios");
  const uploadedMarker = path.join(stateRoot, IOS_UPLOADED_MARKER);
  const distributedMarker = path.join(stateRoot, IOS_DISTRIBUTED_MARKER);
  if (!apply) {
    log(`right-release upload: dry-run (default; pass --apply to upload) ${releaseId} -> TestFlight of app ${plan.appId} (${plan.bundleId})`);
    log(JSON.stringify(plan, null, 2));
    return { applied: false, plan };
  }
  try { assertIosHost({ operation: "right-release upload --apply", platform }); } catch (error) {
    if (error instanceof IosHostError) refuse(error.code, error.message);
    throw error;
  }
  // One apply per release at a time (the same pid lock the build lane uses).
  const lock = acquireLock(path.join(auth.repoRoot, ".right-release", "locks", `ios-upload-${releaseId}.lock.json`), `right-release upload --platform ios ${releaseId}`);
  try {
    // The Keychain is read only in the GUI (Aqua) session; an SSH session is refused before any read. The key is read once, lazily.
    let ascClient;
    const client = () => {
      if (ascClient) return ascClient;
      try { assertGuiSigningSession({ operation: "right-release upload --apply (login Keychain)", env, run, detect: detectSession }); } catch (error) {
        if (error instanceof IosHostError) refuse(error.code, error.message);
        throw error;
      }
      const { keyId, issuer, privateKey } = readCredentials({ credentials: auth.distribution.credentials, env, platform });
      ascClient = createAscClient({ tokenProvider: createTokenProvider({ privateKey, keyId, issuer }), fetchImpl, ...(sleep ? { sleep } : {}) });
      return ascClient;
    };
    if (!auth.distribution.appId) {
      plan.appId = await resolveAscAppId({ client: client(), bundleId: plan.bundleId });
      log(`right-release upload: App Store Connect app ${plan.appId} resolved from bundle id ${plan.bundleId}`);
    }
    const identity = { appId: plan.appId, bundleId: plan.bundleId, marketingVersion: plan.marketingVersion, buildNumber: plan.buildNumber, ipaSha256: ipaEvidence.sha256 };
    if (existsSync(distributedMarker)) {
      readMarker(distributedMarker, identity, "distribution");
      log(`right-release upload: already distributed ${releaseId} to TestFlight`);
      return { applied: false, alreadyDistributed: true, plan };
    }

    // 1. The sealed bytes are uploaded once, in process (or an existing upload is resumed); a rerun after a later failure skips straight
    //    to the TestFlight steps, but only on a marker written for this very IPA, app and build number.
    const waits = { ...(sleep ? { sleep } : {}), ...(monotonic ? { monotonic } : {}) };
    if (!existsSync(uploadedMarker)) {
      const source = fileSource(auth.ipa.file, auth.entry); // streamed hash; refuses bytes that changed since the seal
      log(`right-release upload: uploading ${ipaEvidence.name} through the App Store Connect API (in process, no key file)`);
      const uploaded = await uploadIpaToAppStoreConnect({
        client: client(), appId: plan.appId, marketingVersion: plan.marketingVersion, buildNumber: plan.buildNumber, source, fileName: ipaEvidence.name, fetchImpl,
        // Xcode's export registers an empty build upload during the build; only one created between the sealed export start (less 30 s of
        // clock skew) and the receipt may be adopted. A receipt without exportStartedAt (sealed before it existed) adopts nothing.
        exportWindow: auth.receipt?.exportStartedAt && auth.receipt?.builtAt ? { notBefore: new Date(Date.parse(auth.receipt.exportStartedAt) - 30_000).toISOString(), notAfter: auth.receipt.builtAt } : undefined,
        journal: buildUploadJournal(journalDir ?? (env.RIGHT_RELEASE_VAULT_STATE_ROOT ? path.join(path.resolve(env.RIGHT_RELEASE_VAULT_STATE_ROOT), "ios-build-uploads") : path.join(auth.repoRoot, ".right-release", "state", "ios-build-uploads")), plan.appId),
        pollIntervalMs: auth.distribution.pollIntervalSeconds * 1000, timeoutMs: auth.distribution.waitTimeoutSeconds * 1000, ...waits,
        log: (line) => log(`right-release upload: ${line}`),
      });
      writeJsonAtomic(uploadedMarker, { schema: 1, releaseId, platform: "ios", provider: IOS_PROVIDER, ...identity, buildUploadId: uploaded.buildUploadId, uploadedAt: now().toISOString() });
      log(`right-release upload: uploaded ${releaseId} ${plan.marketingVersion} (${plan.buildNumber})`);
    } else {
      readMarker(uploadedMarker, identity, "upload");
      log(`right-release upload: ${releaseId} was already uploaded; continuing with TestFlight`);
    }

    // 2. TestFlight: the exact build into the beta groups.
    const distributed = await distributeToTestFlight({
      client: client(), appId: plan.appId, marketingVersion: plan.marketingVersion, buildNumber: plan.buildNumber, groups: auth.distribution.groups,
      submitExternalBetaReview: auth.distribution.submitExternalBetaReview,
      pollIntervalMs: auth.distribution.pollIntervalSeconds * 1000, timeoutMs: auth.distribution.waitTimeoutSeconds * 1000,
      ...waits,
      log: (line) => log(`right-release upload: ${line}`),
    });
    const evidence = { schema: 1, releaseId, platform: "ios", provider: IOS_PROVIDER, api: ASC_API, ipa: ipaEvidence, completedAt: now().toISOString(), testflight: distributed };
    writeJsonAtomic(path.join(auth.repoRoot, ".right-release", "receipts", IOS_EVIDENCE_NAME), evidence);
    writeJsonAtomic(distributedMarker, { schema: 1, releaseId, platform: "ios", ...identity, buildId: distributed.buildId, completedAt: evidence.completedAt });
    log(`right-release upload: ${releaseId} is in TestFlight for ${distributed.groups.length} beta group(s)`);
    return { applied: true, plan, evidence };
  } finally { lock.release(); }
}

/** Entry used by upload-release.mjs for --platform ios. Exit code 0 on success, 1 on any refusal. */
export async function main({ releaseId, configName, apply, dryRun }) {
  if (apply && dryRun) { console.error("right-release upload: --apply and --dry-run are mutually exclusive"); return 2; }
  try {
    await runIosUpload({ releaseId, configName, apply: apply === true });
    return 0;
  } catch (error) {
    console.error(`right-release upload: ${error instanceof TestFlightError ? `[${error.code}] ` : ""}${error.message}`);
    return 1;
  }
}
