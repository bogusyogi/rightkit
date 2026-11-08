// Compile-only CI admission for one named Rust fixture. This is not a publication qualification.
// Local Class A is enforced by the gate; hosted CI independently repeats Class A.
import { createHash } from "node:crypto";
import { canonicalJson } from "./receipt-signature.mjs";
import { verifyCiCompileReceipts } from "./qualification-verify.mjs";

export const CI_SCOPE = "rust:rightkit-decimal";
const REPOSITORY = "bogusyogi/rightkit";
const WORKFLOW = ".github/workflows/ci.yml";
const SCHEMA = "rightkit.ci-compile-proof/1";
const GIT_ID = /^[a-f0-9]{40}$/;
const record = (v) => v !== null && typeof v === "object" && !Array.isArray(v);
const exactKeys = (value, keys) => record(value) && Object.keys(value).length === keys.length && keys.every((key) => Object.hasOwn(value, key));
const sha256 = (value) => createHash("sha256").update(value).digest("hex");
const refuse = (message) => { throw new Error(`CI compile admission refused: ${message}`); };

export function ciCompileIdentity({ commit, tree, scope = CI_SCOPE }) {
  if (typeof commit !== "string" || !GIT_ID.test(commit) || typeof tree !== "string" || !GIT_ID.test(tree) || scope !== CI_SCOPE) refuse("invalid commit, tree or scope");
  return { repository: REPOSITORY, workflow: WORKFLOW, commit, tree, scope };
}

function canonicalIdentity(identity) {
  if (!exactKeys(identity, ["repository", "workflow", "commit", "tree", "scope"])) refuse("invalid identity fields");
  const expected = ciCompileIdentity(identity);
  if (identity.repository !== expected.repository || identity.workflow !== expected.workflow) refuse("invalid repository or workflow");
  return expected;
}

export function ciCompileScopeDigest(identity) {
  return sha256(canonicalJson(canonicalIdentity(identity)));
}

export function ciCompileStageNonce(identity) {
  return sha256(`rightkit-ci-compile-v1\n${canonicalJson(canonicalIdentity(identity))}`);
}

export function makeCiCompileProof({ commit, tree, receipts }) {
  const identity = ciCompileIdentity({ commit, tree });
  if (!Array.isArray(receipts) || receipts.length === 0 || !receipts.every((receipt) => exactKeys(receipt, ["id", "provenance"]) && typeof receipt.id === "string" && receipt.id.length > 0 && record(receipt.provenance))) refuse("invalid compile receipts");
  return { schema: SCHEMA, ...identity, unit: { kind: "cargo", name: "rightkit-decimal" }, stages: { compile: { receipts } } };
}

export function verifyCiCompileProof(text, { sha, scope, git }) {
  if (typeof sha !== "string" || !GIT_ID.test(sha) || scope !== CI_SCOPE || typeof git !== "function") refuse("invalid expected commit, scope or git reader");
  let proof;
  try { proof = JSON.parse(text); } catch { refuse("proof is not JSON"); }
  let tree;
  try { tree = String(git("rev-parse", `${sha}^{tree}`)).trim(); } catch { refuse("cannot read expected commit tree"); }
  const identity = ciCompileIdentity({ commit: sha, tree, scope });
  if (!exactKeys(proof, ["schema", "repository", "workflow", "commit", "tree", "scope", "unit", "stages"]) || proof.schema !== SCHEMA || Object.keys(identity).some((key) => proof[key] !== identity[key])) refuse("proof identity does not match expected commit, tree and scope");
  if (!exactKeys(proof.unit, ["kind", "name"]) || proof.unit.kind !== "cargo" || proof.unit.name !== "rightkit-decimal") refuse("proof unit does not match scope");
  if (!exactKeys(proof.stages, ["compile"]) || !exactKeys(proof.stages.compile, ["receipts"])) refuse("invalid compile stage");
  const receipts = proof.stages.compile.receipts;
  if (!Array.isArray(receipts) || !receipts.every((receipt) => exactKeys(receipt, ["id", "provenance"]) && typeof receipt.id === "string" && receipt.id.length > 0 && record(receipt.provenance))) refuse("invalid compile receipts");
  const verified = verifyCiCompileReceipts(proof, { sha, git, stageNonce: ciCompileStageNonce(identity) });
  return { commit: identity.commit, tree: identity.tree, scope: identity.scope, scopeSha256: ciCompileScopeDigest(identity), provenance: verified.compile };
}
