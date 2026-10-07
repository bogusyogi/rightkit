import assert from "node:assert/strict";
import { cpSync, mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import test from "node:test";
import { findOwnedCapability, formatOwnedCapability, renderOwnershipSessionCatalog, OWNERSHIP_CATALOG_PATH, RIGHTKIT_VERSIONS_PATH } from "./ownership-lookup.mjs";
import { syncOwnershipCatalog } from "./ownership-catalog-sync.mjs";

const catalogs = {
  manifest: { schema: 1, capabilities: [
    { package: "rightkit-oauth", capability: "oauth-sign-in", summary: "OAuth login", fingerprints: [] },
    { package: "rightkit-new", capability: "new-feature", summary: "Unreleased feature", fingerprints: [] },
  ] },
  versions: { cargo: {}, stagedCargo: { "rightkit-oauth": "0.2.1" }, npm: {}, stagedNpm: {} },
};

test("lookup uses canonical capability catalog and registry availability", () => {
  const [item] = findOwnedCapability("oauth-sign-in", catalogs);
  assert.equal(item.owner, "rightkit-oauth");
  assert.deepEqual(item.availability, [{ registry: "crates.io", package: "rightkit-oauth", version: "0.2.1", state: "latest" }]);
  assert.match(formatOwnedCapability([item]), /rightkit-oauth@0\.2\.1 \(latest, crates\.io\)/);
  assert.deepEqual(findOwnedCapability("unknown", catalogs), []);
});

test("catalog does not invent availability or implementation details", () => {
  const text = renderOwnershipSessionCatalog(catalogs);
  assert.match(text, /oauth-sign-in → rightkit-oauth: rightkit-oauth@0\.2\.1/);
  assert.match(text, /new-feature → rightkit-new: no published version recorded/);
  assert.doesNotMatch(text, /Platforms:|Minimum version:|API:/);
  assert.match(text, /dependency declarations do not prove integration/);
});

test("packaged snapshot stays equal to release catalog & versions", () => {
  const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../../release");
  assert.deepEqual(JSON.parse(readFileSync(OWNERSHIP_CATALOG_PATH, "utf8")), JSON.parse(readFileSync(path.join(root, "rightkit-ownership.json"), "utf8")));
  assert.deepEqual(JSON.parse(readFileSync(RIGHTKIT_VERSIONS_PATH, "utf8")), JSON.parse(readFileSync(path.join(root, "rightkit-versions.json"), "utf8")));
  assert.doesNotThrow(() => syncOwnershipCatalog({ source: root, output: path.dirname(OWNERSHIP_CATALOG_PATH) }));
});

test("copied client package runs rightkit owns without release sibling", () => {
  const source = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
  const root = mkdtempSync(path.join(tmpdir(), "rk-client-copy-"));
  const copy = path.join(root, "build-control-clients");
  try {
    cpSync(source, copy, { recursive: true });
    const cli = path.join(copy, "src", "rightkit.mjs");
    const result = spawnSync(process.execPath, [cli, "owns", "oauth-sign-in"], { encoding: "utf8", windowsHide: true, cwd: root });
    assert.equal(result.status, 0, result.stderr);
    assert.match(result.stdout, /oauth-sign-in — rightkit-oauth/);
    assert.match(result.stdout, /Availability: rightkit-oauth@\d+\.\d+\.\d+ \(latest, crates\.io\)/);
    const compact = spawnSync(process.execPath, [cli, "owns", "--compact"], { encoding: "utf8", windowsHide: true, cwd: root });
    assert.equal(compact.status, 0, compact.stderr);
    assert.match(compact.stdout, /RightKit ownership index/);
  } finally { rmSync(root, { recursive: true, force: true }); }
});
