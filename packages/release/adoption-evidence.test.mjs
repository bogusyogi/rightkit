import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import test, { after } from "node:test";
import { fileURLToPath } from "node:url";
import {
  APP_PREFIX, cargoTreeFacts, checkAdoption, deriveReadiness, extractPacketTaskIds, loadManifest, main, parseCargoLock, parseJsonStrict,
  parsePackageResolved, parsePnpmLock, readInside, validateManifest,
} from "./adoption-evidence.mjs";

const FIXTURES = new URL("./fixtures/adoption-evidence/", import.meta.url);
const baseManifest = () => JSON.parse(readFileSync(new URL("worked-cases.manifest.json", FIXTURES), "utf8"));
const sha = (value) => createHash("sha256").update(value).digest("hex");
const roots = [];
const tmp = (prefix = "rk-adoption-") => { const dir = mkdtempSync(path.join(tmpdir(), prefix)); roots.push(dir); return dir; };
after(() => { for (const dir of roots) rmSync(dir, { recursive: true, force: true }); });
const put = (root, rel, content) => { mkdirSync(path.dirname(path.join(root, rel)), { recursive: true }); writeFileSync(path.join(root, rel), content); };
const rules = (findings) => [...new Set(findings.map((f) => f.rule))].sort();
const taskIds = (manifest) => Object.values(manifest.apps).flatMap((app) => Object.keys(app.tasks)).sort();
const packets = (manifest, extra = {}) => ({ ids: taskIds(manifest), duplicates: [], ...extra });
const clean = (manifest = baseManifest()) => validateManifest(manifest, { packetIds: packets(manifest) });

// ---- strict JSON -------------------------------------------------------------------------------------------------------------
test("parseJsonStrict reads normal JSON and refuses duplicate keys, trailing text and bad tokens", () => {
  assert.deepEqual(parseJsonStrict('\uFEFF{ "a": [1, -2.5e3, true, null, "x\\u0041\\n"], "b": {} }'), { a: [1, -2500, true, null, "xA\n"], b: {} });
  assert.deepEqual(Object.keys(parseJsonStrict('{"__proto__": 1, "x": 2}')), ["__proto__", "x"]);
  assert.throws(() => parseJsonStrict('{"AP-HR-03": 1, "AP-HR-03": 2}'), /duplicate key "AP-HR-03"/);
  assert.throws(() => parseJsonStrict('{"a": {"k": 1, "k": 2}}'), /duplicate key "k"/);
  assert.doesNotThrow(() => parseJsonStrict('{"a": {"k": 1}, "b": {"k": 2}}'));
  for (const bad of ['{"a": 1,}', '{"a" 1}', "[1 2]", '{"a": 1} x', "{'a': 1}", '{"a": tru}', ""]) assert.throws(() => parseJsonStrict(bad), /./, bad);
});

test("loadManifest hashes the exact bytes and refuses a manifest with a duplicate AP key", () => {
  const dir = tmp();
  const text = JSON.stringify(baseManifest(), null, 2);
  put(dir, "ok.json", text);
  const loaded = loadManifest(path.join(dir, "ok.json"));
  assert.equal(loaded.sha256, sha(Buffer.from(text)));
  assert.equal(loaded.manifest.schemaVersion, 1);
  put(dir, "dup.json", text.replace('"AP-HR-04": {', '"AP-HR-03": {'));
  assert.throws(() => loadManifest(path.join(dir, "dup.json")), /duplicate key "AP-HR-03"/);
});

// ---- frozen packets ----------------------------------------------------------------------------------------------------------
function writePackets(dir, ids, { duplicate = [] } = {}) {
  const files = { PU: "02-pulse.md", HR: "03-heardright.md", VR: "04-viewright.md", MR: "05-mailright.md", SR: "06-scraperight.md", CR: "07-coderight.md" };
  const lines = Object.fromEntries(Object.values(files).map((name) => [name, [`# ${name}`]]));
  for (const id of [...ids, ...duplicate]) lines[files[id.split("-")[1]]].push(`- [ ] ${id.startsWith("AP-CR") || id.startsWith("AP-SR") ? `**${id} - title**` : id} task text`);
  for (const [name, content] of Object.entries(lines)) put(dir, name, `${content.join("\n")}\n| ${ids[0]} | table rows are never definitions |\n`);
  put(dir, "01-packages.md", "- [ ] AP-ZZ-99 not an app packet\n");
  put(dir, "README.md", "- [ ] AP-ZZ-98 not an app packet\n");
}

test("extractPacketTaskIds reads checkbox definitions of packets 02..07 only, in both bold and plain form, and reports duplicates", () => {
  const dir = tmp();
  const ids = taskIds(baseManifest());
  writePackets(dir, ids, { duplicate: ["AP-HR-03"] });
  const extracted = extractPacketTaskIds(dir);
  assert.deepEqual(extracted.ids, ids);
  assert.deepEqual(extracted.duplicates, ["AP-HR-03"]);
  assert.equal(extracted.files.length, 6);
  assert.ok(!extracted.ids.includes("AP-ZZ-99") && !extracted.ids.includes("AP-ZZ-98"));
});

const REAL = fileURLToPath(new URL("../../docs/plans/2026-10-20-rightkit-implementation/", import.meta.url));
test("the frozen packets define 98 tasks with no duplicate, and the published manifest lists exactly them", { skip: !existsSync(path.join(REAL, "handoff", "published-release-manifest.json")) }, () => {
  const extracted = extractPacketTaskIds(REAL);
  assert.equal(extracted.ids.length, 98);
  assert.deepEqual(extracted.duplicates, []);
  const counts = Object.fromEntries(Object.values(APP_PREFIX).map((prefix) => [prefix, extracted.ids.filter((id) => id.startsWith(`AP-${prefix}-`)).length]));
  assert.deepEqual(counts, { PU: 19, HR: 21, VR: 14, MR: 12, SR: 17, CR: 15 });
  const { manifest } = loadManifest(path.join(REAL, "handoff", "published-release-manifest.json"));
  const findings = validateManifest(manifest, { packetIds: extracted });
  assert.deepEqual(findings.filter((f) => /^(ap-|packet|packets-)/.test(f.rule)), []);
});

