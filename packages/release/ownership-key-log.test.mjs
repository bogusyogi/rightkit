import test from "node:test";
import assert from "node:assert/strict";
import { constants, generateKeyPairSync, sign as cryptoSign } from "node:crypto";
import { createKeyEvent, eventHash, keyFingerprint, verifyKeyLog, verifyKeyRequest, verifyOwnershipSignature } from "./ownership-key-log.mjs";

const pair = (platform, deviceName) => {
  const keys = generateKeyPairSync("ec", { namedCurve: "prime256v1" });
  const publicKeyPem = keys.publicKey.export({ type: "spki", format: "pem" });
  return { privateKey: keys.privateKey, record: { keyId: keyFingerprint(publicKeyPem), publicKeyPem, platform, deviceName } };
};
const rsaPair = (platform, deviceName, options = {}) => {
  const keys = generateKeyPairSync("rsa", { modulusLength: 2048, publicExponent: 65537, ...options });
  const publicKeyPem = keys.publicKey.export({ type: "spki", format: "pem" });
  return { privateKey: keys.privateKey, record: { keyId: keyFingerprint(publicKeyPem), publicKeyPem, platform, deviceName } };
};
const signed = (draft, signer, rsaMode = "pss") => ({ ...draft, signature: {
  keyId: signer.record.keyId,
  host: signer.record.platform,
  value: cryptoSign("sha256", Buffer.from(canonical(draft.payload)), signer.privateKey.asymmetricKeyType === "rsa"
    ? rsaMode === "pss" ? { key: signer.privateKey, padding: constants.RSA_PKCS1_PSS_PADDING, saltLength: 32 } : signer.privateKey
    : signer.privateKey).toString("base64"),
} });
const canonical = (value) => {
  if (value === null || typeof value !== "object") return JSON.stringify(value);
  if (Array.isArray(value)) return `[${value.map(canonical).join(",")}]`;
  return `{${Object.keys(value).sort().map((key) => `${JSON.stringify(key)}:${canonical(value[key])}`).join(",")}}`;
};
const genesis = (root) => {
  const draft = createKeyEvent({ schema: 1, events: [] }, { operation: "root", subject: root.record, signerKeyId: root.record.keyId });
  const event = root.privateKey.asymmetricKeyType === "rsa" ? draft : signed(draft, root);
  return { schema: 1, events: [event] };
};
const append = (log, operation, subject, signer, rsaMode) => {
  const draft = createKeyEvent(log, { operation, subject, signerKeyId: signer.record.keyId });
  return { schema: 1, events: [...log.events, signed(draft, signer, rsaMode)] };
};
const revokeSubject = (key) => ({ keyId: key.record.keyId });
const verify = (log, root) => verifyKeyLog(log, { rootFingerprint: root.record.keyId });

test("verifies signed multidevice additions, revocation, and descendants from a revoked issuer", () => {
  const root = pair("windows", "root workstation");
  const issuer = pair("macos", "mac issuer");
  const device = pair("windows", "windows device");
  const log0 = genesis(root);
  const log1 = append(log0, "add", issuer.record, root);
  const log2 = append(log1, "add", device.record, issuer);
  const log3 = append(log2, "revoke", revokeSubject(issuer), root);
  const result = verify(log3, root);
  assert.equal(result.ok, true, result.problems.join("; "));
  assert.deepEqual(result.keys.map((key) => key.keyId), [root.record.keyId, device.record.keyId]);
  assert.equal(result.allKeys.length, 3);
  assert.deepEqual(result.revokedKeyIds, [issuer.record.keyId]);
  assert.equal(result.checkpoint.head, eventHash(log3.events.at(-1)));
});

test("rejects unsigned additions, tampered payloads, invalid domain, and forged roots", () => {
  const root = pair("windows", "root");
  const other = pair("macos", "other");
  const log = genesis(root);
  const unsignedDraft = createKeyEvent(log, { operation: "add", subject: other.record, signerKeyId: root.record.keyId });
  assert.equal(verify({ schema: 1, events: [...log.events, unsignedDraft] }, root).ok, false);
  const added = append(log, "add", other.record, root);
  const tampered = structuredClone(added);
  tampered.events[1].payload.operation = "revoke";
  assert.equal(verify(tampered, root).ok, false);
  const badDomain = structuredClone(added);
  badDomain.events[1].payload.domain = "other";
  assert.equal(verify(badDomain, root).ok, false);
  const forged = structuredClone(log);
  forged.events[0].payload.rootFingerprint = "f".repeat(64);
  assert.equal(verify(forged, root).ok, false);
  assert.equal(verify(log, other).ok, false);
});

