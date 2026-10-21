import assert from "node:assert/strict";
import crypto from "node:crypto";
import test from "node:test";
import { digestScanSet } from "./deterministic-scans.mjs";
import {
  OPERATION_LAST_STAGE, PREREQUISITES, RECEIPT_SCHEMA, STAGES, authorizeNextStage, createMemoryLedger, receiptHash, signingBytes, verifyReceiptChain,
} from "./pipeline-receipts.mjs";

// Test-only signer: a keyed digest standing in for the broker's real signature verifier (no production crypto here).
const sign = (bytes) => crypto.createHash("sha256").update("test-key\0").update(bytes).digest("hex");
const verifySignature = ({ signingBytes: bytes, signature }) => sign(bytes) === signature;
const hex = (label) => crypto.createHash("sha256").update(label).digest("hex");

const NOW = Date.parse("2026-10-22T12:00:00Z");
const iso = (offsetMs) => new Date(NOW + offsetMs).toISOString();
const SCAN_A = [
  { id: "rustfmt:2021", class: "A", command: "rustfmt --check", status: "pass" },
  { id: "cargo-metadata", class: "A", command: "rightkit cargo metadata", status: "pass" },
];
const SCAN_B = [
  { id: "clippy:foo", class: "B", command: "rightkit cargo clippy", status: "pass" },
  { id: "check:foo", class: "B", command: "rightkit cargo check", status: "pass" },
  { id: "doc:foo", class: "B", command: "rightkit cargo doc", status: "pass" },
];
const REQUIRED = { A: SCAN_A.map((s) => s.id), B: SCAN_B.map((s) => s.id) };

const EVIDENCE = {
  ADMISSION: () => ({ inputManifestDigest: hex("in"), dependencyManifestDigest: hex("dep"), toolchainManifestDigest: hex("tc"), authorizedStages: [...STAGES], publishable: true }),
  SCAN_A: () => ({ scanClass: "A", requiredScanIds: REQUIRED.A, scans: SCAN_A, scanSetDigest: digestScanSet(SCAN_A), advisorySnapshotDigest: hex("adv"), advisorySnapshotTime: iso(-3600_000) }),
  ROUTE: () => ({ decision: { executor: "local", visibility: "private", localGrant: false, policyVersion: "p1", estimateSeconds: 60 }, leaseFence: "fence-1" }),
  SCAN_B: () => ({ scanClass: "B", requiredScanIds: REQUIRED.B, scans: SCAN_B, scanSetDigest: digestScanSet(SCAN_B) }),
  BUILD: () => ({ artifactManifestDigest: hex("unsigned"), artifacts: [{ path: "a.crate", sha256: hex("a") }], generationEvidenceDigest: hex("gen"), provenanceDigest: hex("prov"), targetMatrix: ["x86_64-pc-windows-msvc"] }),
  SIGN: () => ({ mode: "SIGN", inputManifestDigest: hex("unsigned"), outputManifestDigest: hex("signed"), signingProfile: "win", signingEvidenceDigest: hex("sig") }),
  QUALIFIED: () => ({ artifactManifestDigest: hex("signed"), testMatrix: [{ name: "install", target: "win", status: "pass" }] }),
  PUBLISH_INTENT: () => ({ releaseManifestDigest: hex("signed"), destinations: [{ id: "crates", identity: "foo", version: "1.0.0", expectedDigest: hex("signed"), idempotencyKey: "k1" }] }),
  PUBLISHED: () => ({ entries: [{ destinationId: "crates", remoteDigest: hex("signed"), outcome: "verified" }] }),
};