// ---- the section 6 worked cases, as JSON fixtures ----------------------------------------------------------------------------
test("the worked-case fixture is a valid manifest", () => {
  assert.deepEqual(clean(), []);
});

test("worked case 1: helper-trust ready while encrypted-lifecycle is still pending", () => {
  const manifest = baseManifest();
  assert.equal(manifest.capabilities["PK03.helper-trust"].status, "satisfied");
  assert.equal(manifest.capabilities["PK03.encrypted-lifecycle"].status, "pending");
  const ready = deriveReadiness(manifest);
  assert.equal(ready["AP-HR-03"].ready, true);
  assert.equal(ready["AP-HR-04"].ready, false);
  assert.match(ready["AP-HR-04"].reasons.join(" "), /PK03\.encrypted-lifecycle is pending/);
  assert.equal(manifest.contracts.PK03.status, "partial", "a rollup, never read by readiness");
  assert.ok(manifest.apps.heardright.readyForAdoption.includes("AP-HR-03"));
  assert.ok(!manifest.apps.heardright.readyForAdoption.includes("AP-HR-04"));
  // A credential user that needs only storage requires protected-store, not helper trust.
  assert.deepEqual(manifest.apps.heardright.tasks["AP-HR-08"].requiresCapabilities, ["PK03.protected-store"]);
});

test("worked case 2: QA/mode ready while fixture-capture stays deferred; a deferred slice is never ready", () => {
  const manifest = baseManifest();
  const ready = deriveReadiness(manifest);
  assert.equal(ready["AP-CR-01"].ready, true);
  assert.equal(ready["AP-CR-02"].ready, false);
  assert.match(ready["AP-CR-02"].reasons.join(" "), /PK14\.fixture-capture is deferred-separate-dispatch/);
  assert.equal(manifest.contracts.PK14.status, "partial");
  const elsewhere = baseManifest();
  elsewhere.capabilities["PK03.encrypted-lifecycle"].status = "deferred-separate-dispatch";
  assert.ok(rules(clean(elsewhere)).includes("capability-invalid"), "deferred-separate-dispatch is allowed only for PK14.fixture-capture");
});

test("worked case 3: device 0.3.1 satisfies PK05.device by a qualify-existing event with no republish", () => {
  const manifest = baseManifest();
  const device = manifest.packages.find((p) => p.id === "cargo:rightkit-device");
  assert.equal(device.event, "qualify-existing");
  assert.equal(device.version, "0.3.1");
  assert.equal(device.publisherTool, null);
  assert.deepEqual(device.packedConsumer, { ok: null, notApplicable: "unchanged published bytes" });
  assert.equal(manifest.capabilities["PK05.device"].status, "satisfied");
  assert.deepEqual(manifest.capabilities["PK05.device"].packageIds, ["cargo:rightkit-device@0.3.1"]);
  assert.equal(deriveReadiness(manifest)["AP-HR-06"].ready, true);
  assert.deepEqual(clean(manifest), []);
  // The event's field rules are enforced: a qualify-existing package may not look like a publish.
  const asPublish = baseManifest();
  asPublish.packages[1].publisherTool = { version: "0.4.1", source: "installed-published", host: "win32" };
  assert.ok(rules(clean(asPublish)).includes("package-invalid"));
  const packed = baseManifest();
  packed.packages[1].packedConsumer = { ok: true, notApplicable: null };
  assert.ok(rules(clean(packed)).includes("package-invalid"));
  const noFetched = baseManifest();
  noFetched.packages[1].fetchedConsumer.ok = false;
  assert.ok(rules(clean(noFetched)).includes("capability-not-satisfied"), "a satisfied slice needs fetchedConsumer.ok");
});

test("worked case 4: an N/A task is never ready, and an unresolved applicability is never ready", () => {
  const manifest = baseManifest();
  const ready = deriveReadiness(manifest);
  assert.equal(ready["AP-HR-05"].ready, false);
  assert.match(ready["AP-HR-05"].reasons.join(" "), /not applicable/);
  assert.equal(ready["AP-HR-08"].ready, false);
  assert.match(ready["AP-HR-08"].reasons.join(" "), /applicability unresolved/);
  assert.ok(!manifest.apps.heardright.readyForAdoption.includes("AP-HR-05"));
  const forced = baseManifest();
  forced.apps.heardright.tasks["AP-HR-05"].ready = true;
  assert.ok(rules(clean(forced)).includes("ready-not-derived"));
  const listed = baseManifest();
  listed.apps.heardright.readyForAdoption.push("AP-HR-05");
  assert.ok(rules(clean(listed)).includes("ready-list-not-derived"));
  const noReason = baseManifest();
  noReason.apps.heardright.tasks["AP-HR-05"].naReason = null;
  assert.ok(rules(clean(noReason)).includes("task-invalid"));
  const resolved = baseManifest();
  resolved.apps.heardright.tasks["AP-HR-08"].applicabilityResolved = true;
  assert.ok(rules(clean(resolved)).includes("ready-not-derived"), "the stored flag must follow the derivation");
  const unresolvedFlag = baseManifest();
  unresolvedFlag.apps.heardright.tasks["AP-HR-03"].applicabilityResolved = false;
  assert.ok(rules(clean(unresolvedFlag)).includes("ready-not-derived"));
  // A preflight task with known applicability and no requirements is ready once the manifest verifies.
  assert.equal(ready["AP-HR-07"].ready, true);
});

