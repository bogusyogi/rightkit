// GB-21/23/57 additive branch (rustright#228, rightkit#1511): a broker-signed revocation of ONE exact sub-handle whose lease the broker can no longer
// answer (UNKNOWN, no member row). It settles that sub-handle for a dead owner's reclaim only. It never stands in for REAPED or UNSENT, never
// overrides an ACTIVE lease or a listed member, and never releases a live owner: every other reclaim proof (owner pid, sibling sub-handles,
// detached groups, token holders, snapshot users) still runs unchanged.
//
// Trust: the host's ENROLLED broker pin only (broker-pins.mjs readEnrolledPins), never the revocation, the record, an environment variable or a
// checkout. The signature scheme is the broker's provenance scheme (ES256, P-256, canonical JSON) under its own domain string, so a receipt can
// never be replayed as a revocation, or the reverse.
//
// Location: exactly `<data root>/snapshot-leases/completed/<name>` (no suffix), where name is the lowercase SHA-256 of the canonical JSON array
// [snapshot, ownership]: the broker's own f227 completion key for that sub-handle, so the revocation also fences legacy exact-handle reuse (Terra,
// 2026-11-05). One file, read, authenticated and reread there; no sidecar. A file there with another schema (a legacy completion) is not a revocation
// and is ignored here; a link or a non-file refuses.
import { createHash, createPublicKey, verify } from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import { canonicalJson } from "./provenance-verify.mjs";

export const REVOCATION_SCHEMA = "rustright.snapshot-revocation.v1";
export const REVOCATION_DOMAIN = "rustright-snapshot-revocation-v1";
export const REVOCATION_DIR = path.join("snapshot-leases", "completed");
/** The signed payload's fields, exactly (Terra's proposal, 2026-11-05; not frozen until the shared golden fixture agrees). */
export const REVOCATION_FIELDS = Object.freeze(["dataRoot", "host", "keyId", "ownerFile", "ownerSha256", "ownership", "proofSha256", "runtime", "schema", "snapshot"]);
const SHA = /^[0-9a-f]{64}$/;

export class RevocationError extends Error {
  constructor(code, message) { super(message); this.name = "RevocationError"; this.code = code; }
}
const refuse = (code, message) => { throw new RevocationError(code, message); };
const sha256Hex = (data) => createHash("sha256").update(data).digest("hex");
const isRecord = (v) => v !== null && typeof v === "object" && !Array.isArray(v);
const ID = /^[A-Za-z0-9._-]+$/;

/** The exact bytes the broker signs for a revocation payload. */
export const revocationSignedBytes = (payload) => Buffer.from(`${REVOCATION_DOMAIN}\n${canonicalJson(payload)}`, "utf8");

/** The f227 completion name of one sub-handle of one snapshot: lowercase SHA-256 of canonical JSON `[snapshot, ownership]`. */
export const revocationName = (snapshot, ownership) => sha256Hex(Buffer.from(canonicalJson([snapshot, ownership]), "utf8"));

/** The one path a revocation of `sub` (`<ownerHandle>#<id>`) of `snapshot` may live at under the managed data root `dataRoot`. */
export function revocationFile(dataRoot, ownerHandle, sub, snapshot) {
  if (typeof ownerHandle !== "string" || !ID.test(ownerHandle)) refuse("handle", `${JSON.stringify(ownerHandle)} is not an owner handle`);
  if (typeof sub !== "string" || !sub.startsWith(`${ownerHandle}#`) || !ID.test(sub.slice(ownerHandle.length + 1))) refuse("handle", `${JSON.stringify(sub)} is not a sub-handle of ${ownerHandle}`);
  if (typeof snapshot !== "string" || !path.isAbsolute(snapshot)) refuse("root", "the owner record names no absolute snapshot");
  if (typeof dataRoot !== "string" || !path.isAbsolute(dataRoot)) refuse("root", "the managed data root is not absolute");
  return path.join(dataRoot, REVOCATION_DIR, revocationName(snapshot, sub));
}

/** The revocation's bytes, or null when none exists. A link, a directory or an unreadable entry is a refusal, never null. */
export function readRevocation(file) {
  let meta;
  try { meta = fs.lstatSync(file); } catch (e) { if (e.code === "ENOENT") return null; return refuse("read", `${file} cannot be read (${e.code ?? e.message})`); }
  if (!meta.isFile()) refuse("read", `${file} is not a plain file`);
  return fs.readFileSync(file);
}

