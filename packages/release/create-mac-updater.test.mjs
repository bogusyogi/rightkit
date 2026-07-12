import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import test from "node:test";

const helper = fileURLToPath(new URL("./create-mac-updater.mjs", import.meta.url));

test("documents the final signed-app to signed-updater sequence in dry-run mode", () => {
  const result = spawnSync(process.execPath, [helper, "--app", "bundle/Test.app", "--output", "bundle/Test.app.tar.gz", "--dry-run"], { encoding: "utf8" });
  assert.equal(result.status, 0, result.stderr);
  assert.match(result.stdout, /tar .*Test\.app\.tar\.gz.*Test\.app/);
  assert.match(result.stdout, /tauri signer sign .*Test\.app\.tar\.gz/);
});
