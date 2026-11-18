import { test } from "node:test";
import assert from "node:assert/strict";
import { allFeaturesCheckIsRedundant as redundant } from "./deterministic-scans.mjs";

const all = ["--all-features"];
test("the default --all-features check is dropped only when a clippy set already turns on every declared feature (NN27)", () => {
  // The witness fixture, 2026-11-17: featureless crates (rk-util, rk-doc, app/engine) and rk-slow (one feature `slow`, clippy sets [] and --features slow).
  assert.equal(redundant({ set: all, declared: [], lintSets: [[]] }), true, "no features: --all-features is the default set");
  assert.equal(redundant({ set: all, declared: ["slow"], lintSets: [[], ["--features", "slow"]] }), true);
  assert.equal(redundant({ set: all, declared: ["a", "b"], lintSets: [["--features", "a,b"]] }), true);
  assert.equal(redundant({ set: all, declared: ["a", "b"], lintSets: [["--features", "a", "--features", "b"]] }), true);
  assert.equal(redundant({ set: all, declared: ["a", "b"], lintSets: [["--features=a b"]] }), true);
  // Kept: a feature no clippy set turns on, an explicit check list, an unreadable manifest, any set other than the default --all-features.
  assert.equal(redundant({ set: all, declared: ["alpha", "beta", "excl-a", "excl-b"], lintSets: [[], ["--features", "alpha"]] }), false);
  assert.equal(redundant({ set: all, declared: [], lintSets: [[]], explicit: true }), false);
  assert.equal(redundant({ set: all, declared: null, lintSets: [[]] }), false);
  assert.equal(redundant({ set: ["--no-default-features"], declared: [], lintSets: [[]] }), false);
  // A clippy set that is itself --all-features never makes the check redundant here (the planner already plans no check then).
  assert.equal(redundant({ set: all, declared: ["a"], lintSets: [["--all-features"]] }), false);
});
