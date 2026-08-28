import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import {
  createNativeFinalizationReceipt,
  mintNativeProvenance,
  sha256File,
  validateNativeFinalizationOutput,
} from "./native-release-finalization.mjs";

function fixture() {
  const root = mkdtempSync(path.join(os.tmpdir(), "right-release-native-finalization-"));
  mkdirSync(path.join(root, "dist"), { recursive: true });
  return root;
}

const identity = {
  kind: "portable-zip",
  name: "fixture-1.0.0-windows-x86_64.zip",
  platform: "win",
  architecture: "x86_64",
};

test("provenance identity changes with signed artifact or package identity", () => {
  const base = {
    app: "fixture",
    version: "1.0.0",
    platform: "win",
    architecture: "x86_64",
    signedArtifacts: [{ path: "bin/fixture.exe", sha256: "a".repeat(64), sizeBytes: 10 }],
    packageIdentity: identity,
  };
  const first = mintNativeProvenance(base);
  const changedArtifact = mintNativeProvenance({ ...base, signedArtifacts: [{ ...base.signedArtifacts[0], sha256: "b".repeat(64) }] });
  const changedPackage = mintNativeProvenance({ ...base, packageIdentity: { ...identity, name: "fixture-1.0.0-windows-arm64.zip" } });
  assert.match(first, /^rightkit-release:\/\//);
  assert.notEqual(first, changedArtifact);
  assert.notEqual(first, changedPackage);
});

test("finalization receipt binds actual archive digest and provenance", () => {
  const root = fixture();
  const archive = path.join(root, "dist", "fixture.zip");
  writeFileSync(archive, "final signed package bytes\n");
  const receipt = createNativeFinalizationReceipt({
    app: "fixture",
    version: "1.0.0",
    platform: "win",
    architecture: "x86_64",
    signedArtifacts: [{ path: "bin/fixture.exe", sha256: "a".repeat(64), sizeBytes: 10 }],
    packageIdentity: identity,
    archivePath: archive,
    root,
    finalizerOutput: { status: "complete", provenance: mintNativeProvenance({
      app: "fixture",
      version: "1.0.0",
      platform: "win",
      architecture: "x86_64",
      signedArtifacts: [{ path: "bin/fixture.exe", sha256: "a".repeat(64), sizeBytes: 10 }],
      packageIdentity: identity,
    }) },
  });
  assert.equal(receipt.archiveSha256, sha256File(archive));
  assert.equal(receipt.archive.sha256, receipt.archiveSha256);
  assert.match(receipt.provenance, /^rightkit-release:\/\//);
  assert.equal(validateNativeFinalizationOutput(receipt, { requireArchive: true }).archiveSha256, receipt.archiveSha256);
});

test("placeholder or fabricated finalizer output is rejected", () => {
  assert.throws(
    () => validateNativeFinalizationOutput({ status: "complete", provenance: "rightkit-release://fixture/pending" }),
    /placeholder|identity/i,
  );
  assert.throws(
    () => validateNativeFinalizationOutput({ status: "complete", provenance: "local-build://fixture/${digest}" }),
    /rightkit-release|placeholder/i,
  );
});

test("receipt rejects a finalizer archive digest that differs from archive bytes", () => {
  const root = fixture();
  const archive = path.join(root, "dist", "fixture.zip");
  writeFileSync(archive, "archive bytes\n");
  assert.throws(
    () => createNativeFinalizationReceipt({
      app: "fixture",
      version: "1.0.0",
      platform: "win",
      architecture: "x86_64",
      signedArtifacts: [{ path: "bin/fixture.exe", sha256: "a".repeat(64), sizeBytes: 10 }],
      packageIdentity: identity,
      archivePath: archive,
      root,
      finalizerOutput: {
        status: "complete",
        provenance: "rightkit-release://fixture/1.0.0/win/x86_64/" + "c".repeat(64),
        archiveSha256: "b".repeat(64),
      },
    }),
    /archive digest/i,
  );
});
