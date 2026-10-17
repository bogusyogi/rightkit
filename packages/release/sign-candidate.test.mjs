import assert from "node:assert/strict";
import { appendFileSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileEvidence } from "./macos-file-evidence.mjs";
import { SIGN_CANDIDATE_CONTRACT, SignCandidateError, signCandidate, validateCandidateBinding } from "./sign-candidate.mjs";

const NAMES = ["App.msix", "helper.exe"];

function prepare(names = NAMES) {
  const root = mkdtempSync(path.join(os.tmpdir(), "sign-candidate-"));
  const artifact = path.join(root, "artifact");
  mkdirSync(artifact, { recursive: true });
  for (const name of names) writeFileSync(path.join(artifact, name), `candidate bytes of ${name}`);
  const receipt = path.join(root, "candidate.json");
  writeFileSync(receipt, JSON.stringify({ schema: 1, files: names.map((name) => ({ path: name, sha256: fileEvidence(path.join(artifact, name)).sha256 })) }));
  return { root, artifact, receipt, out: path.join(root, "out", "binding.json") };
}

const winOptions = (p, extra = {}) => ({
  platform: "win", artifact: p.artifact, receiptPath: p.receipt, outPath: p.out, hostPlatform: "win32",
  readSigner: () => "CN=Orthic Labs", ...extra,
});

test("windows: signs the candidate bytes in place and binds candidate sha to signed sha without rebuilding", () => {
  const p = prepare();
  try {
    const before = new Map(NAMES.map((name) => [name, fileEvidence(path.join(p.artifact, name)).sha256]));
    const signed = [];
    const binding = signCandidate(winOptions(p, {
      publisher: "CN=Orthic Labs",
      signWindows: ({ files }) => { for (const file of files) { appendFileSync(file, "\nAUTHENTICODE"); signed.push(path.basename(file)); } },
    }));
    assert.deepEqual(signed, NAMES);
    assert.equal(binding.contract, SIGN_CANDIDATE_CONTRACT);
    assert.equal(binding.rebuilt, false);
    for (const file of binding.files) {
      assert.equal(file.candidateSha256, before.get(file.path));
      assert.equal(file.signedSha256, fileEvidence(path.join(p.artifact, file.path)).sha256);
    }
    assert.equal(binding.files[0].signerSubject, "CN=Orthic Labs");
    assert.deepEqual(JSON.parse(readFileSync(p.out, "utf8")), binding);
    validateCandidateBinding(binding, { platform: "win", candidateSha256s: [...before.values()] });
  } finally { rmSync(p.root, { recursive: true, force: true }); }
});

test("a candidate whose bytes differ from the receipt is refused and nothing is signed", () => {
  const p = prepare();
  try {
    appendFileSync(path.join(p.artifact, "App.msix"), "rebuilt from source");
    let called = false;
    assert.throws(() => signCandidate(winOptions(p, { signWindows: () => { called = true; } })),
      (error) => error instanceof SignCandidateError && error.code === "SIGN_CANDIDATE_HASH_MISMATCH");
    assert.equal(called, false);
  } finally { rmSync(p.root, { recursive: true, force: true }); }
});

test("a signer that leaves a file unchanged, or signs as another publisher, is refused", () => {
  const p = prepare();
  try {
    assert.throws(() => signCandidate(winOptions(p, { signWindows: () => {} })), (error) => error.code === "SIGN_CANDIDATE_UNCHANGED");
  } finally { rmSync(p.root, { recursive: true, force: true }); }
  const q = prepare();
  try {
    assert.throws(() => signCandidate(winOptions(q, {
      publisher: "CN=Someone Else", signWindows: ({ files }) => { for (const file of files) appendFileSync(file, "x"); },
    })), (error) => error.code === "SIGN_CANDIDATE_PUBLISHER_MISMATCH");
  } finally { rmSync(q.root, { recursive: true, force: true }); }
});

test("dry run only verifies hashes; platform and host must match; mac needs a manifest", () => {
  const p = prepare();
  try {
    const dry = signCandidate(winOptions(p, { dryRun: true, hostPlatform: "darwin" }));
    assert.equal(dry.dryRun, true);
    assert.deepEqual(dry.files.map((file) => file.path), NAMES);
    assert.throws(() => signCandidate(winOptions(p, { hostPlatform: "darwin" })), (error) => error.code === "SIGN_CANDIDATE_PLATFORM");
    assert.throws(() => signCandidate({ ...winOptions(p), platform: "mac" }), (error) => error.code === "SIGN_CANDIDATE_USAGE");
    assert.throws(() => signCandidate({ ...winOptions(p), platform: "linux" }), (error) => error.code === "SIGN_CANDIDATE_USAGE");
  } finally { rmSync(p.root, { recursive: true, force: true }); }
});

test("macos: delegates to the dev artifact signer on a copy of the receipt and binds candidate to signed sha", () => {
  const p = prepare(["tool", "other"]);
  try {
    const manifest = path.join(p.root, "sign.json");
    writeFileSync(manifest, JSON.stringify({ schema: 1, entries: [{ file: "tool", identifier: "com.example.tool" }] }));
    const receiptBefore = readFileSync(p.receipt, "utf8");
    const toolBefore = fileEvidence(path.join(p.artifact, "tool")).sha256;
    let received;
    const binding = signCandidate({
      platform: "mac", artifact: p.artifact, receiptPath: p.receipt, outPath: p.out, manifestPath: manifest, hostPlatform: "darwin",
      signMac: (options) => {
        received = options;
        appendFileSync(path.join(p.artifact, "tool"), "\nSIGNED");
        const after = fileEvidence(path.join(p.artifact, "tool")).sha256;
        return { teamIdentifier: "6KLGD3LLKF", files: [{ path: "tool", sha256Before: toolBefore, sha256After: after, identifier: "com.example.tool", teamIdentifier: "6KLGD3LLKF" }] };
      },
    });
    assert.notEqual(path.resolve(received.metadataPath), path.resolve(p.receipt), "the candidate receipt itself is not rewritten");
    assert.equal(readFileSync(p.receipt, "utf8"), receiptBefore);
    assert.equal(binding.signing, "developer-id:6KLGD3LLKF");
    const tool = binding.files.find((file) => file.path === "tool");
    assert.equal(tool.candidateSha256, toolBefore);
    assert.notEqual(tool.signedSha256, toolBefore);
    const other = binding.files.find((file) => file.path === "other");
    assert.equal(other.unsigned, true);
    assert.equal(other.signedSha256, other.candidateSha256);
    validateCandidateBinding(binding, { platform: "mac" });
  } finally { rmSync(p.root, { recursive: true, force: true }); }
});

test("validateCandidateBinding rejects a rebuilt or unchanged result", () => {
  const sha = "a".repeat(64);
  const other = "b".repeat(64);
  const good = { schema: 1, contract: SIGN_CANDIDATE_CONTRACT, platform: "win", rebuilt: false, files: [{ path: "x", candidateSha256: sha, signedSha256: other }] };
  validateCandidateBinding(good);
  assert.throws(() => validateCandidateBinding({ ...good, rebuilt: true }), (error) => error.code === "SIGN_CANDIDATE_BINDING_INVALID");
  assert.throws(() => validateCandidateBinding({ ...good, files: [{ path: "x", candidateSha256: sha, signedSha256: sha }] }), (error) => error.code === "SIGN_CANDIDATE_BINDING_INVALID");
  assert.throws(() => validateCandidateBinding(good, { candidateSha256s: [other] }), (error) => error.code === "SIGN_CANDIDATE_BINDING_INVALID");
});
