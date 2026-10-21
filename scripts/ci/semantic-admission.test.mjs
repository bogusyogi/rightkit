import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import crypto from "node:crypto";
import { canonicalRunProblems, consumerQualificationProblems, runtimeFeatureProofPolicy, semanticAdmission, namedNpmQualificationProblems, npmPortabilityQualificationProblems, reviewedRecipe } from "./semantic-admission.mjs";
import { derivePublicationObligations } from "./publication-obligations.mjs";
import { RECIPE_FILES as NPM_RECIPE_FILES, SOURCE_FILES as NPM_SOURCE_FILES } from "./npm-consumer.mjs";
import { TEST_COMMAND } from "./test-results.mjs";

const SHA = "a".repeat(40); const TREE = "b".repeat(40); const HEX = "c".repeat(64);
const hash = (data) => crypto.createHash("sha256").update(data).digest("hex");

function cssPortabilityFixture() {
  return {
    packageManifest: { name: "@rightkit/doc-render", main: "dist/index.js", types: "dist/index.d.ts", exports: { ".": { types: "./dist/index.d.ts", import: "./dist/index.js" }, "./styles.css": "./styles.css", "./shiki.css": "./shiki.css" } },
    portability: { entries: 3, imported: ["@rightkit/doc-render"], resolvedOnly: ["@rightkit/doc-render/styles.css", "@rightkit/doc-render/shiki.css"], productionRejected: [], conditionalImports: [], skippedImport: [], patterns: [], bins: [], binSmoke: [], filesChecked: 4, noJsEntry: false },
  };
}

test("named npm portability accepts declared CSS resolutions beside JavaScript import", () => {
  assert.deepEqual(npmPortabilityQualificationProblems(cssPortabilityFixture()), []);
});

test("named npm portability rejects omitted, substituted, duplicated & exceptional proof", () => {
  const mutations = [
    input => { input.portability.imported = []; },
    input => { input.portability.resolvedOnly.push(input.portability.imported.pop()); },
    input => { input.portability.resolvedOnly.pop(); },
    input => { input.portability.resolvedOnly[0] = "@rightkit/doc-render/unknown.css"; },
    input => { input.portability.resolvedOnly.push(input.portability.resolvedOnly[0]); },
    input => { input.portability.imported.push(input.portability.resolvedOnly.shift()); },
    input => { input.portability.imported.push(input.portability.imported[0]); },
    input => { input.portability.entries = 2; },
    input => { input.portability.filesChecked = 3; },
    input => { input.portability.noJsEntry = true; },
    input => { delete input.portability.resolvedOnly; },
    ...["productionRejected", "conditionalImports", "skippedImport", "patterns", "bins", "binSmoke"].map(field => input => { input.portability[field] = ["unexpected"]; }),
    input => { input.packageManifest.exports["./styles.css"] = "./dist/styles.js"; },
    input => { input.packageManifest.exports["./styles.css"] = "./dist/styles.mjs?asset"; },
    input => { input.packageManifest.exports["./styles.css"] = { import: "./styles.css", require: "./styles.cjs" }; },
    input => { input.packageManifest.exports["./styles.css"] = { development: "./styles.css" }; },
    input => { input.packageManifest.exports["./testing/styles.css"] = input.packageManifest.exports["./styles.css"]; delete input.packageManifest.exports["./styles.css"]; input.portability.resolvedOnly[0] = "@rightkit/doc-render/testing/styles.css"; },
  ];
  for (const mutate of mutations) {
    const input = cssPortabilityFixture(); mutate(input);
    assert.deepEqual(npmPortabilityQualificationProblems(input), ["named npm R2a lacks complete unchanged generic portability proof"]);
  }
});

test("named npm portability retains generic entry counting for asset-only & type-only packages", () => {
  const input = cssPortabilityFixture();
  delete input.packageManifest.main; delete input.packageManifest.types; delete input.packageManifest.exports["."];
  input.portability.entries = 2; input.portability.filesChecked = 2; input.portability.imported = [];
  assert.deepEqual(npmPortabilityQualificationProblems(input), []);
  input.packageManifest.exports = { ".": { types: "./dist/index.d.ts" } };
  input.portability.entries = 0; input.portability.filesChecked = 1; input.portability.resolvedOnly = []; input.portability.noJsEntry = true;
  assert.deepEqual(npmPortabilityQualificationProblems(input), []);
});

function binPortabilityFixture() {
  return {
    packageManifest: { name: "@rightkit/qa-chrome", main: "dist/index.js", types: "dist/index.d.ts", exports: { ".": { types: "./dist/index.d.ts", import: "./dist/index.js" } }, bin: { "rightkit-qa-chrome": "dist/cli.js" }, rightkit: { publishCheck: { binSmoke: { "rightkit-qa-chrome": ["list"] } } } },
    portability: { entries: 1, imported: ["@rightkit/qa-chrome"], resolvedOnly: [], productionRejected: [], conditionalImports: [], skippedImport: [], patterns: [], bins: ["dist/cli.js"], binSmoke: [{ bin: "rightkit-qa-chrome", args: ["list"], status: 0 }], filesChecked: 3, noJsEntry: false },
  };
}