test("worked case 5: the manifest AP id set must equal the frozen packet task set exactly", () => {
  const manifest = baseManifest();
  assert.deepEqual(clean(manifest), []);
  const omitted = baseManifest();
  delete omitted.apps.heardright.tasks["AP-HR-08"];
  assert.deepEqual(rules(validateManifest(omitted, { packetIds: packets(baseManifest()) })), ["ap-missing"]);
  const extra = baseManifest();
  extra.apps.coderight.tasks["AP-CR-09"] = structuredClone(extra.apps.coderight.tasks["AP-CR-02"]);
  assert.deepEqual(rules(validateManifest(extra, { packetIds: packets(baseManifest()) })), ["ap-extra"]);
  assert.deepEqual(rules(validateManifest(manifest, { packetIds: packets(manifest, { duplicates: ["AP-HR-03"] }) })), ["packet-duplicate-task"]);
  assert.deepEqual(rules(validateManifest(manifest, {})), ["packets-missing"]);
  assert.deepEqual(rules(validateManifest(manifest, { packetIds: { ids: [], duplicates: [] } })), ["ap-extra", "packets-missing"]);
  const misplaced = baseManifest();
  misplaced.apps.pulse.tasks["AP-HR-99"] = structuredClone(misplaced.apps.pulse.tasks["AP-PU-01"]);
  const extended = { ids: [...taskIds(manifest), "AP-HR-99"].sort(), duplicates: [] };
  assert.ok(rules(validateManifest(misplaced, { packetIds: extended })).includes("task-misplaced"));
  const noApp = baseManifest();
  delete noApp.apps.mailright;
  assert.ok(rules(clean(noApp)).includes("app-missing"));
  const otherApp = baseManifest();
  otherApp.apps.emberright = { tasks: {}, readyForAdoption: [] };
  assert.ok(rules(clean(otherApp)).includes("app-unknown"));
  const prefixMissing = { ids: taskIds(manifest).filter((id) => !id.startsWith("AP-VR-")), duplicates: [] };
  assert.ok(rules(validateManifest(manifest, { packetIds: prefixMissing })).includes("packets-missing"));
});

// ---- derived fields cannot be typed by hand ------------------------------------------------------------------------------------
test("a satisfied capability needs evidence and real, passing, fetched packages; rollups and ready lists are recomputed", () => {
  const noEvidence = baseManifest();
  noEvidence.capabilities["PK05.device"].evidence = [];
  assert.ok(rules(clean(noEvidence)).includes("capability-not-satisfied"));
  const ghost = baseManifest();
  ghost.capabilities["PK05.device"].packageIds = ["cargo:rightkit-device@9.9.9"];
  assert.ok(rules(clean(ghost)).includes("capability-not-satisfied"));
  const failing = baseManifest();
  failing.packages[2].qualification.push({ host: "win32", features: "control", gateEvidence: "evidence/qa-win-control.log", result: "fail" });
  assert.ok(rules(clean(failing)).includes("capability-not-satisfied"));
  const onlyNa = baseManifest();
  onlyNa.packages[1].qualification = [{ host: "win32", features: "default", gateEvidence: "not applicable: reason", result: "not-applicable" }];
  assert.ok(rules(clean(onlyNa)).includes("capability-not-satisfied"), "at least one passing row is required");
  const rollup = baseManifest();
  rollup.contracts.PK03.status = "satisfied";
  assert.deepEqual(rules(clean(rollup)), ["contract-not-derived"]);
  const flipped = baseManifest();
  flipped.capabilities["PK03.encrypted-lifecycle"].status = "satisfied";
  assert.ok(rules(clean(flipped)).includes("capability-not-satisfied"));
  const tooFew = baseManifest();
  delete tooFew.contracts.PK17;
  assert.ok(rules(clean(tooFew)).includes("contract-missing"));
  const orphan = baseManifest();
  orphan.capabilities["PK17.docs"] = { status: "pending", packageIds: [], evidence: [] };
  assert.ok(rules(clean(orphan)).includes("capability-orphan"));
  const dangling = baseManifest();
  dangling.contracts.PK17.capabilities.push("PK17.gate-rows");
  assert.ok(rules(clean(dangling)).includes("capability-missing"));
  const unknownRequirement = baseManifest();
  unknownRequirement.apps.pulse.tasks["AP-PU-01"].requiresCapabilities = ["PK01.not-a-slice"];
  assert.ok(rules(clean(unknownRequirement)).includes("requires-unknown-capability"));
  const dupPackage = baseManifest();
  dupPackage.packages.push(structuredClone(dupPackage.packages[0]));
  assert.ok(rules(clean(dupPackage)).includes("package-duplicate"));
  const badIntegrity = baseManifest();
  badIntegrity.packages[0].integrity = "sha512-not-a-crates-checksum";
  assert.ok(rules(clean(badIntegrity)).includes("package-invalid"));
  const nullReason = baseManifest();
  nullReason.packages[1].sourceCommitNullReason = null;
  assert.ok(rules(clean(nullReason)).includes("package-invalid"), "the reason is required exactly when sourceCommit is null");
  assert.ok(rules(validateManifest({ schemaVersion: 2 }, {})).includes("manifest-invalid"));
  assert.deepEqual(rules(validateManifest(null, {})), ["manifest-invalid"]);
});

test("with a manifest directory, the selected snapshot hash and the evidence snapshots are checked", () => {
  const dir = tmp();
  const manifest = baseManifest();
  const snapshots = { "snapshots/0001-publish-cargo-rightkit-credentials-0.3.1.json": 0, "snapshots/0002-qualify-existing-cargo-rightkit-device-0.3.1.json": 1, "snapshots/0003-publish-cargo-rightkit-qa-0.7.2.json": 2 };
  for (const [file, index] of Object.entries(snapshots)) put(dir, file, JSON.stringify({ schemaVersion: 1, package: manifest.packages[index] }));
  manifest.selectedSnapshot = { path: "snapshots/0003-publish-cargo-rightkit-qa-0.7.2.json", sha256: sha(readFileSync(path.join(dir, "snapshots/0003-publish-cargo-rightkit-qa-0.7.2.json"))) };
  assert.deepEqual(validateManifest(manifest, { packetIds: packets(manifest), baseDir: dir }), []);
  const wrongHash = structuredClone(manifest);
  wrongHash.selectedSnapshot.sha256 = "0".repeat(64);
  assert.deepEqual(rules(validateManifest(wrongHash, { packetIds: packets(wrongHash), baseDir: dir })), ["snapshot-hash-mismatch"]);
  const missing = structuredClone(manifest);
  missing.capabilities["PK05.device"].evidence = ["snapshots/9999-missing.json"];
  assert.deepEqual(rules(validateManifest(missing, { packetIds: packets(missing), baseDir: dir })), ["evidence-unreadable"]);
  const escape = structuredClone(manifest);
  escape.capabilities["PK05.device"].evidence = ["../outside.json"];
  assert.deepEqual(rules(validateManifest(escape, { packetIds: packets(escape), baseDir: dir })), ["evidence-unreadable"]);
  const tampered = structuredClone(manifest);
  const other = structuredClone(manifest.packages[1]);
  other.integrity = "7".repeat(64);
  put(dir, "snapshots/0002-qualify-existing-cargo-rightkit-device-0.3.1.json", JSON.stringify({ package: other }));
  assert.deepEqual(rules(validateManifest(tampered, { packetIds: packets(tampered), baseDir: dir })), ["snapshot-mismatch"]);
});