/** Whether `bytes` claim to be a snapshot revocation at all (its envelope schema); anything else at the completion key is the broker's own record. */
export function isRevocationEnvelope(bytes) {
  try { return JSON.parse(Buffer.isBuffer(bytes) ? bytes.toString("utf8") : String(bytes))?.schema === REVOCATION_SCHEMA; } catch { return false; }
}

const publicKeyOf = (hex) => {
  const bytes = Buffer.from(hex, "hex");
  if (bytes.length !== 65 || bytes[0] !== 0x04) refuse("pin", "the pinned public key is not an uncompressed SEC1 P-256 point");
  return createPublicKey({ key: { kty: "EC", crv: "P-256", x: bytes.subarray(1, 33).toString("base64url"), y: bytes.subarray(33, 65).toString("base64url") }, format: "jwk" });
};

/**
 * Verifies `bytes` as the broker's revocation of exactly `sub` for exactly this owner record. Returns the signed payload; throws RevocationError
 * (stable `code`) on any mismatch.
 *   pins:     the host's enrolled `{ host, keyId, publicKey, brokerDigest }`
 *   expected: { ownerFile, dataRoot (both canonical, realpath), ownerBytes (the record bytes the reclaim judged), owner (parsed), sub }
 */
export function verifyRevocation(bytes, pins, { ownerFile, dataRoot, ownerBytes, owner, sub }) {
  let envelope = null;
  try { envelope = JSON.parse(Buffer.isBuffer(bytes) ? bytes.toString("utf8") : String(bytes)); } catch { envelope = null; }
  if (!isRecord(envelope) || envelope.schema !== REVOCATION_SCHEMA) refuse("schema", "not a handle revocation");
  const { payload, signature } = envelope;
  if (!isRecord(payload) || !isRecord(signature)) refuse("malformed", "payload or signature is missing");
  if (signature.alg !== "ES256") refuse("alg", "signature algorithm must be ES256");
  if (!isRecord(pins) || typeof pins.publicKey !== "string" || sha256Hex(Buffer.from(pins.publicKey, "hex")) !== pins.keyId) refuse("pin", "the pinned key id does not match the pinned public key");
  if (signature.keyId !== pins.keyId || payload.keyId !== pins.keyId) refuse("key", "not signed by the pinned broker key");
  if (typeof signature.value !== "string" || !/^[0-9a-fA-F]{128}$/.test(signature.value)) refuse("signature", "signature is not 64 bytes r||s in hex");
  let good = false;
  try { good = verify("sha256", revocationSignedBytes(payload), { key: publicKeyOf(pins.publicKey), dsaEncoding: "ieee-p1363" }, Buffer.from(signature.value, "hex")); } catch (e) { if (e instanceof RevocationError) throw e; }
  if (!good) refuse("signature", "signature does not verify");
  // Everything below is signed: exactly these fields, bound to this host's broker runtime, this exact record's bytes, its snapshot and this exact sub-handle.
  const fields = Object.keys(payload).sort();
  if (JSON.stringify(fields) !== JSON.stringify(REVOCATION_FIELDS)) refuse("malformed", `the payload carries ${fields.join(", ")}, not exactly ${REVOCATION_FIELDS.join(", ")}`);
  if (payload.schema !== REVOCATION_SCHEMA) refuse("schema", "the signed payload is not a snapshot revocation");
  if (payload.host !== pins.host) refuse("host", "signed for another host");
  if (!isRecord(payload.runtime) || JSON.stringify(Object.keys(payload.runtime)) !== '["brokerDigest"]' || payload.runtime.brokerDigest !== pins.brokerDigest) refuse("runtime", "signed by another broker runtime");
  if (payload.dataRoot !== dataRoot) refuse("root", "revokes a sub-handle under another managed data root");
  if (payload.ownerFile !== ownerFile) refuse("root", "revokes a sub-handle of another ownership record");
  if (payload.snapshot !== owner.snapshot) refuse("root", "revokes a sub-handle of another snapshot");
  if (payload.ownership !== sub) refuse("handle", `revokes ${payload.ownership}, not ${sub}`);
  if (payload.ownerSha256 !== sha256Hex(ownerBytes)) refuse("owner", "signed over other owner record bytes (the record changed, or it is another record)");
  if (typeof payload.proofSha256 !== "string" || !SHA.test(payload.proofSha256)) refuse("proof", "proofSha256 is not a lowercase SHA-256");
  return payload;
}
