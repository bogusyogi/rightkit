// Independent self-verify test for generate_already_clean_corpus.mjs.
// Run the generator first (`node generate_already_clean_corpus.mjs`), then this test:
// it re-validates freshly built rows, re-validates the on-disk JSONL artifact against
// the same contract, and cross-checks disk == fresh (deterministic reproducibility).

import { readFileSync, existsSync } from "node:fs";
import assert from "node:assert/strict";
import {
  SCHEMA, BUCKET, PRISTINE_COUNT, TRIVIAL_COUNT, TOTAL,
  corpusPath, summaryPath,
  sha, buildRows, validate, replayDeterministicDiff, alnumWords, digitSignature,
} from "./generate_already_clean_corpus.mjs";

let failures = 0;
let passes = 0;

function check(name, fn) {
  try {
    fn();
    passes += 1;
    console.log(`PASS: ${name}`);
  } catch (error) {
    failures += 1;
    console.error(`FAIL: ${name}\n  ${error.message}`);
  }
}

check("corpus file exists (run the generator first)", () => {
  assert.ok(existsSync(corpusPath), `missing ${corpusPath} — run: node generate_already_clean_corpus.mjs`);
  assert.ok(existsSync(summaryPath), `missing ${summaryPath}`);
});

const freshRows = buildRows();

check("freshly built rows pass validate()", () => {
  const metrics = validate(freshRows);
  assert.equal(metrics.row_count, TOTAL);
});

const diskText = readFileSync(corpusPath, "utf8");
const diskLines = diskText.trimEnd().split("\n");
const diskRows = diskLines.map((line) => JSON.parse(line));

check("disk row count is exactly 250", () => {
  assert.equal(diskRows.length, TOTAL);
});

check("on-disk rows pass validate() independently", () => {
  const metrics = validate(diskRows);
  assert.equal(metrics.row_count, TOTAL);
});

check("disk artifact is byte-identical to a fresh deterministic build", () => {
  const rebuiltText = `${freshRows.map((r) => JSON.stringify(r)).join("\n")}\n`;
  assert.equal(sha(diskText), sha(rebuiltText), "corpus file does not match a fresh deterministic rebuild");
});

check("summary.json corpus_sha256 matches the on-disk corpus file", () => {
  const summary = JSON.parse(readFileSync(summaryPath, "utf8"));
  assert.equal(summary.corpus_sha256, sha(diskText));
  assert.equal(summary.status, "pass");
});

check("sub-bucket counts are exactly 150 pristine / 100 trivial_fix", () => {
  const counts = { pristine: 0, trivial_fix: 0 };
  for (const r of diskRows) counts[r.sub_bucket] += 1;
  assert.equal(counts.pristine, PRISTINE_COUNT);
  assert.equal(counts.trivial_fix, TRIVIAL_COUNT);
});

check("every row matches the raw pre-serialization contract", () => {
  for (const r of diskRows) {
    assert.equal(r.schema, SCHEMA);
    assert.equal(r.bucket, BUCKET);
    assert.equal(r.split, "train");
    assert.ok(["pristine", "trivial_fix"].includes(r.sub_bucket));
    assert.equal(typeof r.input_text, "string");
    assert.equal(typeof r.output_text, "string");
    assert.equal(typeof r.bounded_context, "object");
    assert.ok(Array.isArray(r.must_preserve));
    assert.ok(Array.isArray(r.deterministic_diff));
    assert.equal(r.source, "builder_deterministic_v1");
    assert.equal(r.construction, "target_first_deterministic_v1");
    assert.match(r.template_hash, /^sha256:[0-9a-f]{64}$/);
    assert.match(r.source_hash, /^sha256:[0-9a-f]{64}$/);
    assert.match(r.row_hash, /^sha256:[0-9a-f]{64}$/);
  }
});

check("pristine rows: output_text === input_text exactly, diff is a no-op", () => {
  const pristine = diskRows.filter((r) => r.sub_bucket === "pristine");
  assert.equal(pristine.length, PRISTINE_COUNT);
  for (const r of pristine) {
    assert.equal(r.output_text, r.input_text);
    assert.equal(r.deterministic_diff.length, 0);
    assert.equal(replayDeterministicDiff(r.input_text, r.deterministic_diff), r.output_text);
  }
});

check("trivial_fix rows: replay(deterministic_diff) reproduces output_text", () => {
  const trivial = diskRows.filter((r) => r.sub_bucket === "trivial_fix");
  assert.equal(trivial.length, TRIVIAL_COUNT);
  for (const r of trivial) {
    assert.notEqual(r.input_text, r.output_text);
    assert.equal(replayDeterministicDiff(r.input_text, r.deterministic_diff), r.output_text);
  }
});

check("trivial_fix rows: alnum word multiset identical (no word-level edits leaked in)", () => {
  const trivial = diskRows.filter((r) => r.sub_bucket === "trivial_fix");
  for (const r of trivial) {
    assert.deepEqual(alnumWords(r.input_text), alnumWords(r.output_text), `${r.id}: word set drift`);
  }
});

check("every must_preserve span appears verbatim in both input_text and output_text", () => {
  let spanRows = 0;
  for (const r of diskRows) {
    if (r.must_preserve.length > 0) spanRows += 1;
    for (const spanText of r.must_preserve) {
      assert.ok(r.input_text.includes(spanText), `${r.id}: span missing from input: ${spanText}`);
      assert.ok(r.output_text.includes(spanText), `${r.id}: span missing from output: ${spanText}`);
    }
  }
  assert.ok(spanRows >= 140, `expected >=140 rows with must_preserve spans, got ${spanRows}`);
});

check("digit signature identical between input_text and output_text for all rows", () => {
  for (const r of diskRows) {
    assert.equal(digitSignature(r.input_text), digitSignature(r.output_text), `${r.id}: digits drifted`);
  }
});

check("no template_id exceeds 20% share of its own sub-bucket", () => {
  const counts = {};
  for (const r of diskRows) counts[r.template_id] = (counts[r.template_id] ?? 0) + 1;
  const pristineCap = Math.ceil(PRISTINE_COUNT * 0.2);
  const trivialCap = Math.ceil(TRIVIAL_COUNT * 0.2);
  for (const [templateId, count] of Object.entries(counts)) {
    const cap = templateId.startsWith("pristine-") ? pristineCap : trivialCap;
    assert.ok(count <= cap, `${templateId} share ${count} exceeds cap ${cap}`);
  }
});

check("row ids and input_text are globally unique", () => {
  const ids = new Set(diskRows.map((r) => r.id));
  const inputs = new Set(diskRows.map((r) => r.input_text));
  assert.equal(ids.size, TOTAL);
  assert.equal(inputs.size, TOTAL);
});

console.log(`\n${passes} passed, ${failures} failed`);
if (failures > 0) process.exit(1);
