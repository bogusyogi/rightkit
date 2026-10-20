import test from "node:test";
import assert from "node:assert/strict";
import { derivePublicationObligations, publicationObligationsVerdict } from "./publication-obligations.mjs";
import { TEST_COMMAND } from "./test-results.mjs";

const sha = "a".repeat(40);
const manifest = (name) => ({ dir: `crates/${name}`, text: `[package]\nname = "${name}"\nversion = "1.0.0"\n` });
const crateInput = () => ({ registry: "crates", units: ["leaf"], host: "windows", config: { rust: { leaf: { featureSets: [[], ["--features", "x"]], alsoSerial: true }, consumer: { checkFeatureSets: [["--features", "use-leaf"]] } } }, memberManifests: [manifest("leaf"), manifest("consumer")], lockText: '[[package]]\nname = "leaf"\nversion = "1.0.0"\n\n[[package]]\nname = "consumer"\nversion = "1.0.0"\ndependencies = [\n "leaf",\n]\n' });

function proof(plan) {
  const results = plan.commands.map((command) => ({ ...command, ok: true, code: 0, timedOut: false, seconds: 1, isTest: TEST_COMMAND.test(command.command), executed: TEST_COMMAND.test(command.command) ? 2 : 0, failed: 0, ignored: 0, skipped: 0, binaries: TEST_COMMAND.test(command.command) ? 1 : 0 }));
  const rows = [...new Set(results.map((result) => result.unit))].map((id) => ({ id, ok: true, known: false, seconds: results.filter((result) => result.unit === id).length, failed: [], skipped: 0 }));
  const units = plan.unitPolicies.map((policy) => {
    const commands = results.filter((result) => result.unit === policy.id);
    return { name: policy.name, id: policy.id, status: policy.status ?? "pass", executed: commands.reduce((sum, result) => sum + result.executed, 0), ignored: 0, skipped: 0, problems: [], steps: commands.map((result) => ({ command: result.command, ok: true, seconds: 1, ...(result.isTest ? { executed: result.executed, failed: 0, ignored: 0, skipped: 0, binaries: result.binaries } : {}) })) };
  });
  return { summary: { host: plan.host, sha, selection: null, steps: plan.steps, rows, results }, unitEvidence: { schema: 2, host: plan.host, registry: plan.registry, sha, ok: true, units } };
}

test("source-derived feature, serial, lint & reverse-dependent rows qualify", () => {
  const plan = derivePublicationObligations(crateInput());
  assert.equal(plan.commands.filter((row) => row.command.startsWith("cargo test ")).length, 4);
  assert.equal(plan.commands.filter((row) => row.command.startsWith("cargo clippy ")).length, 2);
  assert.deepEqual(plan.selection.rust.check, ["consumer"]);
  assert.ok(plan.commands.some((row) => row.command === "cargo check -p consumer --all-targets --features use-leaf"));
  assert.equal(publicationObligationsVerdict(plan, proof(plan), sha).ok, true);
});

for (const damage of ["missing", "duplicate", "substituted", "failed", "timed-out", "missing-code", "invalid-count"]) {
  test(`reverse-dependent ${damage} proof refuses`, () => {
    const plan = derivePublicationObligations(crateInput());
    const data = proof(plan);
    const index = data.summary.results.findIndex((row) => row.command.includes("-p consumer"));
    if (damage === "missing") data.summary.results.splice(index, 1);
    if (damage === "duplicate") data.summary.results.push({ ...data.summary.results[index] });
    if (damage === "substituted") data.summary.results[index].command += " --features other";
    if (damage === "failed") data.summary.results[index].code = 1;
    if (damage === "timed-out") data.summary.results[index].timedOut = true;
    if (damage === "missing-code") delete data.summary.results[index].code;
    if (damage === "invalid-count") data.summary.results[index].executed = -1;
    assert.equal(publicationObligationsVerdict(plan, data, sha).ok, false);
  });
}

test("npm build closure is independently required", () => {
  const plan = derivePublicationObligations({ registry: "npm", units: ["@rightkit/leaf"], host: "macos", packageManifests: [{ dir: "packages/leaf", json: { name: "@rightkit/leaf", scripts: { test: "node --test" }, dependencies: { "@rightkit/base": "1.0.0" } } }, { dir: "packages/base", json: { name: "@rightkit/base", scripts: { build: "tsc" } } }] });
  assert.deepEqual(plan.commands[0], { unit: "npm:build", command: "pnpm --workspace-concurrency=1 --if-present --filter @rightkit/leaf... run build" });
  assert.equal(publicationObligationsVerdict(plan, proof(plan), sha).ok, true);
  const data = proof(plan);
  data.summary.results.shift();
  assert.equal(publicationObligationsVerdict(plan, data, sha).ok, false);
});

