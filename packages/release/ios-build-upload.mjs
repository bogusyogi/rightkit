// In-process IPA upload through the App Store Connect API build-upload resources (ASC API 4.1). No altool, no Transporter and
// NO key file: the JWT is signed in memory and every API call carries only that bearer token (settled decision D4).
//
// Resources and fields, checked against Apple's reference on 2026-10-18 (the doc pages are JS-rendered, so the field tables were
// read from developer.apple.com/tutorials/data/documentation/appstoreconnectapi/...):
//   POST  /v1/buildUploads        https://developer.apple.com/documentation/appstoreconnectapi/post-v1-builduploads
//         data { type "buildUploads", attributes { cfBundleShortVersionString, cfBundleVersion, platform: IOS|MAC_OS|TV_OS|VISION_OS }  (all required),
//                relationships { app } (required) }
//   GET   /v1/apps/{id}/buildUploads, GET /v1/buildUploads/{id}/buildUploadFiles   (the related-endpoint lists on the pages above)
//   POST  /v1/buildUploadFiles    https://developer.apple.com/documentation/appstoreconnectapi/post-v1-builduploadfiles
//         data { type "buildUploadFiles", attributes { assetType ASSET, fileName, fileSize, uti "com.apple.ipa" } (all required),
//                relationships { buildUpload } (required) }
//         response attributes.uploadOperations[]: { method, url, length, offset, partNumber, entityTag, expiration, requestHeaders[{name,value}] }
//   PATCH /v1/buildUploadFiles/{id}  https://developer.apple.com/documentation/appstoreconnectapi/patch-v1-builduploadfiles-_id_  (commit)
//         data { type "buildUploadFiles", id, attributes { uploaded: true, sourceFileChecksums: { file: { hash, algorithm: MD5 } } } } (SHA_256 refused live)
//   GET   /v1/buildUploads/{id}   attributes.state { state: AWAITING_UPLOAD|PROCESSING|FAILED|COMPLETE, errors[], warnings[], infos[] }
//         https://developer.apple.com/documentation/appstoreconnectapi/builduploadstate
// Pre-signed part uploads (https://developer.apple.com/documentation/appstoreconnectapi/uploading-assets-to-app-store-connect, read as
// the .../tutorials/data/... json): send exactly the bytes offset..offset+length with the operation's requestHeaders and NO JWT; the URL
// carries an `Expires` unix timestamp; a part that fails may be resent any time before the commit; after an expiry delete the reservation
// and start over. The page names no fixed host set; its sample is store-030.blobstore.apple.com, so only *.blobstore.apple.com is
// accepted here (UPLOAD_HOST_SUFFIXES) and anything else fails closed, naming the host. Live run 2026-10-19 (Pulse 0.1.0 2026101803): Apple
// handed out northamerica-1.object-storage.apple.com operations, so *.object-storage.apple.com is accepted too.
// Not stated by the reference and so unverified without a live run: that `hash` is lowercase hex, the exact StateDetail error fields,
// and the `limit` query on the buildUploads list.
//
// Idempotent calls (GET, PATCH, each part PUT) are retried with bounded backoff, honouring Retry-After on 429 and 5xx. The create POSTs
// are never retried: an existing buildUploads for the same app, version and build is looked up first and resumed (AWAITING_UPLOAD),
// waited for (PROCESSING), finished (COMPLETE) or refused (FAILED), but ONLY if the record is in this vault's journal for the same IPA
// sha256 (written right after each create POST). Any other record for that version and build is another binary's: refused, never adopted.
import { createHash } from "node:crypto";
import { closeSync, fstatSync, openSync, readSync } from "node:fs";
import { verifyArtifactFile } from "./publication-artifacts.mjs";
import { RETRY_DEFAULTS, TestFlightError, parseRetryAfter, retryDelayMs, retryableStatus } from "./ios-testflight.mjs";

