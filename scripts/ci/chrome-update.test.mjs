import assert from "node:assert/strict";
import test from "node:test";
import { archiveUrl, BUCKET, compareVersions, executableMember, stableVersion } from "./chrome-update.mjs";

test("versions compare numerically, field by field", () => {
  assert.ok(compareVersions("156.0.8078.47", "155.0.8059.39") > 0);
  assert.ok(compareVersions("155.0.8059.39", "155.0.8059.100") < 0);
  assert.equal(compareVersions("156.0.8078.47", "156.0.8078.47"), 0);
});

test("archives come only from Google's Chrome for Testing bucket; executables are the members provision-chrome runs", () => {
  assert.equal(archiveUrl("156.0.8078.47", "mac-arm64", "chrome"), `${BUCKET}156.0.8078.47/mac-arm64/chrome-mac-arm64.zip`);
  assert.equal(archiveUrl("156.0.8078.47", "win64", "headless"), `${BUCKET}156.0.8078.47/win64/chrome-headless-shell-win64.zip`);
  assert.equal(executableMember("mac-arm64", "chrome"), "chrome-mac-arm64/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing");
  assert.equal(executableMember("win64", "chrome"), "chrome-win64/chrome.exe");
  assert.equal(executableMember("linux64", "chrome"), "chrome-linux64/chrome");
  assert.equal(executableMember("win64", "headless"), "chrome-headless-shell-win64/chrome-headless-shell.exe");
});

test("Stable is read from last-known-good-versions; a malformed answer refuses", async () => {
  const ok = async () => ({ ok: true, json: async () => ({ channels: { Stable: { version: "156.0.8078.47" } } }) });
  assert.equal(await stableVersion(ok), "156.0.8078.47");
  const bad = async () => ({ ok: true, json: async () => ({ channels: { Stable: { version: "latest" } } }) });
  await assert.rejects(stableVersion(bad), /no Stable version/);
  const down = async () => ({ ok: false, status: 503 });
  await assert.rejects(stableVersion(down), /HTTP 503/);
});