test("readInside refuses absolute, drive, parent and symlink-style escapes and returns file bytes otherwise", () => {
  const dir = tmp();
  put(dir, "a/b.txt", "hello");
  assert.equal(readInside(dir, "a/b.txt").bytes.toString(), "hello");
  assert.equal(readInside(dir, "a\\b.txt").bytes.toString(), "hello");
  for (const bad of ["../x", "a/../../x", "/etc/passwd", "C:/x", "C:\\x", "", "a", "missing.txt"]) assert.ok(readInside(dir, bad).error, bad);
  assert.ok(readInside(dir, undefined).error);
});

// ---- lockfile and closure parsers --------------------------------------------------------------------------------------------
test("parseCargoLock, parsePnpmLock and parsePackageResolved read pins; unsupported pnpm lockfiles throw", () => {
  const cargo = parseCargoLock('version = 4\n[[package]]\nname = "a"\nversion = "1.0.0"\nsource = "registry+https://github.com/rust-lang/crates.io-index"\nchecksum = "abc"\n[[package]]\nname = "local"\nversion = "0.1.0"\n');
  assert.deepEqual(cargo, [{ name: "a", version: "1.0.0", source: "registry+https://github.com/rust-lang/crates.io-index", checksum: "abc" }, { name: "local", version: "0.1.0", source: null, checksum: null }]);
  const pnpm = parsePnpmLock([
    "lockfileVersion: '9.0'", "", "importers:", "  .:", "    dependencies:", "      '@rightkit/license':", "        specifier: 0.3.0", "        version: 0.3.0", "",
    "packages:", "", "  '@rightkit/license@0.3.0':", "    resolution: {integrity: sha512-AAAA==}", "", "  smol-toml@1.9.1:", "    resolution:", "      integrity: sha512-BBBB==", "",
    "  localpkg@1.0.0:", "    resolution: {directory: ../local, type: directory}", "", "snapshots:", "", "  '@rightkit/license@0.3.0': {}", "",
  ].join("\n"));
  assert.deepEqual(pnpm, [
    { name: "@rightkit/license", version: "0.3.0", integrity: "sha512-AAAA==", registry: true },
    { name: "smol-toml", version: "1.9.1", integrity: "sha512-BBBB==", registry: true },
    { name: "localpkg", version: "1.0.0", integrity: null, registry: false },
  ]);
  assert.throws(() => parsePnpmLock("lockfileVersion: '6.0'\n"), /unsupported pnpm lockfileVersion/);
  assert.throws(() => parsePnpmLock("packages:\n"), /unsupported pnpm lockfileVersion/);
  const swiftV3 = JSON.stringify({ pins: [{ identity: "RightKitSwift", state: { revision: "r".repeat(40), version: "0.9.0" } }], version: 3 });
  assert.deepEqual(parsePackageResolved(swiftV3), [{ identity: "rightkitswift", version: "0.9.0", revision: "r".repeat(40) }]);
  assert.deepEqual(parsePackageResolved(JSON.stringify({ object: { pins: [{ package: "Other", state: { version: "1.0.0", revision: "x" } }] }, version: 1 })), [{ identity: "other", version: "1.0.0", revision: "x" }]);
});

test("cargoTreeFacts reads crate versions and active features from a features tree", () => {
  const facts = cargoTreeFacts([
    "app v0.1.0 (/work/app)",
    "└── rightkit-daemon v0.3.1 (/work/rightkit/crates/rightkit-daemon)",
    '    ├── rightkit-daemon feature "default"',
    '    │   └── rightkit-daemon feature "local"',
    '    └── rightkit-daemon feature "remote"',
    '        └── other v1.2.3 feature "std"',
  ].join("\n"));
  assert.deepEqual([...facts.versions.get("rightkit-daemon")], ["0.3.1"]);
  assert.deepEqual([...facts.features.get("rightkit-daemon")].sort(), ["default", "local", "remote"]);
  assert.deepEqual([...facts.features.get("other")], ["std"]);
  assert.deepEqual([...facts.versions.get("other")], ["1.2.3"]);
});

// ---- adoption ----------------------------------------------------------------------------------------------------------------
const H1 = "1".repeat(64);
const MANIFEST_SHA = "a".repeat(64);
const ENTRY = "src-tauri/src/vault.rs#open_vault";
const RECEIPT = "evidence/AP-HR-03-helper-trust.json";
const TREE = "evidence/cargo-tree-win32-x64.txt";

