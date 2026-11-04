import { strict as assert } from "node:assert";
import { test } from "node:test";
import { verifyFailureClass, verifyReceiptCoverage } from "./provenance-verify.mjs";

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

test("failure class rejects missing, mismatched and malformed copies", () => {
  for (const [receipt, result] of [[{}, { failureClass: "compile" }], [{ failureClass: "compile" }, {}], [{ failureClass: "broker" }, { failureClass: "compile" }], [{ failureClass: "unknown" }, { failureClass: "unknown" }], [{ failureClass: 7 }, { failureClass: 7 }]]) {
    assert.throws(() => check(receipt, result));
  }
});