test("rejects revoked signers, re-addition, invalid revocation, and root replacement", () => {
  const root = pair("windows", "root");
  const spare = pair("macos", "spare");
  let log = append(genesis(root), "add", spare.record, root);
  log = append(log, "revoke", revokeSubject(spare), root);
  assert.throws(() => append(log, "add", pair("windows", "new").record, spare), /active/);
  assert.throws(() => append(log, "add", spare.record, root), /already appeared/);
  assert.throws(() => append(log, "revoke", { keyId: "e".repeat(64) }, root), /not active/);
  const replacement = pair("macos", "replacement");
  const draft = createKeyEvent(log, { operation: "add", subject: replacement.record, signerKeyId: root.record.keyId });
  const fork = structuredClone(log);
  fork.events[1] = signed(createKeyEvent(genesis(root), { operation: "add", subject: replacement.record, signerKeyId: root.record.keyId }), root);
  assert.equal(verify(fork, root).ok, false);
  assert.equal(draft.payload.sequence, 3);
});

test("checkpoint rejects rollback and forks, accepts valid fast-forward", () => {
  const root = pair("windows", "root");
  const a = pair("macos", "a");
  const b = pair("windows", "b");
  const first = append(genesis(root), "add", a.record, root);
  const checkpoint = { sequence: 1, head: eventHash(first.events[1]) };
  const later = append(first, "add", b.record, a);
  assert.equal(verifyKeyLog(first, { rootFingerprint: root.record.keyId, checkpoint }).ok, true);
  assert.equal(verifyKeyLog(later, { rootFingerprint: root.record.keyId, checkpoint }).ok, true);
  assert.equal(verifyKeyLog(genesis(root), { rootFingerprint: root.record.keyId, checkpoint }).ok, false);
  const forked = append(genesis(root), "add", b.record, root);
  assert.equal(verifyKeyLog(forked, { rootFingerprint: root.record.keyId, checkpoint }).ok, false);
  assert.equal(verifyKeyLog(first, { rootFingerprint: root.record.keyId, checkpoint: { sequence: 1, head: "0".repeat(64) } }).ok, false);
});

test("root may be revoked while root fingerprint remains historical anchor", () => {
  const root = pair("windows", "root");
  const delegate = pair("macos", "delegate");
  const log = append(append(genesis(root), "add", delegate.record, root), "revoke", revokeSubject(root), delegate);
  const result = verify(log, root);
  assert.equal(result.ok, true, result.problems.join("; "));
  assert.equal(result.rootPublicKeyPem, root.record.publicKeyPem);
  assert.deepEqual(result.keys.map((key) => key.keyId), [delegate.record.keyId]);
  assert.deepEqual(result.revokedKeyIds, [root.record.keyId]);
});

test("validates key enrollment requests against the pinned root", () => {
  const root = pair("windows", "root");
  const candidate = pair("macos", "candidate");
  const request = { schema: 1, type: "rightkit.ownership-key-request.v1", rootFingerprint: root.record.keyId, key: candidate.record };
  assert.equal(verifyKeyRequest(request, { rootFingerprint: root.record.keyId }).ok, true);
  assert.equal(verifyKeyRequest({ ...request, rootFingerprint: "0".repeat(64) }, { rootFingerprint: root.record.keyId }).ok, false);
  assert.equal(verifyKeyRequest({ ...request, key: { ...candidate.record, keyId: "0".repeat(64) } }, { rootFingerprint: root.record.keyId }).ok, false);
});

test("fails closed on empty or unpinned logs and keeps genesis drafting internal", () => {
  const root = pair("windows", "root");
  const empty = { schema: 1, events: [] };
  assert.equal(verifyKeyLog(empty, { rootFingerprint: root.record.keyId }).ok, false);
  assert.equal(verifyKeyLog(genesis(root)).ok, false);
  assert.equal(genesis(root).events.length, 1);
  assert.equal(verify(genesis(root), root).ok, true);
  assert.equal(verifyKeyLog(genesis(root), { rootFingerprint: "bad-pin" }).ok, false);
});

