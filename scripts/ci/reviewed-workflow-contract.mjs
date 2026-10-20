import assert from "node:assert/strict";
import fs from "node:fs";
import { fileURLToPath } from "node:url";
import { parseYaml } from "./workflow-yaml.mjs";

const CHECKOUT = "actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1";
const NODE = "actions/setup-node@820762786026740c76f36085b0efc47a31fe5020";
const ARTIFACT = "actions/upload-artifact@ea165f8d65b6e75b540449e92b4886f43607fa02";
export const REVIEWED_CONCURRENCY = "publish";
export const MAIN_GUARD = "${{ github.ref == 'refs/heads/main' && github.repository == 'bogusyogi/rightkit' }}";
export const REVIEWED_UPLOAD_IF = "${{ needs.seal-check.result == 'success' && github.ref == 'refs/heads/main' && github.repository == 'bogusyogi/rightkit' }}";
export const REVIEWED_SPARSE = "scripts/ci/upload.mjs\nscripts/ci/registry-spec.mjs\nscripts/ci/reviewed-candidate.mjs\nscripts/ci/supplemental-proof.mjs\nci/approved-artifacts.json\nci/supplemental-proofs.json\nci/linux-required.json\n";
const keys = (object, expected) => assert.deepEqual(Object.keys(object).sort(), [...expected].sort());

// Resolve declarations across actual YAML boundaries; behavioral tests prove writes.
export function assertWorkflowOutputReferences(doc) {
  for (const [jobId, job] of Object.entries(doc.jobs ?? {})) {
    const ids = new Set();
    for (const step of job.steps ?? []) if (step.id) {
      assert.ok(!ids.has(step.id), `${jobId}: duplicate step id ${step.id}`);
      ids.add(step.id);
    }
    const serialized = JSON.stringify(job);
    for (const match of serialized.matchAll(/\bneeds\.([\w-]+)\.outputs\.([\w-]+)/g)) {
      const [, producer, output] = match;
      const needs = typeof job.needs === "string" ? [job.needs] : job.needs ?? [];
      assert.ok(needs.includes(producer), `${jobId}: output producer ${producer} missing from needs`);
      assert.ok(Object.hasOwn(doc.jobs?.[producer]?.outputs ?? {}, output), `${jobId}: undeclared output ${producer}.${output}`);
    }
    for (const match of serialized.matchAll(/\bsteps\.([\w-]+)\.outputs\.([\w-]+)/g)) {
      assert.ok(ids.has(match[1]), `${jobId}: missing output-producing step ${match[1]}`);
    }
  }
  return true;
}

