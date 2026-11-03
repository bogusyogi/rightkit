#!/usr/bin/env node
/**
 * Re-score evaluation rows under the corrected policy and print the
 * category table.
 *
 * Reads JSONL (one row per line) or a JSON array on stdin / from a file path.
 * Each row needs `output_text` and a gold target (`expected_output` or
 * `gold_output`); `protected_spans`, `input_text`, and `finish_reason` are used
 * when present.
 *
 * This is the Phase B instrument: point it at each checkpoint's evaluation rows
 * to get true-failure counts that are not contaminated by harness truncation or
 * the old input-retention deletion rule.
 * See docs/L1_TRAINING_RUN_2026-07-23.md.
 */
import { existsSync, readFileSync } from "node:fs";
import path from "node:path";
import { scoreEvaluation } from "./score_evaluation.mjs";

const parseRows = (text) => {
  const trimmed = text.trim();
  if (!trimmed) return [];
  if (trimmed.startsWith("[")) return JSON.parse(trimmed);
  return trimmed.split(/\r?\n/u).filter((line) => line.trim()).map((line) => JSON.parse(line));
};

const read = async (source) => (source ? readFileSync(source, "utf8") : await new Promise((resolve, reject) => {
  let buffer = "";
  process.stdin.setEncoding("utf8");
  process.stdin.on("data", (chunk) => { buffer += chunk; });
  process.stdin.on("end", () => resolve(buffer));
  process.stdin.on("error", reject);
}));

/**
 * Join a run's `evaluation.json` (model outputs) with its
 * `normalized-evaluation.jsonl` (gold targets and protected spans) on row id.
 * The evaluation payload deliberately stores only outputs, so gold has to come
 * from the frozen eval input that produced it.
 */
export function joinEvaluationDirectory(directory) {
  const evaluationPath = path.join(directory, "evaluation.json");
  const normalizedPath = path.join(directory, "normalized-evaluation.jsonl");
  if (!existsSync(evaluationPath) || !existsSync(normalizedPath)) throw new Error(`missing evaluation.json or normalized-evaluation.jsonl in ${directory}`);
  const gold = new Map(parseRows(readFileSync(normalizedPath, "utf8")).map((row) => [row.row_id ?? row.id, row]));
  return JSON.parse(readFileSync(evaluationPath, "utf8")).rows.map((row) => {
    const reference = gold.get(row.id);
    if (!reference) throw new Error(`no gold row for ${row.id}`);
    return { id: row.id, input_text: reference.input_text, output_text: row.output_text, expected_output: reference.output_text, structured_spans: reference.structured_spans, bounded_context: reference.bounded_context ?? {} };
  });
}

const [source] = process.argv.slice(2).filter((argument) => !argument.startsWith("--"));
const asJson = process.argv.includes("--json");
const isDirectory = source && existsSync(path.join(source, "evaluation.json"));
const report = scoreEvaluation(isDirectory ? joinEvaluationDirectory(source) : parseRows(await read(source)));

if (asJson) {
  process.stdout.write(`${JSON.stringify(report, null, 2)}\n`);
} else {
  process.stdout.write(`rows: ${report.total}\n`);
  for (const [category, count] of Object.entries(report.counts)) if (count) process.stdout.write(`  ${category.padEnd(20)} ${count}\n`);
  process.stdout.write(`true failures: ${report.trueFailureCount}\n`);
  for (const id of report.trueFailureIds) process.stdout.write(`  ${id}\n`);
}
