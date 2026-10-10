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

import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import os from "node:os";

test("a manifest replaces positional files and --entitlements", () => {
  const dir = mkdtempSync(path.join(os.tmpdir(), "rk-sign-manifest-"));
  try {
    const manifest = path.join(dir, "m.json");
    writeFileSync(manifest, JSON.stringify({ schema: 1, entries: [{ file: "/x/Helper", options: [] }, { file: "/x/App.app", entitlements: "/x/app.plist" }] }));
    const ok = spawnSync(process.execPath, [signer, "--dry-run", "--receipt", "r.json", "--manifest", manifest], { encoding: "utf8" });
    assert.equal(ok.status, 0, ok.stderr);
    assert.equal(ok.stdout.trim(), "dry-run: Developer ID signing /x/Helper, /x/App.app");
    const mixed = spawnSync(process.execPath, [signer, "--dry-run", "--receipt", "r.json", "--manifest", manifest, "/x/Other"], { encoding: "utf8" });
    assert.equal(mixed.status, 1);
    assert.match(mixed.stderr, /--manifest cannot be combined/);
    writeFileSync(manifest, JSON.stringify({ schema: 1, entries: [{ file: "relative" }] }));
    const invalid = spawnSync(process.execPath, [signer, "--dry-run", "--receipt", "r.json", "--manifest", manifest], { encoding: "utf8" });
    assert.equal(invalid.status, 1);
    assert.match(invalid.stderr, /absolute path/);
  } finally { rmSync(dir, { recursive: true, force: true }); }
});
