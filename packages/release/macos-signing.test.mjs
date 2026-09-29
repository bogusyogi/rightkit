import assert from "node:assert/strict";
import { spawnSync } from "./background-process.mjs";
import { test } from "node:test";

test("macOS notarizer dry-run does not execute signer import side effects", () => {
  const result = spawnSync(process.execPath, ["notarize-macos.mjs", "--dry-run", "--receipt", "receipt.json", "candidate.zip"], {
    cwd: new URL(".", import.meta.url),
    encoding: "utf8",
  });
  assert.equal(result.status, 0, result.stderr);
  assert.match(result.stdout, /^dry-run: notarize /);
  assert.doesNotMatch(`${result.stdout}\n${result.stderr}`, /right-sign-macos/);
});
