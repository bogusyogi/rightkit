import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import test from "node:test";
import { generateOwnershipCatalog, validateCatalogChanges, writeOwnershipCatalog } from "./ownership-catalog.mjs";
import { ownershipAvailability } from "./ownership-availability.mjs";
import { scanApp } from "./ownership.mjs";
import { spawnSync } from "./background-process.mjs";

const root = path.resolve(import.meta.dirname, "../..");
const catalog = JSON.parse(readFileSync(new URL("./rightkit-ownership.json", import.meta.url)));
const versions = JSON.parse(readFileSync(new URL("./rightkit-versions.json", import.meta.url)));
const digest = (bytes) => createHash("sha256").update(bytes).digest("hex");
const canonical = (value) => JSON.stringify(value, (_key, item) => item && typeof item === "object" && !Array.isArray(item)
  ? Object.fromEntries(Object.keys(item).sort().map((key) => [key, item[key]])) : item);

test("all three generated catalogs have equal byte & canonical policy digests", () => {
  const files = ["release/rightkit-ownership.json", "build-control-policy/src/ownership-session-catalog.json", "build-control-policy/src/ownership-fingerprints.json"];
  const bytes = files.map((file) => readFileSync(path.join(root, "packages", file)));
  assert.equal(new Set(bytes.map(digest)).size, 1);
  assert.equal(new Set(bytes.map((data) => digest(canonical(JSON.parse(data))))).size, 1);
  assert.deepEqual(generateOwnershipCatalog(root), catalog);
});

test("every fingerprint owns its tier & context in schema-v2 policyDigest input", () => {
  for (const entry of catalog.capabilities) for (const fingerprint of entry.fingerprints) {
    if (fingerprint.tier === "block") assert.ok(fingerprint.policy === "banned" || fingerprint.qualification, fingerprint.id);
    else assert.equal(fingerprint.tier, "advise", fingerprint.id);
    assert.ok(fingerprint.usageContext.hosts.length);
    assert.ok(fingerprint.usageContext.scope);
    const policy = { ...fingerprint, capability: entry.capability, package: entry.package, tier: fingerprint.tier };
    assert.notEqual(digest(canonical(policy)), digest(canonical({ ...policy, tier: policy.tier === "block" ? "advise" : "block" })));
    assert.notEqual(digest(canonical(policy)), digest(canonical({ ...policy, usageContext: { ...policy.usageContext, scope: "changed" } })));
    assert.equal(digest(canonical(policy)), digest(canonical({ ...fingerprint, package: entry.package, capability: entry.capability, tier: fingerprint.tier })));
  }
});

test("generation refuses owner conflicts even with approval; new ids require explicit flag", () => {
  const previous = { capabilities: [{ capability: "test-capability", package: "rightkit-test" }, { capability: "family", package: "rightkit-native" }, { capability: "family", package: "@rightkit/renderer" }] };
  assert.throws(() => validateCatalogChanges(previous, { capabilities: [{ capability: "test-capability", package: "rightkit-imposter" }] }), /already belongs/);
  assert.throws(() => validateCatalogChanges(previous, { capabilities: [{ capability: "test-capability", package: "rightkit-imposter" }] }, { approveNewCapabilities: true }), /already belongs/);
  assert.throws(() => validateCatalogChanges(previous, { capabilities: [{ capability: "new-capability", package: "rightkit-test" }] }), /--approve-new-capabilities/);
  assert.doesNotThrow(() => validateCatalogChanges(previous, { capabilities: [{ capability: "new-capability", package: "rightkit-test" }] }, { approveNewCapabilities: true }));
  assert.doesNotThrow(() => validateCatalogChanges(previous, { capabilities: previous.capabilities }));
  assert.throws(() => validateCatalogChanges(previous, { capabilities: [previous.capabilities[0], previous.capabilities[0]] }), /duplicate owner declaration/);
  assert.throws(() => validateCatalogChanges(previous, { capabilities: [{ capability: "new-capability", package: "rightkit-test" }, { capability: "new-capability", package: "rightkit-other" }] }, { approveNewCapabilities: true }), /conflicting new owners/);
});

function fixture(context) {
  const dir = mkdtempSync(path.join(tmpdir(), "rk-catalog-"));
  context.after(() => rmSync(dir, { recursive: true, force: true }));
  for (const group of ["crates", "swift", "packages/release", "packages/fixture"]) mkdirSync(path.join(dir, group), { recursive: true });
  writeFileSync(path.join(dir, "packages/release/rightkit-versions.json"), JSON.stringify(versions));
  writeFileSync(path.join(dir, "packages/release/ownership-internal-owners.json"), '{"owners":[]}');
  writeFileSync(path.join(dir, "packages/fixture/package.json"), '{"name":"@rightkit/fixture"}');
  const declaration = { package: "@rightkit/fixture", capabilities: [{ capability: "new-capability", detection: "declaration-only", fingerprints: [] }] };
  const file = path.join(dir, "packages/fixture/rightkit-ownership.json");
  writeFileSync(file, JSON.stringify(declaration));
  return { dir, file, declaration };
}