/** Build a signed chain up to `upTo`; `mutate[stage](receipt)` edits a receipt before it is signed and linked. */
function chainTo(upTo = "PUBLISHED", { mutate = {}, operation = "publish" } = {}) {
  const chain = [];
  for (const [index, type] of STAGES.slice(0, STAGES.indexOf(upTo) + 1).entries()) {
    const receipt = {
      schema: RECEIPT_SCHEMA, type, receiptId: `r-${type}`, jobId: "job-1", requestId: "req-1", releaseId: "rel-1", repositoryId: "repo-1",
      commit: "a".repeat(40), inputDigest: hex("input"), configDigest: hex("config"), policyDigest: hex("policy"), stagePlanDigest: hex("plan"), runtimeDigest: hex("rt"),
      operation, targetSet: ["win"], attempt: 1, executor: "local-1", parents: index ? [receiptHash(chain[index - 1])] : [],
      issuedAt: iso(-60_000 + index * 1000), expiresAt: iso(600_000), keyId: "k1", evidence: EVIDENCE[type](),
    };
    mutate[type]?.(receipt);
    receipt.signature = sign(signingBytes(receipt));
    chain.push(receipt);
  }
  return chain;
}
const verify = (chain, extra = {}) => verifyReceiptChain(chain, { verifySignature, now: NOW, requiredScans: REQUIRED, ledger: createMemoryLedger(), ...extra });
const codes = (result) => result.problems.map((p) => p.code);

test("a complete valid chain is accepted, hash-linked, and ends at PUBLISHED", () => {
  const chain = chainTo();
  const result = verify(chain, { expected: { requestId: "req-1", operation: "publish", inputDigest: hex("input"), stage: "PUBLISHED" } });
  assert.deepEqual(result.problems, []);
  assert.equal(result.ok, true);
  assert.equal(result.head.type, "PUBLISHED");
  assert.equal(result.head.hash, receiptHash(chain.at(-1)));
  assert.equal(chain[3].parents[0], receiptHash(chain[2]));
});

test("every prefix of the chain verifies, and the stage table is a single path", () => {
  for (const stage of STAGES) assert.equal(verify(chainTo(stage, { operation: "publish" })).ok, true, stage);
  assert.deepEqual(PREREQUISITES.SCAN_A, ["ADMISSION"]);
  assert.deepEqual(PREREQUISITES.PUBLISHED, ["PUBLISH_INTENT"]);
  assert.equal(OPERATION_LAST_STAGE.dev, "BUILD");
});

test("authorizeNextStage: needs exactly the predecessor chain", () => {
  const opts = () => ({ verifySignature, now: NOW, requiredScans: REQUIRED, ledger: createMemoryLedger() });
  assert.equal(authorizeNextStage(chainTo("SCAN_A"), "ROUTE", opts()).authorizes, "ROUTE");
  const skip = authorizeNextStage(chainTo("SCAN_A"), "BUILD", opts());
  assert.equal(skip.ok, false);
  assert.ok(codes(skip).includes("WRONG_STAGE"));
  assert.equal(authorizeNextStage([], "ADMISSION", {}).authorizes, "ADMISSION");
  assert.equal(authorizeNextStage(chainTo("ADMISSION"), "ADMISSION", {}).ok, false);
});

test("a signature verifier is mandatory and a bad signature is rejected", () => {
  assert.equal(verifyReceiptChain(chainTo("ADMISSION"), { now: NOW }).problems[0].code, "NO_SIGNATURE_VERIFIER");
  const chain = chainTo("SCAN_A");
  chain[1].executor = "tampered";
  assert.ok(codes(verify(chain)).includes("BAD_SIGNATURE"));
  assert.ok(codes(verifyReceiptChain(chainTo("ADMISSION"), { now: NOW, verifySignature: () => { throw new Error("x"); } })).includes("BAD_SIGNATURE"));
  assert.ok(codes(verify(chainTo("ADMISSION"), { keyAllowed: () => false })).includes("KEY_REVOKED"));
});

test("rejects a wrong stage (out of order, wrong type, wrong end)", () => {
  const chain = chainTo("BUILD");
  assert.ok(codes(verify([chain[0], chain[2]])).includes("WRONG_STAGE"));
  assert.ok(codes(verify(chain, { expected: { stage: "SIGN" } })).includes("WRONG_STAGE"));
  assert.ok(codes(verify(chainTo("SCAN_A", { mutate: { SCAN_A: (r) => { r.type = "SCAN_X"; } } }))).includes("UNKNOWN_STAGE"));
});

