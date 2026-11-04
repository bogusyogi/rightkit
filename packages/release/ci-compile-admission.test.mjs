import { test } from "node:test";
import assert from "node:assert/strict";
import { CI_SCOPE, ciCompileIdentity, ciCompileScopeDigest, ciCompileStageNonce, makeCiCompileProof, verifyCiCompileProof } from "./ci-compile-admission.mjs";

const commit = "a".repeat(40), tree = "b".repeat(40);
const identity = ciCompileIdentity({ commit, tree });
const git = (...args) => { assert.deepEqual(args, ["rev-parse", `${commit}^{tree}`]); return tree; };

test("CI identity binds exact repository, workflow, commit, tree and scope", () => {
  assert.deepEqual(identity, { repository: "bogusyogi/rightkit", workflow: ".github/workflows/ci.yml", commit, tree, scope: CI_SCOPE });
  assert.match(ciCompileScopeDigest(identity), /^[0-9a-f]{64}$/);
  assert.notEqual(ciCompileScopeDigest(identity), ciCompileStageNonce(identity));
  assert.notEqual(ciCompileStageNonce(identity), ciCompileStageNonce(ciCompileIdentity({ commit, tree: "c".repeat(40) })));
  for (const bad of ["A".repeat(40), "a".repeat(39), 42, { toString: () => commit }]) assert.throws(() => ciCompileIdentity({ commit: bad, tree }));
  assert.throws(() => ciCompileIdentity({ commit, tree, scope: "rust:other" }));
  assert.throws(() => ciCompileScopeDigest({ ...identity, extra: 1 }));
});

test("CI admission rejects substituted identity and malformed unsigned evidence", () => {
  const proof = makeCiCompileProof({ commit, tree, receipts: [{ id: "job", provenance: {} }] });
  const verify = (p) => verifyCiCompileProof(JSON.stringify(p), { sha: commit, scope: CI_SCOPE, git });
  for (const changed of [{ ...proof, tree: "c".repeat(40) }, { ...proof, repository: "elsewhere" }, { ...proof, unit: { kind: "cargo", name: "other" } }, { ...proof, stages: { ...proof.stages, package: {} } }, { ...proof, extra: true }]) assert.throws(() => verify(changed));
  assert.throws(() => verify(proof));
  assert.throws(() => makeCiCompileProof({ commit, tree, receipts: [] }));
  assert.throws(() => verifyCiCompileProof("not json", { sha: commit, scope: CI_SCOPE, git }));
});
