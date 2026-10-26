#!/usr/bin/env node
// The class A scan contract of .github/workflows/ci.yml (rightkit#888), asserted on the PARSED workflow and proven by mutation.
//   node scripts/ci/ci-scans-contract.mjs .github/workflows/ci.yml
// Rules:
//   1. the hygiene job installs the pinned cargo-deny, cargo-audit, checksum-verified actionlint and checksum- and
//      version-verified ShellCheck (the version class A accepts: PINNED_SCAN_TOOLS) before it scans;
//   2. the hygiene job runs the scans for EVERY change: one unconditional `gate.mjs --steps scans` step with the plan's
//      changed-path base (scan_base), not gated on Rust tokens;
//   3. every job that runs class B (shards, closures) needs hygiene and receives its result as RK_SCANS_JOB_RESULT, so a job
//      that did not run the scans can only proceed on the hygiene job's success.
import fs from "node:fs";
import { fileURLToPath } from "node:url";
import { parseYaml } from "./workflow-yaml.mjs";

export const SCAN_TOOLS_ACTION = "taiki-e/install-action@8cca0918f8471c33515202b73f56ab27f2bed283";
export const SCAN_TOOLS = "cargo-deny@0.20.2,cargo-audit@0.22.2";
export const ACTIONLINT_ENV = { ACTIONLINT_VERSION: "1.7.12", ACTIONLINT_SHA256: "8aca8db96f1b94770f1b0d72b6dddcb1ebb8123cb3712530b08cc387b349a3d8" };
// The ShellCheck class A accepts (packages/release/deterministic-scans.mjs PINNED_SCAN_TOOLS; scan-receipt.test.mjs proves they agree).
export const SHELLCHECK_ENV = { SHELLCHECK_VERSION: "0.11.0", SHELLCHECK_SHA256: "8c3be12b05d5c177a04c29e3c78ce89ac86f1595681cab149b65b97c4e227198" };
export const SCANS_RUN = 'node scripts/ci/gate.mjs --select "$HYGIENE_TOKENS" --steps scans ${SCAN_BASE:+--base "$SCAN_BASE"}';
export const SCANS_ENV = { HYGIENE_TOKENS: "${{ needs.plan.outputs.hygiene_tokens }}", SCAN_BASE: "${{ needs.plan.outputs.scan_base }}" };
export const SCANS_RESULT_ENV = "${{ needs.hygiene.result }}";
export const CLASS_B_JOBS = ["shards-windows", "shards-macos", "closure-windows", "closure-macos"];

const stepsOf = (job) => (Array.isArray(job?.steps) ? job.steps : []);
const needsOf = (job) => (Array.isArray(job?.needs) ? job.needs : job?.needs ? [job.needs] : []);
const same = (a, b) => JSON.stringify(a) === JSON.stringify(b);