const lockText = ({ version = "0.3.1", checksum = H1, source = "registry+https://github.com/rust-lang/crates.io-index", extra = "" } = {}) => `version = 4\n\n[[package]]\nname = "rightkit-credentials"\nversion = "${version}"\n${source ? `source = "${source}"\n` : ""}checksum = "${checksum}"\n${extra}`;
const treeText = ({ version = "0.3.1", features = ["default", "helper-trust"] } = {}) => `${[
  "app v0.1.0 (/work/app)",
  `└── rightkit-credentials v${version} (/work/rightkit/crates/rightkit-credentials)`,
  ...features.map((f, i) => `    ${i === features.length - 1 ? "└" : "├"}── rightkit-credentials feature "${f}"`),
].join("\n")}\n`;
const receiptFor = (overrides = {}) => ({
  schema: 1, kind: "test", app: "heardright", capability: "PK03.helper-trust", entrypoint: ENTRY, packages: ["cargo:rightkit-credentials@0.3.1"], target: "win32-x64",
  command: ["right-release", "test", "--suite", "vault"], exitCode: 0, finishedAt: "2026-10-20T01:00:00.000Z", lockfileSha256: { cargo: sha(lockText()) }, ...overrides,
});
const baseAdoption = () => ({
  schema: 1, app: "heardright", manifestSha256: MANIFEST_SHA, target: "win32-x64", cargoTree: { file: TREE, sha256: null },
  tasks: { "AP-HR-03": { adopted: [{ capability: "PK03.helper-trust", packages: [{ id: "cargo:rightkit-credentials", features: ["helper-trust"] }], entrypoints: [ENTRY], receipt: { file: RECEIPT, sha256: null } }] } },
});

/** Build an app tree for the AP-HR-03 / PK03.helper-trust adoption; `mutate` edits the pieces before they are written and hashed. */
function scenario(mutate = () => {}) {
  const ctx = { lock: lockText(), tree: treeText(), source: "pub fn open_vault() {}\n", receipt: undefined, receiptText: undefined, adoption: baseAdoption(), manifest: baseManifest(), tasks: ["AP-HR-03"] };
  mutate(ctx);
  const root = tmp();
  if (ctx.lock !== null) put(root, "src-tauri/Cargo.lock", ctx.lock);
  if (ctx.tree !== null) put(root, TREE, ctx.tree);
  if (ctx.source !== null) put(root, "src-tauri/src/vault.rs", ctx.source);
  const receiptText = ctx.receiptText ?? JSON.stringify(ctx.receipt ?? receiptFor({ lockfileSha256: { cargo: sha(ctx.lock ?? "") } }));
  put(root, RECEIPT, receiptText);
  const entry = ctx.adoption.tasks["AP-HR-03"]?.adopted?.[0];
  if (ctx.adoption.cargoTree && ctx.adoption.cargoTree.sha256 === null) ctx.adoption.cargoTree.sha256 = sha(ctx.tree ?? "");
  if (entry?.receipt && entry.receipt.sha256 === null) entry.receipt.sha256 = sha(receiptText);
  return { root, ctx, run: (extra = {}) => checkAdoption({ manifest: ctx.manifest, manifestSha256: MANIFEST_SHA, appRoot: root, adoption: ctx.adoption, ...extra }) };
}

test("an exact pinned, qualified, exercised adoption passes with no findings", () => {
  const { run } = scenario();
  const result = run();
  assert.deepEqual(result.findings, []);
  assert.equal(result.tasks["AP-HR-03"].ready, true);
  assert.deepEqual(result.tasks["AP-HR-03"].adopted, [{ capability: "PK03.helper-trust", packages: ["cargo:rightkit-credentials@0.3.1"], receipt: RECEIPT }]);
});

test("the active target comes from --target when given, and must be published and qualified for that host", () => {
  const mac = scenario((c) => { c.receipt = receiptFor({ target: "darwin-arm64", lockfileSha256: { cargo: sha(c.lock) } }); });
  assert.deepEqual(mac.run({ target: "darwin-arm64" }).findings, []);
  const unpublished = scenario((c) => { c.adoption.target = "win32-arm64"; c.receipt = receiptFor({ target: "win32-arm64", lockfileSha256: { cargo: sha(c.lock) } }); });
  assert.deepEqual(rules(unpublished.run().findings), ["target-not-published"]);
  const noRow = scenario((c) => {
    c.manifest.packages[0].qualification = c.manifest.packages[0].qualification.filter((row) => row.host !== "win32");
    c.manifest.packages[0].qualification.push({ host: "win32", features: "helper-trust", gateEvidence: "evidence/x.log", result: "fail" });
  });
  assert.ok(rules(noRow.run().findings).includes("target-not-qualified"));
  const missingTarget = scenario((c) => { delete c.adoption.target; });
  assert.deepEqual(rules(missingTarget.run().findings), ["adoption-invalid"]);
});

test("lockfile pins: exact version, exact integrity, registry source, every entry of the name", () => {
  assert.deepEqual(rules(scenario((c) => { c.lock = lockText({ version: "0.3.0" }); }).run().findings), ["lock-version-mismatch"]);
  assert.deepEqual(rules(scenario((c) => { c.lock = lockText({ checksum: "2".repeat(64) }); }).run().findings), ["lock-integrity-mismatch"]);
  assert.deepEqual(rules(scenario((c) => { c.lock = lockText({ source: null }); }).run().findings), ["lock-not-registry"]);
  assert.deepEqual(rules(scenario((c) => { c.lock = lockText({ source: "git+https://example.invalid/rightkit?rev=abc#abc" }); }).run().findings), ["lock-not-registry"]);
  const second = '\n[[package]]\nname = "rightkit-credentials"\nversion = "0.3.0"\nsource = "registry+https://github.com/rust-lang/crates.io-index"\nchecksum = "3333333333333333333333333333333333333333333333333333333333333333"\n';
  assert.deepEqual(rules(scenario((c) => { c.lock = lockText({ extra: second }); }).run().findings), ["lock-version-mismatch"]);
  assert.deepEqual(rules(scenario((c) => { c.lock = 'version = 4\n[[package]]\nname = "other"\nversion = "1.0.0"\n'; }).run().findings), ["lock-missing"]);
  assert.deepEqual(rules(scenario((c) => { c.lock = null; }).run().findings), ["lockfile-missing"]);
  const declared = scenario((c) => { c.adoption.locks = { cargo: "locks/Cargo.lock" }; c.lock = null; });
  assert.deepEqual(rules(declared.run().findings), ["lockfile-missing"]);
  put(declared.root, "locks/Cargo.lock", lockText());
  assert.ok(!rules(declared.run().findings).includes("lockfile-missing"));
});

