#!/usr/bin/env node
/**
 * Score model outputs against the L1 shaping eval (v1/v2 fixtures).
 *
 * Objective checks only — no judge model:
 *   shape dash_bullets : >= expects.min_bullets lines starting with "- "
 *   shape prose        : <= expects.max_bullets (default 0) bullet lines
 *   must_retain        : every listed string present (case-insensitive)
 *   forbid_added       : none of the listed strings present
 *   forbid_context_leak: no non-trivial bounded_context value appears in the
 *                        output unless it already appeared in the transcript
 *
 * Usage: node score_shaping.mjs <fixture.jsonl> <outputs.jsonl>
 * Outputs must carry {id, output_text}; joined on row_id/id.
 */
import { readFileSync } from "node:fs";

const parse = (path) => readFileSync(path, "utf8").split(/\r?\n/u).filter((line) => line.trim()).map((line) => JSON.parse(line));
const lower = (text) => String(text ?? "").toLowerCase();

export function scoreShapingRow(fixture, output) {
  const text = String(output ?? "").trim();
  const expects = fixture.expects ?? {};
  const failures = [];
  const bullets = text.split(/\r?\n/u).filter((line) => /^\s*-\s+/u.test(line)).length;

  if (expects.shape === "dash_bullets") {
    if (bullets < (expects.min_bullets ?? 2)) failures.push(`bullets: expected >=${expects.min_bullets ?? 2}, got ${bullets}`);
  } else if (bullets > (expects.max_bullets ?? 0)) {
    failures.push(`bullets: expected <=${expects.max_bullets ?? 0}, got ${bullets}`);
  }
  for (const required of expects.must_retain ?? []) {
    if (!lower(text).includes(lower(required))) failures.push(`missing: ${required}`);
  }
  for (const banned of expects.forbid_added ?? []) {
    if (lower(text).includes(lower(banned))) failures.push(`added: ${banned}`);
  }
  if (expects.forbid_context_leak) {
    const transcript = lower(fixture.input_text);
    for (const value of Object.values(fixture.bounded_context ?? {})) {
      const candidate = String(value ?? "").trim();
      // Words of 4+ chars from context that are absent from the transcript must
      // not surface in the output (app names, ticket ids, names, amounts).
      for (const token of candidate.split(/\s+/u).filter((word) => word.length >= 4)) {
        if (!transcript.includes(lower(token)) && lower(text).includes(lower(token))) {
          failures.push(`context-leak: ${token}`);
        }
      }
    }
  }
  return { pass: failures.length === 0, bullets, failures };
}

export function scoreShaping(fixtures, outputs) {
  const byId = new Map(outputs.map((row) => [row.id ?? row.row_id, row.output_text]));
  const slices = {};
  const rows = fixtures.map((fixture) => {
    const id = fixture.row_id;
    const result = scoreShapingRow(fixture, byId.get(id));
    const slice = id.split("-").slice(0, 2).join("-");
    (slices[slice] ??= { pass: 0, fail: 0 })[result.pass ? "pass" : "fail"] += 1;
    return { id, ...result };
  });
  const pass = rows.filter((row) => row.pass).length;
  return { total: rows.length, pass, fail: rows.length - pass, passRate: +(pass / rows.length).toFixed(3), slices, rows };
}

if (process.argv[2] && process.argv[3]) {
  const report = scoreShaping(parse(process.argv[2]), parse(process.argv[3]));
  const { rows, ...summary } = report;
  console.log(JSON.stringify(summary, null, 2));
  for (const row of rows.filter((entry) => !entry.pass)) console.log(`FAIL ${row.id}: ${row.failures.join("; ")}`);
}
