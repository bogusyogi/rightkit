import test from "node:test";
import assert from "node:assert/strict";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "./background-process.mjs";

const signer = fileURLToPath(new URL("./sign-macos.mjs", import.meta.url));
test("entitlements option is consumed without becoming a signing target", () => {
  const bundle = path.resolve("OwnershipApproval.app");
  const result = spawnSync(process.execPath, [signer, "--dry-run", "--receipt", "receipt.json", "--entitlements", "ownership.entitlements", bundle], { encoding: "utf8" });
  assert.equal(result.status, 0, result.stderr);
  assert.equal(result.stdout.trim(), `dry-run: Developer ID signing ${bundle}`);
});

test("missing entitlements path fails before signing", () => {
  const result = spawnSync(process.execPath, [signer, "--dry-run", "--entitlements", "--receipt", "receipt.json", "OwnershipApproval.app"], { encoding: "utf8" });
  assert.equal(result.status, 1);
  assert.match(result.stderr, /--entitlements requires a path/);
});
