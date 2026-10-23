import assert from "node:assert/strict";
import test from "node:test";
import { mkdtempSync, writeFileSync, renameSync, rmSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { assertArtifactUnchanged, digestBytes, readVerifiedArtifact, verifyPublicationRelease } from "./publication-artifacts.mjs";
import { uploadVerifiedR2Artifact } from "./r2-verified-upload.mjs";
import { registryEvidenceFixture } from "./test-support/admission.mjs";

const commit = "a".repeat(40);
function fixture(t) {
  const root = mkdtempSync(path.join(os.tmpdir(), "rightkit-publication-"));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const file = path.join(root, "installer.exe");
  writeFileSync(file, "signed installer");
  const receipt = path.join(root, "signing.json");
  const signing = { schema: 1, identity: "Developer ID Application: Fixture", files: [{ after: { sha256: digestBytes(Buffer.from("signed installer")) }, timestampPresent: true, authenticode: "Valid", subject: "CN=Damned Ventures LLC", codesign: "strict-valid", identity: "Developer ID Application: Fixture" }] };
  writeFileSync(receipt, JSON.stringify(signing));
  const manifest = {
    schema: 1, ...registryEvidenceFixture(), commit, app: "fixture", version: "1.0.0", platform: "win",
    ownershipEvidence: { commit }, signing: { receipts: [{ file: receipt, sha256: digestBytes(Buffer.from(JSON.stringify(signing))) }] },
    checkpoints: ["signed", "hardened", "sealed"],
    files: [{ name: "installer.exe", role: "installer", sha256: digestBytes(Buffer.from("signed installer")), sizeBytes: 16 }],
  };
  const save = () => writeFileSync(path.join(root, "release-manifest.json"), JSON.stringify(manifest));
  save();
  return { root, file, receipt, manifest, save, options: { sealedDir: root, repoRoot: root, appRoot: root, commit, app: "fixture", version: "1.0.0", platform: "win" } };
}
const deps = { verifyOwnership: () => ({ ok: true }) };
const code = (expected) => (error) => error.code === expected;

test("publication refuses absent seal, mismatched bytes & size before upload", (t) => {
  const fx = fixture(t);
  rmSync(path.join(fx.root, "release-manifest.json"));
  assert.throws(() => verifyPublicationRelease(fx.options, deps), code("PUBLICATION_SEAL_INVALID"));
  fx.save();
  writeFileSync(fx.file, "tampered installer");
  assert.throws(() => verifyPublicationRelease(fx.options, deps), code("PUBLICATION_HASH_MISMATCH"));
  writeFileSync(fx.file, "signed installer");
  fx.manifest.files[0].sizeBytes = 17; fx.save();
  assert.throws(() => verifyPublicationRelease(fx.options, deps), code("PUBLICATION_SIZE_MISMATCH"));
});

test("publication rejects source, ownership, unsigned & changed receipt evidence", (t) => {
  const fx = fixture(t);
  assert.throws(() => verifyPublicationRelease({ ...fx.options, commit: "b".repeat(40) }, deps), code("PUBLICATION_SOURCE_MISMATCH"));
  assert.throws(() => verifyPublicationRelease(fx.options, { verifyOwnership: () => ({ ok: false, reason: "policy mismatch" }) }), code("PUBLICATION_OWNERSHIP_INVALID"));
  fx.manifest.ownershipEvidence.commit = "b".repeat(40); fx.save();
  assert.throws(() => verifyPublicationRelease(fx.options, deps), code("PUBLICATION_OWNERSHIP_INVALID"));
  fx.manifest.ownershipEvidence.commit = commit;
  fx.manifest.checkpoints = ["sealed"]; fx.save();
  assert.throws(() => verifyPublicationRelease(fx.options, deps), code("PUBLICATION_SIGNATURE_MISSING"));
  fx.manifest.checkpoints = ["signed", "hardened", "sealed"]; fx.save();
  writeFileSync(fx.receipt, "tampered receipt");
  assert.throws(() => verifyPublicationRelease(fx.options, deps), code("PUBLICATION_HASH_MISMATCH"));
});

test("mac publication requires Accepted notarization & archive binding", (t) => {
  const fx = fixture(t);
  fx.manifest.platform = "mac"; fx.save();
  const options = { ...fx.options, platform: "mac" };
  assert.throws(() => verifyPublicationRelease(options, deps), code("PUBLICATION_NOTARIZATION_MISSING"));
  const notarization = { schema: 1, archive: { file: fx.file, sha256: fx.manifest.files[0].sha256, sizeBytes: 16 }, notarization: { status: "Accepted" }, stapling: [{ validated: true }] };
  const write = () => {
    const bytes = Buffer.from(JSON.stringify(notarization));
    fx.manifest.signing.receipts[1] = { file: path.join(fx.root, "notarization.json"), sha256: digestBytes(bytes) }; writeFileSync(fx.manifest.signing.receipts[1].file, bytes); fx.save();
  };
  write();
  assert.ok(verifyPublicationRelease(options, deps).artifacts.size);
  notarization.notarization.status = "Rejected"; write();
  assert.throws(() => verifyPublicationRelease(options, deps), code("PUBLICATION_NOTARIZATION_INVALID"));
});

test("R2 refuses replacement after verification without invoking uploader", (t) => {
  const fx = fixture(t);
  const artifact = verifyPublicationRelease(fx.options, deps).artifacts.get("installer.exe");
  const replacement = path.join(fx.root, "replacement.exe"); writeFileSync(replacement, "signed installer"); renameSync(replacement, fx.file);
  let uploads = 0;
  assert.throws(() => uploadVerifiedR2Artifact(artifact, "fixture/installers/windows/current/installer.exe", "public", { run: () => { uploads++; return { status: 0 }; } }), code("PUBLICATION_SOURCE_CHANGED"));
  assert.equal(uploads, 0);
});

test("R2 uploads verified buffer even if source is swapped at transport boundary", (t) => {
  const fx = fixture(t);
  const artifact = verifyPublicationRelease(fx.options, deps).artifacts.get("installer.exe");
  uploadVerifiedR2Artifact(artifact, "fixture/installers/windows/current/installer.exe", "public", {
    run: (command, args, options) => {
      writeFileSync(fx.file, "swapped source");
      assert.equal(command, "pnpm"); assert.ok(args.includes("--pipe")); assert.ok(!args.includes("--file"));
      assert.equal(options.input.toString(), "signed installer"); assert.equal(digestBytes(options.input), artifact.sha256);
      return { status: 0 };
    },
  });
});

test("verified buffer mutation & missing source are refused", (t) => {
  const fx = fixture(t);
  const artifact = readVerifiedArtifact(fx.file, fx.manifest.files[0]);
  artifact.bytes.fill(0);
  assert.throws(() => assertArtifactUnchanged(artifact), code("PUBLICATION_HASH_MISMATCH"));
  rmSync(fx.file);
  assert.throws(() => assertArtifactUnchanged(artifact), code("PUBLICATION_SOURCE_CHANGED"));
});

test("publication rejects changed config & malformed sealed signing assertions", (t) => {
  const fx = fixture(t), configPath = path.join(fx.root, "config.mjs");
  writeFileSync(configPath, "original config"); fx.manifest.signing.configSha256 = digestBytes(Buffer.from("original config")); fx.save();
  assert.ok(verifyPublicationRelease({ ...fx.options, configPath }, deps).artifacts.size);
  writeFileSync(configPath, "changed config");
  assert.throws(() => verifyPublicationRelease({ ...fx.options, configPath }, deps), code("PUBLICATION_POLICY_MISMATCH"));
  const invalid = Buffer.from(JSON.stringify({ schema: 1, files: [{ after: { sha256: "a".repeat(64) }, authenticode: "Invalid", timestampPresent: true }] }));
  writeFileSync(fx.receipt, invalid); fx.manifest.signing.receipts[0].sha256 = digestBytes(invalid); fx.save();
  assert.throws(() => verifyPublicationRelease(fx.options, deps), code("PUBLICATION_SIGNATURE_INVALID"));
});

test("production-shaped NSIS embedding receipts remain valid", (t) => {
  const fx = fixture(t), receiptFile = path.join(fx.root, "embedding.json"), sha256 = "a".repeat(64);
  const bytes = Buffer.from(JSON.stringify({ schema: 1, rawExe: { signedSha256: sha256, verifiedSha256: sha256 }, embedded: [{ sha256, sizeBytes: 8 }] }));
  writeFileSync(receiptFile, bytes); fx.manifest.signing.receipts.push({ file: receiptFile, sha256: digestBytes(bytes) }); fx.save();
  assert.ok(verifyPublicationRelease(fx.options, deps).artifacts.size);
});

function androidFixture(t, patch = {}) {
  const fx = fixture(t), cert = "c".repeat(64), aabBytes = Buffer.from("signed aab");
  const aabFile = path.join(fx.root, "app-release.aab");
  writeFileSync(aabFile, aabBytes);
  const receipt = { schema: 1, contract: "android-aab-upload-key-v1", file: { name: "app-release.aab", sha256: digestBytes(aabBytes), sizeBytes: aabBytes.length }, signer: { alias: "upload", certSha256: cert, subject: "CN=Upload" }, ...patch.receipt };
  const write = () => {
    const bytes = Buffer.from(JSON.stringify(receipt));
    writeFileSync(path.join(fx.root, "android-aab.json"), bytes);
    fx.manifest.signing.receipts = [{ file: path.join(fx.root, "android-aab.json"), sha256: digestBytes(bytes) }];
    fx.save();
  };
  fx.manifest.platform = "android";
  fx.manifest.signing.uploadCertSha256 = cert;
  fx.manifest.files = [{ role: "aab", name: "app-release.aab", sha256: digestBytes(aabBytes), sizeBytes: aabBytes.length }];
  write();
  return { ...fx, receipt, write, options: { ...fx.options, platform: "android" } };
}
const invalid = /PUBLICATION_SIGNATURE_INVALID/;

test("android publication accepts a receipt bound to the sealed aab and pinned upload certificate", (t) => {
  const fx = androidFixture(t);
  assert.ok(verifyPublicationRelease(fx.options, deps).artifacts.has("app-release.aab"));
});

test("android publication refuses a wrong contract, a foreign platform and a missing receipt", (t) => {
  const fx = androidFixture(t);
  fx.receipt.contract = "android-aab-upload-key-v2"; fx.write();
  assert.throws(() => verifyPublicationRelease(fx.options, deps), (e) => invalid.test(e.message) && /contract/.test(e.message));
  fx.receipt.contract = "android-aab-upload-key-v1"; fx.write();
  const win = fixture(t);
  const bytes = Buffer.from(JSON.stringify(fx.receipt)); writeFileSync(win.receipt, bytes); win.manifest.signing.receipts[0].sha256 = digestBytes(bytes); win.save();
  assert.throws(() => verifyPublicationRelease(win.options, deps), (e) => invalid.test(e.message) && /non-android/.test(e.message));
  const winReceipt = androidFixture(t); // android release carrying only a win-shaped receipt
  const sig = Buffer.from(JSON.stringify({ schema: 1, files: [{ after: { sha256: "a".repeat(64) }, timestampPresent: true, codesign: "strict-valid", identity: undefined }] }));
  writeFileSync(path.join(winReceipt.root, "android-aab.json"), sig); winReceipt.manifest.signing.receipts[0].sha256 = digestBytes(sig); winReceipt.save();
  assert.throws(() => verifyPublicationRelease(winReceipt.options, deps), code("PUBLICATION_SIGNATURE_MISSING"));
});

test("android publication refuses a signer that is not the pinned upload certificate", (t) => {
  const fx = androidFixture(t);
  fx.receipt.signer.certSha256 = "d".repeat(64); fx.write();
  assert.throws(() => verifyPublicationRelease(fx.options, deps), (e) => invalid.test(e.message) && /pinned upload certificate/.test(e.message));
  fx.receipt.signer.certSha256 = "c".repeat(64); fx.write();
  delete fx.manifest.signing.uploadCertSha256; fx.save();
  assert.throws(() => verifyPublicationRelease(fx.options, deps), (e) => invalid.test(e.message) && /no pinned/.test(e.message));
});

test("android publication refuses an AAB digest or size that differs from the sealed entry", (t) => {
  const fx = androidFixture(t);
  fx.receipt.file.sha256 = "e".repeat(64); fx.write();
  assert.throws(() => verifyPublicationRelease(fx.options, deps), (e) => invalid.test(e.message) && /sha256 differs/.test(e.message));
  fx.receipt.file.sha256 = fx.manifest.files[0].sha256; fx.receipt.file.sizeBytes = 99; fx.write();
  assert.throws(() => verifyPublicationRelease(fx.options, deps), (e) => invalid.test(e.message) && /size differs/.test(e.message));
});

test("android publication refuses a sealed release without exactly one aab entry", (t) => {
  const fx = androidFixture(t);
  fx.manifest.files[0].role = "portable"; fx.save();
  assert.throws(() => verifyPublicationRelease(fx.options, deps), (e) => invalid.test(e.message) && /no aab entry/.test(e.message));
});

test("android publication refuses the Android debug certificate", (t) => {
  const fx = androidFixture(t);
  fx.receipt.signer.subject = "CN=Android Debug,O=Android,C=US"; fx.write();
  assert.throws(() => verifyPublicationRelease(fx.options, deps), (e) => invalid.test(e.message) && /debug certificate/.test(e.message));
  fx.receipt.signer.subject = "CN=Upload"; fx.receipt.signer.debugCertificate = true; fx.write();
  assert.throws(() => verifyPublicationRelease(fx.options, deps), (e) => invalid.test(e.message) && /debug certificate/.test(e.message));
});

function iosFixture(t, patch = {}) {
  const fx = fixture(t), ipaBytes = Buffer.from("signed ipa");
  writeFileSync(path.join(fx.root, "Pulse.ipa"), ipaBytes);
  const receipt = { schema: 1, contract: "ios-xcodebuild-archive-automatic-v1", teamId: "6KLGD3LLKF", bundleId: "dev.orthic.pulse.ios", marketingVersion: "0.1.0", buildNumber: "2026101801", file: { name: "Pulse.ipa", sha256: digestBytes(ipaBytes), sizeBytes: ipaBytes.length }, ...patch.receipt };
  const write = () => {
    const bytes = Buffer.from(JSON.stringify(receipt));
    writeFileSync(path.join(fx.root, "ios-archive.json"), bytes);
    fx.manifest.signing.receipts = [{ file: path.join(fx.root, "ios-archive.json"), sha256: digestBytes(bytes) }];
    fx.save();
  };
  fx.manifest.platform = "ios";
  fx.manifest.signing.teamId = "6KLGD3LLKF";
  fx.manifest.signing.bundleId = "dev.orthic.pulse.ios";
  fx.manifest.files = [{ role: "ipa", name: "Pulse.ipa", sha256: digestBytes(ipaBytes), sizeBytes: ipaBytes.length }];
  write();
  return { ...fx, receipt, write, options: { ...fx.options, platform: "ios" } };
}

test("ios publication accepts a receipt bound to the sealed ipa, the pinned team and bundle id", (t) => {
  const fx = iosFixture(t);
  assert.ok(verifyPublicationRelease(fx.options, deps).artifacts.has("Pulse.ipa"));
});

test("ios publication refuses a wrong team, bundle id, digest, size, name and a missing ipa entry", (t) => {
  const fx = iosFixture(t);
  fx.receipt.teamId = "OTHERTEAM1"; fx.write();
  assert.throws(() => verifyPublicationRelease(fx.options, deps), (e) => invalid.test(e.message) && /pinned team/.test(e.message));
  fx.receipt.teamId = "6KLGD3LLKF"; fx.receipt.bundleId = "dev.other.app"; fx.write();
  assert.throws(() => verifyPublicationRelease(fx.options, deps), (e) => invalid.test(e.message) && /pinned bundle id/.test(e.message));
  fx.receipt.bundleId = "dev.orthic.pulse.ios"; fx.receipt.file.sha256 = "e".repeat(64); fx.write();
  assert.throws(() => verifyPublicationRelease(fx.options, deps), (e) => invalid.test(e.message) && /sha256 differs/.test(e.message));
  fx.receipt.file.sha256 = fx.manifest.files[0].sha256; fx.receipt.file.sizeBytes = 99; fx.write();
  assert.throws(() => verifyPublicationRelease(fx.options, deps), (e) => invalid.test(e.message) && /size differs/.test(e.message));
  fx.receipt.file.sizeBytes = fx.manifest.files[0].sizeBytes; fx.receipt.file.name = "Other.ipa"; fx.write();
  assert.throws(() => verifyPublicationRelease(fx.options, deps), (e) => invalid.test(e.message) && /different file/.test(e.message));
  fx.receipt.file.name = "Pulse.ipa"; fx.write();
  fx.manifest.files[0].role = "portable"; fx.save();
  assert.throws(() => verifyPublicationRelease(fx.options, deps), (e) => invalid.test(e.message) && /no ipa entry/.test(e.message));
});

test("ios publication refuses an unpinned team, a foreign platform and a release without the iOS receipt", (t) => {
  const fx = iosFixture(t);
  delete fx.manifest.signing.teamId; fx.save();
  assert.throws(() => verifyPublicationRelease(fx.options, deps), (e) => invalid.test(e.message) && /no pinned iOS team/.test(e.message));
  const android = androidFixture(t);
  assert.throws(() => verifyPublicationRelease({ ...android.options, platform: "ios" }, deps), (e) => /mismatch|non-iOS/.test(e.message));
  const win = fixture(t);
  const bytes = Buffer.from(JSON.stringify(fx.receipt)); writeFileSync(win.receipt, bytes); win.manifest.signing.receipts[0].sha256 = digestBytes(bytes); win.save();
  assert.throws(() => verifyPublicationRelease(win.options, deps), (e) => invalid.test(e.message) && /non-iOS/.test(e.message));
});
