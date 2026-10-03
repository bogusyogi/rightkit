import assert from "node:assert/strict";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { notarytoolAuthArgs } from "./notary-auth.mjs";

const apiEnv = {
  APPLE_API_KEY_PATH: "~/AuthKey.p8",
  APPLE_API_KEY: "SYNTHETIC_KEY",
  APPLE_API_ISSUER: "SYNTHETIC_ISSUER",
};

test("automatic selection uses a complete App Store Connect API credential set", () => {
  assert.deepEqual(notarytoolAuthArgs({ env: apiEnv }), ["--key", path.join(os.homedir(), "AuthKey.p8"), "--key-id", "SYNTHETIC_KEY", "--issuer", "SYNTHETIC_ISSUER"]);
});

test("automatic selection defaults to the device keychain profile without API settings", () => {
  assert.deepEqual(notarytoolAuthArgs({ env: {} }), ["--keychain-profile", "apple-dev-notary"]);
  assert.deepEqual(notarytoolAuthArgs({ env: { APPLE_API_KEY: "  " } }), ["--keychain-profile", "apple-dev-notary"]);
});

test("automatic selection rejects every partial API credential combination", () => {
  const entries = Object.entries(apiEnv);
  for (let mask = 1; mask < 7; mask++) {
    const env = Object.fromEntries(entries.filter((_, index) => mask & (1 << index)));
    assert.throws(() => notarytoolAuthArgs({ env }), /requires APPLE_API_KEY_PATH.*APPLE_API_ISSUER/, `API credential mask ${mask}`);
  }
});

test("an explicit keychain profile overrides every inherited API credential combination", () => {
  const entries = Object.entries(apiEnv);
  for (let mask = 0; mask < 8; mask++) {
    const env = Object.fromEntries(entries.filter((_, index) => mask & (1 << index)));
    assert.deepEqual(notarytoolAuthArgs({ profile: "apple-dev-notary", env }), ["--keychain-profile", "apple-dev-notary"], `API credential mask ${mask}`);
  }
});

test("explicit keychain profile names are trimmed", () => {
  assert.deepEqual(notarytoolAuthArgs({ profile: " custom-profile ", env: apiEnv }), ["--keychain-profile", "custom-profile"]);
});

test("an invalid explicit profile fails instead of switching authentication modes", () => {
  for (const profile of ["", "  ", null, 123]) {
    assert.throws(() => notarytoolAuthArgs({ profile, env: apiEnv }), /keychain profile must be a non-empty string/);
  }
});
