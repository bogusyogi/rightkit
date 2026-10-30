#!/usr/bin/env node
// The security contract of .github/workflows/publish-swift.yml, asserted on the PARSED workflow and proven by
// mutation: SWIFT_MUTATIONS lists edits that each break one property, and proveSwiftMutations fails if any edit
// still passes. The verify job runs this before it verifies anything:
//   node scripts/ci/swift-workflow-contract.mjs
import assert from "node:assert/strict";
import fs from "node:fs";
import { fileURLToPath } from "node:url";
import { parseYaml } from "./workflow-yaml.mjs";

const CHECKOUT = "actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1";
const NODE = "actions/setup-node@820762786026740c76f36085b0efc47a31fe5020";
const ARTIFACT = "actions/upload-artifact@ea165f8d65b6e75b540449e92b4886f43607fa02";
export const SWIFT_CONCURRENCY = { group: "publish", queue: "max", "cancel-in-progress": "false" };
export const SWIFT_MAIN_GUARD = "${{ github.ref == 'refs/heads/main' && github.repository == 'bogusyogi/rightkit' }}";
export const SWIFT_TAG_IF = "${{ needs.verify.result == 'success' && !inputs.dry_run && github.ref == 'refs/heads/main' && github.repository == 'bogusyogi/rightkit' }}";
export const SWIFT_ENV = {
  GITHUB_TOKEN: "${{ github.token }}",
  PUBLISH_SWIFT_VERSION: "${{ inputs.version }}",
  PUBLISH_SWIFT_COMMIT: "${{ inputs.commit }}",
  PUBLISH_DRY_RUN: "${{ inputs.dry_run }}",
};
const keys = (object, expected) => assert.deepEqual(Object.keys(object).sort(), [...expected].sort());
const NODE_STEP = { uses: NODE, with: { "node-version": "26.8.1", "package-manager-cache": "false" } };
const CHECKOUT_STEP = { uses: CHECKOUT, with: { ref: "${{ github.sha }}", "fetch-depth": "0", "persist-credentials": "false", submodules: "false" } };

