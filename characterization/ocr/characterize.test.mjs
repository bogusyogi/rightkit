import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { test } from "node:test";

import {
  expectedRuntimeDigest,
  loadNativeCatalog,
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
  const catalog = await loadNativeCatalog();
  const pinned = catalog.libraries.filter((r) => r.library === "onnxruntime" && r.os === "windows" && r.arch === "x86_64" && r.status !== "stale-unverified" && r.status !== "planned");
  assert.equal(pinned.length, 1);
  assert.equal(expectedRuntimeDigest(catalog, "win32"), pinned[0].sha256);
  assert.match(expectedRuntimeDigest(catalog, "win32"), /^[0-9a-f]{64}$/);
  // The contract carries no runtime digest of its own.
  assert.equal((await loadContract(new URL("contract.json", here))).ortRuntime, undefined);
  assert.throws(
    () => expectedRuntimeDigest({ libraries: [...catalog.libraries, { ...pinned[0], id: "second" }] }, "win32"),
    /No characterized ORT runtime digest for win32/,
  );
  assert.throws(
    () => expectedRuntimeDigest(catalog, "darwin"),
    /No characterized ORT runtime digest for darwin/,
  );
});
