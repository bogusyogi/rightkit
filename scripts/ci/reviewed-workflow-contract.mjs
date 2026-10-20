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
export const REVIEWED_SPARSE = "scripts/ci/upload.mjs\nscripts/ci/registry-spec.mjs\nscripts/ci/reviewed-candidate.mjs\nscripts/ci/supplemental-proof.mjs\nci/approved-artifacts.json\nci/supplemental-proofs.json\n";
const keys = (object, expected) => assert.deepEqual(Object.keys(object).sort(), [...expected].sort());

export function assertReviewedDoc(doc) {
  keys(doc, ["name", "on", "permissions", "concurrency", "jobs"]);
  assert.equal(doc.name, "publish-reviewed");
  keys(doc.on, ["workflow_dispatch"]); keys(doc.on.workflow_dispatch, ["inputs"]);
  assert.deepEqual(doc.on.workflow_dispatch.inputs, { candidate_id: { description: "Committed schema 2 candidate ID", type: "string", required: "true" } });
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
  assert.deepEqual(check.outputs, { registry: "${{ steps.check.outputs.registry }}" });
  assert.equal(check.steps.length, 4); assert.equal(upload.steps.length, 5);
  for (const job of [check, upload]) {
    assert.equal(job["runs-on"], "ubuntu-24.04"); assert.ok(Number(job["timeout-minutes"]) > 0);
    assert.deepEqual(job.steps[0], { uses: CHECKOUT, with: { ref: "${{ github.sha }}", "persist-credentials": "false", "sparse-checkout": REVIEWED_SPARSE, "sparse-checkout-cone-mode": "false" } });
    assert.deepEqual(job.steps[1], { uses: NODE, with: { "node-version": "26.8.1", "package-manager-cache": "false" } });
    for (const step of job.steps) {
      if (step.run) assert.ok(!step.run.includes("${{"), "dispatch values cannot enter shell source");
      if (step.uses) assert.match(step.uses, /@[0-9a-f]{40}$/);
    }
  }
  const baseEnv = { GITHUB_TOKEN: "${{ github.token }}", PUBLISH_CANDIDATE_ID: "${{ inputs.candidate_id }}" };
  keys(check.steps[2], ["name", "id", "env", "run"]);
  assert.equal(check.steps[2].id, "check"); assert.deepEqual(check.steps[2].env, baseEnv);
  assert.equal(check.steps[2].run, "node scripts/ci/reviewed-candidate.mjs run --dry-run");
  for (const [index, registry, variable, secret] of [[2, "npm", "NODE_AUTH_TOKEN", "NPM_TOKEN"], [3, "crates", "CARGO_REGISTRY_TOKEN", "CARGO_REGISTRY_TOKEN"]]) {
    const step = upload.steps[index]; keys(step, ["name", "if", "env", "run"]);
    assert.equal(step.if, `\${{ needs.seal-check.outputs.registry == '${registry}' }}`);
    assert.deepEqual(step.env, { ...baseEnv, [variable]: `\${{ secrets.${secret} }}` });
    assert.equal(step.run, "node scripts/ci/reviewed-candidate.mjs run");
  }
  for (const [step, name, always] of [[check.steps[3], "reviewed-admission-receipt", false], [upload.steps[4], "publication-receipt", true]]) {
    keys(step, always ? ["name", "if", "uses", "with"] : ["name", "uses", "with"]);
    if (always) assert.equal(step.if, "always()");
    assert.equal(step.uses, ARTIFACT);
    assert.deepEqual(step.with, { name, path: "${{ runner.temp }}/publication-receipt", "retention-days": "90", "if-no-files-found": "error" });
  }
  return true;
}

export const REVIEWED_MUTATIONS = [
  ...["seal-check", "upload"].flatMap((job) => ["scripts/ci/supplemental-proof.mjs", "ci/supplemental-proofs.json"].map((file) => [`${job} omits ${file}`, (d) => { d.jobs[job].steps[0].with["sparse-checkout"] = REVIEWED_SPARSE.replace(`${file}\n`, ""); }])),
  ["missing trusted uploader dependency", (d) => { d.jobs.upload.steps[0].with["sparse-checkout"] = REVIEWED_SPARSE.replace("scripts/ci/registry-spec.mjs\n", ""); }],
  ["promotion drops queued candidates", (d) => { delete d.concurrency.queue; }],
  ["promotion narrows pending queue", (d) => { d.concurrency.queue = "single"; }],
  ["promotion cancels in progress", (d) => { d.concurrency["cancel-in-progress"] = "true"; }],
  ["untrusted checkout", (d) => { d.jobs.upload.steps[0].with.ref = "${{ inputs.candidate_id }}"; }],
  ["candidate executable checkout", (d) => { d.jobs.upload.steps[0].with["sparse-checkout"] += "packages\n"; }],
  ["credentials before admission", (d) => { d.jobs["seal-check"].steps[2].env.NODE_AUTH_TOKEN = "${{ secrets.NPM_TOKEN }}"; }],
  ["OIDC before admission", (d) => { d.jobs["seal-check"].permissions["id-token"] = "write"; }],
  ["bypass secretless gate", (d) => { d.jobs.upload.needs = []; }],
  ["bypass secretless result", (d) => { d.jobs.upload.if = MAIN_GUARD; }],
  ["rebuild in credential job", (d) => { d.jobs.upload.steps[2].run = "cargo package"; }],
  ["environment spoof", (d) => { d.jobs.upload.steps[2].env.GITHUB_SHA = "${{ inputs.candidate_id }}"; }],
  ["mutable download step", (d) => { d.jobs.upload.steps.push({ uses: "actions/download-artifact@main" }); }],
  ["separate publication lock", (d) => { d.concurrency.group = "publish-reviewed"; }],
  ["path input", (d) => { d.on.workflow_dispatch.inputs.path = { type: "string" }; }],
  ["restore dispatch dry-run roundtrip", (d) => { d.on.workflow_dispatch.inputs.dry_run = { type: "boolean", default: "true" }; }],
  ["dispatch shell injection", (d) => { d.jobs["seal-check"].steps[2].run += " ${{ inputs.candidate_id }}"; }],
];

if (process.argv[1] && fs.realpathSync(process.argv[1]) === fs.realpathSync(fileURLToPath(import.meta.url))) {
  assertReviewedDoc(parseYaml(fs.readFileSync(new URL("../../.github/workflows/publish-reviewed.yml", import.meta.url), "utf8")));
  process.stdout.write("reviewed promotion workflow contract passed\n");
}
