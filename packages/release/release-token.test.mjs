import assert from "node:assert/strict";
import { mkdtempSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { defaultWindowsReleaseTokenFile, loadReleaseToken } from "./release-token.mjs";

test("loads an explicit release token without exposing admin-session fallback", async () => {
  assert.equal(await loadReleaseToken({ env: { RIGHTAPPS_RELEASE_TOKEN: " durable-token ", RIGHTAPPS_ADMIN_TOKEN: "admin-session" } }), "durable-token");
});

test("loads the release token from an explicit protected-file path", async () => {
  const dir = mkdtempSync(path.join(os.tmpdir(), "right-release-token-"));
  const file = path.join(dir, "token");
  writeFileSync(file, "file-token\n");
  assert.equal(await loadReleaseToken({ env: { RIGHTAPPS_RELEASE_TOKEN_FILE: file }, platform: "linux" }), "file-token");
});

test("uses the portable Windows token location", () => {
  assert.equal(defaultWindowsReleaseTokenFile({ APPDATA: "C:\\Users\\fixture\\AppData\\Roaming" }), path.join("C:\\Users\\fixture\\AppData\\Roaming", "RightKit", "rightapps-release.token"));
});
