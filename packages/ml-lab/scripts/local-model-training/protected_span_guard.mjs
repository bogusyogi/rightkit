#!/usr/bin/env node
/**
 * Runtime protected-span guard: verify, repair, or fall back.
 *
 * Phase E of docs/L1_TRAINING_RUN_2026-07-23.md. The product contract already
 * says unsafe output falls back to the original chunk, and prices fallback at
 * <=2% on the sealed set. This module is what decides "unsafe" — deterministic
 * string checks, no model and no heuristics — so protected-content damage is
 * structurally impossible rather than a property we hope the weights learned.
 *
 *   ASR chunk -> L1 model -> guardChunk() -> repaired text | original chunk
 */
import { validateOutput } from "./validate_output.mjs";

const collapse = (text) => String(text ?? "").replace(/\s+/gu, " ").trim();

const requiredSpans = (row) => (row.structured_spans?.protected ?? row.protected_spans ?? [])
  .filter((span) => span.required ?? true)
  .map((span) => ({ target: span.target_surface ?? span.text ?? span.input_surface, source: span.input_surface ?? span.text, kind: span.kind }))
  .filter((span) => span.target);

/**
 * Repair a span the model dropped or mangled, but ONLY when the fix is
 * unambiguous: the span's source form is still present in the output (the model
 * failed to normalize it) or the output is a clean prefix of what it should be
 * and the span belongs at the tail. Anything else falls back — a guess that
 * silently rewrites user text is worse than returning the original chunk.
 */
function repairSpan(output, span) {
  if (span.source && span.source !== span.target && output.includes(span.source)) return output.replace(span.source, span.target);
  return null;
}

/**
 * @returns {{status: "pass"|"repaired"|"fallback", text: string, reason?: string, repairedSpans?: string[], errors?: string[]}}
 */
export function guardChunk(row) {
  const original = collapse(row.input_text);
  const fallback = { status: "fallback", text: original };

  if (row.finish_reason === "length") return { ...fallback, reason: "generation stopped at the token cap" };

  let output = collapse(row.output_text);
  if (!output) return { ...fallback, reason: "empty output" };

  const spans = requiredSpans(row);
  const repaired = [];
  for (const span of spans) {
    if (output.includes(span.target)) continue;
    const attempt = repairSpan(output, span);
    if (attempt === null) return { ...fallback, reason: `unrepairable protected span: ${span.target}` };
    output = attempt;
    repaired.push(span.target);
  }

  // Re-verify after repair. A repair that did not actually restore every span,
  // or that tripped another product rule, is not a repair.
  const missing = spans.filter((span) => !output.includes(span.target));
  if (missing.length) return { ...fallback, reason: `protected span still missing after repair: ${missing[0].target}` };

  const validation = validateOutput({ ...row, output_text: output });
  if (!validation.ok) return { ...fallback, reason: validation.errors[0], errors: validation.errors };

  return repaired.length ? { status: "repaired", text: output, repairedSpans: repaired } : { status: "pass", text: output };
}

/** Fallback rate across a batch — the number the <=2% product budget is measured against. */
export function guardBatch(rows) {
  const results = rows.map((row) => ({ id: row.row_id ?? row.id, ...guardChunk(row) }));
  const counts = { pass: 0, repaired: 0, fallback: 0 };
  for (const result of results) counts[result.status] += 1;
  return { total: results.length, counts, fallbackRate: results.length ? counts.fallback / results.length : 0, results };
}
