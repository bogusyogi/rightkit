import test from "node:test";
import assert from "node:assert/strict";
import { featureLabels } from "./publication-record.mjs";

test("feature labels come from exact passing gate command arguments", () => {
  assert.deepEqual(featureLabels("cargo", "rightkit-http", [
    "cargo test -p rightkit-http --all-features --no-fail-fast -- --show-output",
    "cargo test -p rightkit-http --features async --no-fail-fast -- --show-output",
    "cargo test -p rightkit-http --features async --no-fail-fast -- --test-threads=1 --show-output",
  ]), ["--all-features", "--features async"]);
});

test("feature labels reject commands without unit identity or explicit feature boundary", () => {
  assert.throws(() => featureLabels("cargo", "rightkit-http", ["cargo test -p another --no-fail-fast"]), /malformed/);
  assert.throws(() => featureLabels("cargo", "rightkit-http", ["cargo test -p rightkit-http"]), /feature boundary/);
  assert.deepEqual(featureLabels("npm", "@rightkit/qa", ["pnpm --dir packages/qa run test"]), ["default"]);
});
