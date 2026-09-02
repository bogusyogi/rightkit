import assert from "node:assert/strict";
import test from "node:test";
import { resolveMacosDeveloperIdIdentity } from "./macos-signing-identity.mjs";

test("uses explicitly configured Developer ID identity", () => {
  assert.equal(resolveMacosDeveloperIdIdentity({ env: { APPLE_DEVELOPER_ID: "Developer ID Application: Fixture (TEAM)" } }), "Developer ID Application: Fixture (TEAM)");
});

test("discovers a sole Developer ID Application identity from Keychain", () => {
  const identity = resolveMacosDeveloperIdIdentity({
    env: {},
    run: () => ({ status: 0, stdout: '  1) HASH "Developer ID Application: Fixture (TEAM)"\n' }),
  });
  assert.equal(identity, "Developer ID Application: Fixture (TEAM)");
});

test("requires explicit selection when Keychain has multiple Developer IDs", () => {
  assert.throws(() => resolveMacosDeveloperIdIdentity({
    env: {},
    run: () => ({ status: 0, stdout: '1) H "Developer ID Application: One (TEAM)"\n2) H "Developer ID Application: Two (TEAM)"' }),
  }), /multiple Developer ID/);
});
