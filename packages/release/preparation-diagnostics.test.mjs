import { strict as assert } from "node:assert";
import { test } from "node:test";
import { preparationStreamEvidence } from "./snapshot-check.mjs";

test("preparation diagnostics retain only error codes and line numbers", () => {
  const text = 'Authorization: Digest response="SECRET"\nERR_PNPM_POLICY secret=value\nERR_PNPM_OTHER https://user:pass@example.test/#secret';
  const evidence = preparationStreamEvidence(text);
  assert.deepEqual(evidence.codes, [{ code: "ERR_PNPM_POLICY", line: 2 }, { code: "ERR_PNPM_OTHER", line: 3 }]);
  assert.equal(evidence.bytes, Buffer.byteLength(text));
  assert.equal(JSON.stringify(evidence).includes("SECRET"), false);
  assert.equal(JSON.stringify(evidence).includes("user:pass"), false);
});

test("oversized codes are omitted in full and marked truncated", () => {
  const evidence = preparationStreamEvidence(`ERR_PNPM_${"X".repeat(500)}\nERR_PNPM_SAFE`);
  assert.deepEqual(evidence.codes, [{ code: "ERR_PNPM_SAFE", line: 2 }]);
  assert.equal(evidence.codesTruncated, true);
});
