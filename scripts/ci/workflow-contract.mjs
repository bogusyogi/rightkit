#!/usr/bin/env node
// The security contract of .github/workflows/publish.yml, asserted on the PARSED workflow (not on
// text patterns) and proven by mutation: MUTATIONS lists edits that each break one property, and
// `proveMutations` fails if any edit still passes. plan runs this before it plans anything:
//   node scripts/ci/workflow-contract.mjs .github/workflows/publish.yml
import fs from "node:fs";
import { fileURLToPath } from "node:url";
import { parseYaml } from "./workflow-yaml.mjs";

export const VERIFY_HOSTS = ["windows-2025", "xcode-27"];
const ALLOWED_UPLOAD_ACTIONS = ["actions/checkout@", "actions/setup-node@", "actions/download-artifact@"];
const ALLOWED_UPLOAD_RUN = ["node scripts/ci/upload.mjs run"];
const ALL_JOBS = ["guard", "plan", "pack", "verify", "seal-check", "upload"];

const same = (a, b) => JSON.stringify(a) === JSON.stringify(b);
const sorted = (list) => [...list].sort();
const stepsOf = (job) => (Array.isArray(job?.steps) ? job.steps : []);
const needsOf = (job) => (Array.isArray(job?.needs) ? job.needs : job?.needs ? [job.needs] : []);

function findKey(value, key, trail = "") {
  const hits = [];
  if (Array.isArray(value)) value.forEach((v, i) => hits.push(...findKey(v, key, `${trail}[${i}]`)));
  else if (value && typeof value === "object") {
    for (const [k, v] of Object.entries(value)) { if (k === key) hits.push(`${trail}.${k}`); hits.push(...findKey(v, key, `${trail}.${k}`)); }
  }
  return hits;
}

