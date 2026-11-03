import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { main, validateExactTokenizerPreflight } from "./orchestrate.mjs";

const digest = (path) => `sha256:${createHash("sha256").update(readFileSync(path)).digest("hex")}`;

test("live tokenizer receipt binds Transformers 5.5.0, all six revisions, and exact recipes", () => {
  const root = mkdtempSync(join(tmpdir(), "heardright-tokenizer-receipt-"));
  try {
    const recipes = join(root, "recipes.json"), receipt = join(root, "tokenizers.json");
    writeFileSync(recipes, "{}\n");
    const candidate = { integer_token_sequence: true, encoded_tokens: 3, token_id_min: 1, token_id_max: 3, input_ids_sha256: `sha256:${"a".repeat(64)}` };
    writeFileSync(receipt, JSON.stringify({ schema_version: "heardright.l1.exact_tokenizer_preflight.v1", status: "passed", transformers_version: "5.5.0", candidate_count: 6, candidates: Array.from({ length: 6 }, () => ({ ...candidate })), recipes_sha256: digest(recipes) }));
    const validated = validateExactTokenizerPreflight({ path: receipt, sha256: digest(receipt) }, recipes);
    assert.equal(validated.candidate_count, 6);
    const payload = JSON.parse(readFileSync(receipt, "utf8")); payload.transformers_version = "4.57.3"; writeFileSync(receipt, JSON.stringify(payload));
    assert.throws(() => validateExactTokenizerPreflight({ path: receipt, sha256: digest(receipt) }, recipes), /Transformers 5\.5\.0/);
    assert.throws(() => validateExactTokenizerPreflight({ path: receipt, sha256: "sha256:wrong" }, recipes), /hash-mismatched/);
    payload.transformers_version = "5.5.0"; payload.candidates[0].encoded_tokens = 0; writeFileSync(receipt, JSON.stringify(payload));
    assert.throws(() => validateExactTokenizerPreflight({ path: receipt, sha256: digest(receipt) }, recipes), /audit all six/);
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test("dry run freezes four sequential lanes and releases its lock", () => {
  const root = mkdtempSync(join(tmpdir(), "heardright-orchestrator-"));
  try {
    const state = main(["--dry-run", "--output-dir", root]);
    assert.deepEqual(state.lane_order, ["falcon", "granite", "gemma", "sotto"]);
    assert.equal(state.evaluation_policy.sha256, "sha256:b4133649209b0a24af75ade5b0d3798e6f632afdf5f7ab0865b34d5f500b5885");
    assert.equal(state.lanes.gemma.preflight.length, 2);
    for (const family of state.lane_order) {
      assert.deepEqual(state.lanes[family].stages, ["compatibility", "backend-preflight:full_parameter", "autotune:full_parameter", "baseline:development", "train:full_parameter:per_epoch", "sync:per_epoch", "evaluate:full_parameter:per_epoch", "select-or-rollback", "merge:full_parameter", "export-q8", "gguf-evaluate", "release-gpu"]);
    }
    assert.equal(state.active_lock, null);
    assert.equal(existsSync(join(root, "active-run.lock")), false);
    assert.equal(JSON.parse(readFileSync(join(root, "state.json"), "utf8")).mode, "dry_run");
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test("opt-in Falcon candidates run only after the default four", () => {
  const root = mkdtempSync(join(tmpdir(), "heardright-orchestrator-opt-in-"));
  try {
    const state = main(["--dry-run", "--include-opt-in-candidates", "--output-dir", root]);
    assert.deepEqual(state.lane_order, ["falcon", "granite", "gemma", "sotto", "falcon_1_5b", "falcon_1_5b_deep"]);
    assert.equal(state.lanes.falcon_1_5b.family, "falcon");
    assert.equal(state.lanes.falcon_1_5b.source.revision, "a275fec02cb612c83709a813e574cd4993ceecbb");
    assert.equal(state.lanes.falcon_1_5b_deep.source.revision, "30bd55da1221b5d275f29a6ea1b7a46e9981378a");
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test("live mode fails closed without a scoped authorization", () => {
  const root = mkdtempSync(join(tmpdir(), "heardright-orchestrator-"));
  try { assert.throws(() => main(["--live", "--output-dir", root]), /live authorization/); }
  finally { rmSync(root, { recursive: true, force: true }); }
});

test("live resume rejects a state from a different authorization", () => {
  const root = mkdtempSync(join(tmpdir(), "heardright-orchestrator-"));
  try {
    const authorization = join(root, "authorization.json");
    writeFileSync(authorization, JSON.stringify({ approved: true, scope: "heardright_l1_four_family_training", profile: "smoke", offer_id: "offer-1", hourly_rate_usd: 0.25, max_usd: 20, max_hours: 24 }));
    writeFileSync(join(root, "state.json"), JSON.stringify({ mode: "live", profile: "smoke", recipes_sha256: "sha256:wrong", authorization: { sha256: "sha256:wrong" }, events: [], started_at: new Date().toISOString() }));
    assert.throws(() => main(["--live", "--profile", "smoke", "--live-authorization", authorization, "--output-dir", root]), /existing run state is not bound/);
    assert.equal(existsSync(join(root, "active-run.lock")), false);
  } finally { rmSync(root, { recursive: true, force: true }); }
});
