import assert from "node:assert/strict";
import test from "node:test";
import { targetDistribution } from "./target-distribution.mjs";

test("per-target distribution wins over the top-level one", () => {
  const ios = { provider: "app-store-connect" };
  const top = { provider: "github-releases" };
  assert.equal(targetDistribution({ distribution: top, targets: { ios: { distribution: ios } } }, "ios"), ios);
  assert.equal(targetDistribution({ distribution: top, targets: { ios: { distribution: ios } } }, "android"), top);
  assert.equal(targetDistribution({ distribution: top }, "ios"), top);
  assert.equal(targetDistribution(undefined, "ios"), undefined);
});
