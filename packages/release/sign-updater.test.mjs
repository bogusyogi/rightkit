import assert from "node:assert/strict";
import { spawnSync } from "./background-process.mjs";
import { fileURLToPath } from "node:url";
import test from "node:test";

const helper = fileURLToPath(new URL("./sign-updater.mjs", import.meta.url));

test("dry-run signs the final OS-signed artifact with Tauri minisign", () => {
  const result = spawnSync(process.execPath, [helper, "--file", "Fixture.exe", "--dry-run"], { encoding: "utf8" });
  assert.equal(result.status, 0, result.stderr);
  assert.match(result.stdout, /tauri signer sign .*Fixture\.exe/);
});

test("win32 key path comes from --key-file, never a hard-coded app directory", { skip: process.platform !== "win32" }, () => {
  const env = { ...process.env, TAURI_SIGNING_PRIVATE_KEY: "", RIGHT_UPDATER_KEY_FILE: "" };
  const none = spawnSync(process.execPath, [helper, "--file", "Fixture.exe"], { encoding: "utf8", env });
  assert.notEqual(none.status, 0);
  assert.match(none.stderr, /--key-file/);
  const missing = spawnSync(process.execPath, [helper, "--file", "Fixture.exe", "--key-file", "Z:\\no\\updater.key"], { encoding: "utf8", env });
  assert.match(missing.stderr, /updater key not found/);
});

test("sign-updater.mjs hard-codes no app name", async () => {
  const { readFileSync } = await import("node:fs");
  assert.doesNotMatch(readFileSync(helper, "utf8"), /ViewRight/);
});