test("rejects a broken or foreign parent hash", () => {
  const chain = chainTo("BUILD", { mutate: { SCAN_B: (r) => { r.parents = [hex("other")]; } } });
  assert.ok(codes(verify(chain)).includes("BROKEN_CHAIN"));
  const twoParents = chainTo("ROUTE", { mutate: { ROUTE: (r) => { r.parents.push(hex("extra")); } } });
  assert.ok(codes(verify(twoParents)).includes("BROKEN_CHAIN"));
  assert.ok(codes(verify(chainTo("SCAN_A", { mutate: { ADMISSION: (r) => { r.parents = [hex("x")]; } } }))).includes("BROKEN_CHAIN"));
});

test("rejects the wrong operation, requestId and inputDigest", () => {
  const chain = chainTo("SIGN");
  assert.ok(codes(verify(chain, { expected: { operation: "build" } })).includes("WRONG_OPERATION"));
  assert.ok(codes(verify(chain, { expected: { requestId: "req-other" } })).includes("WRONG_REQUEST_ID"));
  assert.ok(codes(verify(chain, { expected: { inputDigest: hex("different") } })).includes("WRONG_INPUT_DIGEST"));
  // A receipt in the middle of the chain that carries another request or input is rejected too.
  assert.ok(codes(verify(chainTo("ROUTE", { mutate: { ROUTE: (r) => { r.requestId = "req-2"; } } }))).includes("WRONG_REQUEST_ID"));
  assert.ok(codes(verify(chainTo("ROUTE", { mutate: { ROUTE: (r) => { r.inputDigest = hex("swap"); } } }))).includes("WRONG_INPUT_DIGEST"));
  // An operation cannot reach beyond its last stage: a build request cannot hold a SIGN receipt.
  assert.ok(codes(verify(chainTo("SIGN", { operation: "build" }))).includes("WRONG_OPERATION"));
  assert.equal(verify(chainTo("BUILD", { operation: "dev" })).ok, true);
});

test("rejects an expired receipt", () => {
  const chain = chainTo("ROUTE", { mutate: { SCAN_A: (r) => { r.issuedAt = iso(-7_200_000); r.expiresAt = iso(-3_600_000); } } });
  const result = verify(chain);
  assert.ok(codes(result).includes("EXPIRED"));
  assert.equal(result.problems.find((p) => p.code === "EXPIRED").stage, "SCAN_A");
});

test("rejects a replayed requestId and a replayed receipt", () => {
  const ledger = createMemoryLedger();
  const first = chainTo("SCAN_A");
  assert.equal(verify(first, { ledger }).ok, true);
  ledger.record(first);
  const otherJob = chainTo("SCAN_A", { mutate: { ADMISSION: (r) => { r.jobId = "job-2"; }, SCAN_A: (r) => { r.jobId = "job-2"; } } });
  assert.ok(codes(verify(otherJob, { ledger })).includes("REPLAYED_REQUEST_ID"));
  assert.ok(codes(verify(first, { ledger })).includes("REPLAYED_RECEIPT"));
});

test("authorization is bounded by the requested operation: dev or build BUILD never authorizes SIGN", () => {
  const opts = () => ({ verifySignature, now: NOW, requiredScans: REQUIRED, ledger: createMemoryLedger() });
  for (const operation of ["dev", "build"]) {
    const result = authorizeNextStage(chainTo("BUILD", { operation }), "SIGN", opts());
    assert.equal(result.ok, false, operation);
    assert.equal(result.authorizes, null);
    assert.ok(codes(result).includes("WRONG_OPERATION"));
  }
  assert.equal(authorizeNextStage(chainTo("BUILD", { operation: "sign" }), "SIGN", opts()).authorizes, "SIGN");
  assert.equal(authorizeNextStage(chainTo("SIGN", { operation: "sign" }), "QUALIFIED", opts()).authorizes, null);
});

