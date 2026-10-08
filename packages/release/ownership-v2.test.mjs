import assert from "node:assert/strict";
import { generateKeyPairSync, sign } from "node:crypto";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { spawnSync } from "./background-process.mjs";
import { canonicalJson, createDraft, keyIdForPublicKey, occurrenceHash, policyEntryDigest, sha256 } from "./ownership-approval.mjs";
import { directDependencies, evaluate, scanApp, SCANNER_VERSION } from "./ownership.mjs";
import { createInstalledDraft, main as draftCommand } from "./ownership-approval-command.mjs";
import { prepareMigrationDrafts } from "../../ownership-approval/migrations/prepare-migration-drafts.mjs";
import "../../ownership-approval/migrations/validate-migration-drafts.mjs";

const manifest = { schema: 1, version: "1", capabilities: [{ capability: "browser", package: "rightkit-browser", tier: "block", fingerprints: [
  { id: "browser.source.sendinput", type: "source", pattern: "SendInput" },
  { id: "browser.cargo.oauth", type: "dependency", ecosystem: "cargo", name: "oauth2" },
  { id: "browser.npm.puppeteer", type: "dependency", ecosystem: "npm", name: "puppeteer" },
  { id: "browser.swift.oauth", type: "dependency", ecosystem: "swiftpm", name: "oauth" },
] }] };
function root(files) {
  const dir = mkdtempSync(path.join(os.tmpdir(), "rk-v2-"));
  assert.equal(spawnSync("git", ["-C", dir, "init", "-q"], { windowsHide: true }).status, 0);
  for (const [file, text] of Object.entries(files)) { mkdirSync(path.dirname(path.join(dir, file)), { recursive: true }); writeFileSync(path.join(dir, file), text); }
  return dir;
}
function remove(dir) { assert.ok(path.resolve(dir).startsWith(path.resolve(os.tmpdir()) + path.sep)); rmSync(dir, { recursive: true, force: true }); }
test("scanner preserves full lines, moving lines, and identical-match ordinals", () => {
  const line = "  SendInput();" + "x".repeat(300), dir = root({ "src/control.rs": `${line}\n${line}\n` });
  try {
    const first = scanApp(dir, manifest, { trust: null });
    assert.deepEqual(first.map((f) => f.ordinal), [0, 1]);
    assert.ok(first.every((f) => f.lineText === line && f.text.length === 160));
    assert.notEqual(first[0].occurrenceHash, first[1].occurrenceHash);
    const codeOnly = structuredClone(manifest); codeOnly.capabilities[0].fingerprints[0].codeOnly = true;
    assert.deepEqual(scanApp(dir, codeOnly, { trust: null }).map((f) => f.ordinal), [0, 1]);
    writeFileSync(path.join(dir, "src/control.rs"), `let unrelated = true;\n${line}\n${line}\n`);
    const moved = scanApp(dir, manifest, { trust: null });
    assert.deepEqual(moved.map(occurrenceHash), first.map(occurrenceHash));
    writeFileSync(path.join(dir, "src/control.rs"), `${line.slice(0, 200)}y${line.slice(201)}\n${line}\n`);
    const edited = scanApp(dir, manifest, { trust: null });
    assert.equal(edited[0].text, first[0].text);
    assert.notEqual(edited[0].occurrenceHash, first[0].occurrenceHash);
  } finally { remove(dir); }
});
test("Cargo, npm aliases, Swift requirements, and adjacent locks reach findings", () => {
  const dir = root({
    "Cargo.toml": '[workspace]\nmembers=["engine"]\n[workspace.dependencies]\nauth={package="oauth2",version="4",features=["inherited"]}\n',
    "engine/Cargo.toml": '[package]\nname="consumer"\nversion="1.0.0"\n[target.\'cfg(windows)\'.dependencies]\nauth={workspace=true,features=["extra","inherited"],default-features=false}\n',
    "engine/Cargo.lock": 'version=3\n[[package]]\nname="oauth2"\nversion="4.4.2"\nsource="registry+https://github.com/rust-lang/crates.io-index"\n',
    "web/package.json": JSON.stringify({ dependencies: { browser: "npm:puppeteer@^24" } }),
    "web/package-lock.json": JSON.stringify({ lockfileVersion: 3, packages: { "node_modules/browser": { name: "puppeteer", version: "24.2.0" } } }),
    "ios/Package.swift": '.package(url: "https://github.com/example/oauth.git", .upToNextMajor(from: "1.0.0"))',
    "ios/Package.resolved": JSON.stringify({ version: 2, pins: [{ identity: "oauth", location: "https://github.com/example/oauth.git", state: { version: "1.2.3" } }] }),
  });
  try {
    const deps = directDependencies(dir), findings = scanApp(dir, manifest, { trust: null });
    assert.deepEqual(deps.coverage, []);
    const cargo = findings.find((f) => f.ecosystem === "cargo"), npm = findings.find((f) => f.ecosystem === "npm"), swift = findings.find((f) => f.ecosystem === "swiftpm");
    assert.equal(cargo.section, "target.'cfg(windows)'.dependencies");
    assert.equal(cargo.alias, "auth"); assert.equal(cargo.requirement, "4"); assert.equal(cargo.defaultFeatures, false);
    assert.deepEqual(cargo.features, ["extra", "inherited"]); assert.equal(cargo.source, "registry"); assert.equal(cargo.resolvedVersion, "4.4.2");
    assert.equal(npm.alias, "browser"); assert.equal(npm.section, "dependencies"); assert.equal(npm.requirement, "npm:puppeteer@^24"); assert.equal(npm.resolvedVersion, "24.2.0");
    assert.equal(swift.requirement, '.upToNextMajor(from: "1.0.0")'); assert.equal(swift.source, "git+https://github.com/example/oauth.git"); assert.equal(swift.resolvedVersion, "1.2.3");
    assert.ok(findings.every((f) => f.kind === "dependency" && f.occurrenceHash === occurrenceHash(f)));
    for (const change of [{ requirement: "5" }, { features: ["inherited"] }, { source: "git+https://github.com/example/oauth#main" }]) assert.notEqual(occurrenceHash(cargo), occurrenceHash({ ...cargo, ...change }));
    writeFileSync(path.join(dir, "web/package-lock.json"), JSON.stringify({ packages: { "node_modules/browser": { version: "24.3.0" } } }));
    assert.notEqual(scanApp(dir, manifest, { trust: null }).find((f) => f.ecosystem === "npm").occurrenceHash, npm.occurrenceHash);
  } finally { remove(dir); }
});
for (const [name, lock] of [
  ["pnpm-lock.yaml", "lockfileVersion: '9.0'\nimporters:\n  .:\n    dependencies:\n      puppeteer:\n        specifier: ^24\n        version: 24.1.0\npackages:\n"],
  ["yarn.lock", '"puppeteer@^24":\n  version "24.1.0"\n'],
]) test(`${name} adjacent version is bound`, () => {
  const dir = root({ "package.json": JSON.stringify({ dependencies: { puppeteer: "^24" } }), [name]: lock });
  try { const deps = directDependencies(dir); assert.deepEqual(deps.coverage, []); assert.equal(deps[0].resolvedVersion, "24.1.0"); } finally { remove(dir); }
});
test("malformed adjacent lock emits specific incomplete coverage", () => {
  const dir = root({ "package.json": JSON.stringify({ dependencies: { puppeteer: "24" } }), "package-lock.json": "{}" });
  try { assert.match(directDependencies(dir).coverage[0].reason, /dependency-lock-resolution:npm lock resolution missing: puppeteer/); } finally { remove(dir); }
});
test("exception matching covers listed pairs and computes installed rule digest", () => {
  const fingerprint = manifest.capabilities[0].fingerprints[0].id, identity = "https://github.com/orthic-labs/example";
  const finding = { fingerprint, capability: "browser", package: "rightkit-browser", kind: "source", file: "src/a.rs", lineText: "SendInput();", ordinal: 0 };
  const findings = [finding, { ...finding, ordinal: 1 }, { ...finding, file: "src/b.rs" }];
  const record = createDraft({ repositoryIdentity: identity, fingerprint, policyDigest: policyEntryDigest(manifest, fingerprint), minimumScannerVersion: SCANNER_VERSION, minimumCatalogVersion: "1", findings: [finding], issued: "2026-10-08", owner: "Example", reason: "migration", requestId: "RK-001", trackingRef: { id: "1", url: `${identity}/issues/1`, verified: true } });
  const pair = generateKeyPairSync("ec", { namedCurve: "prime256v1" }), mac = generateKeyPairSync("ec", { namedCurve: "prime256v1" });
  record.signature = { host: "windows", keyId: keyIdForPublicKey("windows", pair.publicKey), value: sign("sha256", Buffer.from(canonicalJson(record.payload)), pair.privateKey).toString("base64") };
  const trust = { schema: 1, minimumScannerVersion: "2", minimumCatalogVersion: "1", revokedKeyIds: [], provenance: [], repositories: [{ repositoryIdentity: identity, roots: ["."], visibility: "public" }], verifiedTrackingEvidence: [sha256({ repositoryIdentity: identity, requestId: record.payload.requestId, trackingRef: record.payload.trackingRef })], hosts: { windows: { keyId: record.signature.keyId, publicKeyPem: pair.publicKey.export({ type: "spki", format: "pem" }) }, macos: { keyId: keyIdForPublicKey("macos", mac.publicKey), publicKeyPem: mac.publicKey.export({ type: "spki", format: "pem" }) } } };
  const options = { manifest, repositoryIdentity: identity, trust, today: "2026-10-08", scannerVersion: SCANNER_VERSION, catalogVersion: "1" };
  const result = evaluate(findings, [{ ...record, index: 0 }], options);
  assert.deepEqual(result.problems, []); assert.equal(result.covered.length, 1); assert.equal(result.violations.length, 2);
  const edited = structuredClone(manifest); edited.capabilities[0].fingerprints[1].name = "other";
  assert.equal(evaluate(findings, [record], { ...options, manifest: edited }).covered.length, 1);
  edited.capabilities[0].fingerprints[0].pattern = "OtherInput";
  assert.match(evaluate(findings, [record], { ...options, manifest: edited }).problems.join(" "), /policy digest/);
});
test("migration preparation emits a v2 set without signing", () => {
  const dir = mkdtempSync(path.join(os.tmpdir(), "rk-migration-v2-"));
  try { const result = prepareMigrationDrafts({ apps: [], outputDir: dir, now: new Date("2026-10-08T00:00:00Z") }); assert.equal(result.schema, 2); assert.equal(result.draftCount, 0); } finally { remove(dir); }
});
test("installed draft path replaces supplied dates, policy digest, and version floors", () => {
  const fingerprint = manifest.capabilities[0].fingerprints[0].id;
  const input = { repositoryIdentity: "https://github.com/orthic-labs/example", fingerprint, findings: [{ fingerprint, file: "src/a.rs", lineText: "SendInput();", ordinal: 0 }], reason: "migration", owner: "Example", requestId: "RK-001", trackingRef: { id: "1", url: "https://example.com/1", verified: true }, issued: "1900-01-01", policyDigest: "a".repeat(64), minimumScannerVersion: "0", minimumCatalogVersion: "0" };
  const options = { catalog: manifest, scannerVersion: SCANNER_VERSION, now: new Date("2026-10-08T12:00:00Z") };
  const record = createInstalledDraft(input, options);
  assert.equal(record.schema, 2); assert.equal(record.payload.issued, "2026-10-08"); assert.equal(record.payload.expires, "2027-01-06");
  assert.equal(record.payload.minimumScannerVersion, SCANNER_VERSION); assert.equal(record.payload.minimumCatalogVersion, manifest.version);
  assert.equal(record.payload.policyDigest, policyEntryDigest(manifest, fingerprint));
  assert.throws(() => createInstalledDraft({ ...input, expires: "2027-01-07" }, options), /within 90 days/);
  assert.throws(() => createInstalledDraft({ ...input, requestId: undefined }, options), /invalid request id/);
});
test("draft CLI refuses missing installed tier rather than inventing policy", () => {
  const shipped = JSON.parse(readFileSync(new URL("./rightkit-ownership.json", import.meta.url), "utf8"));
  if (shipped.capabilities.every((c) => c.tier)) return;
  const missing = shipped.capabilities.find((c) => !c.tier && c.fingerprints.length), dir = mkdtempSync(path.join(os.tmpdir(), "rk-draft-v2-"));
  try { const file = path.join(dir, "input.json"); writeFileSync(file, JSON.stringify({ fingerprint: missing.fingerprints[0].id })); assert.throws(() => draftCommand(["--draft", file]), /malformed catalog policy entry/); } finally { remove(dir); }
});