export const UPLOAD_HOST_SUFFIXES = Object.freeze(["blobstore.apple.com", "object-storage.apple.com"]);
const FORBIDDEN_HEADERS = new Set(["authorization", "cookie", "proxy-authorization"]);
const HEADER_NAME = /^[!#$%&'*+.^_`|~0-9A-Za-z-]+$/;
const EXPIRY_MARGIN_MS = 60_000;

const fail = (code, message) => { throw new TestFlightError(code, message); };
const text = (value) => String(value ?? "");

/** The ordered plan for a dry run (pure data, no secret). */
export function buildUploadPlan({ appId, marketingVersion, buildNumber, ipa }) {
  return [
    { id: "buildUploads.find", method: "GET", url: `https://api.appstoreconnect.apple.com/v1/apps/${appId}/buildUploads`, note: `resume, finish or refuse an existing upload of ${marketingVersion} (${buildNumber})` },
    { id: "buildUploads.create", method: "POST", url: "https://api.appstoreconnect.apple.com/v1/buildUploads", note: `${marketingVersion} (${buildNumber}) for app ${appId}, platform IOS; never retried` },
    { id: "buildUploadFiles.reserve", method: "POST", url: "https://api.appstoreconnect.apple.com/v1/buildUploadFiles", note: `${ipa.name}, ${ipa.sizeBytes} bytes, uti com.apple.ipa; never retried` },
    { id: "uploadOperations.put", method: "PUT", url: "<pre-signed *.blobstore.apple.com URLs returned by buildUploadFiles>", note: "one ranged PUT per reserved operation, streamed from the sealed file, retried with backoff" },
    { id: "buildUploadFiles.commit", method: "PATCH", url: "https://api.appstoreconnect.apple.com/v1/buildUploadFiles/<id>", note: `uploaded: true with the SHA_256 ${ipa.sha256.slice(0, 12)}...` },
    { id: "buildUploads.poll", method: "GET", url: "https://api.appstoreconnect.apple.com/v1/buildUploads/<id>", note: "until state COMPLETE" },
  ];
}

/** A sealed file read in ranges: hashed in a stream here once, and never held in memory as a whole. */
export function fileSource(file, evidence) {
  const verified = verifyArtifactFile(file, evidence);
  const same = (stat) => ["dev", "ino", "size", "mtimeMs", "ctimeMs"].every((key) => stat[key] === verified.identity[key]);
  return {
    file,
    size: verified.sizeBytes,
    sha256: verified.sha256,
    read(offset, length) {
      const fd = openSync(file, "r");
      try {
        if (!same(fstatSync(fd))) fail("IOS_BUILD_UPLOAD_SOURCE_CHANGED", "the sealed IPA changed while it was being uploaded");
        const buffer = Buffer.alloc(length);
        let filled = 0;
        while (filled < length) {
          const count = readSync(fd, buffer, filled, length - filled, offset + filled);
          if (count <= 0) fail("IOS_BUILD_UPLOAD_SOURCE_CHANGED", "the sealed IPA is shorter than its recorded size");
          filled += count;
        }
        return buffer;
      } finally { closeSync(fd); }
    },
    /** MD5 of the sealed bytes (what the commit sends), streamed through read() so a changed file is still refused. */
    md5() {
      if (this._md5) return this._md5;
      const hash = createHash("md5");
      for (let offset = 0; offset < this.size; offset += 8 * 1024 * 1024) hash.update(this.read(offset, Math.min(8 * 1024 * 1024, this.size - offset)));
      return (this._md5 = hash.digest("hex"));
    },
  };
}

/** One upload operation: PUT, https on an Apple upload host, no credential headers, not expired. Throws before a byte is sent. */
export function assertUploadOperation(op, { now = Date.now(), hostSuffixes = UPLOAD_HOST_SUFFIXES } = {}) {
  const bad = (message) => fail("IOS_BUILD_UPLOAD_INVALID", message);
  if (text(op.method).toUpperCase() !== "PUT") bad(`unsupported upload operation method ${text(op.method) || "(none)"}`);
  let url;
  try { url = new URL(text(op.url)); } catch { bad("an upload operation URL is not a URL"); }
  if (url.protocol !== "https:") bad("an upload operation URL is not https");
  if (url.username || url.password) bad("an upload operation URL carries credentials");
  const host = url.hostname.toLowerCase();
  if (!hostSuffixes.some((suffix) => host === suffix || host.endsWith(`.${suffix}`))) bad(`the upload host ${host} is not an Apple upload host (${hostSuffixes.map((suffix) => `*.${suffix}`).join(", ")})`);
  for (const header of op.requestHeaders ?? []) {
    const name = text(header?.name);
    if (!HEADER_NAME.test(name)) bad("an upload operation has an invalid header name");
    if (FORBIDDEN_HEADERS.has(name.toLowerCase())) bad(`an upload operation asks for the ${name} header; credentials are never sent to a pre-signed URL`);
    if (/[\r\n\0]/.test(text(header.value))) bad("an upload operation has an invalid header value");
  }
  const expiries = [];
  if (op.expiration !== undefined) { const at = Date.parse(text(op.expiration)); if (Number.isNaN(at)) bad("an upload operation has an unreadable expiration"); expiries.push(at); }
  const expires = url.searchParams.get("Expires");
  if (expires && /^\d{9,11}$/.test(expires)) expiries.push(Number(expires) * 1000);
  if (expiries.some((at) => at <= now + EXPIRY_MARGIN_MS)) fail("IOS_BUILD_UPLOAD_EXPIRED", "an upload operation has expired; delete the build upload reservation in App Store Connect and run the upload again (Apple: after an expiry start over with a new reservation)");
}

function tile(operations, size) {
  const ordered = [...operations].sort((a, b) => a.offset - b.offset);
  let cursor = 0;
  for (const op of ordered) {
    if (!Number.isInteger(op.offset) || !Number.isInteger(op.length) || op.offset !== cursor || op.length < 1 || op.offset + op.length > size) fail("IOS_BUILD_UPLOAD_INVALID", "the upload operations do not cover the IPA exactly");
    cursor += op.length;
  }
  if (cursor !== size) fail("IOS_BUILD_UPLOAD_INVALID", "the upload operations do not cover the IPA exactly");
  return ordered;
}

async function putPart(op, bytes, { fetchImpl, sleep, retry, partTimeoutMs, now }) {
  const headers = Object.fromEntries((op.requestHeaders ?? []).map((header) => [text(header.name), text(header.value)]));
  for (let attempt = 0; ; attempt += 1) {
    assertUploadOperation(op, { now: now() });
    let response;
    try {
      // The URL is pre-signed and carries its own authorisation: no bearer token goes to it, and it is never logged.
      response = await fetchImpl(op.url, { method: "PUT", headers, body: bytes, redirect: "error", signal: AbortSignal.timeout(partTimeoutMs) });
    } catch {
      if (attempt < retry.attempts - 1) { await sleep(retryDelayMs(attempt, undefined, retry)); continue; }
      fail("IOS_BUILD_UPLOAD_FAILED", `upload of part ${op.partNumber ?? "?"} failed to send`);
    }
    if (response.ok) return;
    if (attempt < retry.attempts - 1 && retryableStatus(response.status)) { await sleep(retryDelayMs(attempt, parseRetryAfter(response.headers?.get?.("retry-after"), now()), retry)); continue; }
    fail("IOS_BUILD_UPLOAD_FAILED", `upload of part ${op.partNumber ?? "?"} was refused with HTTP ${response.status}`);
  }
}

/**
 * Upload the sealed IPA (a fileSource) and wait until App Store Connect reports the build upload COMPLETE.
 * Returns { buildUploadId, fileId, parts, resumed }.
 */
export async function uploadIpaToAppStoreConnect({
  client, appId, marketingVersion, buildNumber, source, fileName, journal, exportWindow, fetchImpl = globalThis.fetch,
  pollIntervalMs = 20_000, timeoutMs = 30 * 60_000, partTimeoutMs = 30 * 60_000, retry = RETRY_DEFAULTS, now = () => Date.now(),
  sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms)), monotonic = () => performance.now(), log = () => {},
}) {
  if (!/^\d+$/.test(text(appId))) fail("IOS_BUILD_UPLOAD_CONFIG", "appId must be the numeric App Store Connect app id");
  if (!journal || typeof journal.read !== "function" || typeof journal.record !== "function") fail("IOS_BUILD_UPLOAD_CONFIG", "a build-upload journal is required: an upload record is adopted only if this vault created it for this IPA");
  if (!source || !Number.isInteger(source.size) || source.size < 1 || !/^[a-f0-9]{64}$/.test(text(source.sha256))) fail("IOS_BUILD_UPLOAD_CONFIG", "the IPA source is required");

  const journalled = journal.read(); // a damaged journal stops the run before any request
  // Look before creating: a create that reached Apple but lost its answer must not be repeated.
  const existing = (await client.getAll(`/v1/apps/${encodeURIComponent(text(appId))}/buildUploads?limit=200`)).filter((item) => {
    const attributes = item.attributes ?? {};
    return text(attributes.cfBundleShortVersionString) === text(marketingVersion) && text(attributes.cfBundleVersion) === text(buildNumber) && (attributes.platform === undefined || attributes.platform === "IOS");
  });
  const stateOf = (item) => text(item.attributes?.state?.state);
  const identity = { appId: text(appId), version: text(marketingVersion), build: text(buildNumber), ipaSha256: source.sha256 };
  const mine = (item) => journalled.find((entry) => entry.buildUploadId === text(item.id) && entry.appId === identity.appId && entry.version === identity.version && entry.build === identity.build);
  // Only a record this vault created, for these very bytes, may be adopted. Anything else with the same version and build is another
  // binary's (a second Mac, a fresh vault, a manual upload): our bytes never go into it and its result is never reported as ours.
  // Xcode's own `-exportArchive` (method app-store-connect) registers an empty build upload for the archive's version and build,
  // even with destination export. That record is this build's: adopt it only while it is still AWAITING_UPLOAD with no file, was
  // created inside the sealed export's window (exportWindow: exportStartedAt - 30 s to builtAt, from the sealed receipt), and is the
  // only record of any state for this version and build.
  const fromOwnExport = async (item) => {
    if (!exportWindow || stateOf(item) !== "AWAITING_UPLOAD") return false;
    if (existing.length !== 1) return false; // the sole record for this version and build, in any state
    const created = Date.parse(text(item.attributes?.createdDate));
    const notBefore = Date.parse(text(exportWindow.notBefore)), notAfter = Date.parse(text(exportWindow.notAfter));
    if (![created, notBefore, notAfter].every(Number.isFinite) || created < notBefore || created > notAfter) return false;
    return (await client.getAll(`/v1/buildUploads/${encodeURIComponent(text(item.id))}/buildUploadFiles`)).length === 0;
  };
  for (const item of existing) {
    let entry = mine(item);
    if (stateOf(item) === "FAILED" && !entry) continue;
    if (!entry && await fromOwnExport(item)) {
      entry = { buildUploadId: text(item.id), buildUploadFileId: null, ...identity, adoptedFromExport: true };
      journal.record(entry);
      journalled.push(entry);
      log(`adopting build upload ${text(item.id)}: Xcode's export registered it for this build (${marketingVersion} ${buildNumber}) and it holds no file`);
    }
    if (!entry) fail("IOS_BUILD_UPLOAD_FOREIGN", `App Store Connect holds build upload ${text(item.id)} (${stateOf(item) || "unknown state"}) for ${marketingVersion} (${buildNumber}) that this machine did not create for this IPA; it is not adopted and nothing is sent into it; build again with a new build number`);
    if (entry.ipaSha256 !== identity.ipaSha256) fail("IOS_BUILD_UPLOAD_FOREIGN", `build upload ${text(item.id)} for ${marketingVersion} (${buildNumber}) was journalled for a different IPA (${text(entry.ipaSha256).slice(0, 12)}..., this one is ${identity.ipaSha256.slice(0, 12)}...); it is not adopted; build again with a new build number`);
  }
  const checkChecksum = (file) => {
    const reported = file?.attributes?.sourceFileChecksums?.file;
    if (reported?.hash && text(reported.algorithm).toUpperCase() === "SHA_256" && text(reported.hash).toLowerCase() !== source.sha256) fail("IOS_BUILD_UPLOAD_FOREIGN", `build upload file ${text(file.id)} reports a different SHA-256 than the sealed IPA; it is not adopted`);
    // The commit sends MD5, so a resumed, PROCESSING or COMPLETE file reports MD5: compare it with the sealed bytes too.
    if (reported?.hash && text(reported.algorithm).toUpperCase() === "MD5" && typeof source.md5 === "function" && text(reported.hash).toLowerCase() !== source.md5()) fail("IOS_BUILD_UPLOAD_FOREIGN", `build upload file ${text(file.id)} reports a different MD5 than the sealed IPA; it is not adopted`);
  };
  const filesOf = async (id) => (await client.getAll(`/v1/buildUploads/${encodeURIComponent(id)}/buildUploadFiles`));
  let buildUploadId;
  let resumed = false;
  let skipPut = false;
  if (existing.length) {
    const failed = existing.filter((item) => stateOf(item) === "FAILED");
    const live = existing.filter((item) => stateOf(item) !== "FAILED");
    const complete = live.find((item) => stateOf(item) === "COMPLETE");
    const processing = live.filter((item) => stateOf(item) === "PROCESSING");
    const awaiting = live.filter((item) => stateOf(item) === "AWAITING_UPLOAD");
    if (complete) {
      for (const file of await filesOf(text(complete.id))) checkChecksum(file);
      log(`build upload ${complete.id} for ${marketingVersion} (${buildNumber}) is already COMPLETE`);
      return { buildUploadId: text(complete.id), fileId: undefined, parts: 0, resumed: true };
    }
    if (live.length > 1 && processing.length + awaiting.length > 1) fail("IOS_BUILD_UPLOAD_AMBIGUOUS", `${live.length} build uploads exist for ${marketingVersion} (${buildNumber}); delete the extras in App Store Connect`);
    if (!live.length) fail("IOS_BUILD_UPLOAD_FAILED", `App Store Connect already holds a FAILED build upload (${failed.map((item) => item.id).join(", ")}) for ${marketingVersion} (${buildNumber}); a build number cannot be reused, so build again with a new build number`);
    buildUploadId = text(live[0].id);
    resumed = true;
    skipPut = stateOf(live[0]) === "PROCESSING";
    if (skipPut) for (const file of await filesOf(buildUploadId)) checkChecksum(file);
    log(`resuming build upload ${buildUploadId} (${stateOf(live[0])})`);
  } else {
    const created = await client.request("POST", "/v1/buildUploads", { data: {
      type: "buildUploads",
      attributes: { cfBundleShortVersionString: text(marketingVersion), cfBundleVersion: text(buildNumber), platform: "IOS" },
      relationships: { app: { data: { type: "apps", id: text(appId) } } },
    } });
    buildUploadId = text(created.data?.id);
    if (!buildUploadId) fail("IOS_BUILD_UPLOAD_INVALID", "buildUploads returned no id");
    journal.record({ buildUploadId, buildUploadFileId: null, ...identity }); // before anything else can fail
  }

  let fileId;
  let parts = 0;
  if (!skipPut) {
    let file = resumed
      ? (await filesOf(buildUploadId)).find((item) => text(item.attributes?.fileName) === fileName)
      : undefined;
    if (file && file.attributes?.fileSize !== undefined && file.attributes.fileSize !== source.size) fail("IOS_BUILD_UPLOAD_MISMATCH", `the existing reservation for ${fileName} is for ${file.attributes.fileSize} bytes, the sealed IPA is ${source.size}`);
    if (file) checkChecksum(file);
    const known = journal.read().find((entry) => entry.buildUploadId === buildUploadId);
    if (file && known?.buildUploadFileId && known.buildUploadFileId !== text(file.id)) fail("IOS_BUILD_UPLOAD_FOREIGN", `build upload file ${text(file.id)} is not the one journalled (${known.buildUploadFileId}); it is not adopted`);
    if (!file) {
      const reserved = await client.request("POST", "/v1/buildUploadFiles", { data: {
        type: "buildUploadFiles",
        attributes: { assetType: "ASSET", fileName, fileSize: source.size, uti: "com.apple.ipa" },
        relationships: { buildUpload: { data: { type: "buildUploads", id: buildUploadId } } },
      } });
      file = reserved.data;
      journal.record({ buildUploadId, buildUploadFileId: text(file?.id) || null, ...identity });
    }
    fileId = text(file?.id);
    const deliveryState = text(file?.attributes?.assetDeliveryState?.state);
    const operations = file?.attributes?.uploadOperations;
    if (!fileId) fail("IOS_BUILD_UPLOAD_INVALID", "buildUploadFiles returned no id");
    if (["UPLOAD_COMPLETE", "COMPLETE"].includes(deliveryState)) {
      log(`build upload file ${fileId} is already committed`);
    } else {
      if (!Array.isArray(operations) || !operations.length) fail("IOS_BUILD_UPLOAD_INVALID", "buildUploadFiles returned no upload operations");
      const ordered = tile(operations, source.size);
      for (const op of ordered) assertUploadOperation(op, { now: now() }); // all structure and header checks before a single byte leaves
      // The commit carries the file's MD5: the live API refused SHA_256 for sourceFileChecksums.file (HTTP 409, 2026-10-19). The parts tile
      // the file in order, so the digest is taken over exactly the bytes sent; the sealed SHA-256 is still enforced by fileSource.
      const md5 = createHash("md5");
      for (const op of ordered) { const bytes = source.read(op.offset, op.length); md5.update(bytes); await putPart(op, bytes, { fetchImpl, sleep, retry, partTimeoutMs, now }); }
      const fileMd5 = md5.digest("hex");
      parts = ordered.length;
      log(`build upload ${buildUploadId}: ${parts} part(s) sent`);
      await client.request("PATCH", `/v1/buildUploadFiles/${encodeURIComponent(fileId)}`, { data: {
        type: "buildUploadFiles", id: fileId,
        attributes: { uploaded: true, sourceFileChecksums: { file: { hash: fileMd5, algorithm: "MD5" } } },
      } });
    }
  }

  const deadline = monotonic() + timeoutMs;
  for (;;) {
    const current = await client.request("GET", `/v1/buildUploads/${encodeURIComponent(buildUploadId)}`);
    const state = current.data?.attributes?.state ?? {};
    if (state.state === "COMPLETE") return { buildUploadId, fileId, parts, resumed };
    if (state.state === "FAILED") {
      const detail = (state.errors ?? []).map((item) => text(item.description || item.message || item.code)).filter(Boolean).join("; ").replace(/Bearer\s+\S+/gi, "Bearer [redacted]").slice(0, 400);
      fail("IOS_BUILD_UPLOAD_FAILED", `App Store Connect rejected build upload ${buildUploadId}${detail ? `: ${detail}` : ""}`);
    }
    if (monotonic() >= deadline) fail("TESTFLIGHT_TIMEOUT", `timed out waiting for build upload ${buildUploadId} to complete (state ${text(state.state) || "unknown"})`);
    await sleep(pollIntervalMs);
  }
}