/** Throws one Error listing every violated property. `doc` is parseYaml output. */
export function assertCiScansDoc(doc, name = "ci.yml") {
  const problems = [];
  const check = (ok, message) => { if (!ok) problems.push(message); };
  const jobs = doc?.jobs ?? {};
  const hygiene = jobs.hygiene;
  const steps = stepsOf(hygiene);

  const tools = steps.filter((s) => (s.uses ?? "").startsWith("taiki-e/install-action@"));
  check(tools.length === 1 && tools[0].uses.trim() === SCAN_TOOLS_ACTION, `hygiene must use the SHA-pinned ${SCAN_TOOLS_ACTION}`);
  check(tools.length === 1 && tools[0].with?.tool === SCAN_TOOLS && same(Object.keys(tools[0].with ?? {}), ["tool"]) && tools[0].if === undefined, `hygiene must install exactly ${SCAN_TOOLS}, unconditionally`);
  const lint = steps.filter((s) => /actionlint/.test(s.run ?? "") && /sha256sum --check --strict/.test(s.run ?? ""));
  check(lint.length === 1 && same(lint[0].env, ACTIONLINT_ENV) && lint[0].if === undefined && /releases\/download\/v\$\{ACTIONLINT_VERSION\}\//.test(lint[0].run), "hygiene must install the checksum-verified pinned actionlint, unconditionally");
  const sc = steps.filter((s) => /SHELLCHECK_VERSION/.test(s.run ?? ""));
  check(sc.length === 1 && same(sc[0].env, SHELLCHECK_ENV) && sc[0].if === undefined && /releases\/download\/v\$\{SHELLCHECK_VERSION\}\/shellcheck-v\$\{SHELLCHECK_VERSION\}\.linux\.x86_64\.tar\.xz/.test(sc[0].run) && /sha256sum --check --strict/.test(sc[0].run) && /version: \$\{SHELLCHECK_VERSION\}/.test(sc[0].run) && /\$RUNNER_TEMP\/shellcheck-bin.*\$GITHUB_PATH/.test(sc[0].run), "hygiene must checksum-verify, version-check & expose the pinned ShellCheck, unconditionally");

  const scanSteps = steps.filter((s) => /gate\.mjs/.test(s.run ?? "") && /--steps\s+scans\b/.test(s.run ?? ""));
  check(scanSteps.length === 1, "hygiene must run exactly one gate.mjs --steps scans step");
  const scan = scanSteps[0];
  check(scan?.run?.trim() === SCANS_RUN && same(scan?.env, SCANS_ENV), "the scans step must run the fixed gate command with the plan's hygiene tokens and scan_base");
  check(scan !== undefined && scan.if === undefined, "the scans step must run for every change: no `if` (no token, path or event condition)");
  check(scan !== undefined && steps.indexOf(scan) > steps.indexOf(tools[0]) && steps.indexOf(scan) > steps.indexOf(lint[0]) && steps.indexOf(scan) > steps.indexOf(sc[0]), "the scan tools must be installed before the scans step");
  check(jobs.plan?.outputs?.scan_base === "${{ steps.plan.outputs.scan_base }}", "plan must expose scan_base (the changed-path base of every runner)");
  check(same(needsOf(hygiene), ["plan"]) && hygiene?.if === undefined, "hygiene must need plan only and run unconditionally");

  for (const jobName of CLASS_B_JOBS) {
    const job = jobs[jobName];
    check(job !== undefined && needsOf(job).includes("hygiene"), `${jobName} must need hygiene (success)`);
    check(!/(?:always|cancelled|failure)\(\)/.test(String(job?.if ?? "")), `${jobName} must not override the implicit success of its needs (no always(), cancelled() or failure())`);
    const gates = stepsOf(job).filter((s) => /gate\.mjs/.test(s.run ?? ""));
    check(gates.length === 1 && gates[0].env?.RK_SCANS_JOB_RESULT === SCANS_RESULT_ENV, `${jobName} must pass the hygiene result to its gate step as RK_SCANS_JOB_RESULT`);
    check(gates.every((s) => !/--steps\s+[^\s]*scans/.test(s.run ?? "")), `${jobName} must not claim to run the scans step`);
  }
  for (const [jobName, job] of Object.entries(jobs)) {
    if (CLASS_B_JOBS.includes(jobName) || jobName === "hygiene") continue;
    // A gate.mjs step with no --steps runs the default steps, which include test and closure (class B): it counts as class B.
    check(!stepsOf(job).some((s) => /gate\.mjs/.test(s.run ?? "") && (!/--steps\s/.test(s.run ?? "") || /--steps\s+[^\s]*(?:test|closure)/.test(s.run ?? ""))), `${jobName} runs class B gate steps but is not a hygiene-gated shard or closure job`);
  }
  if (problems.length) throw new Error(`${name} scan contract violated:\n- ${problems.join("\n- ")}`);
}

const edit = (fn) => (doc) => { fn(doc.jobs, doc); };
const hyg = (j) => j.hygiene.steps;
const scanStep = (j) => hyg(j).find((s) => /--steps scans/.test(s.run ?? ""));
export const MUTATIONS = [
  ["cargo-deny and cargo-audit are no longer installed", edit((j) => { j.hygiene.steps = hyg(j).filter((s) => !(s.uses ?? "").startsWith("taiki-e/")); })],
  ["the scan tools are unpinned", edit((j) => { hyg(j).find((s) => (s.uses ?? "").startsWith("taiki-e/")).with.tool = "cargo-deny,cargo-audit"; })],
  ["the scan tool action is not SHA pinned", edit((j) => { hyg(j).find((s) => (s.uses ?? "").startsWith("taiki-e/")).uses = "taiki-e/install-action@v2"; })],
  ["actionlint is no longer installed", edit((j) => { j.hygiene.steps = hyg(j).filter((s) => !/actionlint/.test(s.run ?? "")); })],
  ["actionlint loses its checksum", edit((j) => { const s = hyg(j).find((x) => /actionlint/.test(x.run ?? "")); s.run = s.run.replace("sha256sum --check --strict", "true"); })],
  ["the actionlint checksum changes", edit((j) => { hyg(j).find((x) => /actionlint/.test(x.run ?? "")).env.ACTIONLINT_SHA256 = "0".repeat(64); })],
  ["ShellCheck is no longer installed (the runner image's ShellCheck would judge the scripts)", edit((j) => { j.hygiene.steps = hyg(j).filter((s) => !/SHELLCHECK_VERSION/.test(s.run ?? "")); })],
  ["ShellCheck loses its checksum", edit((j) => { const s = hyg(j).find((x) => /SHELLCHECK_VERSION/.test(x.run ?? "")); s.run = s.run.replace("sha256sum --check --strict", "true"); })],
  ["ShellCheck is pinned to another version", edit((j) => { hyg(j).find((x) => /SHELLCHECK_VERSION/.test(x.run ?? "")).env.SHELLCHECK_VERSION = "0.10.0"; })],
  ["the scans step is removed (npm, workflow and docs changes skip the scans)", edit((j) => { j.hygiene.steps = hyg(j).filter((s) => s !== scanStep(j)); })],
  ["the scans step is gated on Rust tokens", edit((j) => { scanStep(j).if = "${{ !cancelled() && needs.plan.outputs.hygiene_tokens != '' }}"; })],
  ["the scans step is gated on the event", edit((j) => { scanStep(j).if = "${{ github.event_name == 'push' }}"; })],
  ["the scans step loses the changed-path base", edit((j) => { const s = scanStep(j); s.run = s.run.replace(' ${SCAN_BASE:+--base "$SCAN_BASE"}', ""); })],
  ["the scans step reads another base", edit((j) => { scanStep(j).env.SCAN_BASE = "${{ needs.plan.outputs.windows_base }}"; })],
  ["plan stops exposing scan_base", edit((j) => { delete j.plan.outputs.scan_base; })],
  ["the scans step moves before the tools", edit((j) => { const s = scanStep(j); j.hygiene.steps = [s, ...hyg(j).filter((x) => x !== s)]; })],
  ["hygiene becomes conditional", edit((j) => { j.hygiene.if = "${{ needs.plan.outputs.hygiene_tokens != '' }}"; })],
  ...CLASS_B_JOBS.flatMap((name) => [
    [`${name} stops needing hygiene`, edit((j) => { j[name].needs = j[name].needs.filter((n) => n !== "hygiene"); })],
    [`${name} runs even when hygiene failed`, edit((j) => { j[name].if = `\${{ always() && ${String(j[name].if).replace(/^\$\{\{\s*|\s*\}\}$/g, "")} }}`; })],
    [`${name} stops receiving the hygiene result`, edit((j) => { const g = j[name].steps.find((s) => /gate\.mjs/.test(s.run ?? "")); delete g.env.RK_SCANS_JOB_RESULT; })],
    [`${name} receives another result`, edit((j) => { const g = j[name].steps.find((s) => /gate\.mjs/.test(s.run ?? "")); g.env.RK_SCANS_JOB_RESULT = "success"; })],
  ]),
  ["a new job runs class B without hygiene", edit((j) => { j.extra = { needs: ["plan"], "runs-on": "ubuntu-24.04", steps: [{ run: 'node scripts/ci/gate.mjs --select "x" --steps test' }] }; })],
];

/** Returns the names of mutations assertCiScansDoc failed to reject. */
export function proveCiScanMutations(doc) {
  assertCiScansDoc(doc);
  const survivors = [];
  for (const [name, mutate] of MUTATIONS) {
    const copy = structuredClone(doc);
    try { mutate(copy); } catch (error) { throw new Error(`ci scan mutation "${name}" could not be applied: ${error.message}`); }
    try { assertCiScansDoc(copy); survivors.push(name); } catch { /* rejected, as required */ }
  }
  return survivors;
}

if (process.argv[1] && fs.realpathSync(process.argv[1]) === fs.realpathSync(fileURLToPath(import.meta.url))) {
  const file = process.argv[2];
  if (!file) { process.stderr.write("usage: ci-scans-contract.mjs <ci.yml>\n"); process.exit(2); }
  try {
    const doc = parseYaml(fs.readFileSync(file, "utf8"));
    const survivors = proveCiScanMutations(doc);
    if (survivors.length) throw new Error(`the contract accepts these violations: ${survivors.join("; ")}`);
    process.stdout.write(`${file}: scan contract holds; ${MUTATIONS.length} mutations rejected\n`);
  } catch (error) { process.stderr.write(`${error.message}\n`); process.exit(1); }
}