test("cargo tree evidence: hashed, version-matched, features declared, active, published and equal to a qualified row", () => {
  assert.deepEqual(rules(scenario((c) => { c.adoption.cargoTree = undefined; }).run().findings), ["closure-evidence-missing"]);
  assert.deepEqual(rules(scenario((c) => { c.tree = null; }).run().findings), ["closure-evidence-unreadable"]);
  assert.deepEqual(rules(scenario((c) => { c.adoption.cargoTree.sha256 = "0".repeat(64); }).run().findings), ["closure-evidence-hash-mismatch"]);
  assert.deepEqual(rules(scenario((c) => { c.tree = treeText({ version: "0.3.0" }); }).run().findings), ["closure-version-mismatch"]);
  assert.deepEqual(rules(scenario((c) => { c.tree = "app v0.1.0 (/work/app)\n"; }).run().findings), ["closure-missing-crate"]);
  // helper-trust declared but not active in the closure.
  const inactive = rules(scenario((c) => { c.tree = treeText({ features: ["default"] }); }).run().findings);
  assert.deepEqual(inactive, ["feature-not-active", "feature-required-missing"]);
  // the capability requires helper-trust; declaring nothing is not enough even if the tree shows it.
  assert.deepEqual(rules(scenario((c) => { c.adoption.tasks["AP-HR-03"].adopted[0].packages = ["cargo:rightkit-credentials"]; }).run().findings), ["feature-required-missing"]);
  // a declared set that is published but matches no qualified row.
  const unqualified = rules(scenario((c) => {
    c.adoption.tasks["AP-HR-03"].adopted[0].packages = [{ id: "cargo:rightkit-credentials", features: ["helper-trust", "isolated-test-keychain"] }];
    c.tree = treeText({ features: ["default", "helper-trust", "isolated-test-keychain"] });
  }).run().findings);
  assert.deepEqual(unqualified, ["feature-set-unqualified"]);
  // an active feature that is not in the published list.
  assert.deepEqual(rules(scenario((c) => { c.tree = treeText({ features: ["default", "helper-trust", "undocumented"] }); }).run().findings), ["feature-unpublished"]);
  assert.deepEqual(rules(scenario((c) => { c.adoption.tasks["AP-HR-03"].adopted[0].packages[0].features = ["helper-trust", "bogus"]; }).run().findings).sort(), ["feature-not-active", "feature-set-unqualified", "feature-unpublished"]);
});

test("an exercised-entrypoint receipt is required per adopted capability and must be machine-readable, bound and fresh", () => {
  assert.deepEqual(rules(scenario((c) => { delete c.adoption.tasks["AP-HR-03"].adopted[0].receipt; }).run().findings), ["receipt-missing"]);
  assert.deepEqual(rules(scenario((c) => { c.adoption.tasks["AP-HR-03"].adopted[0].receipt = { file: RECEIPT, sha256: "not-hex" }; }).run().findings), ["receipt-missing"]);
  assert.deepEqual(rules(scenario((c) => { c.adoption.tasks["AP-HR-03"].adopted[0].receipt = { file: "evidence/none.json", sha256: "0".repeat(64) }; }).run().findings), ["receipt-unreadable"]);
  assert.deepEqual(rules(scenario((c) => { c.adoption.tasks["AP-HR-03"].adopted[0].receipt.sha256 = "0".repeat(64); }).run().findings), ["receipt-hash-mismatch"]);
  // Prose is never a receipt, whatever it says.
  assert.deepEqual(rules(scenario((c) => { c.receiptText = "All tests passed on 2026-10-20; vault entrypoint exercised.\n"; }).run().findings), ["receipt-invalid"]);
  const bad = (overrides) => rules(scenario((c) => { c.receipt = receiptFor({ lockfileSha256: { cargo: sha(c.lock) }, ...overrides }); }).run().findings);
  assert.deepEqual(bad({ exitCode: 1 }), ["receipt-invalid"]);
  assert.deepEqual(bad({ exitCode: "0" }), ["receipt-invalid"]);
  assert.deepEqual(bad({ kind: "bench" }), ["receipt-invalid"]);
  assert.deepEqual(bad({ capability: "PK03.protected-store" }), ["receipt-invalid"]);
  assert.deepEqual(bad({ app: "coderight" }), ["receipt-invalid"]);
  assert.deepEqual(bad({ target: "darwin-arm64" }), ["receipt-invalid"]);
  assert.deepEqual(bad({ entrypoint: "src-tauri/src/other.rs#x" }), ["receipt-invalid"]);
  assert.deepEqual(bad({ packages: ["cargo:rightkit-credentials@0.3.0"] }), ["receipt-invalid"]);
  assert.deepEqual(bad({ command: [] }), ["receipt-invalid"]);
  assert.deepEqual(bad({ finishedAt: "yesterday" }), ["receipt-invalid"]);
  assert.deepEqual(bad({ schema: 2 }), ["receipt-invalid"]);
  assert.deepEqual(bad({ lockfileSha256: { cargo: "0".repeat(64) } }), ["receipt-invalid"], "a receipt that predates the current lock is refused");
  assert.deepEqual(bad({ lockfileSha256: {} }), ["receipt-invalid"]);
});

test("entrypoints must be real files inside the app, and a named symbol must appear in the file", () => {
  assert.deepEqual(rules(scenario((c) => { c.source = null; }).run().findings), ["entrypoint-not-a-file"]);
  assert.deepEqual(rules(scenario((c) => { c.source = "pub fn close_vault() {}\n"; }).run().findings), ["entrypoint-symbol-missing"]);
  assert.deepEqual(rules(scenario((c) => { c.source = "pub fn open_vault_twice() {}\n"; }).run().findings), ["entrypoint-symbol-missing"], "a symbol matches as a whole word");
  const ofFile = (entrypoint) => scenario((c) => { c.adoption.tasks["AP-HR-03"].adopted[0].entrypoints = [entrypoint]; c.receipt = receiptFor({ entrypoint, lockfileSha256: { cargo: sha(c.lock) } }); });
  assert.deepEqual(ofFile("open_vault").run().findings.map((f) => f.rule), ["entrypoint-not-a-file"]);
  assert.deepEqual(ofFile("../outside.rs").run().findings.map((f) => f.rule), ["entrypoint-not-a-file"]);
  assert.deepEqual(ofFile("src-tauri/src/vault.rs::open_vault").run().findings, []);
  assert.deepEqual(ofFile("src-tauri/src/vault.rs").run().findings, []);
  assert.deepEqual(rules(scenario((c) => { c.adoption.tasks["AP-HR-03"].adopted[0].entrypoints = []; }).run().findings), ["entrypoint-missing", "receipt-invalid"]);
});

