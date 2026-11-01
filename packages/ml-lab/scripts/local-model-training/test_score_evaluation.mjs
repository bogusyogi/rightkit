import assert from "node:assert/strict";
import test from "node:test";
import { validateOutput } from "./validate_output.mjs";
import { scoreEvaluation, scoreRow } from "./score_evaluation.mjs";

/**
 * Fixtures are verbatim rows from the 2026-07-23 Granite epoch-3 evaluation
 * (.cache/paid-run-recovery/granite-phase1-analysis/granite_epoch3_bad_rows.csv).
 * Each one was scored a hard violation by the old harness. These tests pin the
 * corrected classification so the regression cannot return.
 */

// Byte-identical to gold, failed only by the input-retention deletion rule.
const EXACT_MATCH_TRAP = {
  id: "pc-urls_emails_paths-074",
  input_text: "Send https colon slash slash node dash 74 dot copper dot test slash v2 slash item dash 373 to desk 74 at node dash 74 dot copper dot test.",
  output_text: "Send https://node-74.copper.test/v2/item-373 to desk74@node-74.copper.test.",
  expected_output: "Send https://node-74.copper.test/v2/item-373 to desk74@node-74.copper.test.",
  structured_spans: { protected: [{ text: "https://node-74.copper.test/v2/item-373" }, { text: "desk74@node-74.copper.test" }] },
  bounded_context: {},
};

const EXACT_MATCH_TRAP_CODE = {
  id: "pc-code_identifiers-042",
  input_text: "The command uses dash dash retry dash limit equals 8; its pair is user underscore 141 equals acct underscore 441.",
  output_text: "The command uses --retry-limit=8; its pair is user_141=acct_441.",
  expected_output: "The command uses --retry-limit=8; its pair is user_141=acct_441.",
  structured_spans: { protected: [{ text: "--retry-limit=8" }, { text: "user_141" }, { text: "acct_441" }] },
  bounded_context: {},
};

// Generation budget ran out mid-identifier; output is a strict prefix of gold.
const TRUNCATION = {
  id: "short-v1-synth-000399",
  input_text: "after lunch Inez will close item P-069",
  output_text: "After lunch, Inez will close item P-",
  expected_output: "After lunch, Inez will close item P-069.",
  structured_spans: { protected: [{ text: "P-069" }, { text: "Inez" }] },
  bounded_context: {},
};

const TRUNCATION_COMPOUND = {
  id: "pc-compound_protected_spans-016",
  input_text: "For this record: Keep report_2026-08-16_16.csv unchanged and copy Sela Moor at team16@birch.test.",
  output_text: "For this record: Keep report_2026-08-16_16.csv unchanged",
  expected_output: "For this record: Keep report_2026-08-16_16.csv unchanged and copy Sela Moor at team16@birch.test.",
  structured_spans: { protected: [{ text: "report_2026-08-16_16.csv" }, { text: "Sela Moor" }, { text: "team16@birch.test" }] },
  bounded_context: {},
};

// Real defect: the model rewrote a protected span in place, output is not a prefix.
const TRUE_PROTECTED_EDIT = {
  id: "synthetic-true-protected-edit",
  input_text: "Please send invoice SX-7109 to Amina tomorrow morning.",
  output_text: "Please send invoice SX-7110 to Amina tomorrow morning, thanks.",
  expected_output: "Please send invoice SX-7109 to Amina tomorrow morning.",
  structured_spans: { protected: [{ text: "SX-7109" }] },
  bounded_context: {},
};

// Real defect: substantive content dropped, and it is not a prefix of gold.
const TRUE_CONTENT_DELETION = {
  id: "synthetic-true-content-deletion",
  input_text: "Please archive the quarterly compliance report and notify the finance team before the Friday deadline arrives.",
  output_text: "Archive the quarterly compliance report.",
  expected_output: "Please archive the quarterly compliance report and notify the finance team before the Friday deadline arrives.",
  structured_spans: { protected: [] },
  bounded_context: {},
};

test("INVARIANT: output identical to gold is never a violation", () => {
  for (const row of [EXACT_MATCH_TRAP, EXACT_MATCH_TRAP_CODE]) {
    const result = scoreRow(row);
    assert.equal(result.category, "exact_match", `${row.id} must score exact_match`);
    assert.equal(result.trueFailure, false);
    assert.deepEqual(result.errors, []);
  }
});

test("legitimate ASR compaction no longer trips the deletion rule", () => {
  for (const row of [EXACT_MATCH_TRAP, EXACT_MATCH_TRAP_CODE]) {
    const errors = validateOutput(row).errors.filter((error) => error.startsWith("deletion"));
    assert.deepEqual(errors, [], `${row.id} must not be flagged as deletion`);
  }
});

test("prefix truncation is scored as a harness artifact, not content loss", () => {
  for (const row of [TRUNCATION, TRUNCATION_COMPOUND]) {
    const result = scoreRow(row);
    assert.equal(result.category, "harness_truncation", `${row.id} must score harness_truncation`);
    assert.equal(result.trueFailure, false);
  }
});

test("finish_reason=length is a truncation even when the text is not a clean prefix", () => {
  const result = scoreRow({ ...TRUE_PROTECTED_EDIT, finish_reason: "length" });
  assert.equal(result.category, "harness_truncation");
  assert(validateOutput({ ...TRUE_PROTECTED_EDIT, finish_reason: "length" }).errors.some((error) => error.startsWith("truncation")));
});

test("genuine protected-span damage is still a true failure", () => {
  const result = scoreRow(TRUE_PROTECTED_EDIT);
  assert.equal(result.trueFailure, true);
  assert.equal(result.category, "protected_deletion");
  assert.deepEqual(result.missingProtected, ["SX-7109"]);
});

test("genuine content deletion is still a true failure", () => {
  const result = scoreRow(TRUE_CONTENT_DELETION);
  assert.equal(result.trueFailure, true);
  assert.equal(result.category, "content_deletion");
});

test("aggregate separates harness artifacts from real defects", () => {
  const report = scoreEvaluation([EXACT_MATCH_TRAP, EXACT_MATCH_TRAP_CODE, TRUNCATION, TRUNCATION_COMPOUND, TRUE_PROTECTED_EDIT, TRUE_CONTENT_DELETION]);
  assert.equal(report.total, 6);
  assert.equal(report.counts.exact_match, 2);
  assert.equal(report.counts.harness_truncation, 2);
  assert.equal(report.trueFailureCount, 2);
  assert.deepEqual(report.trueFailureIds, ["synthetic-true-protected-edit", "synthetic-true-content-deletion"]);
});
