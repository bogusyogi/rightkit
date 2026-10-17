import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import * as fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { runInNewContext } from "node:vm";
import { signingReceiptPath } from "./signing-receipts.mjs";
import { notarizeMacosArtifact, validateNotarizationReceipt } from "./macos-notarization.mjs";
import { devtoolsEvidence, resolveDevtoolsAllowance } from "./devtools-policy.mjs";

const source = fs.readFileSync(new URL("./build-release.mjs", import.meta.url), "utf8");
const hashFile = (file) => createHash("sha256").update(fs.readFileSync(file)).digest("hex");
const hashFileText = (text) => createHash("sha256").update(text).digest("hex");

test("MailRight config seals current build receipts despite stale legacy & other-build receipts", (t) => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "right-seal-receipts-"));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const vaultRoot = path.join(root, ".right-release");
  const releaseId = "mailright-0.2.131-12345678";
  const stateRoot = path.join(vaultRoot, "state", releaseId, "mac");
  const configPath = path.join(root, "right-release.config.mjs");
  const dmg = "MailRight_0.2.131_universal.dmg";
  const target = {
    signed: true, signingContract: "macos-developer-id-notarized-portable-v1",
    prePackage: { cmd: "node", args: ["scripts/mac-dmg.mjs", "--prepare"] },
    sign: { prePackageFiles: [dmg] }, notarize: { file: dmg, profile: "apple-dev-notary" },
    package: { cmd: "node", args: ["scripts/mac-dmg.mjs", "--finalize"] },
    installer: { artifacts: [{ file: dmg, key: "mailright/installers/mac/current/MailRight_universal.dmg" }] },
    updater: { artifacts: [{ file: "MailRight.app.tar.gz", signature: "MailRight.app.tar.gz.sig", platform: "darwin-aarch64", key: "mailright/updates/mac/current/MailRight.app.tar.gz" }] },
  };
  const config = { app: "mailright", version: "0.2.131", targets: { mac: target } };
  fs.writeFileSync(configPath, `export default ${JSON.stringify(config)}`);
  for (const file of [dmg, "MailRight.app.tar.gz", "MailRight.app.tar.gz.sig"]) fs.writeFileSync(path.join(root, file), file);
  fs.mkdirSync(stateRoot, { recursive: true });
  fs.writeFileSync(path.join(stateRoot, "macos-signing.json"), JSON.stringify({ schema: 1, signingContract: target.signingContract, files: [{ file: path.join(root, dmg) }] }));
  const run = (cmd, args) => cmd === "/bin/launchctl" ? { status: 0, stdout: "Aqua" }
    : args[0] === "notarytool" ? { status: 0, stdout: JSON.stringify({ status: "Accepted", id: "current-build" }) } : { status: 0 };
  // env: {} so a host SSH session cannot route this through the live signing agent; `run` fakes the Aqua session.
  notarizeMacosArtifact({ archive: path.join(root, dmg), receiptPath: path.join(stateRoot, "macos-notarization.json"), authArgs: [], run, env: {} });
  for (const staleRoot of [path.join(vaultRoot, "receipts"), path.join(vaultRoot, "state", "mailright-0.2.130-deadbeef", "mac")]) {
    fs.mkdirSync(staleRoot, { recursive: true });
    for (const phase of ["signing", "notarization"]) fs.writeFileSync(path.join(staleRoot, `macos-${phase}.json`), "stale receipt");
  }
  // Execute production identity calculation & seal without invoking build/install/signing.
  const identityStart = source.indexOf("  const inputHashes = hashInputs(requiredInputs);");
  const identityEnd = source.indexOf("  const cargoLock =", identityStart);
  const signingIdentity = runInNewContext(`${source.slice(identityStart, identityEnd)}\nsigningIdentity;`, {
    config, target, configPath, platform: "mac", appRoot: root, vaultRoot, shortCommit: "12345678", process,
    requiredInputs: [], hashInputs: () => ({}), hashFile, hashFileText, path, PIPELINE_FINGERPRINT: "test", signingReceiptPath,
  });
  assert.deepEqual(Array.from(signingIdentity.receiptInputs), [path.join(stateRoot, "macos-signing.json"), path.join(stateRoot, "macos-notarization.json")]);
  const sealStart = source.indexOf("function sealRelease(");
  const sealEnd = source.indexOf("\nfunction collectToolVersions", sealStart);
  const validations = [];
  const seal = runInNewContext(`${source.slice(sealStart, sealEnd)}\nsealRelease;`, {
    ...fs, path, process, hashFile, devtoolsEvidence, resolveDevtoolsAllowance,
    writeJson: (file, value) => fs.writeFileSync(file, JSON.stringify(value)),
    fail: (message) => { throw new Error(message); },
    resolveArtifactDir: () => root, materializeArtifacts: () => {}, completeArtifactLease: () => {}, retainArtifacts: () => ({ removed: [] }),
    validateNotarizationReceipt: (args) => validateNotarizationReceipt({ ...args, run: (cmd, args) => { validations.push([cmd, ...args]); return run(cmd, args); } }),
  });
  const options = { configRoot: root, sealedDir: path.join(vaultRoot, "sealed", releaseId, "mac"), releaseId, config, target, platform: "mac", commit: "12345678".repeat(5), inputHashes: {}, toolVersions: {}, cacheKey: "test", signingIdentity, qaAdmission: { digest: "a".repeat(64), floorDigest: "b".repeat(64) } };
  seal(options);
  const manifest = JSON.parse(fs.readFileSync(path.join(options.sealedDir, "release-manifest.json"), "utf8"));
  assert.deepEqual(manifest.signing.receipts.map((item) => item.file), Array.from(signingIdentity.receiptInputs));
  assert.ok(manifest.signing.receipts.every((item) => item.sha256 === hashFile(item.file)));
  assert.deepEqual(validations, [["xcrun", "stapler", "validate", path.join(root, dmg)]]);
  fs.rmSync(path.join(stateRoot, "macos-notarization.json"));
  assert.throws(() => seal({ ...options, sealedDir: `${options.sealedDir}-missing` }), /missing current-build macOS/);
});

test("receipt overrides resolve relative to nested app root & defaults use build state", () => {
  const appRoot = path.resolve("fixture", "tauri-app-next");
  const stateRoot = path.resolve("fixture", ".right-release", "state", "same-build", "mac");
  const target = { sign: { receipt: "custom/sign.json", installerReceipt: "custom/installer.json" }, notarize: { receipt: "custom/notary.json" } };
  assert.equal(signingReceiptPath({ appRoot, stateRoot, target, platform: "mac", phase: "signing" }), path.join(appRoot, "custom/sign.json"));
  assert.equal(signingReceiptPath({ appRoot, stateRoot, target, platform: "mac", phase: "notarization" }), path.join(appRoot, "custom/notary.json"));
  assert.equal(signingReceiptPath({ appRoot, stateRoot, target, platform: "win", phase: "installer" }), path.join(appRoot, "custom/installer.json"));
  assert.equal(signingReceiptPath({ appRoot, stateRoot, target, platform: "win", phase: "embedding" }), path.join(stateRoot, "windows-embedding.json"));
});
