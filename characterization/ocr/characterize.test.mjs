import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { test } from "node:test";

import {
  expectedRuntimeDigest,
  extractOcrOutput,
  loadContract,
  verifyArtifactSet,
} from "./characterize.mjs";

const here = new URL(".", import.meta.url);

test("extractOcrOutput returns only the OCR payload", () => {
  const stdout = [
    "OCR target: fixture.png",
    "",
    "=== EXTRACTED TEXT ===",
    "RIGHTKIT OCR 2026",
    "Shared parity fixture",
    "======================",
    "",
    "2 lines extracted.",
  ].join("\n");

  assert.equal(
    extractOcrOutput(stdout),
    "RIGHTKIT OCR 2026\nShared parity fixture",
  );
});

test("the committed fixture and model inventory match the contract", async () => {
  const contract = await loadContract(new URL("contract.json", here));
  const fixture = Buffer.from(
    (await readFile(new URL(contract.fixture.file, here), "utf8")).trim(),
    "base64",
  );

  const verified = await verifyArtifactSet(
    new Map([["fixture", fixture]]),
    new Map([["fixture", contract.fixture.sha256]]),
  );

  assert.deepEqual(verified, {
    fixture: contract.fixture.sha256,
  });
});

test("verifyArtifactSet rejects a digest mismatch with the artifact name", async () => {
  await assert.rejects(
    verifyArtifactSet(
      new Map([["det.onnx", Buffer.from("wrong")]]),
      new Map([["det.onnx", "0".repeat(64)]]),
    ),
    /det\.onnx digest mismatch/,
  );
});

test("the contract rejects an uncharacterized platform runtime", async () => {
  const contract = await loadContract(new URL("contract.json", here));
  assert.equal(
    expectedRuntimeDigest(contract, "win32"),
    "0a49bb13573807ad309aa9967a46471c07d6495bab5b279d1fad474aefd8ef4b",
  );
  assert.throws(
    () => expectedRuntimeDigest(contract, "darwin"),
    /No characterized ORT runtime digest for darwin/,
  );
});
