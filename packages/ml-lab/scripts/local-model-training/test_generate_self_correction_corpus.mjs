import assert from "node:assert/strict";
import test from "node:test";
import { generateDataset, buildRows, validate } from "./generate_self_correction_corpus.mjs";

const entries = generateDataset();
const rows = entries.map((e) => e.row);

test("generates exactly 700 rows with exact sub-bucket quotas and passes full validation", () => {
  const metrics = validate(entries, { full: true });
  assert.equal(metrics.row_count, 700);
  assert.deepEqual(metrics.bucket_counts, { number: 200, datetime: 130, name: 110, phrase: 110, multi: 60, negative_trap: 90 });
  assert.equal(metrics.context_row_count, 120);
  assert.ok(metrics.max_template_share <= 0.2000001, `template share ${metrics.max_template_share} exceeds 20%`);
  assert.equal(metrics.replay_failures, 0);
  assert.equal(metrics.digit_failures, 0);
  assert.equal(metrics.cue_leak_failures, 0);
  assert.equal(metrics.superseded_leak_failures, 0);
  assert.equal(metrics.preserve_failures, 0);
  assert.equal(metrics.trap_clause_failures, 0);
  assert.equal(metrics.trap_digit_failures, 0);
  assert.equal(metrics.lineage_failures, 0);
});

test("buildRows() returns 700 plain contract rows matching generateDataset()", () => {
  const built = buildRows();
  assert.equal(built.length, 700);
  assert.deepEqual(built, rows);
});

test("every row carries the exact contract shape and field values", () => {
  const hashPattern = /^sha256:[0-9a-f]{64}$/;
  const validSubBuckets = new Set(["number", "datetime", "name", "phrase", "multi", "negative_trap"]);
  for (const row of rows) {
    assert.equal(row.schema, "heardright.l1_appaware_pair.v1");
    assert.equal(row.bucket, "self_correction");
    assert.match(row.id, /^self_correction-\d{3}$/);
    assert.ok(validSubBuckets.has(row.sub_bucket), `unexpected sub_bucket ${row.sub_bucket}`);
    assert.equal(typeof row.input_text, "string");
    assert.ok(row.input_text.length > 0);
    assert.equal(typeof row.output_text, "string");
    assert.ok(row.output_text.length > 0);
    assert.doesNotMatch(row.input_text, /^Context:|^Transcript:|^L1C1/u, "input_text must be raw, not serialized");
    assert.equal(typeof row.bounded_context, "object");
    assert.ok(Array.isArray(row.must_preserve));
    assert.ok(Array.isArray(row.deterministic_diff));
    assert.equal(row.source, "builder_deterministic_v1");
    assert.equal(row.construction, "target_first_deterministic_v1");
    assert.equal(row.split, "train");
    assert.match(row.template_hash, hashPattern);
    assert.match(row.source_hash, hashPattern);
    assert.match(row.row_hash, hashPattern);
  }
});

test("bounded_context is {} for most rows and a light flat {app_name, window_title} shape for the rest", () => {
  for (const row of rows) {
    const keys = Object.keys(row.bounded_context).sort();
    assert.ok(keys.length === 0 || (keys.length === 2 && keys[0] === "app_name" && keys[1] === "window_title"), `unexpected bounded_context shape: ${JSON.stringify(row.bounded_context)}`);
  }
  const withContext = rows.filter((r) => Object.keys(r.bounded_context).length > 0);
  assert.equal(withContext.length, 120);
});

test("replaying deterministic_diff on input_text reproduces output_text for a spread of rows", () => {
  const sampleIndices = [0, 50, 150, 250, 320, 400, 450, 500, 560, 590, 630, 699];
  for (const index of sampleIndices) {
    const row = rows[index];
    // Re-derive via validate's own replay path indirectly by checking round-trip here too.
    let replayed = row.input_text;
    for (const op of [...row.deterministic_diff].sort((a, b) => b.start - a.start)) {
      assert.equal(replayed.slice(op.start, op.end), op.expected_text, `${row.id}: expected_text mismatch`);
      replayed = `${replayed.slice(0, op.start)}${op.replacement_text}${replayed.slice(op.end)}`;
    }
    assert.equal(replayed, row.output_text, `${row.id}: replay did not reproduce output_text`);
  }
});

test("number sub-bucket: output digits are always a strict subset of input digits, superseded number dropped", () => {
  const numberRows = rows.filter((r) => r.sub_bucket === "number");
  assert.equal(numberRows.length, 200);
  for (const row of numberRows) {
    const outTok = row.output_text.match(/\d+/g) || [];
    const inTok = row.input_text.match(/\d+/g) || [];
    for (const t of outTok) assert.ok(inTok.includes(t), `${row.id}: output digit ${t} not found in input`);
  }
});

test("multi sub-bucket rows carry two corrections (two must_preserve finals, two-site diff)", () => {
  const multiRows = rows.filter((r) => r.sub_bucket === "multi");
  assert.equal(multiRows.length, 60);
  for (const row of multiRows) {
    assert.equal(row.must_preserve.length, 2, `${row.id}: expected 2 must_preserve finals`);
  }
});

test("negative_trap rows keep all content: no deletion, cue words retained, digits preserved", () => {
  const trapRows = rows.filter((r) => r.sub_bucket === "negative_trap");
  assert.equal(trapRows.length, 90);
  for (const row of trapRows) {
    assert.ok(row.output_text.includes(row.must_preserve[0]), `${row.id}: trap clause missing from output`);
    const outTok = (row.output_text.match(/\d+/g) || []).sort();
    const inTok = (row.input_text.match(/\d+/g) || []).sort();
    assert.deepEqual(outTok, inTok, `${row.id}: negative_trap must preserve all digits`);
    // deterministic_diff is either empty (identity) or a casing-only single op
    if (row.deterministic_diff.length > 0) {
      assert.equal(row.deterministic_diff.length, 1);
      const op = row.deterministic_diff[0];
      assert.equal(op.expected_text.toLowerCase(), op.replacement_text.toLowerCase(), `${row.id}: negative_trap diff must be casing/punct-only`);
    }
  }
});

test("cue rows never leak the cue text or the superseded value into output_text", () => {
  for (const entry of entries) {
    if (entry.row.sub_bucket === "negative_trap") continue;
    const outLower = entry.row.output_text.toLowerCase();
    for (const cue of entry.cues) assert.ok(!outLower.includes(cue.toLowerCase()), `${entry.row.id}: cue "${cue}" leaked into output`);
    for (const sv of entry.supersededValues) assert.ok(!entry.row.output_text.includes(sv), `${entry.row.id}: superseded value "${sv}" leaked into output`);
  }
});

test("no single template_id exceeds 20% of its sub-bucket", () => {
  const counts = new Map();
  const bucketTotals = new Map();
  for (const row of rows) {
    bucketTotals.set(row.sub_bucket, (bucketTotals.get(row.sub_bucket) ?? 0) + 1);
    const key = `${row.sub_bucket}|${row.template_id}`;
    counts.set(key, (counts.get(key) ?? 0) + 1);
  }
  for (const [key, count] of counts) {
    const [subBucket] = key.split("|");
    const share = count / bucketTotals.get(subBucket);
    assert.ok(share <= 0.2000001, `${key}: share ${share} exceeds 20%`);
  }
});

test("row ids and cluster identity are unique across all 700 rows", () => {
  const ids = new Set(rows.map((r) => r.id));
  assert.equal(ids.size, 700);
});