test("rejects private PEMs, spoofable device names, oversized logs/signatures, and inexact checkpoints", () => {
  const root = pair("windows", "root");
  const privatePemRecord = { ...root.record, publicKeyPem: root.privateKey.export({ type: "pkcs8", format: "pem" }) };
  assert.equal(verifyKeyRequest({ schema: 1, type: "rightkit.ownership-key-request.v1", rootFingerprint: root.record.keyId, key: privatePemRecord }, { rootFingerprint: root.record.keyId }).ok, false);
  const controlName = pair("macos", "spoof\nnew device");
  assert.throws(() => createKeyEvent(genesis(root), { operation: "add", subject: controlName.record, signerKeyId: root.record.keyId }), /device name/);
  const log = genesis(root);
  assert.equal(verifyKeyLog({ schema: 1, events: Array(10_001) }, { rootFingerprint: root.record.keyId }).ok, false);
  const hugeSignature = structuredClone(log);
  hugeSignature.events[0].signature.value = "A".repeat(129);
  assert.equal(verify(hugeSignature, root).ok, false);
  assert.equal(verifyKeyLog(log, { rootFingerprint: root.record.keyId, checkpoint: { sequence: 0, head: eventHash(log.events[0]), extra: true } }).ok, false);
});

test("accepts pinned Windows RSA genesis checkpoint and RSA-signed mutations", () => {
  const root = rsaPair("windows", "Windows Hello root");
  const mac = pair("macos", "Mac device");
  let log = genesis(root);
  const genesisCheckpoint = { sequence: 0, head: eventHash(log.events[0]) };
  assert.equal(verifyKeyLog(log, { rootFingerprint: root.record.keyId, checkpoint: genesisCheckpoint }).ok, true);
  assert.equal(verifyKeyLog(log, { rootFingerprint: "f".repeat(64), checkpoint: genesisCheckpoint }).ok, false);
  log = append(log, "add", mac.record, root);
  assert.equal(verifyKeyLog(log, { rootFingerprint: root.record.keyId, checkpoint: genesisCheckpoint }).ok, true);
  assert.equal(Buffer.from(log.events[1].signature.value, "base64").length, 256);
  log = append(log, "revoke", revokeSubject(mac), root);
  assert.equal(verifyKeyLog(log, { rootFingerprint: root.record.keyId, checkpoint: genesisCheckpoint }).ok, true);
  const unsignedAddition = structuredClone(log);
  unsignedAddition.events[1].signature = null;
  assert.equal(verifyKeyLog(unsignedAddition, { rootFingerprint: root.record.keyId, checkpoint: genesisCheckpoint }).ok, false);
  const badRootPin = structuredClone(genesis(root));
  assert.equal(verifyKeyLog(badRootPin, { rootFingerprint: "0".repeat(64), checkpoint: genesisCheckpoint }).ok, false);
  assert.equal(verifyKeyLog(genesis(root), { rootFingerprint: root.record.keyId }).ok, true);
  assert.equal(verifyKeyLog(genesis(root), { rootFingerprint: root.record.keyId, checkpoint: { sequence: 0, head: "f".repeat(64) } }).ok, false);
  const macRoot = pair("macos", "Mac root");
  const macUnsignedGenesis = structuredClone(genesis(macRoot));
  macUnsignedGenesis.events[0].signature = null;
  assert.equal(verifyKeyLog(macUnsignedGenesis, { rootFingerprint: macRoot.record.keyId }).ok, false);
});

