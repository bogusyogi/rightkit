#!/usr/bin/env node
import assert from "node:assert/strict";

import {
  generatedListMarkerRanges,
  numericAtomSignature,
  repairNumericMaterial,
} from "./numeric-span-repair.mjs";

const fixtures = [
  ["digit verbalization", "Wait 5 minutes.", "Wait five minutes.", "Wait 5 minutes."],
  ["safe decimal spacing", "Use version 3. 8 today.", "Use version 3.8 today.", "Use version 3.8 today."],
  ["safe identifier spacing", "Ship R 2 with L 1.", "Ship R2 with L1.", "Ship R2 with L1."],
  ["magnitude mutation", "Use a 2 7000000000 parameter model.", "Use a 2,700,000,000 parameter model.", "Use a 2 7000000000 parameter model."],
  ["changed value", "Version 2.1 is stable.", "Version 2.2 is stable.", "Version 2.1 is stable."],
  ["reordered values", "Use 3 and then 8.", "Use 8 and then 3.", "Use 3 and then 8."],
  ["multiple verbalizations", "One file is touched by 1 agent in 1 dispatch.", "One file is touched by one agent in one dispatch.", "One file is touched by 1 agent in 1 dispatch."],
  ["numeric false start", "Use 1 work in 1 work tree.", "Use one work tree.", "Use 1 work tree."],
  ["numeric self correction", "Tuesday at 3 no wait Wednesday at 4 works.", "Wednesday at 4 works.", "Wednesday at 4 works."],
  ["unrelated polish retained", "Version 2.1 ships tomorrow it is stable. Keep logging enabled.", "Version 2.2 ships tomorrow. It is stable. Keep logging enabled.", "Version 2.1 ships tomorrow. It is stable. Keep logging enabled."],
  ["added digit sentence fallback", "The answer is unclear. Keep logging enabled.", "The answer is 42. Keep logging enabled.", "The answer is unclear. Keep logging enabled."],
  ["merged atoms sentence fallback", "Use 2 and 3. Keep logging enabled.", "Use 23. Keep logging enabled.", "Use 2 and 3. Keep logging enabled."],
  ["technical identifiers", "Keep HR-204 separate from HR-240.", "Keep HR-204 separate from HR-240.", "Keep HR-204 separate from HR-240."],
  ["proven numbered list", "First send the invoice. Second archive the receipt.", "1. Send the invoice.\n2. Archive the receipt.", "1. Send the invoice.\n2. Archive the receipt."],
];

for (const [id, source, rawOutput, expected] of fixtures) {
  const result = repairNumericMaterial(source, rawOutput);
  assert.equal(result.delivered, expected, id);
  assert.notEqual(result.status, "repair_failed", id);
}

assert.equal(
  generatedListMarkerRanges("Send the invoice.", "1. Send the invoice.").length,
  0,
  "single apparent marker is never exempt",
);
assert.equal(
  generatedListMarkerRanges("Send the invoice then archive it.", "1. Send the invoice.\n2. Archive it.").length,
  0,
  "two numbered lines without spoken enumeration evidence are not exempt",
);
assert.equal(
  generatedListMarkerRanges("First send it. Second archive it.", "1. Send it.\n3. Archive it.").length,
  0,
  "non-sequential markers are not exempt",
);
assert.equal(
  generatedListMarkerRanges("First send it. Second archive it.", "2. Send it.\n3. Archive it.").length,
  0,
  "sequential markers not starting at one are not exempt",
);
assert.equal(
  generatedListMarkerRanges("First send it. Second archive it.", "1. Send it.\n2. Archive it.").length,
  2,
  "sequential item-boundary markers with spoken enumeration are exempt",
);
assert.deepEqual(
  numericAtomSignature("First send it. Second archive it.", "1. Send it.\n2. Archive it."),
  [],
  "proven list markers do not become numeric content",
);
assert.deepEqual(
  numericAtomSignature("Send it then archive it.", "1. Send it.\n2. Archive it."),
  ["1", "2"],
  "unproven apparent markers remain numeric content",
);

console.log(`numeric span repair fixtures passed: ${fixtures.length + 7}/${fixtures.length + 7}`);
