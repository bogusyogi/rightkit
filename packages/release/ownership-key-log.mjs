import { constants, createHash, createPublicKey, verify as cryptoVerify } from "node:crypto";

const HEX = /^[a-f0-9]{64}$/;
const DOMAIN = "rightkit.ownership-key-log.v1";
const REQUEST_TYPE = "rightkit.ownership-key-request.v1";
const PLATFORMS = new Set(["windows", "macos"]);
const OPERATIONS = new Set(["root", "add", "revoke"]);
const exact = (value, keys) => value && typeof value === "object" && !Array.isArray(value)
  && Object.keys(value).sort().join("\0") === [...keys].sort().join("\0");
const canonicalJson = (value) => {
  if (value === null || typeof value !== "object") return JSON.stringify(value);
  if (Array.isArray(value)) return `[${value.map(canonicalJson).join(",")}]`;
  return `{${Object.keys(value).sort().map((key) => `${JSON.stringify(key)}:${canonicalJson(value[key])}`).join(",")}}`;
};
const digest = (value) => createHash("sha256").update(value).digest("hex");
const validText = (s, max) => typeof s === "string" && s.trim().length > 0 && s.length <= max && !/[\u0000-\u001f\u007f]/.test(s);

export function keyFingerprint(pem) {
  const key = createPublicKey(pem);
  const ec = key.asymmetricKeyType === "ec" && key.asymmetricKeyDetails?.namedCurve === "prime256v1";
  const rsa = key.asymmetricKeyType === "rsa" && key.asymmetricKeyDetails?.modulusLength === 2048 && key.asymmetricKeyDetails?.publicExponent === 65537n;
  if (!ec && !rsa) throw new Error("key must be P-256 ECDSA or RSA-2048 with exponent 65537");
  return digest(key.export({ type: "spki", format: "der" }));
}

export function verifyOwnershipSignature(publicKeyPem, payloadBytes, signatureBytes) {
  try {
    const key = createPublicKey(publicKeyPem);
    const message = Buffer.from(payloadBytes);
    const signature = Buffer.from(signatureBytes);
    if (!Buffer.isBuffer(payloadBytes) && !(payloadBytes instanceof Uint8Array)) return false;
    if (!Buffer.isBuffer(signatureBytes) && !(signatureBytes instanceof Uint8Array)) return false;
    if (message.length > 1_000_000 || signature.length > 256) return false;
    keyFingerprint(publicKeyPem);
    if (key.asymmetricKeyType === "rsa") {
      if (signature.length !== 256) return false;
      return cryptoVerify("sha256", message, { key, padding: constants.RSA_PKCS1_PSS_PADDING, saltLength: 32 }, signature)
        || cryptoVerify("sha256", message, { key, padding: constants.RSA_PKCS1_PADDING }, signature);
    }
    return cryptoVerify("sha256", message, key, signature);
  } catch { return false; }
}

export function validateKeyRecord(record) {
  const problems = [];
  if (!exact(record, ["keyId", "publicKeyPem", "platform", "deviceName"])) return ["key record fields are malformed"];
  if (!HEX.test(record.keyId ?? "")) problems.push("key id must be a lowercase SHA-256 fingerprint");
  if (!PLATFORMS.has(record.platform)) problems.push("key platform is invalid");
  if (!validText(record.deviceName, 128)) problems.push("device name must be nonempty and at most 128 characters");
  if (typeof record.publicKeyPem !== "string" || record.publicKeyPem.length > 4096
    || !/^-----BEGIN PUBLIC KEY-----\r?\n[\s\S]+\r?\n-----END PUBLIC KEY-----\r?\n?$/.test(record.publicKeyPem)) problems.push("public key PEM must be a bounded SPKI public key");
  else {
    try {
      const key = createPublicKey(record.publicKeyPem);
      if (keyFingerprint(record.publicKeyPem) !== record.keyId) problems.push("key id does not match public key");
      if (key.asymmetricKeyType === "rsa" && record.platform !== "windows") problems.push("RSA keys are permitted only for Windows devices");
    } catch { problems.push("public key must be P-256 SPKI or Windows RSA-2048 SPKI"); }
  }
  return problems;
}

export function eventHash(event) {
  return digest(Buffer.from(canonicalJson(event?.payload), "utf8"));
}

