#!/usr/bin/env node
import fs from "node:fs";

import { cleanupNumericFormatting } from "./numeric-format-cleanup.mjs";

const args = new Map();
for (let index = 2; index < process.argv.length; index += 2) args.set(process.argv[index], process.argv[index + 1]);
const inputPath = args.get("--input");
const outputPath = args.get("--output");
const model = args.get("--model");
if (!inputPath || !outputPath || !model) throw new Error("--input, --output & --model are required");
if (fs.existsSync(outputPath)) throw new Error(`refusing to overwrite ${outputPath}`);

const rows = fs.readFileSync(inputPath, "utf8").split(/\r?\n/).filter(Boolean).map(JSON.parse);
const replay = rows.map(row => {
  const rawOutput = row.provider?.raw_output ?? row.raw_output;
  if (typeof rawOutput !== "string") throw new Error(`missing raw output for ${row.row_id}`);
  const result = cleanupNumericFormatting(rawOutput);
  let reconstructed = rawOutput;
  for (const edit of [...result.edits].sort((left, right) => right.start - left.start)) {
    if (rawOutput.slice(edit.start, edit.end) !== edit.before) throw new Error(`invalid edit range for ${row.row_id}`);
    reconstructed = `${reconstructed.slice(0, edit.start)}${edit.after}${reconstructed.slice(edit.end)}`;
  }
  if (reconstructed !== result.delivered) throw new Error(`nonlocal mutation for ${row.row_id}`);
  const secondPass = cleanupNumericFormatting(result.delivered);
  if (secondPass.delivered !== result.delivered || secondPass.edits.length) throw new Error(`non-idempotent cleanup for ${row.row_id}`);
  return {
    row_id: row.row_id,
    model,
    captured_prompt_version: row.prompt_version || "l1_app_polish_prompt_v13_1_candidate",
    raw_output: rawOutput,
    v13_2_cleanup: result,
    deterministic_checks: {
      declared_local_edits_only: true,
      idempotent: true,
      no_fallback: true,
    },
  };
});
fs.writeFileSync(outputPath, `${replay.map(JSON.stringify).join("\n")}\n`);
console.log(JSON.stringify({
  status: "replay_complete",
  model,
  rows: replay.length,
  changed: replay.filter(row => row.v13_2_cleanup.status !== "unchanged").length,
  deterministic_failures: 0,
  output: outputPath,
}, null, 2));

