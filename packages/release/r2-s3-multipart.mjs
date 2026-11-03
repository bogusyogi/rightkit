// S3-compatible R2 transport for objects above the wrangler 300 MiB cap (#902): a multipart upload and a streamed sha256 + size readback, both
// signed with AWS Signature V4 using node:crypto and fetch only (no S3 client dependency).
//
// Credentials are derived in memory from the Cloudflare API token and never printed, logged, put on a command line or written to disk
// (docs/rules/cloudflare-access.md "R2 - app distribution"): the Access Key ID is the token's id (what the token-verify endpoint returns for
// it) and the Secret Access Key is the SHA-256 of the token value (https://developers.cloudflare.com/r2/api/tokens/). Every error message
// carries an HTTP status and the S3 error code, never a header, URL query or credential.
//
// The upload hashes the bytes it sends and compares the whole-file sha256 with the publication evidence before it completes the object, so an
// object that does not match the evidence is aborted, never created. The readback streams GetObject through a sha256 and a byte counter, so no
// large object is held in memory or written to disk, and `wrangler r2 object get` (which aborts mid-stream on large objects, Rule 6) is not used.
import { createHash, createHmac } from "node:crypto";
import { closeSync, fstatSync, openSync, readSync } from "node:fs";
import { PublicationError } from "./publication-artifacts.mjs";

export const R2_ACCOUNT_ID = "03ae77ccd7a07bcbb2dcfde47fa7ba3a";
/** At or above this size the object goes through the S3 route; below it wrangler's single request (cap 300 MiB) is used. */
export const R2_MULTIPART_THRESHOLD_BYTES = 200 * 1024 * 1024;
export const R2_PART_BYTES = 64 * 1024 * 1024;
const MAX_PARTS = 10000;
const REGION = "auto";
const SERVICE = "s3";
const TOKEN_VERIFY_URLS = (account) => [
  "https://api.cloudflare.com/client/v4/user/tokens/verify",
  `https://api.cloudflare.com/client/v4/accounts/${account}/tokens/verify`,
];