test("manifest refuses conflicts/new ids before any output write", (context) => {
  const { dir, file, declaration } = fixture(context);
  const output = path.join(dir, "packages/release/rightkit-ownership.json");
  writeFileSync(output, "unchanged\n");
  assert.throws(() => writeOwnershipCatalog(dir, { previous: { capabilities: [] } }), /--approve-new-capabilities/);
  assert.equal(readFileSync(output, "utf8"), "unchanged\n");
  writeOwnershipCatalog(dir, { previous: { capabilities: [] }, approveNewCapabilities: true });
  declaration.capabilities[0].capability = "sqlite-storage";
  writeFileSync(file, JSON.stringify(declaration));
  const before = readFileSync(output, "utf8");
  assert.throws(() => writeOwnershipCatalog(dir, { approveNewCapabilities: true }), /already belongs/);
  assert.equal(readFileSync(output, "utf8"), before);
});

test("generation validates owner metadata, code scope & qualified block tier", (context) => {
  const { dir, file, declaration } = fixture(context);
  declaration.package = "@rightkit/imposter";
  writeFileSync(file, JSON.stringify(declaration));
  assert.throws(() => generateOwnershipCatalog(dir), /owner differs/);
  declaration.package = "@rightkit/fixture";
  declaration.capabilities[0].detection = "fingerprinted";
  declaration.capabilities[0].fingerprints = [{ id: "fixture.npm.library", type: "dependency", ecosystem: "npm", name: "library", tier: "block", usageContext: { hosts: ["macos", "windows"], scope: "test" }, tierReason: "test" }];
  writeFileSync(file, JSON.stringify(declaration));
  assert.throws(() => generateOwnershipCatalog(dir), /two-host qualification/);
  Object.assign(declaration.capabilities[0].fingerprints[0], { tier: "advise", type: "source", pattern: "CommonName" });
  writeFileSync(file, JSON.stringify(declaration));
  assert.throws(() => generateOwnershipCatalog(dir), /code & extension scoped/);
});

test("availability distinguishes registry, staging & Swift owner alias", () => {
  // Synthetic versions: the live rightkit-versions.json moves with every publish, so asserting its numbers here
  // went stale each time (qa 0.2.15 and asr 0.1.0 are staged beyond their registry rows).
  const fixture = {
    cargo: { "rightkit-pinned": "0.1.0" },
    stagedCargo: { "rightkit-recorded": "0.3.0", "rightkit-same": "0.2.0", "rightkit-pinned": "0.1.0" },
    registry: { cargo: { "rightkit-recorded": "0.2.0", "rightkit-same": "0.2.0" }, npm: { "@rightkit/x": "1.0.0" } },
    stagedNpm: { "@rightkit/x": "1.1.0" },
    stagedSwift: { "rightkit-swift": { url: "https://example.test/rightkit-swift.git", version: "0.4.0" } },
    swift: { "rightkit-swift": { url: "https://example.test/rightkit-swift.git", version: "0.4.0" } },
  };
  const rows = (pkg) => ownershipAvailability({ package: pkg }, fixture).map(({ version, state }) => [version, state]);
  assert.deepEqual(rows("rightkit-recorded"), [["0.2.0", "registry-recorded"], ["0.3.0", "staged-unverified"]]);
  assert.deepEqual(rows("rightkit-same"), [["0.2.0", "registry-recorded"]], "staged equal to the recorded version adds no row");
  assert.deepEqual(rows("rightkit-pinned"), [["0.1.0", "registry-pinned"]], "no registry record: the pin stands, staged equal to it adds no row");
  assert.deepEqual(rows("@rightkit/x"), [["1.0.0", "registry-recorded"], ["1.1.0", "staged-unverified"]]);
  assert.deepEqual(rows("RightKitSwift"), [["0.4.0", "registry-pinned"]]);
  assert.deepEqual(rows("rightkit-swift"), rows("RightKitSwift"), "both Swift names resolve to one owner");
  assert.deepEqual(rows("rightkit-not-a-crate"), []);
  // The live file: a recorded row first whenever the registry block names the package.
  const live = ownershipAvailability({ package: "rightkit-qa" }, versions);
  assert.equal(live[0].state, versions.registry.cargo["rightkit-qa"] ? "registry-recorded" : "registry-pinned");
  assert.ok(live.slice(1).every(({ state }) => state === "staged-unverified"));
});

test("catalog never treats broad URL literals, API declarations or common infrastructure as implementations", (context) => {
  const dir = mkdtempSync(path.join(tmpdir(), "rk-catalog-negative-"));
  context.after(() => rmSync(dir, { recursive: true, force: true }));
  assert.equal(spawnSync("git", ["-C", dir, "init", "-q"], { windowsHide: true }).status, 0);
  writeFileSync(path.join(dir, "Cargo.toml"), '[package]\nname="consumer"\nversion="0.1.0"\n[dependencies]\nserde="1"\ntokio="1"\ntracing-subscriber="1"\nsecurity-framework="1"\nsqlx="1"\n');
  writeFileSync(path.join(dir, "source.rs"), 'let url="https://github.com/login/device/code";\nlet protocol="notifications/initialized";\nlet info="JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE";\n');
  writeFileSync(path.join(dir, "source.swift"), 'public func copySyncedToDeviceOnly(_ value: String) {}\n');
  assert.deepEqual([...scanApp(dir, catalog, { trust: null })], []);
});

test("banned dependencies block without owner qualification; puppeteer & playwright are banned", () => {
  const banned = catalog.capabilities.flatMap((entry) => entry.fingerprints).filter((f) => f.policy === "banned");
  for (const name of ["puppeteer", "puppeteer-core", "playwright", "playwright-core"]) assert.ok(banned.some((f) => f.name === name && f.tier === "block" && f.policyAuthority), name);
});
