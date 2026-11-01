#!/usr/bin/env node
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { existsSync, readFileSync } from "node:fs";
import path from "node:path";

const sourcePolicyPath = "bakeoff/l1-four-family-training-v1/evaluation-policy.v2.json";
const sourceControlPath = "bakeoff/l1-four-family-training-v1/evaluation-policy-control.v1.json";
const policyPath = existsSync(sourcePolicyPath) ? sourcePolicyPath : "evaluation-policy.v2.json";
const controlPath = existsSync(sourceControlPath) ? sourceControlPath : "evaluation-policy-control.v1.json";
const privateRoot = "C:\\Users\\adrds\\Downloads\\heardright-l1-adjudication-repaired-20260722-private\\evidence-final";
const policyBytes = readFileSync(policyPath);
const sha = (value) => `sha256:${createHash("sha256").update(value).digest("hex")}`;
const policy = JSON.parse(policyBytes);
const control = JSON.parse(readFileSync(controlPath, "utf8"));
const rows = (name) => readFileSync(path.join(privateRoot, name), "utf8").split(/\r?\n/u).filter(Boolean).map(JSON.parse);
const intersect = (left, right) => [...left].filter((value) => right.has(value));
const set = (items, getter) => new Set(items.map(getter));

assert.equal(control.status, "policy_frozen_no_execution_authorized");
assert.equal(control.policy.sha256, sha(policyBytes));
assert.equal(policy.status, "frozen_policy_only_no_training_or_provider_authorization");
assert.deepEqual(policy.roster.sequential_order, ["falcon_h1_0_5b", "falcon_h1_1_5b", "falcon_h1_1_5b_deep", "granite_h_350m", "gemma3_1b", "sotto_lfm25_350m"]);
assert.equal(Object.keys(policy.roster.families).length, 6);
assert.equal(policy.evidence_sets.reserved_synthetic.rows, 177);
assert.equal(policy.evidence_sets.development.must_not_be_gradient_input, true);
assert.equal(policy.evidence_sets.sealed_product_holdout.never_leaves_local_private_storage, true);
assert.equal(policy.epoch_policy.continue.includes("strictly improves"), true);
assert.equal(policy.epoch_policy.stop_and_rollback.includes("hard failure"), true);
assert.equal(policy.epoch_policy.prompt_change.includes("newly sealed development set"), true);
assert.equal(policy.metrics_and_resolution.provider_spend.includes("No new provider call is required"), true);
assert.equal(policy.metrics_and_resolution.resolution.includes("verified improvement requires resolver evidence"), true);
assert.equal(policy.checkpoint_sync_and_prune.vast_disk_cap_gb, 50);
assert.equal(Object.values(policy.checkpoint_sync_and_prune.allocation_gb).reduce((sum, value) => sum + value, 0), 50);

const train = rows("training-1500.jsonl");
const development = rows("development-100.jsonl");
const preflight = rows("gemma-preflight-30.jsonl");
assert.equal(train.length, policy.evidence_sets.training.rows);
assert.equal(development.length, policy.evidence_sets.development.rows);
assert.equal(preflight.length, policy.evidence_sets.gemma_preflight.rows);
assert.equal(sha(readFileSync(path.join(privateRoot, "training-1500.jsonl"))), policy.evidence_sets.training.sha256);
assert.equal(sha(readFileSync(path.join(privateRoot, "development-100.jsonl"))), policy.evidence_sets.development.sha256);
assert.equal(sha(readFileSync(path.join(privateRoot, "gemma-preflight-30.jsonl"))), policy.evidence_sets.gemma_preflight.sha256);
assert.deepEqual(intersect(set(train, (row) => row.row_id), set(development, (row) => row.row_id)), []);
assert.deepEqual(intersect(set(train, (row) => row.source_record_hash), set(development, (row) => row.source_record_hash)), []);
assert.deepEqual(intersect(set(train, (row) => row.cluster_id), set(development, (row) => row.cluster_id)), []);
assert.equal(intersect(set(preflight, (row) => row.row_id), set(development, (row) => row.row_id)).length, 30);

const holdoutBytes = readFileSync(policy.evidence_sets.sealed_product_holdout.private_path);
const holdout = JSON.parse(holdoutBytes);
const holdoutHashes = new Set(holdout.cases.map((item) => `sha256:${item.content_sha256}`));
assert.equal(holdout.cases.length, policy.evidence_sets.sealed_product_holdout.rows);
assert.equal(sha(holdoutBytes), policy.evidence_sets.sealed_product_holdout.file_sha256);
assert.equal(sha([...holdoutHashes].sort().join("\n")), policy.evidence_sets.sealed_product_holdout.content_hash_set_sha256);
assert.deepEqual(intersect(holdoutHashes, set(train, (row) => sha(row.input_text))), []);
assert.deepEqual(intersect(holdoutHashes, set(development, (row) => sha(row.input_text))), []);

const recipes = JSON.parse(readFileSync("scripts/local-model-training/model-recipes.json", "utf8"));
const recipeRoster = {
  falcon_h1_0_5b: recipes.families.falcon.source,
  falcon_h1_1_5b: recipes.candidate_pool.falcon_h1_1_5b[0],
  falcon_h1_1_5b_deep: recipes.candidate_pool.falcon_h1_1_5b[1],
  granite_h_350m: recipes.families.granite.source,
  gemma3_1b: recipes.families.gemma.stock,
  sotto_lfm25_350m: recipes.families.sotto.source,
};
for (const [name, source] of Object.entries(recipeRoster)) {
  assert.equal(policy.roster.families[name].source_id, source.id);
  assert.equal(policy.roster.families[name].revision, source.revision);
}
console.log(JSON.stringify({ status: "ok", policy_sha256: control.policy.sha256, train: train.length, development: development.length, sealed_holdout: policy.evidence_sets.sealed_product_holdout.rows }));
