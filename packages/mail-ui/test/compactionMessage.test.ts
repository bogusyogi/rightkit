import test from "node:test";
import assert from "node:assert/strict";
import { compactionResultMessage } from "../src/lib/compactionMessage";

test("result reports reclaimed bytes and rounded seconds", () => {
  const msg = compactionResultMessage({
    before_bytes: 200 * 1024 * 1024,
    after_bytes: 150 * 1024 * 1024,
    seconds: 42.6,
    rebuild_registered: false,
  });
  assert.equal(msg, "Reclaimed 52.4 MB in 43 s.");
});

test("re-index sentence is added only when a rebuild was registered", () => {
  const msg = compactionResultMessage({
    before_bytes: 2048,
    after_bytes: 1024,
    seconds: 1,
    rebuild_registered: true,
  });
  assert.equal(
    msg,
    "Reclaimed 1.0 KB in 1 s. Search is being re-indexed in the background.",
  );
});

test("a result that grew never reports negative reclaimed space", () => {
  const msg = compactionResultMessage({
    before_bytes: 1000,
    after_bytes: 5000,
    seconds: 0,
    rebuild_registered: false,
  });
  assert.equal(msg, "Reclaimed 0 B in 0 s.");
});
