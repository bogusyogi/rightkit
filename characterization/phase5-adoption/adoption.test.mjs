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

// The shared source is identical; the shipped models are not. CodeRight bundles the artifacts
// PROVENANCE.toml records (436-line dictionary); ScrapeRight bundles genuine PP-OCRv6
// (18,708-line unified dictionary, per its det/rec_config.json). Phase 5's gate demands "the same
// crate/model digest", so this must fail closed until both apps ship one model set.
test("deployed OCR model artifacts diverge between CodeRight and ScrapeRight", async () => {
  const report = await inspectPhase5Adoption();

  assert.deepEqual(report.ocr.artifactDrift, [
    {
      file: "det.onnx",
      recorded: "61824840edf6e74581898930b8091b1b2318f4b2705a2e8a40ad3de7ac480133",
      coderight: "61824840edf6e74581898930b8091b1b2318f4b2705a2e8a40ad3de7ac480133",
      scraperight: "eb13b44b25bb36f89528b68720af8a61d9cf381176107f465db1757b65d086e1",
    },
    {
      file: "rec.onnx",
      recorded: "4e16deb22c4da6468bdca539b2cd3c8687825538b67109177c47d359ab994cd7",
      coderight: "4e16deb22c4da6468bdca539b2cd3c8687825538b67109177c47d359ab994cd7",
      scraperight: "5435fd747c9e0efe15a96d0b378d5bd157e9492ed8fd80edf08f30d02fa24634",
    },
    {
      file: "en_dict.txt",
      recorded: "e025a66d31f327ba0c232e03f407ae8d105e1e709e7ccb3f408aa778c24e70d6",
      coderight: "e025a66d31f327ba0c232e03f407ae8d105e1e709e7ccb3f408aa778c24e70d6",
      scraperight: "b5f2bfe2bdd9448429e3e82b51c789775d9b42f2403d082b00662eb77e401c5d",
    },
  ]);
  assert.equal(report.readyToPublish, false);
});

test("unresolved provenance and Mac gates keep all Phase 5 crates internal", async () => {
  const report = await inspectPhase5Adoption();

  assert.equal(report.ocr.provenanceStatus, "conflict-unresolved");
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