test("named npm portability accepts declared Chrome CLI smoke evidence", () => {
  assert.deepEqual(npmPortabilityQualificationProblems(binPortabilityFixture()), []);
  const input = binPortabilityFixture();
  input.packageManifest.bin = "dist/cli.js";
  input.packageManifest.rightkit.publishCheck.binSmoke = { "qa-chrome": ["list"] };
  input.portability.binSmoke[0].bin = "qa-chrome";
  assert.deepEqual(npmPortabilityQualificationProblems(input), []);
});

test("named npm portability binds every bin alias independently in any evidence order", () => {
  const input = binPortabilityFixture();
  input.packageManifest.bin.alias = "dist/cli.js";
  input.packageManifest.rightkit.publishCheck.binSmoke.alias = { args: ["--help"], exitCode: 2 };
  input.portability.bins.push("dist/cli.js");
  input.portability.binSmoke.unshift({ bin: "alias", args: ["--help"], status: 2 });
  assert.deepEqual(npmPortabilityQualificationProblems(input), []);
  input.portability.binSmoke[0] = structuredClone(input.portability.binSmoke[1]);
  assert.ok(npmPortabilityQualificationProblems(input).length, "duplicate bin cannot replace another alias");
});

test("named npm portability rejects incomplete, malformed, substituted or failed bin evidence", () => {
  const mutations = [
    input => { delete input.portability.binSmoke; },
    input => { input.portability.binSmoke = null; },
    input => { input.portability.binSmoke = {}; },
    input => { input.portability.binSmoke = []; },
    input => { input.portability.binSmoke = [null]; },
    input => { input.portability.binSmoke = ["rightkit-qa-chrome"]; },
    input => { input.portability.binSmoke[0].bin = "unknown"; },
    input => { input.portability.binSmoke[0].args = []; },
    input => { input.portability.binSmoke[0].args = "list"; },
    input => { input.portability.binSmoke[0].args = ["list", "--extra"]; },
    input => { delete input.portability.binSmoke[0].status; },
    input => { input.portability.binSmoke[0].status = null; },
    input => { input.portability.binSmoke[0].status = "0"; },
    input => { input.portability.binSmoke[0].status = 1; },
    input => { input.portability.binSmoke[0].extra = true; },
    input => { input.portability.binSmoke.push(structuredClone(input.portability.binSmoke[0])); },
    input => { input.portability.binSmoke.push({ bin: "unknown", args: ["list"], status: 0 }); },
    input => { input.portability.bins = []; },
    input => { input.portability.bins = ["dist/unknown.js"]; },
    input => { input.portability.filesChecked = 2; },
    input => { delete input.packageManifest.rightkit.publishCheck.binSmoke; },
    input => { input.packageManifest.rightkit.publishCheck.binSmoke = []; },
    input => { input.packageManifest.rightkit.publishCheck.binSmoke["rightkit-qa-chrome"] = { args: [1] }; },
    input => { input.packageManifest.rightkit.publishCheck.binSmoke["rightkit-qa-chrome"] = { args: ["list"], exitCode: 256 }; },
    input => { input.packageManifest.rightkit.publishCheck.binSmoke["rightkit-qa-chrome"] = { args: ["list"], output: "" }; },
    input => { input.packageManifest.rightkit.publishCheck.binSmoke.unknown = ["list"]; },
    input => { input.packageManifest.bin["rightkit-qa-chrome"] = "dist/cli.sh"; input.portability.bins = ["dist/cli.sh"]; },
    input => { input.packageManifest.bin.alias = "dist/cli.js"; input.packageManifest.rightkit.publishCheck.binSmoke.alias = ["list"]; input.portability.bins.push("dist/cli.js"); },
  ];
  for (const mutate of mutations) {
    const input = binPortabilityFixture(); mutate(input);
    assert.deepEqual(npmPortabilityQualificationProblems(input), ["named npm R2a lacks complete unchanged generic portability proof"]);
  }
});