test("publishable:false never reaches PUBLISH_INTENT or PUBLISHED, and ADMISSION's authorized stages bound the chain", () => {
  const notPublishable = { ADMISSION: (r) => { r.evidence.publishable = false; } };
  assert.ok(codes(verify(chainTo("PUBLISHED", { mutate: notPublishable }))).includes("NOT_PUBLISHABLE"));
  assert.ok(codes(verify(chainTo("PUBLISH_INTENT", { mutate: notPublishable }))).includes("NOT_PUBLISHABLE"));
  assert.equal(verify(chainTo("BUILD", { mutate: notPublishable })).ok, true);
  const onlyAdmission = { ADMISSION: (r) => { r.evidence.authorizedStages = ["ADMISSION"]; } };
  assert.ok(codes(verify(chainTo("PUBLISHED", { mutate: onlyAdmission }))).includes("STAGE_NOT_AUTHORIZED"));
  const opts = () => ({ verifySignature, now: NOW, requiredScans: REQUIRED, ledger: createMemoryLedger() });
  const next = authorizeNextStage(chainTo("QUALIFIED", { mutate: notPublishable }), "PUBLISH_INTENT", opts());
  assert.equal(next.ok, false);
  assert.ok(codes(next).includes("NOT_PUBLISHABLE"));
});

test("releaseId and targetSet are bound across the whole chain", () => {
  // A signed chain that switches both from SIGN on is rejected.
  const swap = (r) => { r.releaseId = "rel-2"; r.targetSet = ["mac"]; };
  const switched = chainTo("PUBLISHED", { mutate: { SIGN: swap, QUALIFIED: swap, PUBLISH_INTENT: swap, PUBLISHED: swap } });
  const result = verify(switched);
  assert.equal(result.ok, false);
  assert.ok(result.problems.some((p) => p.stage === "SIGN" && /releaseId/.test(p.message)));
  assert.ok(result.problems.some((p) => p.stage === "SIGN" && /targetSet/.test(p.message)));
  // A single stage with another releaseId or targetSet is rejected.
  assert.equal(verify(chainTo("QUALIFIED", { mutate: { QUALIFIED: (r) => { r.releaseId = "rel-2"; } } })).ok, false);
  assert.equal(verify(chainTo("BUILD", { mutate: { BUILD: (r) => { r.targetSet = ["win", "mac"]; } } })).ok, false);
  // The caller's expectation binds both too.
  assert.ok(codes(verify(chainTo("SIGN"), { expected: { releaseId: "rel-x" } })).includes("BINDING"));
  assert.ok(codes(verify(chainTo("SIGN"), { expected: { targetSet: ["mac"] } })).includes("BINDING"));
  assert.equal(verify(chainTo("SIGN"), { expected: { releaseId: "rel-1", targetSet: ["win"] } }).ok, true);
});

test("replay check is mandatory: no ledger (or a partial one) fails closed", () => {
  const chain = chainTo("SCAN_A");
  const noLedger = verifyReceiptChain(chain, { verifySignature, now: NOW, requiredScans: REQUIRED });
  assert.equal(noLedger.ok, false);
  assert.ok(codes(noLedger).includes("NO_LEDGER"));
  assert.ok(codes(verifyReceiptChain(chain, { verifySignature, now: NOW, requiredScans: REQUIRED, ledger: { requestOwner: () => undefined } })).includes("NO_LEDGER"));
  assert.equal(authorizeNextStage(chain, "ROUTE", { verifySignature, now: NOW, requiredScans: REQUIRED }).ok, false);
});

test("SCAN_B missing any class B scan is rejected", () => {
  const without = SCAN_B.filter((s) => s.id !== "doc:foo");
  const mutate = { SCAN_B: (r) => { r.evidence.scans = without; r.evidence.scanSetDigest = digestScanSet(without); } };
  const result = verify(chainTo("SCAN_B", { mutate }));
  assert.ok(codes(result).includes("SCAN_INCOMPLETE"));
  assert.match(result.problems.find((p) => p.code === "SCAN_INCOMPLETE").message, /doc:foo/);
  // The receipt cannot shrink its own required set: the trusted set from policy decides.
  const shrunk = { SCAN_B: (r) => { r.evidence.scans = without; r.evidence.scanSetDigest = digestScanSet(without); r.evidence.requiredScanIds = without.map((s) => s.id); } };
  assert.ok(codes(verify(chainTo("SCAN_B", { mutate: shrunk }))).includes("SCAN_INCOMPLETE"));
  // A failed scan is not a pass either.
  const failed = SCAN_B.map((s) => (s.id === "check:foo" ? { ...s, status: "fail" } : s));
  assert.equal(verify(chainTo("SCAN_B", { mutate: { SCAN_B: (r) => { r.evidence.scans = failed; r.evidence.scanSetDigest = digestScanSet(failed); } } })).ok, false);
  // A scanSetDigest that does not match the listed scans is rejected.
  assert.equal(verify(chainTo("SCAN_B", { mutate: { SCAN_B: (r) => { r.evidence.scanSetDigest = hex("x"); } } })).ok, false);
});

