import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import test from "node:test";

const helper = fileURLToPath(new URL("./sign-updater.mjs", import.meta.url));

test("dry-run signs the final OS-signed artifact with Tauri minisign", () => {
  const result = spawnSync(process.execPath, [helper, "--file", "Fixture.exe", "--dry-run"], { encoding: "utf8" });
  assert.equal(result.status, 0, result.stderr);
  assert.match(result.stdout, /tauri signer sign .*Fixture\.exe/);
});
