import test from "node:test";
import assert from "node:assert/strict";
import { batchClippyInvocations } from "./local-publish.mjs";
import { batchReceiptProblems } from "./publication-gate.mjs";

test("batch clippy groups only crates in the same Cargo workspace", () => {
  const rows = batchClippyInvocations(["a", "b", "side"], { workspaceOf: (unit) => unit === "side" ? "crates/side" : "." });
  assert.deepEqual(rows.map(({ crates, workspace }) => ({ crates, workspace })), [
    { crates: ["a", "b"], workspace: "." },
    { crates: ["side"], workspace: "crates/side" },
  ]);
});

test("batch receipt requires fmt and clippy coverage in the crate's workspace", () => {
  const commit = "a".repeat(40), tree = "b".repeat(40);
  const row = (name, command, crates, workspace) => ({ name, command, crates, workspace, ok: true, commit, tree });
  const batch = { id: "batch", commit, tree, crates: ["a", "side"], workspaces: { a: ".", side: "crates/side" }, checks: [
    row("fmt", "cargo fmt --all --check", ["a"], "."),
    row("fmt", "cargo fmt --all --check", ["side"], "crates/side"),
    row("secret-scan", "scan", ["a", "side"]),
    row("clippy", "cargo clippy -p side --all-targets -- -D warnings", ["side"], "crates/side"),
  ] };
  const evidence = { name: "side", commit };
  assert.deepEqual(batchReceiptProblems(batch, evidence, tree, "crates/side"), []);
  batch.checks[1].workspace = ".";
  assert.ok(batchReceiptProblems(batch, evidence, tree, "crates/side").some((p) => p.includes("fmt does not cover")));
  batch.checks[1].workspace = "crates/side";
  batch.checks[3].workspace = ".";
  assert.ok(batchReceiptProblems(batch, evidence, tree, "crates/side").some((p) => p.includes("clippy does not cover")));
  batch.checks[3].workspace = "crates/side";
  batch.workspaces.side = ".";
  batch.checks[1].workspace = ".";
  batch.checks[3].workspace = ".";
  assert.ok(batchReceiptProblems(batch, evidence, tree, "crates/side").some((p) => p.includes("differs from authenticated")));
  assert.ok(batchReceiptProblems(batch, evidence, tree).some((p) => p.includes("expected workspace")));
  delete batch.workspaces;
  assert.ok(batchReceiptProblems(batch, evidence, tree, "crates/side").some((p) => p.includes("batch.workspaces")));
});

test("workspace identity comes from committed manifests, including exclusions", async () => {
  const { committedCargoWorkspace } = await import("./committed-cargo-workspace.mjs");
  const files = new Map([
    ["Cargo.toml", '[workspace]\nmembers = ["crates/*"]\nexclude = ["crates/side"]'],
    ["crates/a/Cargo.toml", '[package]\nname = "a"\nversion = "1.0.0"'],
    ["crates/side/Cargo.toml", '[package]\nname = "side"\nversion = "1.0.0"\n[workspace]'],
  ]);
  const read = (file) => files.get(file) ?? null;
  assert.equal(committedCargoWorkspace({ unitDir: "crates/a", read }), ".");
  assert.equal(committedCargoWorkspace({ unitDir: "crates/side", read }), "crates/side");
  files.set("crates/side/Cargo.toml", '[package]\nname = "side"\nversion = "1.0.0"');
  assert.throws(() => committedCargoWorkspace({ unitDir: "crates/side", read }), /excluded/);
  files.set("crates/Cargo.toml", "[workspace]");
  assert.throws(() => committedCargoWorkspace({ unitDir: "crates/a", read }), /nested/);
});

test("batch clippy requires every feature and target in the authenticated workspace", async () => {
  const { batchClippyMissingConfigurations } = await import("./publication-gate.mjs");
  const commit = "a".repeat(40), tree = "b".repeat(40), workspace = ".";
  const evidence = { name: "a", commit };
  const batch = { tree, workspaces: { a: "." }, checks: [] };
  const row = (features, target, where = ".") => ({ name: "clippy", ok: true, commit, tree, workspace: where, crates: ["a"], command: ["cargo", "clippy", "-p", "a", "--message-format=json-render-diagnostics", "--no-deps", "--all-targets", ...(target ? ["--target", target] : []), ...features, "--", "-D", "warnings"].join(" ") });
  batch.checks.push(row([], null), row(["--all-features"], null, "crates/side"), row([], "foreign"));
  const sets = [[], ["--all-features"]];
  assert.deepEqual(batchClippyMissingConfigurations(batch, evidence, sets, ["foreign"], workspace), [
    { features: ["--all-features"], target: null }, { features: ["--all-features"], target: "foreign" },
  ]);
  batch.checks.push(row(["--all-features"], null), row(["--all-features"], "foreign"));
  assert.deepEqual(batchClippyMissingConfigurations(batch, evidence, sets, ["foreign"], workspace), []);
  batch.checks.at(-1).command += " --unexpected";
  assert.deepEqual(batchClippyMissingConfigurations(batch, evidence, sets, ["foreign"], workspace), [{ features: ["--all-features"], target: "foreign" }]);
});

