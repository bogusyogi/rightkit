#!/usr/bin/env node
/**
 * Gold-aware scoring for L1 evaluation rows.
 *
 * `validate_output.mjs` is the runtime safety gate: it never sees a gold
 * target, so it cannot tell a model defect from a harness artifact. This
 * module runs only during evaluation, where the expected output IS available,
 * and classifies every row into a product-meaningful category.
 *
 * It exists because the 2026-07-23 paid run scored 37 Granite "hard
 * violations" that decomposed entirely into harness truncation and deletion
 * false positives. See docs/L1_TRAINING_RUN_2026-07-23.md.
 */
import { validateOutput } from "./validate_output.mjs";

const collapse = (text) => String(text ?? "").replace(/\s+/gu, " ").trim();

/** Categories in severity order. The first that applies wins. */
export const CATEGORIES = ["exact_match", "harness_truncation", "protected_deletion", "protected_edit", "content_deletion", "formatting_only", "quality_diff"];

/** Categories that represent a real model defect against the product contract. */
export const TRUE_FAILURE_CATEGORIES = new Set(["protected_deletion", "protected_edit", "content_deletion"]);

const protectedTargets = (row) => (row.structured_spans?.protected ?? row.protected_spans ?? [])
  .filter((span) => span.required ?? true)
  .map((span) => span.target_surface ?? span.text ?? span.input_surface)
  .filter(Boolean);

/**
 * True when `actual` is the leading portion of `expected` — the signature of a
 * generation budget that ran out mid-string rather than a model that chose to
 * drop content. A trailing partial token (`repeat-1045` -> `repeat-`) still
 * counts, so compare on the last complete character boundary.
 */
export function isPrefixTruncation(actual, expected) {
  const a = collapse(actual), e = collapse(expected);
  if (!a || a.length >= e.length) return false;
  return e.startsWith(a);
}

/**
 * Score one evaluation row against its gold target.
 *
 * @param {object} row - must carry `input_text`, `output_text`, and the gold
 *   target as `expected_output` (or `gold_output`). `finish_reason` should be
 *   `"length"` when generation hit the token cap.
 * @returns {{category: string, trueFailure: boolean, errors: string[], validation: object}}
 */
export function scoreRow(row) {
  const expected = row.expected_output ?? row.gold_output;
  const validation = validateOutput(row);
  if (expected === undefined || expected === null) return { category: "quality_diff", trueFailure: false, errors: validation.errors, validation, ungraded: true };

  const actualText = collapse(row.output_text), expectedText = collapse(expected);

  // INVARIANT: output identical to the gold target is never a violation. Any
  // rule that fires here is a defect in the rule, not in the model.
  if (actualText === expectedText) return { category: "exact_match", trueFailure: false, errors: [], validation, suppressed: validation.errors };

  // Harness artifacts are scored as harness artifacts, never as content loss.
  if (row.finish_reason === "length" || isPrefixTruncation(actualText, expectedText)) {
    return { category: "harness_truncation", trueFailure: false, errors: validation.errors, validation };
  }

  const missing = protectedTargets(row).filter((target) => !actualText.includes(target));
  if (missing.length) {
    // Present in gold but gone from output: deletion. Absent from gold too:
    // the span was legitimately rewritten by the target itself.
    const deleted = missing.filter((target) => expectedText.includes(target));
    return { category: deleted.length ? "protected_deletion" : "protected_edit", trueFailure: true, errors: validation.errors, missingProtected: missing, validation };
  }

  if (validation.errors.some((error) => error.startsWith("deletion"))) return { category: "content_deletion", trueFailure: true, errors: validation.errors, validation };
  if (collapse(actualText.toLowerCase().replace(/[^\p{L}\p{N}\s]/gu, "")) === collapse(expectedText.toLowerCase().replace(/[^\p{L}\p{N}\s]/gu, ""))) return { category: "formatting_only", trueFailure: false, errors: validation.errors, validation };
  return { category: "quality_diff", trueFailure: false, errors: validation.errors, validation };
}

/** Aggregate scored rows into the per-category table Phase B publishes. */
export function scoreEvaluation(rows) {
  const counts = Object.fromEntries(CATEGORIES.map((category) => [category, 0]));
  const scored = rows.map((row) => {
    const result = scoreRow(row);
    counts[result.category] += 1;
    return { id: row.row_id ?? row.id, ...result };
  });
  const trueFailures = scored.filter((result) => result.trueFailure);
  return { total: scored.length, counts, trueFailureCount: trueFailures.length, trueFailureIds: trueFailures.map((result) => result.id), rows: scored };
}