test("runtime classification reuses owner & always-on stays capability HOLD", () => {
  for (const input of [
    { crate: "rightkit-custom", meta: { continuous: true } },
    { crate: "rightkit-custom", meta: { continuous: "always-on" } },
    { crate: "rightkit-custom", meta: { runtime_budget: true } },
    { crate: "rightkit-custom", hasFile: true },
    { crate: "rightkit-logs" },
  ]) {
    const result = runtimeFeatureProofPolicy(input);
    assert.equal(result.classification, "always-on");
    assert.equal(result.status, "HOLD");
    assert.deepEqual(result.failures, ["authenticated isolated runtime feature/workload proof missing"]);
  }
});
test("noncontinuous & held-resource declarations retain structural classification", () => {
  for (const [continuous, kind] of [[false, "false"], ["held-resource", "held-resource"]]) {
    // A budget file does not override either explicit declaration in owner policy.
    const result = runtimeFeatureProofPolicy({ crate: "rightkit-custom", meta: { continuous }, hasFile: true });
    assert.equal(result.classification, kind);
    assert.equal(result.status, "not-applicable");
    assert.deepEqual(result.failures, []);
  }
  assert.equal(runtimeFeatureProofPolicy({ crate: "rightkit-custom" }).classification, "undeclared");
});
test("listed owners cannot bypass HOLD with false/held-resource or unchecked proof claims", () => {
  for (const continuous of [false, "held-resource", undefined]) {
    const input = { crate: "rightkit-logs", meta: { continuous }, runtime: true, verified: true, producer: { isolated: true }, callback: () => true };
    const result = runtimeFeatureProofPolicy(input);
    assert.equal(result.listedOwner, true);
    assert.equal(result.status, "HOLD");
    assert.equal(result.required, true);
  }
  const result = runtimeFeatureProofPolicy({ crate: "rightkit-logs", runtimeProof: { candidateId: "claimed", proofSha256: HEX }, candidate: {}, unit: {} });
  assert.equal(result.status, "HOLD", "raw runtime JSON is never an authenticated handle");
});
function canonical() {
  const expected = { kind: "cargo", name: "rightkit-logs", version: "0.3.4", commit: SHA, artifactSha256: HEX };
  const actualResults = [{ command: "cargo test -p rightkit-logs --no-fail-fast", executed: 2, failed: 0 }, { command: "cargo test -p rightkit-logs --features x --no-fail-fast", executed: 3, failed: 0 }];
  const evidence = { schema: 1, ...expected, runs: [{ commit: SHA, artifactSha256: HEX, host: { os: "darwin", arch: "arm64" }, command: actualResults.map((r) => r.command).join(" && "), features: ["default", "--features x"], passed: 5, failed: 0, operator: "actor/job", timestamp: "2026-10-20T00:00:00Z" }] };
  return { expected, actualResults, evidence, host: "macos" };
}
test("canonical native content binds actual commands, counts, features & host", () => {
  assert.deepEqual(canonicalRunProblems(canonical()), []);
  for (const damage of [
    (f) => { f.evidence.commit = TREE; },
    (f) => { f.evidence.runs[0].passed = 6; },
    (f) => { f.evidence.runs[0].failed = 1; },
    (f) => { f.evidence.runs[0].command += " --invented"; },
    (f) => { f.evidence.runs[0].features = ["default"]; },
    (f) => { f.evidence.runs[0].host.os = "win32"; },
    (f) => { delete f.evidence.runs[0].host.arch; },
    (f) => { delete f.evidence.runs[0].operator; },
    (f) => { delete f.evidence.runs[0].timestamp; },
    (f) => { f.evidence.runs.push(f.evidence.runs[0]); },
    (f) => { f.evidence = null; },
  ]) { const f = canonical(); damage(f); assert.ok(canonicalRunProblems(f).length); }
});

