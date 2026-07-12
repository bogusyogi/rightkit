import assert from "node:assert/strict";
import os from "node:os";
import test from "node:test";
import { notarytoolAuthArgs } from "./notary-auth.mjs";

test("prefers a complete App Store Connect API credential set", () => {
  const before = { ...process.env };
  try {
    process.env.APPLE_API_KEY_PATH = "~/AuthKey.p8";
    process.env.APPLE_API_KEY = "KEY123";
    process.env.APPLE_API_ISSUER = "ISSUER123";
    assert.deepEqual(notarytoolAuthArgs(), ["--key", `${os.homedir()}/AuthKey.p8`, "--key-id", "KEY123", "--issuer", "ISSUER123"]);
  } finally {
    process.env = before;
  }
});

test("falls back to the device keychain profile and rejects partial API auth", () => {
  const before = { ...process.env };
  try {
    delete process.env.APPLE_API_KEY_PATH;
    delete process.env.APPLE_API_KEY;
    delete process.env.APPLE_API_ISSUER;
    assert.deepEqual(notarytoolAuthArgs(), ["--keychain-profile", "apple-dev-notary"]);
    process.env.APPLE_API_KEY = "KEY123";
    assert.throws(() => notarytoolAuthArgs(), /requires APPLE_API_KEY_PATH.*APPLE_API_ISSUER/);
  } finally {
    process.env = before;
  }
});