test("SCAN_A claiming a class B scan is rejected (and SCAN_B carrying class A)", () => {
  const mixed = [...SCAN_A, { id: "clippy:foo", class: "B", command: "rightkit cargo clippy", status: "pass" }];
  const result = verify(chainTo("SCAN_A", { mutate: { SCAN_A: (r) => { r.evidence.scans = mixed; r.evidence.scanSetDigest = digestScanSet(mixed); } } }));
  assert.ok(codes(result).includes("SCAN_CLASS_MIX"));
  // Disguised: labelled class A but the id is a class B scan.
  const disguised = [...SCAN_A, { id: "tsc:foo", class: "A", command: "pnpm exec tsc", status: "pass" }];
  assert.ok(codes(verify(chainTo("SCAN_A", { mutate: { SCAN_A: (r) => { r.evidence.scans = disguised; r.evidence.scanSetDigest = digestScanSet(disguised); } } }))).includes("SCAN_CLASS_MIX"));
  const withA = [...SCAN_B, SCAN_A[0]];
  assert.ok(codes(verify(chainTo("SCAN_B", { mutate: { SCAN_B: (r) => { r.evidence.scans = withA; r.evidence.scanSetDigest = digestScanSet(withA); } } }))).includes("SCAN_CLASS_MIX"));
});

test("stage evidence: private repositories never route to CI; a CI route needs a dispatch binding", () => {
  const ci = (r) => { r.evidence.decision.executor = "ci"; };
  assert.ok(verify(chainTo("ROUTE", { mutate: { ROUTE: ci } })).problems.some((p) => /never routes to CI/.test(p.message)));
  const publicCi = (r) => { r.evidence.decision.executor = "ci"; r.evidence.decision.visibility = "public"; r.evidence.dispatchBinding = { runId: "1", nonce: "n" }; };
  assert.equal(verify(chainTo("ROUTE", { mutate: { ROUTE: publicCi } })).ok, true);
  assert.equal(verify(chainTo("ROUTE", { mutate: { ROUTE: (r) => { publicCi(r); delete r.evidence.dispatchBinding; } } })).ok, false);
  assert.equal(verify(chainTo("ROUTE", { mutate: { ROUTE: (r) => { r.evidence.decision.visibility = "unknown"; } } })).ok, false);
});

test("stage evidence: manifests must line up from BUILD through PUBLISHED", () => {
  assert.equal(verify(chainTo("SIGN", { mutate: { SIGN: (r) => { r.evidence.inputManifestDigest = hex("not-built"); } } })).ok, false);
  assert.equal(verify(chainTo("QUALIFIED", { mutate: { QUALIFIED: (r) => { r.evidence.artifactManifestDigest = hex("unsigned"); } } })).ok, false);
  assert.equal(verify(chainTo("QUALIFIED", { mutate: { QUALIFIED: (r) => { r.evidence.testMatrix[0].status = "fail"; } } })).ok, false);
  assert.equal(verify(chainTo("PUBLISH_INTENT", { mutate: { PUBLISH_INTENT: (r) => { r.evidence.releaseManifestDigest = hex("other"); } } })).ok, false);
  assert.equal(verify(chainTo("PUBLISHED", { mutate: { PUBLISHED: (r) => { r.evidence.entries[0].remoteDigest = hex("other"); } } })).ok, false);
  assert.equal(verify(chainTo("PUBLISHED", { mutate: { PUBLISHED: (r) => { r.evidence.entries[0].outcome = "unknown"; } } })).ok, false);
  assert.equal(verify(chainTo("PUBLISHED", { mutate: { PUBLISHED: (r) => { r.evidence.entries = []; } } })).ok, false);
  assert.equal(verify(chainTo("BUILD", { mutate: { BUILD: (r) => { r.evidence.artifacts = []; } } })).ok, false);
});