test("publisher requirements preserve npm scripts and execution environment", async () => {
  const { compileRequirements } = await import("./local-publish.mjs");
  const plan = compileRequirements({ kind: "npm", unit: "@rightkit/example", dir: "packages/example", config: {}, manifest: { scripts: { test: "node test.mjs" } }, env: { RIGHTKIT_ISOLATED_CHECKOUT: "1" } });
  assert.deepEqual(plan.requirements.map(({ executable, argv, phase }) => ({ executable, argv, phase })), [
    { executable: "pnpm", argv: ["-r", "--workspace-concurrency=1", "run", "build"], phase: "compile" },
    { executable: "pnpm", argv: ["--dir", "packages/example", "run", "test"], phase: "test" },
  ]);
  assert.equal(plan.requirements[0].env.RIGHTKIT_ISOLATED_CHECKOUT, "1");
  assert.equal(plan.requirements[0].env.RIGHTKIT_REQUIRE_ASSETS, "1");
  assert.equal(plan.verdict.allowIgnored, false);
  assert.equal(plan.verdict.allowNoTests, true);
  assert.notEqual(plan.requirements[0].coverageKey, plan.requirements[1].coverageKey);
});

test("local admission accepts batch checks while requiring unit qualification steps", async () => {
  const { evidenceStepProblems } = await import("../../scripts/ci/local-admission.mjs");
  const { localTestPlan } = await import("./local-publish.mjs");
  const { crossTargetsFor } = await import("./cross-targets.mjs");
  const commit = "a".repeat(40), tree = "b".repeat(40), workspace = ".", unit = { name: "a", dir: "crates/a" };
  const plan = localTestPlan({ kind: "cargo", unit: unit.name, dir: unit.dir, config: {}, batch: true, platform: "darwin" });
  const steps = plan.steps.map((step) => ({ command: `${step.command} ${step.args.join(" ")}`, ok: true, ...(step.test ? { executed: 1, failed: 0 } : {}) }));
  steps.push({ command: "cargo package --locked -p a", ok: true });
  const check = (name, command) => ({ name, command, ok: true, commit, tree, workspace, crates: ["a"] });
  const checks = [check("fmt", "cargo fmt --all --check"), check("secret-scan", "scanCrateForSecrets crates/a")];
  for (const target of [null, ...crossTargetsFor({}, "darwin")]) checks.push(check("clippy", ["cargo", "clippy", "-p", "a", "--message-format=json-render-diagnostics", "--no-deps", "--all-targets", ...(target ? ["--target", target] : []), "--", "-D", "warnings"].join(" ")));
  const evidence = { name: "a", commit, runs: [{ steps, batch: { id: "batch", commit, tree, crates: ["a"], workspaces: { a: workspace }, checks } }] };
  assert.deepEqual(evidenceStepProblems({ evidence, unit, gateConfig: {}, hostOs: "darwin", workspace, tree }), []);
  checks.pop();
  assert.ok(evidenceStepProblems({ evidence, unit, gateConfig: {}, hostOs: "darwin", workspace, tree }).some((problem) => problem.includes("batch clippy missing")));
  checks.push(check("clippy", "invalid"));
  steps.pop();
  assert.ok(evidenceStepProblems({ evidence, unit, gateConfig: {}, hostOs: "darwin", workspace, tree }).some((problem) => problem.includes("cargo package")));
});


test("publisher requirements include secret scan, fmt, asset policy and test verdict", async () => {
  const { compileRequirements } = await import("./local-publish.mjs");
  const before = process.env.RIGHTKIT_CROSS_CC_DIR;
  process.env.RIGHTKIT_CROSS_CC_DIR = "/fixture/cross-cc";
  try {
    const plan = compileRequirements({ kind: "cargo", unit: "a", dir: "crates/a", config: { rust: { a: { allowIgnored: "documented" } } }, assetGated: { crates: { a: {} } }, workspace: ".", env: { API_TOKEN: "removed" }, platform: "darwin" });
    assert.equal(plan.requirements[0].internal, "scanCrateForSecrets");
    assert.deepEqual(plan.requirements[1].argv, ["fmt", "-p", "a", "--check"]);
    assert.equal(plan.requirements[1].env.RIGHTKIT_REQUIRE_ASSETS, "0");
    assert.equal(plan.requirements[1].env.API_TOKEN, undefined);
    assert.equal(plan.verdict.allowIgnored, true);
    assert.equal(plan.verdict.allowIgnoredReason, "documented");
    assert.equal(plan.verdict.allowMissingAssets, true);
    assert.equal(plan.requirements.filter((row) => row.argv[0] === "clippy").length, 3);
  } finally {
    if (before === undefined) delete process.env.RIGHTKIT_CROSS_CC_DIR;
    else process.env.RIGHTKIT_CROSS_CC_DIR = before;
  }
});