const sha256Hex = (data) => createHash("sha256").update(data).digest("hex");
const hmac = (key, data) => createHmac("sha256", key).update(data).digest();
const rfc3986 = (value) => encodeURIComponent(value).replace(/[!'()*]/g, (char) => `%${char.charCodeAt(0).toString(16).toUpperCase()}`);

/** { accessKeyId, secretAccessKey } for an R2-capable API token, derived in memory. Throws PUBLICATION_UPLOAD_FAILED without detail that could carry the token. */
export async function deriveR2S3Credentials(token, { account = R2_ACCOUNT_ID, fetchImpl = fetch } = {}) {
  if (typeof token !== "string" || !token.trim()) throw new PublicationError("PUBLICATION_UPLOAD_FAILED", "CLOUDFLARE_API_TOKEN is required for the S3-compatible R2 route");
  let lastStatus = "no response";
  for (const url of TOKEN_VERIFY_URLS(account)) {
    let response;
    try { response = await fetchImpl(url, { headers: { authorization: `Bearer ${token}` } }); } catch (error) { lastStatus = `network error ${error.name}`; continue; }
    lastStatus = `HTTP ${response.status}`;
    if (!response.ok) continue;
    let body;
    try { body = await response.json(); } catch { continue; }
    const id = body?.result?.id;
    if (body?.success === true && typeof id === "string" && id && body?.result?.status === "active") {
      return { accessKeyId: id, secretAccessKey: sha256Hex(token) };
    }
  }
  throw new PublicationError("PUBLICATION_UPLOAD_FAILED", `the Cloudflare API token could not be verified for R2 S3 credentials (${lastStatus})`);
}

/** The Authorization and x-amz-* headers of one SigV4 request. `query` is an array of [name, value]; `payloadHash` is the hex sha256 of the body. */
export function signRequest({ method, host, pathname, query = [], payloadHash, credentials, now = new Date(), headers = {} }) {
  const amzDate = now.toISOString().replace(/[:-]|\.\d{3}/g, "");
  const date = amzDate.slice(0, 8);
  const signedHeaders = { ...Object.fromEntries(Object.entries(headers).map(([name, value]) => [name.toLowerCase(), String(value).trim()])), host, "x-amz-content-sha256": payloadHash, "x-amz-date": amzDate };
  const names = Object.keys(signedHeaders).sort();
  const canonicalQuery = [...query].map(([name, value]) => [rfc3986(name), rfc3986(value)]).sort((a, b) => (a[0] < b[0] ? -1 : a[0] > b[0] ? 1 : a[1] < b[1] ? -1 : a[1] > b[1] ? 1 : 0)).map(([name, value]) => `${name}=${value}`).join("&");
  const canonicalRequest = `${method}\n${pathname}\n${canonicalQuery}\n${names.map((name) => `${name}:${signedHeaders[name]}\n`).join("")}\n${names.join(";")}\n${payloadHash}`;
  const scope = `${date}/${REGION}/${SERVICE}/aws4_request`;
  const stringToSign = ["AWS4-HMAC-SHA256", amzDate, scope, sha256Hex(canonicalRequest)].join("\n");
  const signingKey = hmac(hmac(hmac(hmac(`AWS4${credentials.secretAccessKey}`, date), REGION), SERVICE), "aws4_request");
  const signature = createHmac("sha256", signingKey).update(stringToSign).digest("hex");
  return {
    ...Object.fromEntries(Object.entries(signedHeaders).filter(([name]) => name !== "host")),
    authorization: `AWS4-HMAC-SHA256 Credential=${credentials.accessKeyId}/${scope}, SignedHeaders=${names.join(";")}, Signature=${signature}`,
    canonicalQuery,
  };
}

/**
 * A time-limited GET URL for one private object (SigV4 query authentication, `UNSIGNED-PAYLOAD`, host header only), rightkit#1230. The URL is the
 * product of this call and the only place the signature appears; the credentials themselves are never returned. `expiresSeconds` is 60..86400.
 */
export function presignGetUrl({ bucket, key, credentials, account = R2_ACCOUNT_ID, expiresSeconds = 3600, now = new Date() }) {
  if (!Number.isInteger(expiresSeconds) || expiresSeconds < 60 || expiresSeconds > 86400) throw new PublicationError("PUBLICATION_ROUTE_INVALID", "a presigned URL lives 60 to 86400 seconds");
  if (!/^[A-Za-z0-9][A-Za-z0-9._/-]*$/.test(key ?? "") || key.includes("..") || !/^[a-z0-9][a-z0-9-]*$/.test(bucket ?? "")) throw new PublicationError("PUBLICATION_ROUTE_INVALID", "invalid R2 object route");
  const host = `${account}.r2.cloudflarestorage.com`;
  const amzDate = now.toISOString().replace(/[:-]|\.\d{3}/g, "");
  const date = amzDate.slice(0, 8);
  const scope = `${date}/${REGION}/${SERVICE}/aws4_request`;
  const query = [["X-Amz-Algorithm", "AWS4-HMAC-SHA256"], ["X-Amz-Credential", `${credentials.accessKeyId}/${scope}`], ["X-Amz-Date", amzDate], ["X-Amz-Expires", String(expiresSeconds)], ["X-Amz-SignedHeaders", "host"]];
  const canonicalQuery = query.map(([name, value]) => [rfc3986(name), rfc3986(value)]).sort((a, b) => (a[0] < b[0] ? -1 : a[0] > b[0] ? 1 : 0)).map(([name, value]) => `${name}=${value}`).join("&");
  const pathname = `/${bucket}/${key.split("/").map(rfc3986).join("/")}`;
  const canonicalRequest = `GET\n${pathname}\n${canonicalQuery}\nhost:${host}\n\nhost\nUNSIGNED-PAYLOAD`;
  const stringToSign = ["AWS4-HMAC-SHA256", amzDate, scope, sha256Hex(canonicalRequest)].join("\n");
  const signingKey = hmac(hmac(hmac(hmac(`AWS4${credentials.secretAccessKey}`, date), REGION), SERVICE), "aws4_request");
  const signature = createHmac("sha256", signingKey).update(stringToSign).digest("hex");
  return { url: `https://${host}${pathname}?${canonicalQuery}&X-Amz-Signature=${signature}`, expiresAt: new Date(now.getTime() + expiresSeconds * 1000).toISOString() };
}

const xmlText = (xml, tag) => new RegExp(`<${tag}>([^<]*)</${tag}>`).exec(xml)?.[1];
const escapeXml = (value) => String(value).replaceAll("&", "&amp;").replaceAll("<", "&lt;").replaceAll(">", "&gt;");

function s3Client({ bucket, credentials, account = R2_ACCOUNT_ID, fetchImpl = fetch, retries = 3, sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms)) }) {
  const host = `${account}.r2.cloudflarestorage.com`;
  const objectPath = (key) => `/${bucket}/${key.split("/").map(rfc3986).join("/")}`;
  async function send(method, key, { query = [], body, payloadHash, headers = {}, stream = false }) {
    let failure = "no response";
    for (let attempt = 1; attempt <= retries; attempt += 1) {
      const pathname = objectPath(key);
      const signed = signRequest({ method, host, pathname, query, payloadHash, credentials, headers });
      const { canonicalQuery, ...requestHeaders } = signed;
      let response;
      try {
        response = await fetchImpl(`https://${host}${pathname}${canonicalQuery ? `?${canonicalQuery}` : ""}`, { method, headers: requestHeaders, body });
      } catch (error) { failure = `network error ${error.name}`; }
      if (response) {
        if (response.ok) return response;
        const text = await response.text().catch(() => "");
        failure = `HTTP ${response.status}${xmlText(text, "Code") ? ` ${xmlText(text, "Code")}` : ""}`;
        // A client error other than throttling is final; a server error or throttle is retried.
        if (response.status < 500 && response.status !== 429 && response.status !== 408) break;
      }
      if (attempt < retries) await sleep(500 * 2 ** (attempt - 1));
    }
    throw new Error(`${method} ${stream ? "object" : "request"} failed (${failure})`);
  }
  return { send };
}

