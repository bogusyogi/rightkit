// The signature half of a RustRight broker job receipt, in pure JS (node:crypto only), exactly as rustright-protocol's attestation.rs
// `verify_receipt` does: canonical JSON, the signed bytes, the pinned ES256 key and the checks that need no caller-side nonce. It is the part of
// CI's publication admission that outlived the carrier: qualification-verify.mjs imports it for a seam receipt (the reset plan section 2, Trust); the carrier-era
// verifier that wrapped it (provenance-verify.mjs) was deleted at plan step 5.
// Never trusts a value from the receipt that the caller can pin instead.
import { createHash, createPublicKey, verify } from "node:crypto";

export const PROVENANCE_SCHEMA = "rustright.provenance.v1";
export const PROVENANCE_DOMAIN = "rustright-provenance-v1";

export class ProvenanceError extends Error {
  constructor(code, message) { super(message); this.name = "ProvenanceError"; this.code = code; }
}
const refuse = (code, message) => { throw new ProvenanceError(code, message); };

const sha256Hex = (data) => createHash("sha256").update(data).digest("hex");
const isRecord = (v) => v !== null && typeof v === "object" && !Array.isArray(v);

function writeString(text, out) {
  let s = '"';
  for (const ch of text) {
    const code = ch.codePointAt(0);
    if (ch === '"') s += '\\"';
    else if (ch === "\\") s += "\\\\";
    else if (code < 0x20) s += `\\u${code.toString(16).padStart(4, "0")}`;
    else s += ch;
  }
  out.push(`${s}"`);
}

function writeCanonical(value, out) {
  if (value === null) out.push("null");
  else if (typeof value === "boolean") out.push(value ? "true" : "false");
  else if (typeof value === "number") {
    // Integers only: serde_json prints floats with its own algorithm, and an integer beyond 2^53 was already corrupted by JSON.parse.
    // Either would make a signature fail for the wrong reason, so refuse loudly instead.
    if (!Number.isSafeInteger(value)) refuse("canonical", "canonical JSON takes only safe integers");
    out.push(String(value));
  } else if (typeof value === "string") writeString(value, out);
  else if (Array.isArray(value)) {
    out.push("[");
    value.forEach((item, index) => { if (index > 0) out.push(","); writeCanonical(item, out); });
    out.push("]");
  } else if (isRecord(value)) {
    // Rust sorts by the UTF-8 bytes of the key; JavaScript's default sort is by UTF-16 code unit and differs above U+FFFF.
    const keys = Object.keys(value).sort((a, b) => Buffer.compare(Buffer.from(a, "utf8"), Buffer.from(b, "utf8")));
    out.push("{");
    keys.forEach((key, index) => { if (index > 0) out.push(","); writeString(key, out); out.push(":"); writeCanonical(value[key], out); });
    out.push("}");
  } else refuse("canonical", `canonical JSON cannot carry a ${typeof value}`);
}

/** Canonical JSON: keys sorted by byte order, no whitespace, strings escape only `"`, `\` and control characters < 0x20 (\u00xx lower-case). */
export function canonicalJson(value) {
  const out = [];
  writeCanonical(value, out);
  return out.join("");
}

/** The exact bytes the broker signs for a payload. */
export function signedBytes(payload) {
  return Buffer.from(`${PROVENANCE_DOMAIN}\n${canonicalJson(payload)}`, "utf8");
}

const HEX = /^(?:[0-9a-fA-F]{2})+$/;
const keyFromSec1 = (publicKeyHex) => {
  if (typeof publicKeyHex !== "string" || !HEX.test(publicKeyHex)) refuse("pin", "pinned public key is not hex");
  const bytes = Buffer.from(publicKeyHex, "hex");
  if (bytes.length !== 65 || bytes[0] !== 0x04) refuse("pin", "pinned public key is not an uncompressed SEC1 P-256 point");
  try {
    return createPublicKey({ key: { kty: "EC", crv: "P-256", x: bytes.subarray(1, 33).toString("base64url"), y: bytes.subarray(33, 65).toString("base64url") }, format: "jwk" });
  } catch { return refuse("pin", "pinned public key is not a P-256 point"); }
};

/**
 * Verifies receipt.provenance's signature against the pins the caller holds (never from the receipt) and the job the caller expects. Every
 * check fails closed (throws ProvenanceError with a stable `code`) and returns the signed payload on success. It checks no nonce and no
 * request digest: a seam receipt has none.
 *   pins:  { publicKey (SEC1 uncompressed hex), keyId, host, brokerDigest }
 *   jobId: the receipt's job id; the signed payload's `jobId` and the receipt's own `id` must both equal it.
 */
export function verifyProvenanceSignature(receipt, pins, jobId) {
  const envelope = receipt?.provenance;
  if (!isRecord(envelope)) refuse("legacy", "the receipt has no provenance envelope");
  if (envelope.schema !== PROVENANCE_SCHEMA) refuse("schema", "unknown provenance schema");
  const payload = envelope.payload;
  if (!isRecord(payload)) refuse("malformed", "payload is missing");
  const signature = envelope.signature;
  if (!isRecord(signature)) refuse("malformed", "signature is missing");
  if (signature.alg !== "ES256") refuse("alg", "signature algorithm must be ES256");
  if (!isRecord(pins) || typeof pins.publicKey !== "string" || !HEX.test(pins.publicKey)) refuse("pin", "pinned public key is not hex");
  if (sha256Hex(Buffer.from(pins.publicKey, "hex")) !== pins.keyId) refuse("pin", "pinned key id does not match the pinned public key");
  if (signature.keyId !== pins.keyId || payload.keyId !== pins.keyId) refuse("key", "the receipt was not signed by the pinned key");
  const key = keyFromSec1(pins.publicKey);
  if (typeof signature.value !== "string" || !/^[0-9a-fA-F]+$/.test(signature.value)) refuse("signature", "signature is not hex");
  if (signature.value.length !== 128) refuse("signature", "signature is not 64 bytes r||s");
  let good = false;
  try { good = verify("sha256", signedBytes(payload), { key, dsaEncoding: "ieee-p1363" }, Buffer.from(signature.value, "hex")); } catch (error) { if (error instanceof ProvenanceError) throw error; }
  if (!good) refuse("signature", "signature does not verify");
  if (payload.host !== pins.host) refuse("host", "signed for another host");
  if (payload.broker?.brokerDigest !== pins.brokerDigest) refuse("runtime", "signed by another broker runtime");
  if (typeof jobId !== "string" || payload.jobId !== jobId || receipt.id !== jobId) refuse("job", "the receipt is for another job");
  return payload;
}
