import assert from "node:assert/strict";
import test from "node:test";
import { select1500 } from "./finalize_l1_training_corpus.mjs";

const repeat = (count, factory) => Array.from({ length: count }, (_, index) => factory(index + 1));
const shortCounts = { short_noop: 180, short_single_repair: 150, short_punctuation: 120, short_filler_or_restart: 90, short_dictated_format: 60 };
const precisionCounts = { names: 100, numbers_dates_money: 35, urls_emails_paths: 100, code_identifiers: 100, negation_qualifiers: 100, profanity_tone: 100, deliberate_repetition: 100, semantic_asr_repair: 100, compound_protected_spans: 100 };
const longCounts = { contextual_semantic_repair: 59, false_start_or_self_correction: 52, no_op_or_intentional_repetition: 31, misleading_context: 48, multi_error_repair: 68, protected_span: 73, truncation_or_deletion_adversarial: 11 };

function fixture() {
  const rows = [];
  for (const [bucket, count] of Object.entries(shortCounts)) rows.push(...repeat(count, (index) => ({ row_id: `short-v1-${bucket}-${index}`, sub_bucket: bucket })));
  for (const [bucket, count] of Object.entries(precisionCounts)) rows.push(...repeat(count, (index) => ({ row_id: `pc-${bucket}-${index}`, primary_bucket: bucket })));
  for (const [bucket, count] of Object.entries(longCounts)) rows.push(...repeat(count, (index) => ({ row_id: `ss15-${bucket}-${index}`, failure_tags: [bucket], session_cluster_id: `${bucket}-${Math.ceil(index / 3)}` })));
  return rows;
}

test("selects the frozen 450/708/342 corpus deterministically", () => {
  const first = select1500(fixture());
  const second = select1500(fixture().reverse());
  assert.deepEqual(first.counts, second.counts);
  assert.equal(first.selected.length, 1500);
  assert.deepEqual(first.selected.map((row) => row.row_id), second.selected.map((row) => row.row_id));
  assert.deepEqual({ short: first.counts.short, precision: first.counts.precision, long: first.counts.long }, { short: 450, precision: 708, long: 342 });
});

test("fails closed when a quota cannot be met", () => {
  const rows = fixture().filter((row) => row.row_id !== "pc-numbers_dates_money-35");
  assert.throws(() => select1500(rows), /precision\/numbers_dates_money/);
});