/** Read `length` bytes at `position` of an open file descriptor into a fresh Buffer. */
function readPart(fd, position, length) {
  const buffer = Buffer.allocUnsafe(length);
  let filled = 0;
  while (filled < length) {
    const count = readSync(fd, buffer, filled, length - filled, position + filled);
    if (count === 0) throw new Error("file ended before the expected size");
    filled += count;
  }
  return buffer;
}

/**
 * Upload `file` to `bucket/key` as one multipart object. The whole-file sha256 and size of what was sent must equal `evidence` before the object is
 * completed; any failure aborts the multipart upload. Returns { parts, sizeBytes }.
 */
export async function uploadMultipart({ file, evidence, bucket, key, credentials, partBytes = R2_PART_BYTES, account, fetchImpl, sleep }) {
  const client = s3Client({ bucket, credentials, account, fetchImpl, sleep });
  const fd = openSync(file, "r");
  let uploadId;
  try {
    const size = fstatSync(fd).size;
    if (evidence.sizeBytes !== undefined && size !== evidence.sizeBytes) throw new PublicationError("PUBLICATION_SIZE_MISMATCH", `artifact size mismatch: ${file}`);
    const chunk = Math.max(partBytes, Math.ceil(size / MAX_PARTS));
    const created = await client.send("POST", key, { query: [["uploads", ""]], payloadHash: sha256Hex("") });
    uploadId = xmlText(await created.text(), "UploadId");
    if (!uploadId) throw new Error("CreateMultipartUpload returned no UploadId");
    const whole = createHash("sha256");
    const etags = [];
    for (let offset = 0, number = 1; offset < size; offset += chunk, number += 1) {
      const body = readPart(fd, offset, Math.min(chunk, size - offset));
      whole.update(body);
      const response = await client.send("PUT", key, { query: [["partNumber", String(number)], ["uploadId", uploadId]], body, payloadHash: sha256Hex(body) });
      const etag = response.headers.get("etag");
      if (!etag) throw new Error(`part ${number} returned no ETag`);
      etags.push({ number, etag });
    }
    if (whole.digest("hex") !== evidence.sha256) throw new PublicationError("PUBLICATION_HASH_MISMATCH", `artifact sha256 mismatch: ${file}`);
    const xml = `<CompleteMultipartUpload>${etags.map(({ number, etag }) => `<Part><PartNumber>${number}</PartNumber><ETag>${escapeXml(etag)}</ETag></Part>`).join("")}</CompleteMultipartUpload>`;
    const completeBody = Buffer.from(xml);
    const completed = await client.send("POST", key, { query: [["uploadId", uploadId]], body: completeBody, payloadHash: sha256Hex(completeBody), headers: { "content-type": "application/xml" } });
    // CompleteMultipartUpload can answer 200 with an error document.
    const text = await completed.text();
    if (/<Error>/.test(text)) throw new Error(`CompleteMultipartUpload failed (${xmlText(text, "Code") ?? "error"})`);
    uploadId = undefined;
    return { parts: etags.length, sizeBytes: size };
  } catch (error) {
    if (uploadId) await client.send("DELETE", key, { query: [["uploadId", uploadId]], payloadHash: sha256Hex("") }).catch(() => {});
    throw error;
  } finally { closeSync(fd); }
}

/** One signed GetObject response (optionally `range: "bytes=N-"`): the body is streamed by the caller. Throws with the HTTP status only. */
export async function getObject({ bucket, key, credentials, account, fetchImpl, sleep, range }) {
  const client = s3Client({ bucket, credentials, account, fetchImpl, sleep });
  return client.send("GET", key, { payloadHash: sha256Hex(""), stream: true, ...(range ? { headers: { range } } : {}) });
}

/** Stream `bucket/key` through a sha256 and a byte counter. Returns { sha256, sizeBytes }; nothing is held in memory or written to disk. */
export async function streamObjectDigest({ bucket, key, credentials, account, fetchImpl, sleep }) {
  const client = s3Client({ bucket, credentials, account, fetchImpl, sleep });
  const response = await client.send("GET", key, { payloadHash: sha256Hex(""), stream: true });
  if (!response.body) throw new Error("GetObject returned no body");
  const hash = createHash("sha256");
  let sizeBytes = 0;
  for await (const piece of response.body) { hash.update(piece); sizeBytes += piece.length; }
  const advertised = Number(response.headers.get("content-length"));
  if (Number.isFinite(advertised) && response.headers.has("content-length") && advertised !== sizeBytes) throw new Error(`GetObject stream ended at ${sizeBytes} of ${advertised} bytes`);
  return { sha256: hash.digest("hex"), sizeBytes };
}
