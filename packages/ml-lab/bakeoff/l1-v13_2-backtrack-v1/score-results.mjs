#!/usr/bin/env node
import fs from "node:fs";

const args = new Map();
for (let index = 2; index < process.argv.length; index += 2) args.set(process.argv[index], process.argv[index + 1]);
const resultsPath = args.get("--results");
const scoringPath = args.get("--scoring");
const outputPath = args.get("--output");
if (!resultsPath || !scoringPath || !outputPath) throw new Error("--results, --scoring & --output are required");
if (fs.existsSync(outputPath)) throw new Error(`refusing to overwrite ${outputPath}`);

const readJsonl = file => fs.readFileSync(file, "utf8").split(/\r?\n/).filter(Boolean).map(JSON.parse);
const results = readJsonl(resultsPath);
const scoring = new Map(readJsonl(scoringPath).map(row => [row.row_id, row]));

function scoreRow(row) {
  const authority = scoring.get(row.row_id);
  if (!authority) throw new Error(`missing scoring row ${row.row_id}`);
  if (!row.provider?.ok || !row.v13_2) return { row_id: row.row_id, category: authority.category, pass: false, failures: ["provider failure"] };
  const output = row.v13_2.delivered;
  const foldedOutput = output.toLocaleLowerCase("en-US");
  const includes = literal => foldedOutput.includes(literal.toLocaleLowerCase("en-US"));
  const failures = [];
  for (const literal of authority.required_literals || []) if (!includes(literal)) failures.push(`missing ${JSON.stringify(literal)}`);
  for (const group of authority.required_any_groups || []) {
    if (!group.some(includes)) failures.push(`missing any of ${JSON.stringify(group)}`);
  }
  for (const literal of authority.forbidden_literals || []) if (includes(literal)) failures.push(`contains forbidden ${JSON.stringify(literal)}`);
  if (authority.exact_output_required && output !== authority.required_literals?.[0]) failures.push("exact output mismatch");
  if (authority.starts_lowercase && /^[A-Z]/.test(output)) failures.push("must start lowercase");
  if (!row.raw_checks?.nonempty) failures.push("empty output");
  if (!row.raw_checks?.no_envelope) failures.push("control envelope leaked");
  if (!row.raw_checks?.no_forbidden_format) failures.push("forbidden formatting");
  return { row_id: row.row_id, category: authority.category, pass: failures.length === 0, failures };
}

const rows = results.map(scoreRow);
const categories = {};
for (const row of rows) {
  categories[row.category] ||= { rows: 0, passed: 0, failed: 0 };
  categories[row.category].rows++;
  categories[row.category][row.pass ? "passed" : "failed"]++;
}
const report = {
  rows: rows.length,
  provider_ok: results.filter(row => row.provider?.ok).length,
  passed: rows.filter(row => row.pass).length,
  failed: rows.filter(row => !row.pass).length,
  changed_by_cleanup: results.filter(row => row.v13_2?.status === "formatted_numeric_spans").length,
  categories,
  failures: rows.filter(row => !row.pass),
  row_scores: rows,
};
fs.writeFileSync(outputPath, `${JSON.stringify(report, null, 2)}\n`);
console.log(JSON.stringify(report, null, 2));
