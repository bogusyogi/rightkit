import assert from "node:assert/strict";
import { mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import {
  materializeCycloneDxSbom,
  materializeInTotoSlsaProvenance,
  validateCycloneDxSbom,
  validateInTotoSlsaProvenance,
} from "./supply-chain-evidence.mjs";

const identity = {
  product: "legion",
  version: "0.1.0",
  target: "windows-x86_64",
  sourceCommit: "a".repeat(40),
};
const file = { name: "legion.zip", sha256: "b".repeat(64), size: 42 };
const startedAt = "2026-08-28T08:00:00.000Z";
const finishedAt = "2026-08-28T08:01:00.000Z";

test("materializes conforming CycloneDX 1.6 and in-toto SLSA v1 evidence", () => {
  const root = mkdtempSync(join(tmpdir(), "rightrelease-supply-chain-"));
  const sbom = join(root, "legion.cdx.json");
  const provenance = join(root, "legion.intoto.jsonl");
  materializeCycloneDxSbom({ outputPath: sbom, ...identity, files: [file], createdAt: finishedAt });
  materializeInTotoSlsaProvenance({
    outputPath: provenance,
    ...identity,
    sourceRepository: "https://github.com/Orthic-Labs/legion",
    subjects: [file],
    startedAt,
    finishedAt,
  });
  assert.equal(validateCycloneDxSbom(sbom, { expectedFile: file }).specVersion, "1.6");
  assert.equal(validateInTotoSlsaProvenance(provenance, { expectedSubject: file }).predicateType, "https://slsa.dev/provenance/v1");
  assert.equal(readFileSync(provenance, "utf8").trim().split(/\r?\n/).length, 1);
});

test("validators reject renamed arbitrary JSON and unbound evidence", () => {
  const root = mkdtempSync(join(tmpdir(), "rightrelease-supply-chain-"));
  const fakeSbom = join(root, "fake.cdx.json");
  const fakeProvenance = join(root, "fake.intoto.jsonl");
  writeFileSync(fakeSbom, "{}\n");
  writeFileSync(fakeProvenance, "{}\n");
  assert.throws(() => validateCycloneDxSbom(fakeSbom), /CycloneDX/);
  assert.throws(() => validateInTotoSlsaProvenance(fakeProvenance), /in-toto\/SLSA/);
});