function consumer() {
  const expected = { name: "rightkit-logs", version: "0.3.4", commit: SHA, manifestSha256: HEX, archiveSha256: HEX, host: "macos", runId: "77", runAttempt: "1", manifestRunId: "77", manifestRunAttempt: "1" };
  const recipe = { pkId: "PK11", id: "ci/consumers/rightkit-logs/src/main.rs", sourceCommit: SHA, files: { "Cargo.toml": HEX, "src/main.rs": HEX }, sha256: HEX, manifestSha256: HEX, exampleSha256: HEX, dependency: { name: expected.name, version: "=0.3.4", features: [], defaultFeatures: false } };
  const cwd = "/runner/temp/consumer";
  const metadata = { packages: [{ id: "selected", name: expected.name, version: expected.version, source: "registry+https://github.com/rust-lang/crates.io-index" }], resolve: { nodes: [{ id: "selected", features: [] }] } };
  const commands = [["metadata", "--format-version", "1", "--locked", "--offline"], ["build", "--locked", "--offline"], ["run", "--locked", "--offline"]].map((args) => ({ command: "cargo", args, cwd, status: 0, stdout: args[0] === "metadata" ? JSON.stringify(metadata) : args[0] === "run" ? "actual API output\n" : "", stderr: "" }));
  const report = { schema: 1, kind: "registry-consumer", qualification: "R2a", ...expected, arch: "arm64", recipe, verified: true, finalSource: "sealed-directory-registry", finalManifestSha256: HEX, lockSha256: HEX, resolvedFeatures: [], toolchain: { rustc: "rustc 1.90", cargo: "cargo 1.90" }, lockEvidence: [{ name: expected.name, version: expected.version, source: "registry+https://github.com/rust-lang/crates.io-index", checksum: HEX, archiveSha256: HEX }], consumerDirectory: cwd, commands, build: { locked: true, offline: true, status: 0 }, runtime: { locked: true, offline: true, status: 0 }, runtimeOutput: "actual API output\n" };
  return { expected, recipe, report };
}
test("R2a accepts bound named recipe & actual locked/offline command evidence", () => {
  assert.deepEqual(consumerQualificationProblems(consumer()), []);
});
test("R2a accepts the implicit default row of a no-runs recipe & refuses lookalikes", () => {
  const f = consumer();
  f.report.runs = [{ id: "default", args: [], features: [], envNames: [], status: "passed" }];
  assert.deepEqual(consumerQualificationProblems(f), []);
  const widened = consumer();
  widened.report.runs = [{ id: "default", args: [], features: ["x"], envNames: [], status: "passed" }];
  assert.ok(consumerQualificationProblems(widened).length, "default row with features must refuse");
  const extraKey = consumer();
  extraKey.report.runs = [{ id: "default", args: [], features: [], envNames: [], status: "passed", hosts: null }];
  assert.ok(consumerQualificationProblems(extraKey).length, "default row with extra keys must refuse");
  const reviewed = consumer();
  reviewed.recipe.runs = [{ id: "core", features: [], args: ["core"], hosts: null, env: {} }];
  reviewed.report.runs = [{ id: "default", args: [], features: [], envNames: [], status: "passed" }];
  assert.ok(consumerQualificationProblems(reviewed).length, "implicit row must not substitute for reviewed runs");
});
test("R2a refuses null or missing args, features and envNames on the implicit default row", () => {
  const exact = consumer();
  exact.report.runs = [{ id: "default", args: [], features: [], envNames: [], status: "passed" }];
  assert.deepEqual(consumerQualificationProblems(exact), []);
  for (const key of ["args", "features", "envNames"]) {
    for (const value of [null, undefined]) {
      const f = consumer();
      f.report.runs = [{ id: "default", args: [], features: [], envNames: [], status: "passed" }];
      f.report.runs[0][key] = value;
      const problems = consumerQualificationProblems(f);
      assert.ok(problems.length, `${key}=${value} must refuse`);
      if (value === null) assert.ok(problems.some((p) => p.includes(key) && p.includes("array")), `${key} null reason must name the array requirement`);
    }
  }
});
test("R2a accepts a multi-run recipe whose rows each bind an actual command", () => {
  const f = consumer();
  const cwd = f.report.consumerDirectory;
  f.recipe.runs = [
    { id: "core", features: [], args: ["core"], hosts: null, env: {} },
    { id: "native-db", features: ["sqlcipher-system"], args: ["native-db"], hosts: ["win32", "darwin"], env: { RIGHTKIT_TEST_REAL_USER_STORES: "literal1" } },
    { id: "native-helper", features: [], args: ["native-helper"], hosts: ["darwin"], env: {} },
  ];
  f.report.runs = [
    { id: "core", args: ["core"], features: [], envNames: [], status: "passed" },
    { id: "native-db", args: ["native-db"], features: ["sqlcipher-system"], envNames: [], status: "passed" },
    { id: "native-helper", args: ["native-helper"], features: [], envNames: [], status: "passed" },
  ];
  f.report.commands = f.report.commands.filter((row) => !(row.command === "cargo" && row.args[0] === "run"));
  f.report.commands.push(
    { command: "cargo", args: ["run", "--locked", "--offline", "--", "core"], cwd, status: 0, stdout: "core ok\n", stderr: "" },
    { command: "cargo", args: ["run", "--locked", "--offline", "--features", "sqlcipher-system", "--", "native-db"], cwd, status: 0, stdout: "db ok\n", stderr: "" },
    { command: "/tmp/CredentialsConsumer.app/Contents/MacOS/consumer", args: ["native-helper"], cwd, status: 0, stdout: "helper ok\n", stderr: "" },
  );
  f.report.runtimeOutput = "[run core] passed\n[run native-db] passed\n[run native-helper] passed\ncore ok\ndb ok\nhelper ok\n";
  assert.deepEqual(consumerQualificationProblems(f), []);
  const damaged = consumer();
  damaged.report.runs = f.report.runs; damaged.report.commands = f.report.commands.slice(0, -1); damaged.report.runtimeOutput = f.report.runtimeOutput;
  assert.ok(consumerQualificationProblems(damaged).length, "missing helper launch command must refuse");
  const skipped = consumer();
  skipped.recipe.runs = f.recipe.runs; skipped.report.runs = f.report.runs.map((run) => run.id === "native-helper" ? { ...run, status: "not-applicable" } : run);
  skipped.report.commands = f.report.commands; skipped.report.runtimeOutput = f.report.runtimeOutput;
  assert.ok(consumerQualificationProblems(skipped).length, "marking a host-applicable row not-applicable must refuse");
});
for (const [label, damage] of [
  ["generic import", (f) => { delete f.report.qualification; }],
  ["unverified", (f) => { f.report.verified = false; }],
  ["foreign source", (f) => { f.report.commit = TREE; }],
  ["foreign run", (f) => { f.report.runId = "78"; }],
  ["foreign attempt", (f) => { f.report.runAttempt = "2"; }],
  ["foreign archive", (f) => { f.report.archiveSha256 = "d".repeat(64); }],
  ["foreign recipe", (f) => { f.report.recipe = { ...f.recipe, exampleSha256: "d".repeat(64) }; }],
  ["missing recipe", (f) => { f.recipe = undefined; }],
  ["runtime failure", (f) => { f.report.runtime.status = 1; }],
  ["online build", (f) => { f.report.build.offline = false; }],
  ["missing command", (f) => { f.report.commands.splice(1, 1); }],
  ["failed actual command", (f) => { f.report.commands[1].status = 1; }],
  ["invented output", (f) => { f.report.runtimeOutput = "invented"; }],
  ["invented features", (f) => { f.report.resolvedFeatures = ["invented"]; }],
  ["path dependency", (f) => { f.report.lockEvidence[0].source = null; }],
  ["missing filesystem fixture", (f) => { f.expected.name = "rightkit-fs"; }],
]) test(`R2a refuses ${label}`, () => { const f = consumer(); damage(f); assert.ok(consumerQualificationProblems(f).length); });