export function assertReviewedDoc(doc) {
  assertWorkflowOutputReferences(doc);
  keys(doc, ["name", "on", "permissions", "concurrency", "jobs"]);
  assert.equal(doc.name, "publish-reviewed");
  keys(doc.on, ["workflow_dispatch"]); keys(doc.on.workflow_dispatch, ["inputs"]);
  assert.deepEqual(doc.on.workflow_dispatch.inputs, {
    candidate_id: { description: "Committed schema 2 candidate ID", type: "string", required: "true" },
    dry_run: { description: "Authenticate semantic admission & transfer without uploading", type: "boolean", default: "true" },
  });
  assert.deepEqual(doc.permissions, { contents: "read" });
  assert.deepEqual(doc.concurrency, { group: REVIEWED_CONCURRENCY, queue: "max", "cancel-in-progress": "false" });
  keys(doc.jobs, ["seal-check", "upload"]);
  const check = doc.jobs["seal-check"]; const upload = doc.jobs.upload;
  keys(check, ["if", "runs-on", "timeout-minutes", "permissions", "outputs", "steps"]);
  keys(upload, ["needs", "if", "environment", "runs-on", "timeout-minutes", "permissions", "steps"]);
  assert.equal(check.if, MAIN_GUARD); assert.equal(upload.if, REVIEWED_UPLOAD_IF);
  assert.deepEqual(upload.needs, ["seal-check"]); assert.equal(upload.environment, "release");
  assert.deepEqual(check.permissions, { contents: "read", actions: "read" });
  assert.deepEqual(upload.permissions, { contents: "read", actions: "read", "id-token": "write" });
  assert.deepEqual(check.outputs, {
    registry: "${{ steps.identify.outputs.registry }}",
    admission_id: "${{ steps.admission.outputs.artifact-id }}",
    admission_digest: "${{ steps.admission.outputs.artifact-digest }}",
  });
  assert.equal(check.steps.length, 8); assert.equal(upload.steps.length, 7);
  for (const job of [check, upload]) {
    assert.equal(job["runs-on"], "ubuntu-24.04"); assert.ok(Number(job["timeout-minutes"]) > 0);
    assert.deepEqual(job.steps[1], { uses: NODE, with: { "node-version": "26.8.1", "package-manager-cache": "false" } });
    for (const step of job.steps) {
      if (step.run) assert.ok(!step.run.includes("${{"), "dispatch values cannot enter shell source");
      if (step.uses) assert.match(step.uses, /@[0-9a-f]{40}$/);
    }
  }
  assert.deepEqual(check.steps[0], { uses: CHECKOUT, with: { ref: "${{ github.sha }}", path: "verifier", "persist-credentials": "false" } });
  assert.deepEqual(upload.steps[0], { uses: CHECKOUT, with: { ref: "${{ github.sha }}", "persist-credentials": "false", "sparse-checkout": REVIEWED_SPARSE, "sparse-checkout-cone-mode": "false" } });
  assert.deepEqual(check.steps[2], { uses: "pnpm/action-setup@0977fd99725f1db4007ccb2928dbb4e90d06cc86", with: { version: "11.24.0", package_json_file: "verifier/package.json", run_install: "false" } });
  keys(check.steps[3], ["name", "run"]);
  assert.equal(check.steps[3].run, "pnpm --dir verifier install --filter rightkit-shared --filter @rightkit/release... --frozen-lockfile --ignore-scripts");
  const baseEnv = { GITHUB_TOKEN: "${{ github.token }}", PUBLISH_CANDIDATE_ID: "${{ inputs.candidate_id }}" };
  keys(check.steps[4], ["name", "id", "env", "run"]);
  assert.equal(check.steps[4].id, "identify"); assert.deepEqual(check.steps[4].env, baseEnv);
  assert.equal(check.steps[4].run, "node verifier/scripts/ci/reviewed-candidate.mjs identify");
  assert.deepEqual(check.steps[5], { uses: CHECKOUT, with: { ref: "${{ steps.identify.outputs.source_sha }}", path: "candidate", "fetch-depth": "0", "persist-credentials": "false", submodules: "false" } });
  keys(check.steps[6], ["name", "id", "env", "run"]);
  assert.equal(check.steps[6].id, "check"); assert.deepEqual(check.steps[6].env, { ...baseEnv, PUBLISH_SOURCE_DIR: "${{ github.workspace }}/candidate" });
  assert.equal(check.steps[6].run, "node verifier/scripts/ci/reviewed-candidate.mjs run --dry-run");
  const transferEnv = { ...baseEnv, PUBLISH_ADMISSION_ID: "${{ needs.seal-check.outputs.admission_id }}", PUBLISH_ADMISSION_DIGEST: "${{ needs.seal-check.outputs.admission_digest }}" };
  keys(upload.steps[2], ["name", "env", "run"]);
  assert.deepEqual(upload.steps[2].env, transferEnv);
  assert.equal(upload.steps[2].run, "node scripts/ci/reviewed-candidate.mjs run --verify-admission");
  for (const [index, registry, variable, secret] of [[3, "npm", "NODE_AUTH_TOKEN", "NPM_TOKEN"], [4, "crates", "CARGO_REGISTRY_TOKEN", "CARGO_REGISTRY_TOKEN"]]) {
    const step = upload.steps[index]; keys(step, ["name", "if", "env", "run"]);
    assert.equal(step.if, `\${{ !inputs.dry_run && needs.seal-check.outputs.registry == '${registry}' }}`);
    assert.deepEqual(step.env, { ...transferEnv, [variable]: `\${{ secrets.${secret} }}` });
    assert.equal(step.run, "node scripts/ci/reviewed-candidate.mjs run");
  }
  for (const [step, name, directory, condition] of [
    [check.steps[7], "reviewed-admission-receipt", "publication-admission", null],
    [upload.steps[5], "reviewed-admission-transfer", "publication-admission-transfer", "always()"],
    [upload.steps[6], "publication-receipt", "publication-receipt", "${{ always() && !inputs.dry_run }}"],
  ]) {
    keys(step, condition ? ["name", "if", "uses", "with"] : ["name", "id", "uses", "with"]);
    if (!condition) assert.equal(step.id, "admission");
    else assert.equal(step.if, condition);
    assert.equal(step.uses, ARTIFACT);
    assert.deepEqual(step.with, { name, path: `\${{ runner.temp }}/${directory}`, "retention-days": "90", "if-no-files-found": "error" });
  }
  return true;
}

