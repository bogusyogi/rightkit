import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import test from "node:test";
import { parseYaml } from "./workflow-yaml.mjs";

const readYaml = (file) => parseYaml(fs.readFileSync(new URL(file, import.meta.url), "utf8"));
const ACTION = readYaml("../../.github/actions/rk-setup/action.yml");
const WORKFLOW = readYaml("../../.github/workflows/ci.yml");
const setup = (job) => job.steps.find((step) => step.uses === "./.github/actions/rk-setup");

test("native CI requires correlated dispatch while push & PR remain static", () => {
  assert.ok(Object.hasOwn(WORKFLOW.on, "push"));
  assert.ok(Object.hasOwn(WORKFLOW.on, "pull_request"));
  for (const input of ["request_id", "expected_sha"]) {
    assert.equal(WORKFLOW.on.workflow_dispatch.inputs[input].required, "true");
    assert.equal(WORKFLOW.on.workflow_dispatch.inputs[input].type, "string");
  }
  assert.match(WORKFLOW["run-name"], /inputs.request_id/);
  const guard = WORKFLOW.jobs["private-repo-guard"].steps[0];
  assert.equal(guard.if, "${{ github.event_name == 'workflow_dispatch' }}");
  assert.deepEqual(guard.env, { RK_REQUEST_ID: "${{ inputs.request_id }}", RK_EXPECTED_SHA: "${{ inputs.expected_sha }}" });
  assert.match(guard.run, /test "\$RK_EXPECTED_SHA" = "\$GITHUB_SHA"/);
  assert.doesNotMatch(guard.run, /\$\{\{/);
  for (const name of ["shards-windows", "shards-macos", "closure-windows", "closure-macos", "mark-green-windows", "mark-green-macos"]) assert.match(WORKFLOW.jobs[name].if, /github.event_name == 'workflow_dispatch'/);
  assert.equal(setup(WORKFLOW.jobs.ownership).with["ignore-install-scripts"], "true");
  assert.equal(WORKFLOW.jobs.summary.steps.find((step) => step.id === "verdict").if, "${{ github.event_name == 'workflow_dispatch' }}");
});

test("static summary requires all static jobs & refuses unexpected native execution", () => {
  const step = WORKFLOW.jobs.summary.steps.find((step) => step.name === "Static validation verdict");
  assert.equal(step.if, "${{ github.event_name != 'workflow_dispatch' }}");
  const code = step.run.match(/-e '([\s\S]+)'\s*$/)[1];
  const needs = Object.fromEntries(["private-repo-guard", "plan", "hygiene", "ownership"].map((name) => [name, { result: "success" }]));
  for (const name of ["shards-windows", "closure-windows", "shards-macos", "closure-macos"]) needs[name] = { result: "skipped" };
  const run = (input) => spawnSync(process.execPath, ["--input-type=module", "-e", code], { env: { ...process.env, NEEDS: JSON.stringify(input) }, encoding: "utf8" });
  assert.equal(run(needs).status, 0);
  assert.notEqual(run({ ...needs, hygiene: { result: "failure" } }).status, 0);
  assert.notEqual(run({ ...needs, "shards-windows": { result: "success" } }).status, 0);
});

test("Android compilation has no automatic trigger & validates correlation before setup", () => {
  const android = readYaml("../../.github/workflows/android.yml");
  assert.deepEqual(Object.keys(android.on), ["workflow_dispatch"]);
  for (const input of ["request_id", "expected_sha"]) assert.equal(android.on.workflow_dispatch.inputs[input].required, "true");
  const guard = android.jobs["cargo-ndk-check"].steps[0];
  assert.equal(guard.env.RK_EXPECTED_SHA, "${{ inputs.expected_sha }}");
  assert.match(guard.run, /test "\$RK_EXPECTED_SHA" = "\$GITHUB_SHA"/);
  assert.doesNotMatch(guard.run, /\$\{\{/);
});

test("shared setup keeps install enabled by default and caches only pnpm store", () => {
  assert.equal(ACTION.inputs["install-dependencies"].default, "true");
  assert.equal(ACTION.inputs["install-filter"].default, "");
  assert.equal(ACTION.inputs["ignore-install-scripts"].default, "false");
  const steps = ACTION.runs.steps;
  const install = steps.find((step) => step.name === "Install frozen pnpm dependencies");
  assert.equal(install.if, "${{ inputs.install-dependencies == 'true' }}");
  assert.match(install.run, /pnpm "\$\{args\[@\]\}"/);
  assert.match(install.run, /--frozen-lockfile/);
  assert.equal(install.env.RK_INSTALL_FILTER, "${{ inputs.install-filter }}");
  assert.equal(install.env.RK_IGNORE_INSTALL_SCRIPTS, "${{ inputs.ignore-install-scripts }}");
  const diagnostic = steps.find((step) => step.name === "Print pnpm-lock.yaml patch when the lockfile is stale");
  assert.match(diagnostic.run, /--lockfile-only/);
  assert.match(diagnostic.run, /pnpm "\$\{args\[@\]\}"/);
  assert.deepEqual(diagnostic.env, install.env);
  const cache = steps.find((step) => step.name === "Cache pnpm store");
  assert.equal(cache.uses, "actions/cache@0400d5f644dc74513175e3cd8d07132dd4860809");
  assert.equal(cache.with.path, "${{ steps.pnpm-store.outputs.path }}");
  assert.equal(cache.with.key, "${{ runner.os }}-pnpm-11.24.0-${{ hashFiles('pnpm-lock.yaml') }}");
  assert.equal(steps.find((step) => step.id === "pnpm-store").run, 'echo "path=$(pnpm store path)" >> "$GITHUB_OUTPUT"');
});

test("Swift cache targets SwiftPM build directories with scoped compatibility keys", () => {
  const cache = ACTION.runs.steps.find((step) => step.name === "Cache SwiftPM build products");
  assert.equal(cache.with.path.trimEnd(), ".build\nswift/RightKitSwift/.build\nswift/RightKitCloudKitHelper/.build");
  assert.match(cache.with.key, /rust-toolchain\.toml/);
  assert.match(cache.with.key, /Package\.resolved/);
  assert.match(cache.with.key, /Package\.swift/);
  assert.match(cache.with.key, /swift\/\*\*\/\*\.swift/);
  assert.doesNotMatch(cache.with.path, /DerivedData/);
});

test("CI omits workspace install only from Rust and Swift work", () => {
  const jobs = WORKFLOW.jobs;
  for (const name of ["shards-windows", "shards-macos"]) {
    assert.equal(setup(jobs[name]).with["install-dependencies"], "${{ matrix.kind == 'npm' || matrix.kind == 'root' }}");
  }
  for (const name of ["closure-windows", "closure-macos"]) assert.equal(setup(jobs[name]).with["install-dependencies"], "false");
  assert.equal(setup(jobs.hygiene).with["install-dependencies"], undefined);
  assert.equal(setup(jobs.hygiene).with["install-filter"], "--filter @rightkit/release...");
  assert.equal(setup(jobs.hygiene).with["ignore-install-scripts"], "true");
  for (const name of ["shards-windows", "shards-macos", "closure-windows", "closure-macos"]) {
    assert.match(setup(jobs[name]).with.rust, /matrix\.rust|true/);
  }
});

test("shared native matrices wait for hygiene while hygiene stays independent", () => {
  const jobs = WORKFLOW.jobs;
  assert.deepEqual(jobs.hygiene.needs, ["plan"]);
  for (const name of ["shards-windows", "shards-macos", "closure-windows", "closure-macos"]) {
    assert.deepEqual(jobs[name].needs, ["plan", "hygiene"]);
  }
  assert.ok(jobs.summary.needs.includes("hygiene"));
  for (const name of ["shards-windows", "shards-macos", "closure-windows", "closure-macos"]) assert.ok(jobs.summary.needs.includes(name));
});

test("main can finish while PRs coalesce; ownership stays required outside compile admission", () => {
  assert.equal(WORKFLOW.concurrency["cancel-in-progress"], "${{ github.event_name == 'pull_request' }}");
  const jobs = WORKFLOW.jobs;
  assert.deepEqual(jobs.ownership.needs, ["plan"]);
  assert.ok(!jobs.hygiene.steps.some((step) => step.run?.includes("right-release ownership")));
  assert.ok(jobs.ownership.steps.some((step) => step.run?.includes("right-release ownership --app .")));
  assert.ok(jobs.summary.needs.includes("ownership"));
  for (const host of ["windows", "macos"]) {
    const marker = jobs[`mark-green-${host}`];
    assert.ok(!marker.needs.includes("ownership"));
    assert.doesNotMatch(marker.if, /needs\.ownership/);
    for (const prerequisite of ["private-repo-guard", "plan", "hygiene", "summary", `shards-${host}`, `closure-${host}`]) assert.ok(marker.needs.includes(prerequisite));
    assert.ok(marker.if.includes(`needs.summary.outputs.${host}_green == 'true'`));
    assert.ok(marker.if.includes(`(needs.shards-${host}.result == 'success' || needs.closure-${host}.result == 'success')`));
    for (const prerequisite of ["private-repo-guard", "plan", "hygiene"]) assert.ok(marker.if.includes(`needs.${prerequisite}.result == 'success'`));
  }
  const scan = jobs.hygiene.steps.find((step) => step.name === "Secret scan (selected crates & packages)");
  assert.match(scan.if, /scan_packages != ''/);
  assert.match(scan.run, /--dirs "\$SCAN_PACKAGES"/);
  assert.equal(scan.env.SCAN_PACKAGES, "${{ needs.plan.outputs.scan_packages }}");
  assert.equal(jobs.plan.outputs.scan_packages, "${{ steps.plan.outputs.scan_packages }}");
});

test("ownership failure or omission blocks aggregate while empty compile baselines remain valid", () => {
  const scratch = fs.mkdtempSync(path.join(os.tmpdir(), "rk-summary-"));
  try {
    const outputs = {
      windows_shard_count: "0", windows_closure_count: "0", macos_shard_count: "0", macos_closure_count: "0", evidence: "[]",
    };
    const fixture = () => ({
      "private-repo-guard": { result: "success" }, plan: { result: "success", outputs }, hygiene: { result: "success" }, ownership: { result: "success" },
      "shards-windows": { result: "skipped" }, "closure-windows": { result: "skipped" }, "shards-macos": { result: "skipped" }, "closure-macos": { result: "skipped" },
    });
    for (const result of ["success", "failure", "cancelled", "skipped", undefined]) {
      const needs = fixture();
      if (result === undefined) delete needs.ownership; else needs.ownership.result = result;
      const out = path.join(scratch, "outputs"); fs.writeFileSync(out, "");
      const run = spawnSync(process.execPath, [fileURLToPath(new URL("./ci-summary.mjs", import.meta.url))], {
        encoding: "utf8", env: { ...process.env, NEEDS: JSON.stringify(needs), GITHUB_SHA: "a".repeat(40), GITHUB_OUTPUT: out, EVIDENCE_DIR: scratch },
      });
      assert.equal(run.status, result === "success" ? 0 : 1, run.stderr);
      assert.equal(fs.readFileSync(out, "utf8"), "windows_green=true\nmacos_green=true\n");
    }
  } finally { fs.rmSync(scratch, { recursive: true, force: true }); }
});

test("the evidence plan travels to the summary as the ci-plan artifact, never as a job output", () => {
  // Run 40034020934: a 132 KB `evidence` output made the summary step's NEEDS string too long to start bash.
  const plan = WORKFLOW.jobs.plan;
  assert.equal(plan.outputs.evidence, undefined);
  const upload = plan.steps.find((step) => step.name === "Upload evidence plan");
  assert.equal(upload.if, "${{ github.event_name == 'workflow_dispatch' }}");
  assert.equal(upload.with.name, "ci-plan");
  assert.equal(upload.with.path, "${{ runner.temp }}/ci-plan/evidence.json");
  assert.equal(upload.with["if-no-files-found"], "error");
  const summary = WORKFLOW.jobs.summary.steps;
  const download = summary.find((step) => step.with?.name === "ci-plan");
  assert.ok(download?.uses?.startsWith("actions/download-artifact@"));
  assert.equal(download.with.path, "ci-plan");
  assert.ok(summary.indexOf(download) < summary.findIndex((step) => step.id === "verdict"));
  assert.equal(summary.find((step) => step.id === "verdict").env.EVIDENCE_PLAN, "ci-plan/evidence.json");
  for (const job of Object.values(WORKFLOW.jobs)) for (const value of Object.values(job.outputs ?? {})) assert.doesNotMatch(String(value), /outputs\.evidence/);
});

test("shared baselines use authenticated Actions history rather than cache markers", () => {
  const plan = WORKFLOW.jobs.plan;
  assert.deepEqual(plan.permissions, { contents: "read", actions: "read" });
  const resolve = plan.steps.find((step) => step.name === "Resolve authenticated per-OS baseline");
  assert.equal(resolve.if, "github.event_name != 'pull_request'");
  assert.deepEqual(resolve.env, { GITHUB_TOKEN: "${{ github.token }}" });
  assert.equal(resolve.run, "node scripts/ci/last-green.mjs resolve --out last-green.json");
  assert.equal(plan.steps.find((step) => step.id === "plan").run, 'node scripts/ci/ci-plan.mjs --last-green "$RUNNER_TEMP/last-green.json" --evidence-out "$RUNNER_TEMP/ci-plan/evidence.json"');
  for (const job of [plan, WORKFLOW.jobs["mark-green-windows"], WORKFLOW.jobs["mark-green-macos"]]) {
    assert.ok(!job.steps.some((step) => step.uses?.startsWith("actions/cache")), "cache cannot transport green proof");
  }
});

test("shared CI and crates qualification run the same native Windows OpenSSL scripts", () => {
  const publish = readYaml("../../.github/workflows/publish.yml");
  const scripts = [
    ["Select native Windows Perl for vendored OpenSSL", "select-native-perl.ps1"],
    ["Locate native Windows OpenSSL for SQLCipher system crypto", "locate-native-openssl.ps1"],
  ];
  for (const [name, file] of scripts) {
    const call = `& "$env:GITHUB_WORKSPACE/scripts/ci/${file}"`;
    const shared = ACTION.runs.steps.find((step) => step.name === name);
    const qualification = publish.jobs.verify.steps.find((step) => step.name === name);
    assert.equal(shared.if, "${{ inputs.rust == 'true' && runner.os == 'Windows' }}");
    assert.equal(shared.run.trim(), call);
    assert.equal(qualification.if, "${{ needs.plan.outputs.registry == 'crates' && runner.os == 'Windows' }}");
    assert.equal(qualification.run.trim(), call);
    assert.equal(shared.shell, "pwsh");
    assert.equal(qualification.shell, "pwsh");
    const source = fs.readFileSync(new URL(`./${file}`, import.meta.url), "utf8");
    assert.ok(!/secrets|GITHUB_TOKEN|Invoke-WebRequest|Invoke-RestMethod/i.test(source), `${file} must stay offline and secret-free`);
  }
  const locate = fs.readFileSync(new URL("./locate-native-openssl.ps1", import.meta.url), "utf8");
  for (const name of ["OPENSSL_INCLUDE_DIR", "OPENSSL_LIB_DIR"]) assert.ok(locate.includes(name), name);
});
