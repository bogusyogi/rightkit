import { strict as assert } from "node:assert";
import { test } from "node:test";
import { verifyFailureClass, verifyPostChecks, verifyReceiptCoverage } from "./provenance-verify.mjs";

const check = (receipt, result) => {
  const payload = { result };
  verifyReceiptCoverage(receipt, payload);
  verifyFailureClass(receipt, payload);
};

test("failure class accepts legacy absence and matched signed class", () => {
  assert.doesNotThrow(() => check({}, {}));
  assert.doesNotThrow(() => check({ failureClass: null }, { failureClass: null }));
  assert.doesNotThrow(() => check({ failureClass: "compile" }, { failureClass: "compile" }));
});

const validChecks = () => ({ schema: "rustright.post-checks.v1", ran: true, failed: [], inputClosure: "verified", inputBoundary: "intact", verified: true });
const checksPayload = (checks, failureClass = "compile") => ({ result: { failureClass, postChecks: checks }, inputBoundary: { status: "intact" } });
const checkPost = (checks, payload = checksPayload(checks), options) => {
  const receipt = { postChecks: structuredClone(checks), failureClass: payload.result.failureClass };
  verifyReceiptCoverage(receipt, payload);
  return verifyPostChecks(receipt, payload, options);
};

test("signed post-checks require matching copies & permit legacy absence or null", () => {
  assert.equal(verifyPostChecks({}, { result: {} }, { snapshotAttested: true }), null);
  assert.equal(verifyPostChecks({}, { result: { postChecks: null } }, { snapshotAttested: true }), null);
  const checks = validChecks();
  assert.deepEqual(checkPost(checks, checksPayload(checks), { snapshotAttested: true }), checks);
  assert.throws(() => verifyReceiptCoverage({}, checksPayload(checks)));
  assert.throws(() => verifyPostChecks({ postChecks: { ...checks, verified: false } }, checksPayload(checks)));
  assert.throws(() => verifyPostChecks({ postChecks: checks }, { result: {} }));
});

test("signed post-checks enforce shape, enums, derived verdict & compile checks", () => {
  const invalid = [
    { ...validChecks(), extra: true },
    Object.fromEntries(Object.entries(validChecks()).filter(([k]) => k !== "ran")),
    { ...validChecks(), schema: "rustright.post-checks.v0" },
    { ...validChecks(), ran: 1 },
    { ...validChecks(), failed: ["cache"], verified: false },
    { ...validChecks(), inputClosure: "maybe", verified: false },
    { ...validChecks(), inputBoundary: "maybe", verified: false },
    { ...validChecks(), inputClosure: "inputs-mutated" },
    { ...validChecks(), ran: false, verified: false },
    { ...validChecks(), failed: ["projection"], verified: false },
  ];
  for (const checks of invalid) assert.throws(() => checkPost(checks));
  const mutated = { ...validChecks(), inputClosure: "inputs-mutated", verified: false };
  assert.equal(checkPost(mutated).verified, false);
  const brokerFailure = { ...validChecks(), failed: ["freshness"], verified: false };
  assert.equal(checkPost(brokerFailure, checksPayload(brokerFailure, "broker")).verified, false);
});

test("snapshot post-checks require applicable matching signed boundary & closure", () => {
  const checks = validChecks();
  assert.throws(() => checkPost(checks, { ...checksPayload(checks), inputBoundary: { status: "violated" } }));
  assert.throws(() => checkPost(checks, { result: { failureClass: "compile", postChecks: checks } }, { snapshotAttested: true }));
  for (const changed of [{ inputClosure: "not-applicable" }, { inputBoundary: "not-applicable" }]) {
    const value = { ...checks, ...changed };
    const payload = { ...checksPayload(value), inputBoundary: { status: value.inputBoundary } };
    assert.doesNotThrow(() => checkPost(value, payload));
    assert.throws(() => checkPost(value, payload, { snapshotAttested: true }));
  }
});

test("failure class rejects missing, mismatched and malformed copies", () => {
  for (const [receipt, result] of [[{}, { failureClass: "compile" }], [{ failureClass: "compile" }, {}], [{ failureClass: "broker" }, { failureClass: "compile" }], [{ failureClass: "unknown" }, { failureClass: "unknown" }], [{ failureClass: 7 }, { failureClass: 7 }]]) {
    assert.throws(() => check(receipt, result));
  }
});