test("a task that is not ready, is N/A, or is unknown is never adopted; every required capability must be adopted", () => {
  const notReady = scenario((c) => {
    c.adoption.tasks = { "AP-HR-04": { adopted: [{ capability: "PK03.encrypted-lifecycle", packages: ["cargo:rightkit-credentials"], entrypoints: [ENTRY], receipt: { file: RECEIPT, sha256: null } }] } };
  });
  const result = notReady.run();
  assert.ok(rules(result.findings).includes("task-not-ready"));
  assert.ok(rules(result.findings).includes("capability-not-satisfied"));
  assert.equal(result.tasks["AP-HR-04"].ready, false);
  const na = scenario((c) => { c.adoption.tasks = { "AP-HR-05": { adopted: [] } }; });
  assert.deepEqual(rules(na.run().findings), ["task-not-applicable"]);
  assert.equal(na.run().tasks["AP-HR-05"].ready, false);
  const unresolved = scenario((c) => { c.adoption.tasks = { "AP-HR-08": { adopted: [] } }; });
  assert.deepEqual(rules(unresolved.run().findings), ["required-capability-not-adopted", "task-not-ready"]);
  assert.deepEqual(rules(scenario((c) => { c.adoption.tasks = { "AP-HR-99": { adopted: [] } }; }).run().findings), ["task-unknown"]);
  // AP-HR-06 requires PK05.device; adopting a different capability does not stand in for it.
  const wrongCapability = scenario((c) => { c.adoption.tasks = { "AP-HR-06": c.adoption.tasks["AP-HR-03"] }; });
  assert.ok(rules(wrongCapability.run().findings).includes("required-capability-not-adopted"));
  const wrongApp = scenario((c) => { c.adoption.app = "coderight"; });
  assert.ok(rules(wrongApp.run().findings).includes("task-wrong-app"));
  const stale = scenario((c) => { c.adoption.manifestSha256 = "b".repeat(64); });
  assert.deepEqual(rules(stale.run().findings), ["adoption-stale"]);
  assert.deepEqual(rules(scenario((c) => { c.adoption.tasks["AP-HR-03"].adopted[0].capability = "PK99.nothing"; }).run().findings).sort(), ["capability-unknown", "required-capability-not-adopted"]);
  assert.deepEqual(rules(scenario((c) => { c.adoption.tasks["AP-HR-03"].adopted[0].packages = ["cargo:rightkit-device"]; }).run().findings), ["package-not-in-capability"]);
  assert.deepEqual(rules(scenario((c) => { c.adoption.tasks["AP-HR-03"].adopted[0].packages = []; }).run().findings), ["adoption-invalid"]);
  assert.deepEqual(rules(checkAdoption({ manifest: baseManifest(), manifestSha256: MANIFEST_SHA, appRoot: tmp(), adoption: { schema: 2 } }).findings), ["adoption-invalid"]);
});

test("npm packages are pinned through pnpm-lock.yaml integrity and swift packages through Package.resolved", () => {
  const SRI = `sha512-${"A".repeat(86)}==`;
  const manifest = baseManifest();
  manifest.packages.push({
    ...structuredClone(manifest.packages[0]), id: "npm:@rightkit/license", ecosystem: "npm", name: "@rightkit/license", version: "0.3.0", integrity: SRI,
    features: [], qualification: [
      { host: "win32", features: "default", gateEvidence: "evidence/license-win.log", result: "pass" },
      { host: "darwin", features: "default", gateEvidence: "evidence/license-mac.log", result: "pass" },
    ],
  });
  manifest.packages.push({
    ...structuredClone(manifest.packages[0]), id: "swift:RightKitSwift", ecosystem: "swift", name: "RightKitSwift", version: "0.9.0", integrity: "a".repeat(40),
    features: [], targets: ["darwin-arm64"], qualification: [{ host: "darwin", features: "default", gateEvidence: "evidence/swift-mac.log", result: "pass" }],
  });
  manifest.capabilities["PK05.license-client"] = { status: "satisfied", packageIds: ["npm:@rightkit/license@0.3.0"], evidence: ["snapshots/x.json"] };
  manifest.capabilities["PK16.swift-release"] = { status: "satisfied", packageIds: ["swift:RightKitSwift@0.9.0"], evidence: ["snapshots/y.json"] };
  manifest.contracts.PK05.capabilities.push("PK05.license-client");
  manifest.apps.heardright.tasks["AP-HR-09"] = { requiresCapabilities: ["PK05.license-client", "PK16.swift-release"], ready: true, applicable: true, applicabilityResolved: true, naReason: null, evidence: [] };
  const receipt = (capability, pkg, kind, lockHash) => ({ schema: 1, kind: "run", app: "heardright", capability, entrypoint: "src/app.ts", packages: [pkg], target: "darwin-arm64", command: ["pnpm", "run", "smoke"], exitCode: 0, finishedAt: "2026-10-20T02:00:00.000Z", lockfileSha256: { [kind]: lockHash } });
  const pnpm = (version, integrity) => `lockfileVersion: '9.0'\n\npackages:\n\n  '@rightkit/license@${version}':\n    resolution: {integrity: ${integrity}}\n\nsnapshots:\n\n  '@rightkit/license@${version}': {}\n`;
  const resolved = (version, revision) => JSON.stringify({ pins: [{ identity: "rightkitswift", location: "https://example.invalid/rightkit", state: { revision, version } }], version: 3 });
  const build = ({ pnpmText = pnpm("0.3.0", SRI), resolvedText = resolved("0.9.0", "a".repeat(40)) } = {}) => {
    const root = tmp();
    put(root, "pnpm-lock.yaml", pnpmText);
    put(root, "Package.resolved", resolvedText);
    put(root, "src/app.ts", "export const app = 1;\n");
    const r1 = JSON.stringify(receipt("PK05.license-client", "npm:@rightkit/license@0.3.0", "pnpm", sha(pnpmText)));
    const r2 = JSON.stringify(receipt("PK16.swift-release", "swift:RightKitSwift@0.9.0", "swift", sha(resolvedText)));
    put(root, "evidence/r1.json", r1);
    put(root, "evidence/r2.json", r2);
    const adoption = {
      schema: 1, app: "heardright", manifestSha256: MANIFEST_SHA, target: "darwin-arm64",
      tasks: { "AP-HR-09": { adopted: [
        { capability: "PK05.license-client", packages: ["npm:@rightkit/license"], entrypoints: ["src/app.ts"], receipt: { file: "evidence/r1.json", sha256: sha(r1) } },
        { capability: "PK16.swift-release", packages: ["swift:RightKitSwift"], entrypoints: ["src/app.ts"], receipt: { file: "evidence/r2.json", sha256: sha(r2) } },
      ] } },
    };
    return checkAdoption({ manifest, manifestSha256: MANIFEST_SHA, appRoot: root, adoption });
  };
  assert.deepEqual(build().findings, []);
  assert.deepEqual(rules(build({ pnpmText: pnpm("0.2.9", SRI) }).findings), ["lock-version-mismatch"]);
  assert.deepEqual(rules(build({ pnpmText: pnpm("0.3.0", `sha512-${"B".repeat(86)}==`) }).findings), ["lock-integrity-mismatch"]);
  assert.deepEqual(rules(build({ pnpmText: "lockfileVersion: '6.0'\n" }).findings), ["lockfile-unsupported"]);
  assert.deepEqual(rules(build({ resolvedText: resolved("0.9.0", "b".repeat(40)) }).findings), ["lock-integrity-mismatch"]);
  assert.deepEqual(rules(build({ resolvedText: resolved("0.8.0", "a".repeat(40)) }).findings), ["lock-version-mismatch"]);
});

