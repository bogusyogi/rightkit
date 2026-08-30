import assert from "node:assert/strict";
import test from "node:test";
import { resolveReleaseCacheMode } from "./release-cache-mode.mjs";

test("GitHub Actions signing uses local legacy cache by default", () => {
  assert.equal(resolveReleaseCacheMode({ platform: "mac", env: { GITHUB_ACTIONS: "true" } }), "legacy");
  assert.equal(resolveReleaseCacheMode({ platform: "win", env: { GITHUB_ACTIONS: "true" } }), "legacy");
});

test("non-CI native builds retain shared cache and explicit mode remains authoritative", () => {
  assert.equal(resolveReleaseCacheMode({ platform: "mac", env: {} }), "shared");
  assert.equal(resolveReleaseCacheMode({ platform: "win", env: {} }), "shared");
  assert.equal(resolveReleaseCacheMode({ requested: "shared", platform: "mac", env: { GITHUB_ACTIONS: "true" } }), "shared");
  assert.equal(resolveReleaseCacheMode({ requested: "legacy", platform: "win", env: {} }), "legacy");
});

test("cache mode fails closed for invalid explicit values", () => {
  assert.throws(() => resolveReleaseCacheMode({ requested: "off", platform: "mac", env: { GITHUB_ACTIONS: "true" } }), /RIGHT_RELEASE_CACHE_MODE/);
});
