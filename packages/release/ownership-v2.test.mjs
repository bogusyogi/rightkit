import assert from "node:assert/strict";
import { generateKeyPairSync, sign } from "node:crypto";
import { cpSync, mkdirSync, mkdtempSync, readdirSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { spawnSync } from "./background-process.mjs";
import { canonicalJson, createDraft, keyIdForPublicKey, occurrenceHash, policyEntryDigest, sha256 } from "./ownership-approval.mjs";
import { directDependencies, evaluate, scanApp, SCANNER_VERSION } from "./ownership.mjs";
import { main as approvalCommand, OWNERSHIP_APPROVAL_RETIRED } from "./ownership-approval-command.mjs";
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
test("plain exception entry covers every finding of its capability and package; other capabilities stay violations (#262)", () => {
  const fingerprint = manifest.capabilities[0].fingerprints[0].id;
  const finding = { fingerprint, capability: "browser", package: "rightkit-browser", kind: "source", file: "src/a.rs", lineText: "SendInput();", ordinal: 0 };
  const other = { ...finding, fingerprint: "other.fp", capability: "input", package: "rightkit-qa-driver", file: "src/c.rs" };
  const findings = [finding, { ...finding, ordinal: 1 }, { ...finding, file: "src/b.rs" }, other];
  const entry = { capability: "browser", package: "rightkit-browser", reason: "migration", owner: "Example", date: "2026-10-17", index: 0 };
  const result = evaluate(findings, [entry], {});
  assert.deepEqual(result.problems, []); assert.deepEqual(result.warnings.filter((w) => /stale/.test(w)), []);
  assert.equal(result.covered.length, 3); assert.equal(result.violations.length, 1); assert.equal(result.violations[0].file, "src/c.rs");
  const stale = evaluate([other], [entry], {});
  assert.equal(stale.covered.length, 0); assert.equal(stale.violations.length, 1); assert.equal(stale.warnings.filter((w) => /stale/.test(w)).length, 1);
});
test("migration preparation emits a v2 set without signing", () => {
  const dir = mkdtempSync(path.join(os.tmpdir(), "rk-migration-v2-"));
  try { const result = prepareMigrationDrafts({ apps: [], outputDir: dir, now: new Date("2026-10-08T00:00:00Z") }); assert.equal(result.schema, 2); assert.equal(result.draftCount, 0); } finally { remove(dir); }
});
test("ownership-approval is a retired no-op: it says so, exits 0 and touches nothing", () => {
  const lines = [];
  for (const args of [[], ["--draft", "input.json"], ["--draft", "input.json", "--out", "draft.json"], ["--verify", "signed.json", "--context", "scan.json"], ["--import", "signed.json", "--out", "record.json"]]) {
    assert.equal(approvalCommand(args, { print: (line) => lines.push(line) }), 0);
  }
  assert.equal(lines.length, 5);
  for (const line of lines) { assert.ok(line.includes(OWNERSHIP_APPROVAL_RETIRED)); assert.match(line, /retired/); assert.match(line, /ownership-exceptions\.json/); }
});

test("findings carry their fingerprint tier and banned policy; a missing tier defaults to block", async () => {
  const { isBlockingFinding } = await import("./ownership-verdict.mjs");
  const dir = root({ "package.json": JSON.stringify({ dependencies: { "puppeteer": "24", "axios": "1", "left-pad": "1" } }) });
  try {
    const catalog = { schema: 1, version: "3", capabilities: [{ capability: "browser-automation", package: "rightkit-browser", fingerprints: [
      { id: "browser.npm.puppeteer", type: "dependency", ecosystem: "npm", name: "puppeteer", tier: "advise", policy: "banned" },
      { id: "http.npm.axios", type: "dependency", ecosystem: "npm", name: "axios", tier: "advise" },
      { id: "x.npm.left-pad", type: "dependency", ecosystem: "npm", name: "left-pad" },
    ] }] };
    const byId = Object.fromEntries(scanApp(dir, catalog, { trust: null }).map((f) => [f.fingerprint, f]));
    assert.equal(byId["browser.npm.puppeteer"].policy, "banned");
    assert.equal(isBlockingFinding(byId["browser.npm.puppeteer"]), true);
    assert.equal(byId["http.npm.axios"].tier, "advise");
    assert.equal(isBlockingFinding(byId["http.npm.axios"]), false);
    assert.equal(byId["x.npm.left-pad"].tier, "block");
    assert.equal(isBlockingFinding(byId["x.npm.left-pad"]), true);
  } finally { remove(dir); }
});