export const REVIEWED_MUTATIONS = [
  ...["scripts/ci/supplemental-proof.mjs", "ci/supplemental-proofs.json", "ci/linux-required.json"].map((file) => [`upload omits ${file}`, (d) => { d.jobs.upload.steps[0].with["sparse-checkout"] = REVIEWED_SPARSE.replace(`${file}\n`, ""); }]),
  ["missing trusted uploader dependency", (d) => { d.jobs.upload.steps[0].with["sparse-checkout"] = REVIEWED_SPARSE.replace("scripts/ci/registry-spec.mjs\n", ""); }],
  ["promotion drops queued candidates", (d) => { delete d.concurrency.queue; }],
  ["promotion narrows pending queue", (d) => { d.concurrency.queue = "single"; }],
  ["promotion cancels in progress", (d) => { d.concurrency["cancel-in-progress"] = "true"; }],
  ["untrusted checkout", (d) => { d.jobs.upload.steps[0].with.ref = "${{ inputs.candidate_id }}"; }],
  ["candidate executable checkout", (d) => { d.jobs.upload.steps[0].with["sparse-checkout"] += "packages\n"; }],
  ["credentials before admission", (d) => { d.jobs["seal-check"].steps[6].env.NODE_AUTH_TOKEN = "${{ secrets.NPM_TOKEN }}"; }],
  ["OIDC before admission", (d) => { d.jobs["seal-check"].permissions["id-token"] = "write"; }],
  ["bypass secretless gate", (d) => { d.jobs.upload.needs = []; }],
  ["bypass secretless result", (d) => { d.jobs.upload.if = MAIN_GUARD; }],
  ["rebuild in credential job", (d) => { d.jobs.upload.steps[3].run = "cargo package"; }],
  ["environment spoof", (d) => { d.jobs.upload.steps[3].env.GITHUB_SHA = "${{ inputs.candidate_id }}"; }],
  ["mutable download step", (d) => { d.jobs.upload.steps.push({ uses: "actions/download-artifact@main" }); }],
  ["separate publication lock", (d) => { d.concurrency.group = "publish-reviewed"; }],
  ["path input", (d) => { d.on.workflow_dispatch.inputs.path = { type: "string" }; }],
  ["dry-run upload bypass", (d) => { d.jobs.upload.steps[3].if = "${{ needs.seal-check.outputs.registry == 'npm' }}"; }],
  ["dispatch shell injection", (d) => { d.jobs["seal-check"].steps[6].run += " ${{ inputs.candidate_id }}"; }],
  ["candidate executes semantic verifier", (d) => { d.jobs["seal-check"].steps[6].run = "node candidate/scripts/ci/reviewed-candidate.mjs run --dry-run"; }],
  ["candidate dependency install", (d) => { d.jobs["seal-check"].steps[3].run = "pnpm --dir candidate install"; }],
  ["candidate checkout from dispatch", (d) => { d.jobs["seal-check"].steps[5].with.ref = "${{ inputs.candidate_id }}"; }],
  ["shallow source", (d) => { d.jobs["seal-check"].steps[5].with["fetch-depth"] = "1"; }],
  ["candidate submodule execution", (d) => { d.jobs["seal-check"].steps[5].with.submodules = "recursive"; }],
  ["candidate credentials retained", (d) => { d.jobs["seal-check"].steps[5].with["persist-credentials"] = "true"; }],
  ["candidate overlays trusted verifier", (d) => { d.jobs["seal-check"].steps[5].with.path = "verifier"; }],
  ["lifecycle scripts in semantic admission", (d) => { d.jobs["seal-check"].steps[3].run = d.jobs["seal-check"].steps[3].run.replace(" --ignore-scripts", ""); }],
  ["registry output from dry-run step", (d) => { d.jobs["seal-check"].outputs.registry = "${{ steps.check.outputs.registry }}"; }],
  ["missing admission digest output", (d) => { delete d.jobs["seal-check"].outputs.admission_digest; }],
  ["mutable admission identity", (d) => { d.jobs.upload.steps[2].env.PUBLISH_ADMISSION_ID = "${{ inputs.candidate_id }}"; }],
  ["missing credential-free transfer check", (d) => { d.jobs.upload.steps.splice(2, 1); }],
  ["transfer contaminates publication artifact", (d) => { d.jobs.upload.steps[5].with.path = "${{ runner.temp }}/publication-receipt"; }],
  ["transfer evidence omitted on dry run", (d) => { d.jobs.upload.steps[5].if = "${{ !inputs.dry_run }}"; }],
  ["publication artifact required on dry run", (d) => { d.jobs.upload.steps[6].if = "always()"; }],
  ["publication artifact includes transfer directory", (d) => { d.jobs.upload.steps[6].with.path = "${{ runner.temp }}"; }],
  ["transfer replaces publication artifact identity", (d) => { d.jobs.upload.steps[5].with.name = "publication-receipt"; }],
  ["semantic dependencies under credentials", (d) => { d.jobs.upload.steps[0].with["sparse-checkout"] += "scripts/ci/semantic-admission.mjs\npackages/release\n"; }],
];

if (process.argv[1] && fs.realpathSync(process.argv[1]) === fs.realpathSync(fileURLToPath(import.meta.url))) {
  assertReviewedDoc(parseYaml(fs.readFileSync(new URL("../../.github/workflows/publish-reviewed.yml", import.meta.url), "utf8")));
  process.stdout.write("reviewed promotion workflow contract passed\n");
}