test("envelope: missing releaseId on SIGN, bad attempt, empty chain and non-object receipts are rejected", () => {
  assert.equal(verify(chainTo("SIGN", { mutate: { SIGN: (r) => { delete r.releaseId; } } })).ok, false);
  assert.equal(verify(chainTo("ADMISSION", { mutate: { ADMISSION: (r) => { r.attempt = 0; } } })).ok, false);
  assert.equal(verify([]).ok, false);
  assert.equal(verify([null]).ok, false);
  assert.equal(verify(chainTo("ADMISSION", { mutate: { ADMISSION: (r) => { r.schema = "other/1"; } } })).ok, false);
});

test("a non-publishable or dev chain can never reach SIGN or any later stage", () => {
  const notPublishable = { ADMISSION: (r) => { r.evidence.publishable = false; } };
  for (const stage of ["SIGN", "QUALIFIED", "PUBLISH_INTENT", "PUBLISHED"]) {
    const result = verify(chainTo(stage, { mutate: notPublishable }));
    assert.ok(codes(result).includes("NOT_PUBLISHABLE"), stage);
    assert.equal(result.ok, false, stage);
  }
  assert.equal(verify(chainTo("BUILD", { mutate: notPublishable })).ok, true);
  const missingFlag = { ADMISSION: (r) => { delete r.evidence.publishable; } };
  assert.equal(verify(chainTo("SIGN", { mutate: missingFlag })).ok, false);
  const opts = () => ({ verifySignature, now: NOW, requiredScans: REQUIRED, ledger: createMemoryLedger() });
  // authorizeNextStage refuses SIGN, QUALIFIED and PUBLISH_* for a non-publishable ADMISSION, even with the right operation.
  for (const [have, next] of [["BUILD", "SIGN"], ["SIGN", "QUALIFIED"], ["QUALIFIED", "PUBLISH_INTENT"], ["PUBLISH_INTENT", "PUBLISHED"]]) {
    const result = authorizeNextStage(chainTo(have, { mutate: notPublishable }), next, opts());
    assert.equal(result.ok, false, next);
    assert.equal(result.authorizes, null, next);
    assert.ok(codes(result).includes("NOT_PUBLISHABLE"), next);
  }
  // A dev chain cannot reach SIGN (operation bound), however its ADMISSION reads.
  assert.ok(codes(authorizeNextStage(chainTo("BUILD", { operation: "dev" }), "SIGN", opts())).includes("WRONG_OPERATION"));
});

test("operation build cannot authorize SIGN, QUALIFIED or PUBLISH_*; each operation stops at its last stage", () => {
  const opts = () => ({ verifySignature, now: NOW, requiredScans: REQUIRED, ledger: createMemoryLedger() });
  for (const stage of ["SIGN", "QUALIFIED", "PUBLISH_INTENT", "PUBLISHED"]) {
    assert.ok(codes(verify(chainTo(stage, { operation: "build" }))).includes("WRONG_OPERATION"), `verify ${stage}`);
  }
  const prior = { SIGN: "BUILD", QUALIFIED: "SIGN", PUBLISH_INTENT: "QUALIFIED", PUBLISHED: "PUBLISH_INTENT" };
  for (const [stage, have] of Object.entries(prior)) {
    const result = authorizeNextStage(chainTo(have, { operation: "build" }), stage, opts());
    assert.equal(result.ok, false, stage);
    assert.equal(result.authorizes, null, stage);
    assert.ok(codes(result).includes("WRONG_OPERATION"), stage);
  }
  assert.equal(authorizeNextStage(chainTo("SIGN", { operation: "qualify" }), "QUALIFIED", opts()).authorizes, "QUALIFIED");
  assert.equal(authorizeNextStage(chainTo("QUALIFIED", { operation: "qualify" }), "PUBLISH_INTENT", opts()).authorizes, null);
  assert.equal(authorizeNextStage(chainTo("QUALIFIED", { operation: "publish" }), "PUBLISH_INTENT", opts()).authorizes, "PUBLISH_INTENT");
});

