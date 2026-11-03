#!/usr/bin/env node
import assert from "node:assert/strict";

import { cleanupNumericFormatting } from "./numeric-format-cleanup.mjs";

const changed = [
  ["negative sign", "It should be negative five degrees.", "It should be -5 degrees."],
  ["positive sign", "Set it to positive twenty-one.", "Set it to +21."],
  ["contrast keeps both", "It should be negative five degrees, not positive five degrees.", "It should be -5 degrees, not +5 degrees."],
  ["decimal spacing", "Use version 3. 8 today.", "Use version 3.8 today."],
  ["token spacing", "Ship R 2 with L 1.", "Ship R2 with L1."],
  ["long integer", "Use 27000000000 parameters.", "Use 27,000,000,000 parameters."],
];

for (const [id, input, expected] of changed) {
  const result = cleanupNumericFormatting(input);
  assert.equal(result.delivered, expected, id);
  assert.equal(result.status, "formatted_numeric_spans", id);
  assert.ok(result.edits.length > 0, id);
  for (const edit of result.edits) {
    assert.equal(input.slice(edit.start, edit.end), edit.before, `${id}: edit range`);
  }
}

const unchanged = [
  ["numeric correction remains L1-owned", "Let's do 5, no 6."],
  ["named correction remains L1-owned", "Send it to John, scratch that, Jane."],
  ["ambiguous values remain", "Use 3 and then 8."],
  ["identifier remains", "Keep HR-204 separate from HR-240."],
  ["version remains", "Preserve v1.2.03-rc1."],
  ["port remains", "Connect to 127.0.0.1:20128."],
  ["date-like value remains", "Build 20260824 is ready."],
  ["leading zeros remain", "Build 0017000000 is ready."],
  ["ordinary single letters remain", "I 2 am not a token and A 3 is unchanged."],
  ["separate long values remain", "Use 2 7000000000 parameters."],
];

for (const [id, input] of unchanged) {
  const result = cleanupNumericFormatting(input);
  assert.equal(result.delivered, input, id);
  assert.equal(result.status, "unchanged", id);
  assert.deepEqual(result.edits, [], id);
}

const source = "First, use negative five. Then ship R 2 with 27000000000 parameters.";
const result = cleanupNumericFormatting(source);
let reconstructed = source;
for (const edit of [...result.edits].sort((left, right) => right.start - left.start)) {
  reconstructed = `${reconstructed.slice(0, edit.start)}${edit.after}${reconstructed.slice(edit.end)}`;
}
assert.equal(reconstructed, result.delivered, "delivery consists only of declared local edits");
assert.ok(!result.delivered.includes(source), "cleanup never duplicates whole source");

console.log(`numeric formatting fixtures passed: ${changed.length + unchanged.length + 2}/${changed.length + unchanged.length + 2}`);
