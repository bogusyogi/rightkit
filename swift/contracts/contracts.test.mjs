import assert from "node:assert/strict";
import { createPublicKey, verify } from "node:crypto";
import { readFile } from "node:fs/promises";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";

const here = dirname(fileURLToPath(import.meta.url));
const paths = {
  swift: resolve(here, "license-v2.json"),
  swiftTests: resolve(here, "../RightKitSwift/Tests/RightKitLicenseTests/Contracts/license-v2.json"),
  rust: resolve(here, "../../crates/rightkit-license/test-vectors/license-v2.json"),
  typescript: resolve(here, "../../packages/license/test-vectors/license-v2.json"),
};

function decodeBase64url(value) {
  const standard = value.replaceAll("-", "+").replaceAll("_", "/");
  return Buffer.from(standard.padEnd(Math.ceil(standard.length / 4) * 4, "="), "base64");
}

test("Swift (both copies), Rust, and TypeScript consume the same license-v2 golden vector", async () => {
  const entries = await Promise.all(
    Object.entries(paths).map(async ([name, path]) => [name, await readFile(path)]),
  );
  const vectors = Object.fromEntries(entries);

  assert.deepEqual(vectors.swift, vectors.swiftTests);
  assert.deepEqual(vectors.swift, vectors.rust);
  assert.deepEqual(vectors.swift, vectors.typescript);
});

test("license-v2 vector is a valid Ed25519 entitlement contract", async () => {
  const vector = JSON.parse(await readFile(paths.swift, "utf8"));
  const parts = vector.token.split(".");
  assert.equal(parts.length, 2);

  const payloadBytes = decodeBase64url(parts[0]);
  const signature = decodeBase64url(parts[1]);
  const rawPublicKey = decodeBase64url(vector.publicKeyBase64url);
  assert.equal(rawPublicKey.length, 32);
  assert.equal(signature.length, 64);

  // RFC 8410 SubjectPublicKeyInfo prefix for a raw Ed25519 public key.
  const publicKey = createPublicKey({
    key: Buffer.concat([Buffer.from("302a300506032b6570032100", "hex"), rawPublicKey]),
    format: "der",
    type: "spki",
  });
  assert.equal(verify(null, payloadBytes, publicKey, signature), true);

  const payload = JSON.parse(payloadBytes.toString("utf8"));
  assert.equal(payload.v, 2);
  assert.equal(payload.app, vector.expectedApp);
  assert.equal(payload.device_id, vector.expectedDeviceId);
  assert.equal(payload.tier, vector.expectedTier);
  assert.deepEqual(payload.features, vector.expectedFeatures);
  assert.ok(payload.iat <= vector.nowUnix);
  assert.ok(vector.nowUnix <= payload.exp);
});