test("positive tests cannot hide missing feature rows or zero-test feature runs", () => {
  const plan = derivePublicationObligations(crateInput());
  for (const damage of ["missing-feature", "zero-count"]) {
    const data = proof(plan);
    const index = data.summary.results.findIndex((row) => row.command.startsWith("cargo test") && row.command.includes("--features"));
    if (damage === "missing-feature") data.summary.results.splice(index, 1);
    else data.summary.results[index].executed = 0;
    assert.equal(publicationObligationsVerdict(plan, data, sha).ok, false);
  }
});

test("waivers are explicit policy-derived states", () => {
  const input = crateInput();
  input.config.rust.leaf.os = ["macos"];
  assert.throws(() => derivePublicationObligations(input), /would be skipped/);
  input.allow = { allowHostExclusion: ["leaf"] };
  const plan = derivePublicationObligations(input);
  assert.equal(plan.commands.length, 0);
  assert.equal(publicationObligationsVerdict(plan, proof(plan), sha).ok, true);
  const noTest = { registry: "npm", units: ["@rightkit/plain"], host: "macos", packageManifests: [{ dir: "packages/plain", json: { name: "@rightkit/plain" } }] };
  assert.throws(() => derivePublicationObligations(noTest), /no test script/);
  const npmPlan = derivePublicationObligations({ ...noTest, allow: { allowNoTest: ["@rightkit/plain"] } });
  assert.equal(publicationObligationsVerdict(npmPlan, proof(npmPlan), sha).ok, true);
});

test("skip counts require explicit waiver & positive execution", () => {
  const input = crateInput();
  input.assetGated = { crates: { leaf: "asset" } };
  assert.throws(() => derivePublicationObligations(input), /need assets/);
  input.allow = { allowMissingAssets: ["leaf"] };
  const plan = derivePublicationObligations(input);
  const data = proof(plan);
  const index = data.summary.results.findIndex((row) => row.isTest);
  data.summary.results[index].skipped = 1;
  data.summary.rows.find((row) => row.id === "rust:leaf").skipped = 1;
  const unit = data.unitEvidence.units[0];
  unit.steps.find((row) => row.command === data.summary.results[index].command).skipped = 1;
  unit.status = "allowed-skips";
  unit.skipped = 1;
  assert.equal(publicationObligationsVerdict(plan, data, sha).ok, true);
  assert.equal(publicationObligationsVerdict({ ...plan, unitPolicies: plan.unitPolicies.map((policy) => ({ ...policy, allowMissingAssets: false })) }, data, sha).ok, false);
});

test("producer expected commands & green flags cannot replace command data", () => {
  const plan = derivePublicationObligations(crateInput());
  const data = proof(plan);
  data.unitEvidence.dependentClosure = { ok: true, expected: [], results: [] };
  data.summary.results = data.summary.results.filter((row) => row.unit !== "rust:consumer");
  assert.equal(publicationObligationsVerdict(plan, data, sha).ok, false);
});


test("publication proof requires closure consumers & one shared locked metadata check", () => {
  const input = crateInput();
  const manifestPath = "ci/consumers/native/Cargo.toml";
  const rows = [{ manifestPath }, { manifestPath, args: ["--features", "engine"] }];
  input.config.rust.consumer.consumerRows = rows;
  input.config.rust.leaf.consumerRows = rows;
  const plan = derivePublicationObligations(input);
  const metadata = `cargo metadata --manifest-path ${manifestPath} --locked --format-version 1`;
  assert.equal(plan.commands.filter((row) => row.command === metadata).length, 1);
  assert.equal(plan.commands.filter((row) => row.unit === "rust:consumer" && row.command.includes("--manifest-path")).length, 2);
  assert.equal(publicationObligationsVerdict(plan, proof(plan), sha).ok, true);
  for (const omitted of plan.commands.filter((row) => row.command === metadata || (row.unit === "rust:consumer" && row.command.includes("--manifest-path")))) {
    const data = proof(plan);
    data.summary.results = data.summary.results.filter((row) => row.unit !== omitted.unit || row.command !== omitted.command);
    assert.equal(publicationObligationsVerdict(plan, data, sha).ok, false);
  }
  const repaired = proof(plan);
  const stale = repaired.summary.results.find((row) => row.command === metadata);
  stale.ok = false;
  stale.code = 1;
  repaired.summary.results.push({ ...stale, command: `cargo update --manifest-path ${manifestPath} --workspace`, ok: true, code: 0 });
  assert.equal(publicationObligationsVerdict(plan, repaired, sha).ok, false);
});
