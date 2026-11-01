import test from "node:test";
import assert from "node:assert/strict";
import {
  meaningAvailable,
  semanticStatusText,
  showInstallModel,
  type SemanticStatusView,
} from "../src/lib/semanticStatus";
import {
  MEANING_EMPTY_NOTE,
  MEANING_ERROR_NOTE,
  meaningOrKeyword,
} from "../src/lib/meaningSearch";

const view = (over: Partial<SemanticStatusView>): SemanticStatusView => ({
  state: "off",
  indexed: 0,
  total: 0,
  ...over,
});

test("status text uses plain words for every state", () => {
  assert.equal(semanticStatusText(null), "Checking status...");
  assert.equal(semanticStatusText(view({ state: "off" })), "Off");
  assert.equal(
    semanticStatusText(view({ state: "runtime_missing" })),
    "Not available on this computer yet",
  );
  assert.equal(semanticStatusText(view({ state: "model_missing" })), "Model not installed");
  assert.equal(
    semanticStatusText(view({ state: "indexing", indexed: 12400, total: 164000 })),
    "Indexing 12,400 of 164,000",
  );
  assert.equal(semanticStatusText(view({ state: "ready" })), "Ready");
  assert.match(
    semanticStatusText(view({ state: "error", error_code: "embedder_load" })),
    /embedder_load.*Restart App/,
  );
  assert.match(semanticStatusText(view({ state: "error" })), /^Indexing stopped\./);
});

test("Install model shows only when the model is missing and installable", () => {
  assert.equal(showInstallModel(view({ state: "model_missing", can_install: true })), true);
  assert.equal(showInstallModel(view({ state: "model_missing", can_install: false })), false);
  assert.equal(showInstallModel(view({ state: "model_missing" })), false);
  assert.equal(showInstallModel(view({ state: "ready", can_install: true })), false);
  assert.equal(showInstallModel(null), false);
});

test("the Meaning toggle shows only when ready", () => {
  assert.equal(meaningAvailable(view({ state: "ready" })), true);
  for (const state of ["off", "model_missing", "runtime_missing", "indexing", "error"] as const) {
    assert.equal(meaningAvailable(view({ state })), false, state);
  }
  assert.equal(meaningAvailable(null), false);
});

test("meaning results are used as is when there are some", async () => {
  const out = await meaningOrKeyword(
    async () => ["m1"],
    async () => {
      throw new Error("keyword must not run");
    },
  );
  assert.deepEqual(out, { rows: ["m1"], note: null, usedMeaning: true });
});

test("an empty meaning result falls back to keyword results with a note", async () => {
  const out = await meaningOrKeyword(
    async () => [],
    async () => ["k1", "k2"],
  );
  assert.deepEqual(out, { rows: ["k1", "k2"], note: MEANING_EMPTY_NOTE, usedMeaning: false });
});

test("a meaning error falls back to keyword results with a note", async () => {
  const out = await meaningOrKeyword(
    async () => {
      throw new Error("boom");
    },
    async () => ["k1"],
  );
  assert.deepEqual(out, { rows: ["k1"], note: MEANING_ERROR_NOTE, usedMeaning: false });
});