export function assertSwiftDoc(doc) {
  keys(doc, ["name", "on", "permissions", "concurrency", "jobs"]);
  assert.equal(doc.name, "publish-swift");
  keys(doc.on, ["workflow_dispatch"]); keys(doc.on.workflow_dispatch, ["inputs"]);
  keys(doc.on.workflow_dispatch.inputs, ["version", "commit", "dry_run"]);
  for (const name of ["version", "commit"]) {
    keys(doc.on.workflow_dispatch.inputs[name], ["description", "type", "required"]);
    assert.equal(doc.on.workflow_dispatch.inputs[name].type, "string"); assert.equal(doc.on.workflow_dispatch.inputs[name].required, "true");
  }
  keys(doc.on.workflow_dispatch.inputs.dry_run, ["description", "type", "default"]);
  assert.equal(doc.on.workflow_dispatch.inputs.dry_run.type, "boolean"); assert.equal(doc.on.workflow_dispatch.inputs.dry_run.default, "true", "dry run is the default");
  assert.deepEqual(doc.permissions, { contents: "read" });
  assert.deepEqual(doc.concurrency, SWIFT_CONCURRENCY, "one publication lock shared with publish-reviewed, queued, never cancelled");
  keys(doc.jobs, ["verify", "tag"]);
  const verify = doc.jobs.verify; const tag = doc.jobs.tag;
  keys(verify, ["if", "runs-on", "timeout-minutes", "permissions", "steps"]);
  keys(tag, ["needs", "if", "environment", "runs-on", "timeout-minutes", "permissions", "steps"]);
  assert.equal(verify.if, SWIFT_MAIN_GUARD); assert.equal(tag.if, SWIFT_TAG_IF);
  assert.equal(verify["runs-on"], "xcode-27"); assert.equal(tag["runs-on"], "ubuntu-24.04");
  assert.deepEqual(verify.permissions, { contents: "read", actions: "read" }, "verification never holds a write permission");
  assert.deepEqual(tag.permissions, { contents: "write", actions: "read" }, "contents: write belongs to the tag job alone");
  assert.deepEqual(tag.needs, ["verify"]); assert.equal(tag.environment, "release");
  for (const job of [verify, tag]) assert.ok(Number(job["timeout-minutes"]) > 0);
  assert.equal(verify.steps.length, 7); assert.equal(tag.steps.length, 4);
  for (const job of [verify, tag]) {
    for (const step of job.steps) {
      if (step.run) assert.ok(!step.run.includes("${{"), "dispatch values cannot enter shell source");
      if (step.uses) assert.match(step.uses, /@[0-9a-f]{40}$/, "actions are pinned to a commit");
      assert.ok(!JSON.stringify(step).includes("secrets."), "no repository secret is used; the push uses the job token only");
    }
    assert.deepEqual(job.steps[0], CHECKOUT_STEP); assert.deepEqual(job.steps[1], NODE_STEP);
  }
  // Order in verify: checkout, node, preflight (P1: before anything compiles), contract, guards, qualify (the compile), describe.
  keys(verify.steps[2], ["name", "run"]);
  assert.equal(verify.steps[2].run, 'node scripts/preflight.mjs --commit "$GITHUB_SHA"', "preflight runs first, on exactly the checked-out commit");
  assert.deepEqual(verify.steps[3], { name: "Check this workflow's own contract", run: "node scripts/ci/swift-workflow-contract.mjs" });
  keys(verify.steps[4], ["name", "env", "run"]); assert.deepEqual(verify.steps[4].env, SWIFT_ENV);
  assert.equal(verify.steps[4].run, "node scripts/ci/publish-swift.mjs verify");
  keys(verify.steps[5], ["name", "run"]);
  assert.equal(verify.steps[5].run, "node scripts/ci/publish-swift.mjs qualify", "the read-only compile is the verify job's, after preflight and the guards");
  keys(verify.steps[6], ["name", "run"]);
  assert.equal(verify.steps[6].run, 'swift package describe --type json > "$RUNNER_TEMP/describe.json"');
  const compiles = (step) => /(^|[\s;&|])(swift|xcodebuild)\s|publish-swift\.mjs qualify/.test(step.run ?? "");
  assert.deepEqual(verify.steps.map((step, index) => (compiles(step) ? index : -1)).filter((index) => index >= 0), [5, 6], "only qualify and describe compile in verify, after preflight");
  assert.ok(!tag.steps.some(compiles), "the write job compiles nothing");
  assert.ok(!("environment" in verify) && !("env" in verify) && !("secrets" in verify), "verify holds no environment, job env or secret");
  keys(tag.steps[2], ["name", "env", "run"]); assert.deepEqual(tag.steps[2].env, SWIFT_ENV);
  assert.equal(tag.steps[2].run, "node scripts/ci/publish-swift.mjs tag");
  keys(tag.steps[3], ["name", "if", "uses", "with"]);
  assert.equal(tag.steps[3].if, "${{ always() }}"); assert.equal(tag.steps[3].uses, ARTIFACT);
  assert.deepEqual(tag.steps[3].with, { name: "swift-publication-receipt", path: "${{ runner.temp }}/swift-publication", "retention-days": "90", "if-no-files-found": "ignore" });
  const everything = JSON.stringify(doc);
  assert.equal(everything.split("git push").length, 1, "the only push is inside publish-swift.mjs");
  assert.equal(everything.split("id-token").length, 1, "no OIDC token");
  return true;
}