function npmFixture(t) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "rk-semantic-fixture-"));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const packDir = path.join(root, "packs"); fs.mkdirSync(packDir);
  fs.mkdirSync(path.join(root, "packages/no-test"), { recursive: true });
  const json = { name: "@rightkit/no-test", version: "1.0.0" };
  fs.writeFileSync(path.join(root, "packages/no-test/package.json"), JSON.stringify(json));
  const bytes = Buffer.from("sealed archive fixture");
  const unit = { name: json.name, version: json.version, dir: "packages/no-test", file: "no-test.tgz", size: bytes.length, sha256: hash(bytes) };
  const manifest = { schema: 1, registry: "npm", commit: SHA, runId: "77", runAttempt: "1", units: [unit] };
  const rawManifest = JSON.stringify(manifest); fs.writeFileSync(path.join(packDir, "manifest.json"), rawManifest); fs.writeFileSync(path.join(packDir, unit.file), bytes);
  const candidate = { sourceSha: SHA, manifestSha256: hash(rawManifest), registry: "npm", units: [json.name], runId: "77", runAttempt: "1" };
  const hosts = {}; const receipts = {}; const qa = {};
  for (const host of ["macos", "windows"]) {
    hosts[host] = derivePublicationObligations({ registry: "npm", units: [json.name], host, packageManifests: [{ dir: unit.dir, json }], allow: { allowNoTest: [json.name] } });
    const summary = { host, sha: SHA, selection: null, steps: "lock,fmt,test,closure", results: [], rows: [] };
    const unitEvidence = { schema: 2, host, registry: "npm", sha: SHA, ok: true, units: [{ name: json.name, id: "npm:packages/no-test", status: "allowed-no-test", executed: 0, ignored: 0, skipped: 0, problems: [], steps: [] }] };
    qa[host] = { summary, unitEvidence, summarySha256: hash(JSON.stringify(summary)), unitEvidenceSha256: hash(JSON.stringify(unitEvidence)) };
    receipts[host] = { schema: 1, kind: "verify", host, registry: "npm", commit: SHA, manifestSha256: candidate.manifestSha256, runId: "77", runAttempt: "1", manifestRunId: "77", manifestRunAttempt: "1", sourceGate: { summarySha256: qa[host].summarySha256, unitEvidenceSha256: qa[host].unitEvidenceSha256 }, units: [{ ...unit, status: "allowed-no-test", bytesVerified: true, publicationEvidence: null, consumer: { verified: true } }] };
  }
  return { candidate, manifest, source: { source: { sha: SHA, tree: TREE }, hosts, root, sourceHashes: {} }, receipts, qa, packDir };
}
test("explicit no-test policy stays separate & cannot manufacture canonical positive runs or npm PK", (t) => {
  const input = npmFixture(t); const original = JSON.stringify(input.receipts);
  const result = semanticAdmission(input);
  assert.equal(result.ok, false); assert.equal(result.units.length, 1);
  assert.equal(result.hostVerdicts.macos.ok, true);
  assert.equal(result.units[0].policies.macos, "allowed-no-test");
  assert.ok(result.units[0].failures.some((s) => s.includes("policy conflict")));
  assert.ok(result.units[0].failures.some((s) => s.includes("missing canonical")));
  assert.ok(result.units[0].failures.some((s) => s.includes("generic import is not PK")));
  assert.equal(JSON.stringify(input.receipts), original);
  assert.equal(result.archiveHashes[input.manifest.units[0].name], input.manifest.units[0].sha256);
});
test("changed actual archive & raw QA hash fail independently", (t) => {
  const input = npmFixture(t);
  fs.writeFileSync(path.join(input.packDir, input.manifest.units[0].file), "changed bytes");
  input.qa.windows.summarySha256 = "d".repeat(64);
  const result = semanticAdmission(input);
  assert.ok(result.failures.some((s) => s.includes("sourceGate differs")));
  assert.ok(result.units[0].failures.some((s) => s.includes("actual archive")));
});

