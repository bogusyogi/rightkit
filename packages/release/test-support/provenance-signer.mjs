// A test-only stand-in for the RustRight broker's signer: one generated P-256 key that signs provenance envelopes the way
// rustright-protocol's attestation.rs does (ES256, r||s hex, over "rustright-provenance-v1\n" + canonical JSON).
import { createHash, generateKeyPairSync, sign } from "node:crypto";
import { signedBytes } from "../provenance-verify.mjs";

export function makeSigner({ host = "test-host.local", brokerDigest = "9".repeat(64) } = {}) {
  const { publicKey, privateKey } = generateKeyPairSync("ec", { namedCurve: "P-256" });
  const jwk = publicKey.export({ format: "jwk" });
  const publicKeyHex = Buffer.concat([Buffer.from([4]), Buffer.from(jwk.x, "base64url"), Buffer.from(jwk.y, "base64url")]).toString("hex");
  const keyId = createHash("sha256").update(Buffer.from(publicKeyHex, "hex")).digest("hex");
  const pins = { publicKey: publicKeyHex, keyId, host, brokerDigest };
  /** A job receipt `{ id, state, provenance }` signed by this key; `over` overrides payload fields (deep merge is the caller's job). */
  const receipt = ({ jobId, nonce, workspace = "/work/rightkit", request = { kind: "cargo", shim: "cargo", requestedArgv: ["test"], workspace, workspaceRoot: workspace }, result = { status: "SUCCEEDED", exitCode: 0 }, payload: over = {} }) => {
    const payload = { host, broker: { version: "test", brokerDigest, clientDigest: "8".repeat(64) }, keyId, jobId, nonce, requestDigest: "7".repeat(64), request, result, ...over };
    const value = sign("sha256", signedBytes(payload), { key: privateKey, dsaEncoding: "ieee-p1363" }).toString("hex");
    return { id: jobId, state: "SUCCEEDED", provenance: { schema: "rustright.provenance.v1", payload, signature: { alg: "ES256", keyId, value } } };
  };
  return { pins, receipt };
}
