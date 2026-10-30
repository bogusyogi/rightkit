// GB-10 / GB-11: the Node receipt verifier reaches the lenient AND strict outcome of every case of RustRight's receipt-vectors.v1.json (signed with the
// production bytes and a fixed test key; the Rust test is rustright-protocol tests/receipt_vectors.rs).
import "./test-support/broker-guard.mjs";
import assert from "node:assert/strict";
import test from "node:test";
import { ProvenanceError, verifyReceiptLenient, verifyReceiptStrict } from "./provenance-verify.mjs";
import { loadVector } from "./test-support/vectors.mjs";

const vector = loadVector("receipt-vectors.v1.json");
const outcome = (verify, receipt) => { try { verify(structuredClone(receipt), vector.pins, vector.expected); return "ok"; } catch (e) { if (e instanceof ProvenanceError) return e.code; throw e; } };

test("[GB-10] every case of the receipt vector has RustRight's lenient and strict outcome (error codes included)", () => {
  assert.equal(vector.cases.length, 45);
  for (const c of vector.cases) {
    assert.equal(outcome(verifyReceiptLenient, c.receipt), c.lenient, `lenient: ${c.name}`);
    assert.equal(outcome(verifyReceiptStrict, c.receipt), c.strict, `strict: ${c.name}`);
  }
});

test("[GB-10] the vector covers both directions: a case the lenient verifier accepts and the strict one refuses is refused here by strict only", () => {
  const split = vector.cases.filter((c) => c.lenient === "ok" && c.strict !== "ok");
  assert.ok(split.length >= 10, "the no-resultEvidence, missing-phase and malformed-BR19 cases");
  for (const c of split) { assert.equal(outcome(verifyReceiptLenient, c.receipt), "ok", c.name); assert.equal(outcome(verifyReceiptStrict, c.receipt), "evidence", c.name); }
  assert.ok(vector.cases.some((c) => c.name === "a failed signed result keeps its failure" && c.strict === "ok"), "a failed signed result is a valid receipt: the gate's own success check stays separate");
});