// Candidate fixtures expose authenticated bytes only. No candidate module is loaded or run.
function namedNpmFixture(t) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "rk-named-npm-semantic-"));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const packDir = path.join(root, "packs"); fs.mkdirSync(packDir);
  const sourceData = new Map([...NPM_RECIPE_FILES, ...NPM_SOURCE_FILES].map(relative => [relative, fs.readFileSync(new URL(`../../${relative}`, import.meta.url))]));
  const packageManifest = JSON.parse(sourceData.get("packages/logs/package.json"));
  fs.mkdirSync(path.join(root, "packages/logs"), { recursive: true });
  fs.writeFileSync(path.join(root, "packages/logs/package.json"), sourceData.get("packages/logs/package.json"));
  const bytes = Buffer.from("original sealed npm candidate fixture");
  const unit = { name: packageManifest.name, version: packageManifest.version, dir: "packages/logs", file: "logs.tgz", size: bytes.length, sha256: hash(bytes), integrity: `sha512-${crypto.createHash("sha512").update(bytes).digest("base64")}` };
  const manifest = { schema: 1, registry: "npm", commit: SHA, runId: "77", runAttempt: "1", units: [unit] };
  const rawManifest = JSON.stringify(manifest); fs.writeFileSync(path.join(packDir, "manifest.json"), rawManifest); fs.writeFileSync(path.join(packDir, unit.file), bytes);
  const candidate = { sourceSha: SHA, manifestSha256: hash(rawManifest), registry: "npm", units: [unit.name], runId: "77", runAttempt: "1" };
  const hosts = {}, receipts = {}, qa = {}, reads = [];
  const source = { source: { sha: SHA, tree: TREE }, root, hosts, sourceHashes: {}, readData(relative) { reads.push(relative); const bytes = sourceData.get(relative); if (!bytes) throw new Error(`missing authenticated blob ${relative}`); this.sourceHashes[relative] = hash(bytes); return Buffer.from(bytes); } };
  for (const host of ["macos", "windows"]) {
    hosts[host] = derivePublicationObligations({ registry: "npm", units: [unit.name], host, packageManifests: [{ dir: unit.dir, json: packageManifest }] });
    const results = hosts[host].commands.map(row => ({ ...row, ok: true, code: 0, timedOut: false, seconds: 1, isTest: TEST_COMMAND.test(row.command), executed: TEST_COMMAND.test(row.command) ? 4 : 0, failed: 0, ignored: 0, skipped: 0, binaries: TEST_COMMAND.test(row.command) ? 1 : 0 }));
    const rows = [...new Set(results.map(row => row.unit))].map(id => ({ id, ok: true, known: false, failed: [], seconds: results.filter(row => row.unit === id).length, skipped: 0 }));
    const summary = { host, sha: SHA, selection: null, steps: "lock,fmt,test,closure", results, rows };
    const policy = hosts[host].unitPolicies[0];
    const selected = results.filter(row => row.unit === policy.id);
    const steps = selected.map(row => ({ command: row.command, ok: true, seconds: 1, ...(row.isTest ? Object.fromEntries(["executed", "failed", "ignored", "skipped", "binaries"].map(key => [key, row[key]])) : {}) }));
    const tests = selected.filter(row => row.isTest);
    const executed = tests.reduce((sum, row) => sum + row.executed, 0);
    const unitEvidence = { schema: 2, host, registry: "npm", sha: SHA, ok: true, units: [{ name: unit.name, id: policy.id, status: "pass", executed, ignored: 0, skipped: 0, problems: [], steps }] };
    qa[host] = { summary, unitEvidence, summarySha256: hash(JSON.stringify(summary)), unitEvidenceSha256: hash(JSON.stringify(unitEvidence)) };
    const arch = host === "macos" ? "arm64" : "x64";
    const publicationEvidence = { schema: 1, kind: "npm", name: unit.name, version: unit.version, commit: SHA, tree: TREE, artifactSha256: unit.sha256, runs: [{ commit: SHA, artifactSha256: unit.sha256, host: { os: host === "macos" ? "darwin" : "win32", arch }, command: tests.map(row => row.command).join(" && "), features: ["default"], passed: executed, failed: 0, operator: "fixture/native-job", timestamp: "2026-10-20T00:00:00Z" }] };
    const consumer = namedNpmReport({ sourceData, candidate, manifest, unit, host, arch });
    receipts[host] = { schema: 1, kind: "verify", host, registry: "npm", commit: SHA, manifestSha256: candidate.manifestSha256, runId: "77", runAttempt: "1", manifestRunId: "77", manifestRunAttempt: "1", allowHostExclusion: [], allowNoTest: [], allowIgnored: [], allowMissingAssets: [], sourceGate: { tree: TREE, summarySha256: qa[host].summarySha256, unitEvidenceSha256: qa[host].unitEvidenceSha256 }, units: [{ ...unit, status: "pass", bytesVerified: true, publicationEvidence, consumer }] };
  }
  return { input: { candidate, manifest, source, receipts, qa, packDir }, sourceData, reads };
}
function namedNpmReport({ sourceData, candidate, manifest, unit, host, arch }) {
  const api = host === "windows" ? path.win32 : path.posix;
  const scratch = host === "windows" ? "D:\\scratch\\consumer" : "/scratch/consumer";
  const checkout = host === "windows" ? "D:\\checkout" : "/checkout";
  const archive = host === "windows" ? "D:\\packs\\logs.tgz" : "/packs/logs.tgz";
  const commands = [], sourceBindings = {};
  const add = (stage, command, args, stdout, cwd = scratch) => commands.push({ stage, command, args, stdout, stderr: "", cwd, status: 0 });
  for (const [relative, bytes] of sourceData) {
    sourceBindings[relative] = { sha256: hash(bytes), gitBlob: crypto.createHash("sha1").update(Buffer.concat([Buffer.from(`blob ${bytes.length}\0`), bytes])).digest("hex") };
    add("source", "git", ["show", `${SHA}:${relative}`], bytes.toString("utf8"), checkout);
  }
  const pkg = JSON.parse(sourceData.get("packages/logs/package.json"));
  const recipeFiles = Object.fromEntries(NPM_RECIPE_FILES.map(relative => [relative, sourceBindings[relative]]));
  add("archive-read", "tar", ["-xOzf", "logs.tgz", "package/package.json"], JSON.stringify(pkg), api.dirname(archive));
  add("archive-read", "tar", ["-xOzf", "logs.tgz", "package/README.md"], sourceData.get("packages/logs/README.md").toString("utf8"), api.dirname(archive));
  const pin = pkg.packageManager, store = api.join(scratch, "store"), home = api.join(scratch, "home");
  const pnpm = (stage, args, output) => host === "windows" ? add(stage, "C:\\Windows\\System32\\cmd.exe", ["/d", "/s", "/c", `pnpm ${args.map(arg => `"${arg}"`).join(" ")}`], output) : add(stage, "pnpm", args, output);
  pnpm("toolchain", ["--version"], `${pin.slice(5)}\n`);
  pnpm("install", ["install", "--ignore-scripts", "--no-frozen-lockfile", "--ignore-workspace", "--store-dir", store], "installed");
  const vectors = JSON.parse(sourceData.get(NPM_SOURCE_FILES.at(-1)));
  const counts = { exportNames: 9, parity: 37, crashParity: vectors.vectors.filter(row => row.windowCrash).length, diagnostics: 10, crashCapture: 3, logsExample: 1 };
  add("runtime", host === "windows" ? "C:\\node\\node.exe" : "/usr/bin/node", ["consumer.mjs"], JSON.stringify({ schema: 1, recipe: "PK11-npm-logs", passed: true, counts }));
  const consumerManifest = { ...JSON.parse(sourceData.get("ci/consumers/npm-logs/package.json")), dependencies: { [unit.name]: `file:${archive.replaceAll("\\", "/")}` } };
  const lock = `lockfileVersion: 9\nresolution: file:${archive.replaceAll("\\", "/")}\nintegrity: ${unit.integrity}\n`;
  return { schema: 1, kind: "npm-consumer", qualification: "R2a", recipeId: "PK11-npm-logs", name: unit.name, version: unit.version, commit: SHA, runId: String(candidate.runId), runAttempt: String(candidate.runAttempt), manifestRunId: String(manifest.runId), manifestRunAttempt: String(manifest.runAttempt), manifestSha256: candidate.manifestSha256, archiveSha256: unit.sha256, archiveSize: unit.size, archiveIntegrity: unit.integrity, host, runnerOS: host === "windows" ? "Windows" : "macOS", arch, verified: true, commands, sourceBindings, counts, toolchain: { node: "v24.0.0", pnpm: pin.slice(5), packageManager: pin }, recipe: { sourceCommit: SHA, files: recipeFiles, sha256: hash(JSON.stringify(recipeFiles)) }, paritySource: { path: NPM_SOURCE_FILES.at(-1), ...sourceBindings[NPM_SOURCE_FILES.at(-1)] }, lock, lockSha256: hash(lock), isolation: { checkout, scratch, originalArchive: archive, home, userconfig: api.join(home, ".npmrc"), globalconfig: api.join(home, "global.npmrc"), store, systemRoot: "C:\\Windows", ignoreScripts: true, inheritWorkspace: false, scriptLifecycle: "disabled", manifest: consumerManifest, consumerManifestSha256: hash(`${JSON.stringify(consumerManifest, null, 2)}\n`) }, portability: { entries: 1, imported: [unit.name], resolvedOnly: [], productionRejected: [], conditionalImports: [], skippedImport: [], patterns: [], bins: [], binSmoke: [], filesChecked: 2, noJsEntry: false } };
}
test("semantic admission accepts named npm evidence on both native hosts through data-only source reader", t => {
  const fixture = namedNpmFixture(t);
  const result = semanticAdmission(fixture.input);
  assert.deepEqual(result.failures, []);
  assert.deepEqual(result.units[0].failures, []);
  assert.equal(result.ok, true);
  for (const relative of [...NPM_RECIPE_FILES, ...NPM_SOURCE_FILES]) assert.ok(fixture.reads.includes(relative));
});
test("named npm semantic integration refuses source/output/portability/host/pin substitutions", t => {
  const mutations = [
    fixture => { fixture.input.receipts.macos.units[0].consumer.counts.parity = 36; },
    fixture => { fixture.input.receipts.macos.units[0].consumer.commands.at(-1).stdout = "{}"; },
    fixture => { fixture.input.receipts.macos.units[0].consumer.recipe.sha256 = HEX; },
    fixture => { fixture.input.receipts.macos.units[0].consumer.portability.imported = []; },
    fixture => { fixture.input.receipts.macos.units[0].consumer.portability.filesChecked = 1; },
    fixture => { fixture.input.receipts.macos.units[0].consumer.portability.skippedImport = ["."]; },
    fixture => { fixture.input.receipts.macos.units[0].consumer.arch = "x64"; },
    fixture => { fixture.input.receipts.macos.units[0].consumer.archiveIntegrity = "sha512-forged"; },
    fixture => { fixture.input.receipts.macos.units[0].consumer.toolchain.pnpm = "1.0.0"; },
    fixture => { fixture.input.source.readData = undefined; },
    fixture => { fixture.sourceData.set("packages/logs/src/crash.ts", Buffer.from("candidate changed code")); },
    fixture => { fixture.sourceData.set(NPM_SOURCE_FILES.at(-1), Buffer.from('{"schema":2}')); },
    fixture => { fixture.sourceData.set("ci/consumers/npm-logs/consumer.mjs", Buffer.from("throw new Error('candidate recipe must not execute')")); },
    fixture => { const pkg = JSON.parse(fixture.sourceData.get("package.json")); pkg.packageManager = "pnpm@1.0.0"; fixture.sourceData.set("package.json", Buffer.from(JSON.stringify(pkg))); },
  ];
  for (const mutate of mutations) { const fixture = namedNpmFixture(t); mutate(fixture); const result = semanticAdmission(fixture.input); assert.equal(result.ok, false); assert.ok(result.units[0].failures.length); }
});
test("unknown npm remains HOLD even with a forged named consumer", t => {
  const fixture = namedNpmFixture(t);
  assert.ok(namedNpmQualificationProblems({ unit: { name: "@rightkit/other" }, report: fixture.input.receipts.macos.units[0].consumer }).some(problem => problem.includes("generic import is not PK qualification")));
});
test("reviewed recipe binds candidate runs to trusted inventory runs", t => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "rk-admission-runs-"));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const repo = path.resolve(import.meta.dirname, "../..");
  fs.cpSync(path.join(repo, "ci/consumers/rightkit-credentials"), path.join(root, "ci/consumers/rightkit-credentials"), { recursive: true });
  fs.mkdirSync(path.join(root, "ci"), { recursive: true });
  const config = JSON.parse(fs.readFileSync(path.join(repo, "ci/consumer-recipes.json"), "utf8"));
  const source = () => ({ root, source: { sha: "a".repeat(40) } });
  const unit = { name: "rightkit-credentials", version: "0.4.2" };
  fs.writeFileSync(path.join(root, "ci/consumer-recipes.json"), JSON.stringify(config));
  assert.equal(reviewedRecipe(source(), unit).runs.length, config.cargo["rightkit-credentials"].runs.length);
  const narrowed = structuredClone(config);
  narrowed.cargo["rightkit-credentials"].runs.find(run => run.id === "native-db").hosts = ["win32"];
  fs.writeFileSync(path.join(root, "ci/consumer-recipes.json"), JSON.stringify(narrowed));
  assert.throws(() => reviewedRecipe(source(), unit), /candidate recipe runs differ from reviewed inventory runs/);
  const dropped = structuredClone(config);
  dropped.cargo["rightkit-credentials"].runs = dropped.cargo["rightkit-credentials"].runs.filter(run => run.id !== "native-helper");
  fs.writeFileSync(path.join(root, "ci/consumer-recipes.json"), JSON.stringify(dropped));
  assert.throws(() => reviewedRecipe(source(), unit), /candidate recipe runs differ from reviewed inventory runs/);
});
