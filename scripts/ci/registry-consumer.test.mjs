import assert from "node:assert/strict";
import test from "node:test";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { assertRegistryManifest, isWithin, registryLockEvidence, runRegistryConsumer } from "./registry-consumer.mjs";
import { sha } from "./upload.mjs";

const unit = { name: "rightkit-logs", version: "0.3.4", sha256: "a".repeat(64) };
const manifest = `[package]\nname="consumer"\nversion="0.0.0"\n[workspace]\n[dependencies]\nrightkit-logs = { version = "=0.3.4", default-features = false, features = [] }\n`;
const lock = (u = unit, extra = {}) => `version = 4\n\n[[package]]\nname = "consumer"\nversion = "0.0.0"\n\n[[package]]\nname = "${u.name}"\nversion = "${extra.version ?? u.version}"\nsource = "${extra.source ?? "registry+https://github.com/rust-lang/crates.io-index"}"\nchecksum = "${extra.checksum ?? u.sha256}"\n`;

test("consumer requires exact version & refuses path/git/patch escape hatches", () => {
  assert.doesNotThrow(() => assertRegistryManifest(manifest, unit));
  for (const altered of [
    manifest.replace("=0.3.4", "0.3.4"),
    manifest.replace("=0.3.4", "=0.3.5"),
    manifest.replace('version = "=0.3.4"', 'path = "../rightkit-logs"'),
    `${manifest}\n[patch.crates-io]\nrightkit-logs={path="../logs"}\n`,
    `${manifest}\n[replace]\n"rightkit-logs:0.3.4"={git="https://example.invalid"}\n`,
    manifest.replace('version = "=0.3.4"', 'version = "=0.3.4", git="https://example.invalid"'),
  ]) assert.throws(() => assertRegistryManifest(altered, unit));
});

test("lock proof binds registry identity, exact version & archive checksum", () => {
  assert.deepEqual(registryLockEvidence(lock(), [unit]), [{ ...unit, source: "registry+https://github.com/rust-lang/crates.io-index", checksum: unit.sha256, archiveSha256: unit.sha256 }].map(({ sha256, ...row }) => row));
  for (const extra of [{ version: "0.3.5" }, { checksum: "b".repeat(64) }, { source: "path+file:///tmp/logs" }, { source: "registry+https://example.invalid/index" }]) assert.throws(() => registryLockEvidence(lock(unit, extra), [unit]), /registry lock mismatch/);
  assert.throws(() => registryLockEvidence(lock().replace(/^checksum = .*$/m, ""), [unit]), /registry lock mismatch/);
  // Every same-name resolution is checked; a correct row cannot hide another version.
  assert.throws(() => registryLockEvidence(lock() + lock(unit, { version: "0.3.3" }), [unit]), /registry lock mismatch/);
  assert.deepEqual(registryLockEvidence(lock(), [{ ...unit, name: "unused-sealed-sibling" }]), []);
});

test("scratch containment rejects checkout & descendants, accepts sibling prefix", () => {
  const root = path.resolve(os.tmpdir(), "workspace");
  assert.equal(isWithin(root, root), true);
  assert.equal(isWithin(root, path.join(root, "scratch")), true);
  assert.equal(isWithin(root, `${root}-scratch`), false);
});

test("runner refuses local execution without launching any process", () => {
  assert.throws(() => runRegistryConsumer({ env: {}, spawn: () => assert.fail("must not execute") }), /requires GitHub CI/);
});

test("archive tampering refuses before extraction or Cargo", () => {
  const scratch = fs.mkdtempSync(path.join(os.tmpdir(), "rk-registry-negative-"));
  try {
    const root = path.join(scratch, "checkout");
    const packs = path.join(scratch, "packed");
    fs.mkdirSync(root);
    fs.mkdirSync(packs);
    const archive = Buffer.from("sealed crate");
    const sealedUnit = { ...unit, file: "rightkit-logs-0.3.4.crate", size: archive.length, sha256: sha("sha256", archive) };
    fs.writeFileSync(path.join(packs, sealedUnit.file), "tampered");
    fs.writeFileSync(path.join(packs, "manifest.json"), JSON.stringify({ schema: 1, registry: "crates", commit: "b".repeat(40), runId: "42", runAttempt: "1", units: [sealedUnit] }));
    assert.throws(() => runRegistryConsumer({ packDir: packs, root, scratchRoot: scratch, env: { GITHUB_ACTIONS: "true", GITHUB_SHA: "b".repeat(40), GITHUB_RUN_ID: "42", GITHUB_RUN_ATTEMPT: "1" }, spawn: () => assert.fail("must not execute") }), /sha256 does not match/);
  } finally { fs.rmSync(scratch, { recursive: true, force: true }); }
});