/** Throws one Error listing every violated property. `doc` is parseYaml output. */
export function assertPublishDoc(doc, name = "publish.yml") {
  const problems = [];
  const check = (ok, message) => { if (!ok) problems.push(message); };
  const jobs = doc?.jobs ?? {};

  // Triggers and ambient authority.
  check(doc?.on && typeof doc.on === "object" && same(Object.keys(doc.on), ["workflow_dispatch"]), "must be triggered by workflow_dispatch only");
  check(same(doc?.permissions, { contents: "read" }), "top-level permissions must be exactly contents: read");
  check(doc?.concurrency?.["cancel-in-progress"] === "false", "a publish must never be cancelled half-way");
  check(same(sorted(Object.keys(jobs)), sorted(ALL_JOBS)), `jobs must be exactly ${ALL_JOBS.join(", ")}`);
  const { guard, plan, pack, verify, upload } = jobs;
  const sealCheck = jobs["seal-check"];
  const rest = { ...doc }; delete rest.jobs;
  check(!/secrets\.|id-token/.test(JSON.stringify(rest)), "no secret or id-token outside a job");
  check(findKey(doc, "continue-on-error").length === 0, `continue-on-error is forbidden (${findKey(doc, "continue-on-error").join(", ")})`);
  check(findKey(doc, "pull_request_target").length === 0, "pull_request_target is forbidden");

  // The main-only, public-only guard, and every job behind it.
  const guardStep = stepsOf(guard)[0];
  check(guard && stepsOf(guard).length === 1 && guard.if === undefined && guardStep?.if === undefined, "guard must be one unconditional step");
  check(guardStep?.env?.REF === "${{ github.ref }}" && guardStep?.env?.PRIVATE === "${{ github.event.repository.private }}", "guard must read the ref and repository visibility from the event");
  check(/if \[ "\$REF" != "refs\/heads\/main" \]; then\s+echo [^\n]*\s+exit 1\s+fi/.test(guardStep?.run ?? ""), "guard must exit 1 unless the ref is refs/heads/main");
  check(/if \[ "\$PRIVATE" != "false" \]; then\s+echo [^\n]*\s+exit 1\s+fi/.test(guardStep?.run ?? ""), "guard must exit 1 unless the repository is public");
  const reaches = (job, target, seen = new Set()) => needsOf(jobs[job]).some((n) => n === target || (!seen.has(n) && seen.add(n) && reaches(n, target, seen)));
  for (const job of ALL_JOBS.filter((j) => j !== "guard")) check(reaches(job, "guard"), `${job} must run only after guard`);
  for (const [jobName, job] of Object.entries(jobs)) {
    check((job["timeout-minutes"] ?? "") !== "", `${jobName} must set timeout-minutes`);
  }

  // Both verify hosts exist and gate the upload.
  check(same(sorted(verify?.strategy?.matrix?.os ?? []), VERIFY_HOSTS), `verify must run on exactly ${VERIFY_HOSTS.join(" and ")}`);
  check(verify?.strategy?.matrix && !("exclude" in verify.strategy.matrix) && !("include" in verify.strategy.matrix), "verify matrix must not exclude or include legs");
  check(verify?.["runs-on"] === "${{ matrix.os }}", "verify must run on the matrix host");
  for (const needed of ["guard", "plan", "pack"]) check(needsOf(verify).includes(needed), `verify must need ${needed}`);
  for (const needed of ["verify", "pack", "plan"]) check(needsOf(sealCheck).includes(needed), `seal-check must need ${needed}`);
  for (const needed of ["guard", "plan", "pack", "verify", "seal-check"]) check(needsOf(upload).includes(needed), `upload must need ${needed}`);
  for (const [jobName, job] of [["verify", verify], ["pack", pack], ["seal-check", sealCheck]]) {
    check(job?.if === "${{ needs.plan.outputs.count != '0' }}", `${jobName} may be skipped only when the plan is empty`);
  }
  const verifyRuns = stepsOf(verify).map((s) => s.run ?? "");
  const gateAt = verifyRuns.findIndex((r) => /node scripts\/ci\/gate\.mjs --registry /.test(r) && /--allow-host-exclusion/.test(r) && /--allow-no-test/.test(r));
  const receiptAt = verifyRuns.findIndex((r) => /node scripts\/ci\/publish\.mjs verify/.test(r));
  check(gateAt >= 0 && receiptAt > gateAt, "verify must run the unit gate, then publish.mjs verify, on every host");
  check(stepsOf(verify).some((s) => /actions\/download-artifact@/.test(s.uses ?? "") && s.with?.name === "publish-packed"), "verify must download the sealed publish-packed artifact");
  check(stepsOf(pack).some((s) => /node scripts\/ci\/publish\.mjs pack/.test(s.run ?? "")) && stepsOf(pack).some((s) => /actions\/upload-artifact@/.test(s.uses ?? "") && s.with?.name === "publish-packed"), "pack must seal and upload publish-packed");
  check(stepsOf(plan).some((s) => /node scripts\/ci\/workflow-contract\.mjs/.test(s.run ?? "")), "plan must run this contract before planning");

  // Secrets and id-token exist only in upload, and nothing else is privileged.
  for (const [jobName, job] of Object.entries(jobs)) {
    if (jobName === "upload") continue;
    const text = JSON.stringify(job);
    check(!/secrets\./.test(text), `${jobName} must not reference a secret`);
    check(!/id-token/.test(text), `${jobName} must not request id-token`);
    check(job.environment === undefined, `${jobName} must not use an environment`);
    check(job.permissions === undefined || same(job.permissions, { contents: "read" }), `${jobName} permissions must be contents: read`);
  }
  check(upload?.environment === "release", "upload must use environment: release");
  check(same(upload?.permissions, { contents: "read", "id-token": "write" }), "upload permissions must be exactly contents: read and id-token: write");
  const gate = String(upload?.if ?? "");
  check(gate.includes("needs.plan.outputs.dry_run == 'false'") && gate.includes("github.ref == 'refs/heads/main'"), "upload must run only for a real publish from main");
  check(!/\b(always|failure|cancelled|success)\(/.test(gate), "upload's condition must not override the needs result");
  check(upload?.["runs-on"] === "ubuntu-24.04", "upload runs on a fresh hosted runner");
  const tokenSteps = [];
  for (const step of stepsOf(upload)) {
    if (step.uses) check(ALLOWED_UPLOAD_ACTIONS.some((a) => step.uses.startsWith(a)) || /^actions\/upload-artifact@/.test(step.uses), `upload may not use ${step.uses}`);
    if (step.run !== undefined) check(ALLOWED_UPLOAD_RUN.includes(step.run.trim()), `upload may run only ${ALLOWED_UPLOAD_RUN.join(", ")}, not ${JSON.stringify(step.run.trim().slice(0, 60))}`);
    if (/secrets\./.test(JSON.stringify(step))) tokenSteps.push(step);
  }
  const checkout = stepsOf(upload).find((s) => /^actions\/checkout@/.test(s.uses ?? ""));
  check(checkout?.with?.["sparse-checkout"] === "scripts/ci/upload.mjs" && checkout?.with?.["sparse-checkout-cone-mode"] === "false", "upload must check out scripts/ci/upload.mjs and nothing else");
  const byVar = Object.fromEntries(tokenSteps.flatMap((s) => Object.entries(s.env ?? {}).filter(([, v]) => /secrets\./.test(v)).map(([k, v]) => [k, { value: v, step: s }])));
  check(same(sorted(Object.keys(byVar)), ["CARGO_REGISTRY_TOKEN", "NODE_AUTH_TOKEN"]), "upload must read exactly NODE_AUTH_TOKEN and CARGO_REGISTRY_TOKEN");
  check(byVar.NODE_AUTH_TOKEN?.value === "${{ secrets.NPM_TOKEN }}" && byVar.CARGO_REGISTRY_TOKEN?.value === "${{ secrets.CARGO_REGISTRY_TOKEN }}", "tokens must come from secrets.NPM_TOKEN and secrets.CARGO_REGISTRY_TOKEN");
  check(tokenSteps.length === 2 && tokenSteps.every((s) => Object.values(s.env ?? {}).filter((v) => /secrets\./.test(v)).length === 1), "each upload step may read exactly one token");
  check(byVar.NODE_AUTH_TOKEN?.step?.if === "${{ needs.plan.outputs.registry == 'npm' }}" && byVar.CARGO_REGISTRY_TOKEN?.step?.if === "${{ needs.plan.outputs.registry == 'crates' }}", "each token step must run only for its own registry");
  check(tokenSteps.every((s) => s.run?.trim() === "node scripts/ci/upload.mjs run"), "a token step may only run the upload script");

  // The dry run: same script, no secret, no id-token, nothing sent.
  check(stepsOf(sealCheck).some((s) => s.run?.trim() === "node scripts/ci/upload.mjs run --dry-run"), "seal-check must run the upload script with --dry-run");
  check(stepsOf(sealCheck).every((s) => !/upload\.mjs run\s*$/.test(s.run ?? "")), "seal-check must never run the upload script without --dry-run");
  const sealCheckout = stepsOf(sealCheck).find((s) => /^actions\/checkout@/.test(s.uses ?? ""));
  check(sealCheckout?.with?.["sparse-checkout"] === "scripts/ci/upload.mjs", "seal-check must check out only the upload script");

  // Hygiene on every job.
  for (const [jobName, job] of Object.entries(jobs)) {
    for (const step of stepsOf(job)) {
      if (step.uses) {
        check(/@[0-9a-f]{40}(?:\s|$)/.test(step.uses) || /@[0-9a-f]{40}$/.test(step.uses), `${jobName}: ${step.uses} must be pinned to a 40-hex commit`);
        if (/^actions\/checkout@/.test(step.uses)) check(step.with?.["persist-credentials"] === "false", `${jobName}: checkout must set persist-credentials: false`);
      }
      if (step.run !== undefined) check(!/\$\{\{/.test(step.run), `${jobName}: run text must not interpolate expressions; pass values through env`);
    }
  }
  if (problems.length) throw new Error(`${name} violates the publish contract:\n  - ${problems.join("\n  - ")}`);
  return true;
}

export function assertPublishWorkflow(text, name = "publish.yml") { return assertPublishDoc(parseYaml(text), name); }

const edit = (fn) => (doc) => { fn(doc.jobs, doc); };
const actionStep = (uses) => ({ name: "injected", uses });

/** Each mutation breaks exactly one property; the contract must reject every one. */
export const MUTATIONS = [
  ["drop the Windows verify host", edit((j) => { j.verify.strategy.matrix.os = ["xcode-27"]; })],
  ["drop the macOS verify host", edit((j) => { j.verify.strategy.matrix.os = ["windows-2025"]; })],
  ["exclude a verify leg", edit((j) => { j.verify.strategy.matrix.exclude = [{ os: "windows-2025" }]; })],
  ["upload no longer needs verify", edit((j) => { j.upload.needs = j.upload.needs.filter((n) => n !== "verify"); })],
  ["upload no longer needs seal-check", edit((j) => { j.upload.needs = j.upload.needs.filter((n) => n !== "seal-check"); })],
  ["seal-check no longer needs verify", edit((j) => { j["seal-check"].needs = j["seal-check"].needs.filter((n) => n !== "verify"); })],
  ["continue-on-error on a job", edit((j) => { j.verify["continue-on-error"] = "true"; })],
  ["continue-on-error on a step", edit((j) => { j.verify.steps[0]["continue-on-error"] = "true"; })],
  ["guard allows another branch", edit((j) => { j.guard.steps[0].run = j.guard.steps[0].run.replace('"$REF" != "refs/heads/main"', '"$REF" != "refs/heads/dev"'); })],
  ["guard accepts a private repository", edit((j) => { j.guard.steps[0].run = j.guard.steps[0].run.replace('"$PRIVATE" != "false"', '"$PRIVATE" != "maybe"'); })],
  ["guard no longer exits", edit((j) => { j.guard.steps[0].run = j.guard.steps[0].run.replaceAll("exit 1", "true"); })],
  ["plan escapes the guard", edit((j) => { j.plan.needs = []; })],
  ["verify requests id-token", edit((j) => { j.verify.permissions = { contents: "read", "id-token": "write" }; })],
  ["pack reads a secret", edit((j) => { j.pack.steps.at(-2).env = { ...(j.pack.steps.at(-2).env ?? {}), NODE_AUTH_TOKEN: "${{ secrets.NPM_TOKEN }}" }; })],
  ["verify reads a secret", edit((j) => { j.verify.steps.at(-3).env = { ...(j.verify.steps.at(-3).env ?? {}), CARGO_REGISTRY_TOKEN: "${{ secrets.CARGO_REGISTRY_TOKEN }}" }; })],
  ["seal-check reads a secret", edit((j) => { j["seal-check"].steps.find((s) => s.run).env.NODE_AUTH_TOKEN = "${{ secrets.NPM_TOKEN }}"; })],
  ["seal-check joins the release environment", edit((j) => { j["seal-check"].environment = "release"; })],
  ["seal-check requests id-token", edit((j) => { j["seal-check"].permissions = { contents: "read", "id-token": "write" }; })],
  ["upload installs dependencies", edit((j) => { j.upload.steps.splice(2, 0, { name: "install", run: "pnpm install --frozen-lockfile" }); })],
  ["upload builds", edit((j) => { j.upload.steps.splice(2, 0, { name: "build", run: "cargo build --release" }); })],
  ["upload adds pnpm", edit((j) => { j.upload.steps.splice(2, 0, actionStep("pnpm/action-setup@0977fd99725f1db4007ccb2928dbb4e90d06cc86")); })],
  ["upload adds a Rust toolchain", edit((j) => { j.upload.steps.splice(2, 0, actionStep("dtolnay/rust-toolchain@f8be11a05b1d4f3fcebe6410cc16743212b999b0")); })],
  ["upload checks out the whole workspace", edit((j) => { delete j.upload.steps[0].with["sparse-checkout"]; })],
  ["upload loses its environment", edit((j) => { delete j.upload.environment; })],
  ["upload loses id-token", edit((j) => { j.upload.permissions = { contents: "read" }; })],
  ["upload runs even after failures", edit((j) => { j.upload.if = "${{ always() }}"; })],
  ["upload no longer needs a real publish", edit((j) => { j.upload.if = "${{ github.ref == 'refs/heads/main' }}"; })],
  ["upload no longer needs main", edit((j) => { j.upload.if = "${{ needs.plan.outputs.dry_run == 'false' }}"; })],
  ["one upload step reads both tokens", edit((j) => { const s = j.upload.steps.find((x) => x.env?.NODE_AUTH_TOKEN); s.env.CARGO_REGISTRY_TOKEN = "${{ secrets.CARGO_REGISTRY_TOKEN }}"; })],
  ["a token step runs for either registry", edit((j) => { delete j.upload.steps.find((x) => x.env?.NODE_AUTH_TOKEN).if; })],
  ["the npm token comes from another secret", edit((j) => { j.upload.steps.find((x) => x.env?.NODE_AUTH_TOKEN).env.NODE_AUTH_TOKEN = "${{ secrets.OTHER }}"; })],
  ["top-level permissions widen", edit((_, d) => { d.permissions = { contents: "write" }; })],
  ["a push trigger appears", edit((_, d) => { d.on.push = { branches: ["main"] }; })],
  ["pull_request_target appears", edit((_, d) => { d.on.pull_request_target = {}; })],
  ["an action is unpinned", edit((j) => { j.verify.steps[0].uses = "actions/checkout@v7"; })],
  ["a checkout keeps credentials", edit((j) => { j.verify.steps[0].with["persist-credentials"] = "true"; })],
  ["an input is interpolated into a script", edit((j) => { j.verify.steps.at(-3).run = 'echo "${{ inputs.units }}"'; })],
  ["plan stops running the contract", edit((j) => { j.plan.steps = j.plan.steps.filter((s) => !/workflow-contract/.test(s.run ?? "")); })],
  ["verify skips the unit gate", edit((j) => { j.verify.steps = j.verify.steps.filter((s) => !/gate\.mjs/.test(s.run ?? "")); })],
  ["verify skips the receipt", edit((j) => { j.verify.steps = j.verify.steps.filter((s) => !/publish\.mjs verify/.test(s.run ?? "")); })],
  ["verify may be skipped by a condition", edit((j) => { j.verify.if = "${{ always() }}"; })],
  ["a job has no timeout", edit((j) => { delete j.pack["timeout-minutes"]; })],
];

/** Returns the names of mutations the contract failed to reject (empty when every one is caught). */
export function proveMutations(doc) {
  assertPublishDoc(doc);
  const survivors = [];
  for (const [name, mutate] of MUTATIONS) {
    const copy = structuredClone(doc);
    try { mutate(copy); } catch (error) { throw new Error(`mutation "${name}" could not be applied: ${error.message}`); }
    try { assertPublishDoc(copy); survivors.push(name); } catch { /* rejected, as required */ }
  }
  return survivors;
}

if (process.argv[1] && fs.realpathSync(process.argv[1]) === fs.realpathSync(fileURLToPath(import.meta.url))) {
  const file = process.argv[2];
  if (!file) { process.stderr.write("usage: workflow-contract.mjs <publish.yml>\n"); process.exit(2); }
  try {
    const doc = parseYaml(fs.readFileSync(file, "utf8"));
    assertPublishDoc(doc, file);
    const survivors = proveMutations(doc);
    if (survivors.length) throw new Error(`the contract accepts these violations: ${survivors.join("; ")}`);
    process.stdout.write(`${file}: publish contract holds; ${MUTATIONS.length} mutations rejected\n`);
  } catch (error) { process.stderr.write(`${error.message}\n`); process.exit(1); }
}