test("ADMISSION's authorizedStages is the only authority: a missing or malformed list authorizes nothing", () => {
  const opts = () => ({ verifySignature, now: NOW, requiredScans: REQUIRED, ledger: createMemoryLedger() });
  for (const authorizedStages of [undefined, "SIGN", [], ["ADMISSION", "SCAN_A", "ROUTE", "SCAN_B", "BUILD"]]) {
    const mutate = { ADMISSION: (r) => { if (authorizedStages === undefined) delete r.evidence.authorizedStages; else r.evidence.authorizedStages = authorizedStages; } };
    assert.equal(verify(chainTo("SIGN", { mutate })).ok, false, JSON.stringify(authorizedStages));
    const next = authorizeNextStage(chainTo("BUILD", { mutate }), "SIGN", opts());
    assert.equal(next.ok, false, JSON.stringify(authorizedStages));
    assert.ok(codes(next).includes("STAGE_NOT_AUTHORIZED"));
  }
});

test("the required scan set comes only from the injected policy matrix: its absence rejects", () => {
  const noPolicy = (chain, extra = {}) => verifyReceiptChain(chain, { verifySignature, now: NOW, ledger: createMemoryLedger(), ...extra });
  const a = noPolicy(chainTo("SCAN_A"));
  assert.equal(a.ok, false);
  assert.ok(codes(a).includes("NO_SCAN_POLICY"));
  const b = noPolicy(chainTo("SCAN_B"));
  assert.ok(codes(b).includes("NO_SCAN_POLICY"));
  // Only one class supplied: the other class still rejects.
  const onlyA = noPolicy(chainTo("SCAN_B"), { requiredScans: { A: REQUIRED.A } });
  assert.ok(onlyA.problems.some((p) => p.stage === "SCAN_B" && p.code === "NO_SCAN_POLICY"));
  assert.ok(!onlyA.problems.some((p) => p.stage === "SCAN_A"));
  // Not arrays: rejected.
  assert.ok(codes(noPolicy(chainTo("SCAN_A"), { requiredScans: { A: "rustfmt:2021", B: [] } })).includes("NO_SCAN_POLICY"));
  // Shrinking the receipt's own declaration and its results together cannot help without the policy.
  const shrunkA = SCAN_A.slice(0, 1);
  const shrunk = { SCAN_A: (r) => { r.evidence.scans = shrunkA; r.evidence.scanSetDigest = digestScanSet(shrunkA); r.evidence.requiredScanIds = shrunkA.map((s) => s.id); } };
  assert.equal(noPolicy(chainTo("SCAN_A", { mutate: shrunk })).ok, false);
  assert.equal(verify(chainTo("SCAN_A", { mutate: shrunk })).ok, false);
  // authorizeNextStage fails closed the same way.
  assert.equal(authorizeNextStage(chainTo("SCAN_A"), "ROUTE", { verifySignature, now: NOW, ledger: createMemoryLedger() }).ok, false);
  // With the policy the same chain verifies.
  assert.equal(verify(chainTo("SCAN_B")).ok, true);
});

test("the replay store is mandatory: absence rejects with NO_LEDGER for every stage", () => {
  for (const stage of STAGES) {
    const result = verifyReceiptChain(chainTo(stage, { operation: "publish" }), { verifySignature, now: NOW, requiredScans: REQUIRED });
    assert.equal(result.ok, false, stage);
    assert.ok(codes(result).includes("NO_LEDGER"), stage);
  }
  assert.ok(codes(verifyReceiptChain(chainTo("ADMISSION"), { verifySignature, now: NOW, requiredScans: REQUIRED, ledger: { isConsumed: () => false } })).includes("NO_LEDGER"));
});