test("accepts only P-256 or Windows RSA-2048 keys and rejects malformed RSA signature sizes", () => {
  const root = rsaPair("windows", "root");
  const wrongPlatform = rsaPair("macos", "wrong platform");
  const request = (key) => ({ schema: 1, type: "rightkit.ownership-key-request.v1", rootFingerprint: root.record.keyId, key });
  assert.equal(verifyKeyRequest(request(wrongPlatform.record), { rootFingerprint: root.record.keyId }).ok, false);
  const tooLargePem = generateKeyPairSync("rsa", { modulusLength: 3072 }).publicKey.export({ type: "spki", format: "pem" });
  assert.throws(() => keyFingerprint(tooLargePem), /RSA-2048/);
  const oddExponentPem = generateKeyPairSync("rsa", { modulusLength: 2048, publicExponent: 3 }).publicKey.export({ type: "spki", format: "pem" });
  assert.throws(() => keyFingerprint(oddExponentPem), /RSA-2048/);
  const ec384 = generateKeyPairSync("ec", { namedCurve: "secp384r1" }).publicKey.export({ type: "spki", format: "pem" });
  assert.throws(() => keyFingerprint(ec384), /P-256/);
  const ed25519 = generateKeyPairSync("ed25519").publicKey.export({ type: "spki", format: "pem" });
  assert.throws(() => keyFingerprint(ed25519), /P-256/);
  const malformed = append(genesis(root), "add", pair("macos", "device").record, root);
  malformed.events[1].signature.value = malformed.events[1].signature.value.slice(4);
  assert.equal(verifyKeyLog(malformed, { rootFingerprint: root.record.keyId, checkpoint: { sequence: 0, head: eventHash(genesis(root).events[0]) } }).ok, false);
});

test("verifies only supported RSA encodings: PKCS1v1.5 and PSS-SHA256 salt32", () => {
  const root = rsaPair("windows", "Windows Hello");
  const subject = pair("macos", "Mac");
  const genesisLog = genesis(root);
  const pss = append(genesisLog, "add", subject.record, root, "pss");
  const pkcs1 = append(genesisLog, "add", subject.record, root, "pkcs1");
  const checkpoint = { sequence: 0, head: eventHash(genesisLog.events[0]) };
  assert.equal(verifyKeyLog(pss, { rootFingerprint: root.record.keyId, checkpoint }).ok, true);
  assert.equal(verifyKeyLog(pkcs1, { rootFingerprint: root.record.keyId, checkpoint }).ok, true);
  const altered = structuredClone(pss);
  const signatureBytes = Buffer.from(altered.events[1].signature.value, "base64");
  signatureBytes[0] ^= 1;
  altered.events[1].signature.value = signatureBytes.toString("base64");
  assert.equal(verifyKeyLog(altered, { rootFingerprint: root.record.keyId, checkpoint }).ok, false);
  const payload = Buffer.from(canonical(pss.events[1].payload));
  assert.equal(verifyOwnershipSignature(root.record.publicKeyPem, payload, Buffer.from(pss.events[1].signature.value, "base64")), true);
  assert.equal(verifyOwnershipSignature(root.record.publicKeyPem, payload, Buffer.alloc(255)), false);
});

test("validates Windows RSA null genesis against later protected checkpoints", () => {
  const root = rsaPair("windows", "Windows Hello");
  const mac = pair("macos", "Mac");
  const win = pair("windows", "Windows device");
  const extra = pair("macos", "Second Mac");
  const genesisLog = genesis(root);
  let approved = append(genesisLog, "add", mac.record, root);
  approved = append(approved, "add", win.record, mac);
  approved = append(approved, "add", extra.record, root);
  const cp1 = { sequence: 1, head: eventHash(approved.events[1]) };
  const cp2 = { sequence: 2, head: eventHash(approved.events[2]) };
  assert.equal(verifyKeyLog(approved, { rootFingerprint: root.record.keyId, checkpoint: cp2 }).ok, true);
  assert.equal(verifyKeyLog(approved, { rootFingerprint: root.record.keyId, checkpoint: cp1 }).ok, true);
  assert.equal(verifyKeyLog(genesisLog, { rootFingerprint: root.record.keyId, checkpoint: cp1 }).ok, false);
  let fork = append(genesisLog, "add", mac.record, root);
  fork = append(fork, "add", extra.record, mac);
  assert.equal(verifyKeyLog(fork, { rootFingerprint: root.record.keyId, checkpoint: cp2 }).ok, false);
  assert.doesNotThrow(() => assert.equal(verifyKeyLog(approved, { rootFingerprint: root.record.keyId, checkpoint: null }).ok, false));
});