// ---- the command -------------------------------------------------------------------------------------------------------------
function handoff(manifest) {
  const dir = tmp();
  const snapshotDir = path.join(dir, "handoff");
  const files = ["snapshots/0001-publish-cargo-rightkit-credentials-0.3.1.json", "snapshots/0002-qualify-existing-cargo-rightkit-device-0.3.1.json", "snapshots/0003-publish-cargo-rightkit-qa-0.7.2.json"];
  files.forEach((file, index) => put(snapshotDir, file, JSON.stringify({ package: manifest.packages[index] })));
  manifest.selectedSnapshot = { path: files[2], sha256: sha(readFileSync(path.join(snapshotDir, files[2]))) };
  const text = `${JSON.stringify(manifest, null, 2)}\n`;
  put(snapshotDir, "published-release-manifest.json", text);
  writePackets(dir, taskIds(manifest));
  return { dir, manifestPath: path.join(snapshotDir, "published-release-manifest.json"), manifestSha256: sha(Buffer.from(text)) };
}
const run = async (args) => { let out = ""; let err = ""; const code = await main(args, { stdout: { write: (s) => { out += s; } }, stderr: { write: (s) => { err += s; } } }); return { code, out, err }; };

test("command, manifest only: exit 0 for a valid manifest, 1 for any finding, 2 for a usage error", async () => {
  const { dir, manifestPath } = handoff(baseManifest());
  const good = await run(["--manifest", manifestPath]);
  assert.equal(good.code, 0, good.out);
  assert.match(good.out, /ok: no findings/);
  assert.deepEqual(JSON.parse((await run(["--manifest", manifestPath, "--json"])).out).findings, []);
  assert.equal((await run([])).code, 2);
  assert.equal((await run(["--manifest", path.join(dir, "nope.json")])).code, 2);
  writePackets(dir, taskIds(baseManifest()).filter((id) => id !== "AP-CR-02"));
  const missing = await run(["--manifest", manifestPath]);
  assert.equal(missing.code, 1);
  assert.match(missing.out, /ap-extra/);
  const elsewhere = await run(["--manifest", manifestPath, "--packets", path.join(dir, "no-such-dir")]);
  assert.equal(elsewhere.code, 1);
  assert.match(elsewhere.out, /packets-missing/);
});

test("command with --app: reads the adoption record, binds it to the manifest bytes and exits 1 on a finding", async () => {
  const handed = handoff(baseManifest());
  const { root, ctx } = scenario((c) => { c.adoption.manifestSha256 = handed.manifestSha256; });
  put(root, "rightkit-adoption.json", JSON.stringify(ctx.adoption));
  const ok = await run(["--manifest", handed.manifestPath, "--app", root]);
  assert.equal(ok.code, 0, ok.out);
  assert.match(ok.out, /AP-HR-03: ok \(PK03\.helper-trust\)/);
  const stale = JSON.parse(JSON.stringify(ctx.adoption));
  stale.manifestSha256 = "c".repeat(64);
  put(root, "rightkit-adoption.json", JSON.stringify(stale));
  const refused = await run(["--manifest", handed.manifestPath, "--app", root, "--json"]);
  assert.equal(refused.code, 1);
  assert.deepEqual(rules(JSON.parse(refused.out).findings), ["adoption-stale"]);
  put(root, "rightkit-adoption.json", "{ not json");
  assert.equal((await run(["--manifest", handed.manifestPath, "--app", root])).code, 2);
  assert.equal((await run(["--manifest", handed.manifestPath, "--app", path.join(root, "missing")])).code, 2);
  // A manifest that fails its own checks is not a basis for adoption: its findings are reported and the app is not judged.
  const broken = baseManifest();
  broken.apps.heardright.tasks["AP-HR-05"].ready = true;
  const bad = handoff(broken);
  put(root, "rightkit-adoption.json", JSON.stringify(ctx.adoption));
  const blocked = await run(["--manifest", bad.manifestPath, "--app", root, "--json"]);
  assert.equal(blocked.code, 1);
  assert.deepEqual(rules(JSON.parse(blocked.out).findings), ["ready-not-derived"]);
});