const payloadKeys = ["domain", "rootFingerprint", "sequence", "previousHash", "operation", "subject", "signerKeyId"];
function validatePayload(payload) {
  const problems = [];
  if (!exact(payload, payloadKeys)) return ["event payload fields are malformed"];
  if (payload.domain !== DOMAIN) problems.push("event domain is invalid");
  if (!HEX.test(payload.rootFingerprint ?? "")) problems.push("root fingerprint is invalid");
  if (!Number.isSafeInteger(payload.sequence) || payload.sequence < 0) problems.push("event sequence is invalid");
  if (payload.sequence === 0 ? payload.previousHash !== null : !HEX.test(payload.previousHash ?? "")) problems.push("event previous hash is invalid");
  if (!OPERATIONS.has(payload.operation)) problems.push("event operation is invalid");
  if (!HEX.test(payload.signerKeyId ?? "")) problems.push("event signer key id is invalid");
  if (payload.operation === "revoke") {
    if (!exact(payload.subject, ["keyId"]) || !HEX.test(payload.subject?.keyId ?? "")) problems.push("revocation subject is malformed");
  } else problems.push(...validateKeyRecord(payload.subject).map((p) => `subject ${p}`));
  return problems;
}

export function verifyKeyLog(log, { rootFingerprint, checkpoint } = {}) {
  const problems = [];
  const fail = (message) => problems.push(message);
  if (!exact(log, ["schema", "events"]) || log.schema !== 1 || !Array.isArray(log.events)) {
    return { ok: false, problems: ["key log schema or events are malformed"], keys: [], allKeys: [], revokedKeyIds: [], checkpoint: null, rootFingerprint: null, rootPublicKeyPem: null };
  }
  if (!HEX.test(rootFingerprint ?? "")) fail("pinned root fingerprint is required");
  if (log.events.length === 0) fail("key log must contain a signed root event");
  if (log.events.length > 10_000) fail("key log exceeds 10000 events");
  if (problems.length) return { ok: false, problems, keys: [], allKeys: [], revokedKeyIds: [], checkpoint: null, rootFingerprint: rootFingerprint ?? null, rootPublicKeyPem: null };
  const keys = new Map();
  const allKeys = new Map();
  const revoked = new Set();
  let rootPem = null;
  let previousHash = null;
  for (let i = 0; i < log.events.length; i++) {
    const event = log.events[i];
    if (!exact(event, ["schema", "payload", "signature"]) || event.schema !== 1
      || (event.signature !== null && !exact(event.signature, ["keyId", "host", "value"]))) { fail(`event ${i} envelope is malformed or unsigned`); break; }
    const payload = event.payload;
    const payloadProblems = validatePayload(payload);
    if (payloadProblems.length) { payloadProblems.forEach((p) => fail(`event ${i}: ${p}`)); break; }
    if (payload.sequence !== i) { fail(`event ${i} sequence is not contiguous`); break; }
    if (payload.previousHash !== previousHash) { fail(`event ${i} previous hash does not match log head`); break; }
    if (rootFingerprint && payload.rootFingerprint !== rootFingerprint) { fail(`event ${i} root fingerprint does not match pin`); break; }
    if (i === 0 && payload.operation !== "root") { fail("first event must establish root"); break; }
    if (i > 0 && payload.operation === "root") { fail(`event ${i} cannot replace root`); break; }
    if (!rootFingerprint && i === 0 && payload.rootFingerprint !== payload.subject.keyId) { fail("root event fingerprint does not match root key"); break; }

    const signer = i === 0 ? payload.subject : keys.get(payload.signerKeyId);
    if (!signer || signer.keyId !== payload.signerKeyId || revoked.has(payload.signerKeyId)) { fail(`event ${i} signer is not active`); break; }
    const signerKey = createPublicKey(signer.publicKeyPem);
    const rsaSigner = signerKey.asymmetricKeyType === "rsa";
    if (event.signature === null) {
      const permittedWindowsAnchor = i === 0 && payload.operation === "root" && signer.platform === "windows" && rsaSigner
        && payload.rootFingerprint === rootFingerprint && payload.rootFingerprint === payload.subject.keyId;
      if (!permittedWindowsAnchor) { fail(`event ${i} unsigned root is not a pinned Windows RSA genesis checkpoint`); break; }
    } else {
      const maxLength = rsaSigner ? 344 : 128;
      const signatureBytes = typeof event.signature.value === "string" ? Buffer.from(event.signature.value, "base64") : Buffer.alloc(0);
      if (event.signature.keyId !== payload.signerKeyId || event.signature.host !== signer.platform || typeof event.signature.value !== "string"
        || event.signature.value.length > maxLength
        || (rsaSigner ? event.signature.value.length !== 344 || signatureBytes.length !== 256 : event.signature.value.length > 128 || signatureBytes.length === 0)
        || !/^(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?$/.test(event.signature.value)
        || signatureBytes.toString("base64") !== event.signature.value) { fail(`event ${i} signature envelope does not match signer`); break; }
      const verified = verifyOwnershipSignature(signer.publicKeyPem, Buffer.from(canonicalJson(payload), "utf8"), signatureBytes);
      if (!verified) { fail(`event ${i} signature is invalid`); break; }
    }

    if (i === 0) {
      if (payload.signerKeyId !== payload.subject.keyId || payload.rootFingerprint !== keyFingerprint(payload.subject.publicKeyPem)) { fail("root event is not self-signed or pin does not match"); break; }
      rootPem = payload.subject.publicKeyPem;
      keys.set(signer.keyId, signer);
      allKeys.set(signer.keyId, signer);
    } else if (payload.operation === "add") {
      if (allKeys.has(payload.subject.keyId)) { fail(`event ${i} adds a duplicate or previously revoked key`); break; }
      keys.set(payload.subject.keyId, payload.subject);
      allKeys.set(payload.subject.keyId, payload.subject);
    } else if (payload.operation === "revoke") {
      if (!keys.has(payload.subject.keyId)) { fail(`event ${i} revokes a key that is not active`); break; }
      keys.delete(payload.subject.keyId);
      revoked.add(payload.subject.keyId);
    }
    previousHash = eventHash(event);
  }
  const head = log.events.length ? previousHash : null;
  const current = { sequence: log.events.length - 1, head };
  if (!problems.length && checkpoint !== undefined) {
    if (!exact(checkpoint, ["sequence", "head"]) || !Number.isSafeInteger(checkpoint.sequence) || checkpoint.sequence < 0 || !HEX.test(checkpoint.head ?? "")) fail("protected checkpoint is malformed");
    else if (current.sequence < checkpoint.sequence) fail("key log is shorter than protected checkpoint");
    else if (log.events[checkpoint.sequence] && eventHash(log.events[checkpoint.sequence]) !== checkpoint.head) fail("key log forks before or at protected checkpoint");
  }
  const pin = rootFingerprint ?? (log.events[0]?.payload?.rootFingerprint ?? null);
  return { ok: problems.length === 0, problems, keys: [...keys.values()], allKeys: [...allKeys.values()], revokedKeyIds: [...revoked], checkpoint: current, rootFingerprint: pin, rootPublicKeyPem: rootPem };
}

export function createKeyEvent(log, { operation, subject, signerKeyId } = {}) {
  if (!OPERATIONS.has(operation) || !HEX.test(signerKeyId ?? "")) throw new Error("operation and signer key id are required");
  const events = log?.events ?? [];
  const pin = events[0]?.payload?.rootFingerprint ?? (operation === "root" ? subject?.keyId : null);
  const emptyLog = !log || (log.schema === 1 && Array.isArray(log.events) && log.events.length === 0);
  const state = emptyLog && operation === "root"
    ? { ok: true, problems: [], keys: [], allKeys: [] }
    : verifyKeyLog(log ?? { schema: 1, events: [] }, { rootFingerprint: pin });
  if (!state.ok) throw new Error(`cannot extend invalid key log: ${state.problems.join("; ")}`);
  const seq = events.length;
  const previousHash = seq ? eventHash(events[seq - 1]) : null;
  const payload = { domain: DOMAIN, rootFingerprint: pin, sequence: seq, previousHash, operation, subject, signerKeyId };
  const errors = validatePayload(payload);
  if (errors.length) throw new Error(errors.join("; "));
  if (operation === "root" && (seq !== 0 || signerKeyId !== subject.keyId || pin !== subject.keyId)) throw new Error("root event must be first and self-signed");
  if (operation !== "root" && !state.keys.some((key) => key.keyId === signerKeyId)) throw new Error("signer is not active");
  if (operation === "add" && state.allKeys.some((key) => key.keyId === subject.keyId)) throw new Error("key already appeared in log");
  if (operation === "revoke" && !state.keys.some((key) => key.keyId === subject.keyId)) throw new Error("revoked key is not active");
  return { schema: 1, payload, signature: null };
}

export function verifyKeyRequest(request, { rootFingerprint } = {}) {
  const problems = [];
  if (!exact(request, ["schema", "type", "rootFingerprint", "key"]) || request.schema !== 1 || request.type !== REQUEST_TYPE) problems.push("key request schema or type is malformed");
  if (request?.rootFingerprint !== rootFingerprint || !HEX.test(rootFingerprint ?? "")) problems.push("key request root fingerprint does not match pin");
  problems.push(...validateKeyRecord(request?.key).map((p) => `request key ${p}`));
  return { ok: problems.length === 0, problems };
}
