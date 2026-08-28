import assert from "node:assert/strict";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { RIGHTRELEASE_MANIFEST_SIGNER, signReleaseManifestWithAzure, verifyManifestCatalogSignature } from "./sign-release-manifest.mjs";

if (process.platform !== "win32") throw new Error("protected manifest signer integration must run on Windows");

const root = mkdtempSync(join(tmpdir(), "rightrelease-live-signer-"));
const manifestPath = join(root, "release-manifest.json");
const signaturePath = join(root, "release-manifest.cat");
try {
  writeFileSync(manifestPath, '{"kind":"rightrelease-live-signer-probe","schemaVersion":1}\n');
  const signing = signReleaseManifestWithAzure({ manifestPath, signaturePath });
  const verification = verifyManifestCatalogSignature({ manifestPath, signaturePath, expectedSigner: signing.signer });
  assert.equal(verification.verified, true);
  assert.equal(verification.signer.id, RIGHTRELEASE_MANIFEST_SIGNER.id);

  writeFileSync(manifestPath, `${readFileSync(manifestPath, "utf8")}tampered\n`);
  assert.throws(
    () => verifyManifestCatalogSignature({ manifestPath, signaturePath, expectedSigner: signing.signer }),
    /manifest catalog signature is invalid/,
  );
  process.stdout.write(`${JSON.stringify({ status: "PASS", provider: signing.signer.provider, providerVersion: signing.signer.providerVersion, algorithm: signing.signer.algorithm, subject: signing.signer.subject, certificateSha256: signing.signer.certificateSha256 })}\n`);
} finally {
  rmSync(root, { recursive: true, force: true });
}
