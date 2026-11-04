import test from "node:test";
import assert from "node:assert/strict";
import { claimIdentity, claimKey, dispatchInputs, parseArgs } from "./rightkit-dispatch.mjs";

const sha = "a".repeat(40);
const tree = "b".repeat(40);

test("CI dispatch requires a proof and carries only the named fixture", () => {
  assert.throws(() => parseArgs(["--workflow", "ci", "--sha", sha]));
  const opts = parseArgs(["--workflow", "ci", "--sha", sha, "--ci-proof", "/proof.json"]);
  assert.deepEqual(dispatchInputs(opts, "job-1", null, "proof"), {
    request_id: "job-1", expected_sha: sha, units: "rust:rightkit-decimal", compile_proof: "proof",
  });
  assert.throws(() => parseArgs(["--workflow", "ci", "--sha", sha, "--ci-proof", "/proof.json", "--unit", "other"]));
});

test("CI claim keys distinguish admitted scopes without changing publication keys", () => {
  const identity = (scope) => claimIdentity({ repo: "bogusyogi/rightkit", workflow: "ci.yml", sha, tree, registry: "ci", unit: scope, version: sha });
  assert.notEqual(claimKey(identity("1".repeat(64))), claimKey(identity("2".repeat(64))));
  assert.equal(claimKey(identity("1".repeat(64))), claimKey(identity("1".repeat(64))));
});
