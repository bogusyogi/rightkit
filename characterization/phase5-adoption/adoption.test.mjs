import assert from "node:assert/strict";
import test from "node:test";

import { inspectPhase5Adoption } from "./adoption.mjs";

test("OCR shared core remains identical in CodeRight and ScrapeRight", async () => {
  const report = await inspectPhase5Adoption();

  assert.deepEqual(report.ocr.coreDrift, []);
  assert.deepEqual(report.ocr.adapters, {
    coderight: {
      modelEnv: "CODERIGHT_OCR_MODEL_DIR",
      modelSuffix: ["CodeRight", "models", "ocr"],
      ortCrate: "coderight-ort-common",
    },
    scraperight: {
      modelEnv: "SCRAPERIGHT_OCR_MODEL_DIR",
      modelSuffix: ["ScrapeRight", "models", "ocr"],
      ortCrate: "scraperight-ort-common",
    },
  });
});

// Both apps must ship the exact model set PROVENANCE.toml records (CodeRight is canonical).
// ScrapeRight previously hand-placed PP-OCRv6 while the shared-source check still passed green;
// hashing the deployed artifacts is the only thing that catches that.
test("deployed OCR model artifacts are identical in CodeRight and ScrapeRight", async () => {
  const report = await inspectPhase5Adoption();

  assert.deepEqual(report.ocr.artifactDrift, []);
});

test("Mac gates keep all Phase 5 crates internal", async () => {
  const report = await inspectPhase5Adoption();

  assert.equal(report.ocr.provenanceStatus, "resolved");
  assert.equal(report.readyToPublish, false);
  assert.deepEqual(report.crates, {
    "rightkit-ort": { publish: false },
    "rightkit-ocr": { publish: false },
    "rightkit-media": { publish: false },
  });
  assert.deepEqual(report.prematureDependencies, []);
});

test("Windows media parity is frozen while Mac artifact rows remain unapproved", async () => {
  const report = await inspectPhase5Adoption();

  assert.deepEqual(report.media.windows, {
    id: "ffmpeg-7.1.5-windows-x64",
    status: "approved",
    sha256: "ded75c0edf344b9eed5876cd6cc469b7366b00c4dd0f25b3965062965c33990d",
    allCopiesMatch: true,
  });
  assert.deepEqual(
    report.media.macos.map(({ arch, status }) => ({ arch, status })),
    [
      { arch: "arm64", status: "stale-unverified" },
      { arch: "x86_64", status: "stale-unverified" },
    ],
  );
});