export const SWIFT_MUTATIONS = [
  ["write permission at workflow level", (d) => { d.permissions = { contents: "write" }; }],
  ["verify gains contents write", (d) => { d.jobs.verify.permissions.contents = "write"; }],
  ["verify gains id-token", (d) => { d.jobs.verify.permissions["id-token"] = "write"; }],
  ["tag drops its write permission", (d) => { d.jobs.tag.permissions.contents = "read"; }],
  ["tag gains pull-requests write", (d) => { d.jobs.tag.permissions["pull-requests"] = "write"; }],
  ["tag loses the verify gate", (d) => { d.jobs.tag.needs = []; }],
  ["tag runs for a dry run", (d) => { d.jobs.tag.if = SWIFT_MAIN_GUARD; }],
  ["tag no longer needs the verify result", (d) => { d.jobs.tag.if = "${{ !inputs.dry_run && github.ref == 'refs/heads/main' && github.repository == 'bogusyogi/rightkit' }}"; }],
  ["tag loses the release environment", (d) => { delete d.jobs.tag.environment; }],
  ["verify runs from any ref", (d) => { delete d.jobs.verify.if; }],
  ["verify accepts a fork", (d) => { d.jobs.verify.if = "${{ github.ref == 'refs/heads/main' }}"; }],
  ["dry run defaults to false", (d) => { d.on.workflow_dispatch.inputs.dry_run.default = "false"; }],
  ["a path input appears", (d) => { d.on.workflow_dispatch.inputs.path = { type: "string" }; }],
  ["a ref input appears", (d) => { d.on.workflow_dispatch.inputs.ref = { type: "string" }; }],
  ["publications stop sharing one lock", (d) => { d.concurrency.group = "publish-swift"; }],
  ["publications are cancelled", (d) => { d.concurrency["cancel-in-progress"] = "true"; }],
  ["publications are dropped from the queue", (d) => { delete d.concurrency.queue; }],
  ["checkout follows an input", (d) => { d.jobs.tag.steps[0].with.ref = "${{ inputs.commit }}"; }],
  ["checkout keeps credentials", (d) => { d.jobs.tag.steps[0].with["persist-credentials"] = "true"; }],
  ["checkout is shallow", (d) => { d.jobs.verify.steps[0].with["fetch-depth"] = "1"; }],
  ["checkout runs submodules", (d) => { d.jobs.verify.steps[0].with.submodules = "recursive"; }],
  ["mutable action", (d) => { d.jobs.tag.steps[1].uses = "actions/setup-node@v7"; }],
  ["dispatch value in shell source", (d) => { d.jobs.tag.steps[2].run += " ${{ inputs.version }}"; }],
  ["repository secret in the tag step", (d) => { d.jobs.tag.steps[2].env.NODE_AUTH_TOKEN = "${{ secrets.NPM_TOKEN }}"; }],
  ["tag step skips verification mode", (d) => { d.jobs.tag.steps[2].run = "git tag $PUBLISH_SWIFT_VERSION && git push origin $PUBLISH_SWIFT_VERSION"; }],
  ["verify step tags", (d) => { d.jobs.verify.steps[4].run = "node scripts/ci/publish-swift.mjs tag"; }],
  ["version comes from elsewhere", (d) => { d.jobs.tag.steps[2].env.PUBLISH_SWIFT_VERSION = "0.9.0"; }],
  ["commit comes from elsewhere", (d) => { d.jobs.verify.steps[4].env.PUBLISH_SWIFT_COMMIT = "${{ github.sha }}"; }],
  ["dry run flag is dropped from tag", (d) => { delete d.jobs.tag.steps[2].env.PUBLISH_DRY_RUN; }],
  ["contract self-check removed", (d) => { d.jobs.verify.steps.splice(3, 1); }],
  ["preflight missing", (d) => { d.jobs.verify.steps.splice(2, 1); }],
  ["preflight runs after the guards", (d) => { const [step] = d.jobs.verify.steps.splice(2, 1); d.jobs.verify.steps.splice(4, 0, step); }],
  ["preflight checks another commit", (d) => { d.jobs.verify.steps[2].run = 'node scripts/preflight.mjs --commit "${{ inputs.commit }}"'; }],
  ["qualify missing", (d) => { d.jobs.verify.steps.splice(5, 1); }],
  ["qualify runs before preflight", (d) => { const [step] = d.jobs.verify.steps.splice(5, 1); d.jobs.verify.steps.splice(2, 0, step); }],
  ["qualify moved into the tag job", (d) => { const [step] = d.jobs.verify.steps.splice(5, 1); d.jobs.tag.steps.splice(2, 0, step); }],
  ["an extra compile in the write job", (d) => { d.jobs.tag.steps.splice(2, 0, { name: "build", run: "swift build" }); }],
  ["verify compiles before preflight", (d) => { d.jobs.verify.steps.splice(2, 0, { name: "build", run: "swift build" }); }],
  ["verify gains the release environment", (d) => { d.jobs.verify.environment = "release"; }],
  ["verify gains a job secret", (d) => { d.jobs.verify.env = { NODE_AUTH_TOKEN: "${{ secrets.NPM_TOKEN }}" }; }],
  ["verify qualify step gains a secret", (d) => { d.jobs.verify.steps[5].env = { TOKEN: "${{ secrets.NPM_TOKEN }}" }; }],
  ["extra step in the write job", (d) => { d.jobs.tag.steps.splice(3, 0, { run: "git push --force origin main" }); }],
  ["receipt artifact is replaced", (d) => { d.jobs.tag.steps[3].with.name = "publication-receipt"; }],
];

/** Names of the mutations assertSwiftDoc failed to reject. */
export function proveSwiftMutations(doc) {
  assertSwiftDoc(doc);
  const survivors = [];
  for (const [name, mutate] of SWIFT_MUTATIONS) {
    const copy = JSON.parse(JSON.stringify(doc));
    mutate(copy);
    try { assertSwiftDoc(copy); survivors.push(name); } catch { /* rejected */ }
  }
  return survivors;
}

if (process.argv[1] && fs.realpathSync(process.argv[1]) === fs.realpathSync(fileURLToPath(import.meta.url))) {
  try {
    const doc = parseYaml(fs.readFileSync(new URL("../../.github/workflows/publish-swift.yml", import.meta.url), "utf8"));
    const survivors = proveSwiftMutations(doc);
    if (survivors.length) throw new Error(`the contract accepts these violations: ${survivors.join("; ")}`);
    process.stdout.write(`publish-swift workflow contract holds; ${SWIFT_MUTATIONS.length} mutations rejected\n`);
  } catch (error) { process.stderr.write(`${error.message}\n`); process.exit(1); }
}
