import assert from "node:assert/strict";
import test from "node:test";

import {
  MODEL_FILES,
  buildCargoWorkspace,
  normalizeTranscript,
  parseTranscript,
  sha256File,
} from "./characterize.mjs";

test("model digest covers the complete shared runtime artifact", () => {
  assert.deepEqual(MODEL_FILES, [
    "config.json",
    "encoder-model.onnx",
    "decoder_joint-model.int8.onnx",
    "mel_nemo.onnx",
    "mel_nemo.onnx.data",
    "tokenizer.model",
    "vocab.txt",
  ]);
});

test("buildCargoWorkspace creates two isolated runtime probes", () => {
  const files = buildCargoWorkspace({
    heardRightCrate: "C:/fixtures/hr/parakeet-rs",
    scrapeRightCrate: "C:/fixtures/sr/parakeet-rs",
  });

  assert.equal(files.has("Cargo.toml"), false, "separate lockfiles avoid same-name path collision");
  assert.match(
    files.get("heardright/Cargo.toml"),
    /parakeet-rs = \{ path = "C:\/fixtures\/hr\/parakeet-rs"/,
  );
  assert.match(files.get("heardright/src/main.rs"), /HrTransducer/);
  assert.match(files.get("scraperight/src/main.rs"), /ScrapeRightTransducer/);
  assert.match(files.get("heardright/src/main.rs"), /resample_to_16k/);
});

test("parseTranscript normalizes runtime labels and whitespace", () => {
  assert.equal(parseTranscript("loaded\nTRANSCRIPT   Hello,   world! \n"), "hello, world!");
  assert.equal(normalizeTranscript("  Hello\r\nWORLD  "), "hello world");
  assert.throws(() => parseTranscript("no transcript here"), /TRANSCRIPT/);
});

test("sha256File returns a stable lowercase digest", async () => {
  const digest = await sha256File(new URL(import.meta.url));
  assert.match(digest, /^[a-f0-9]{64}$/);
  assert.equal(digest, await sha256File(new URL(import.meta.url)));
});
