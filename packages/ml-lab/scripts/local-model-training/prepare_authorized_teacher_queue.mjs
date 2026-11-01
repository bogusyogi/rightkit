#!/usr/bin/env node
import { createHash } from "node:crypto";
import { readFileSync, renameSync, writeFileSync } from "node:fs";
import path from "node:path";
import { pathToFileURL } from "node:url";
import { replayPermittedEdits } from "./replay_permitted_edits.mjs";

const sha256 = (value) => `sha256:${createHash("sha256").update(value).digest("hex")}`;
const readJsonl = (file) => readFileSync(file, "utf8").split(/\r?\n/u).filter(Boolean).map(JSON.parse);
const atomicWrite = (file, content) => { const temp = `${file}.tmp-${process.pid}`; writeFileSync(temp, content, "utf8"); renameSync(temp, file); };

export function createAuthorizedTeacherRequest(row) {
  if (row?.proposed_output_matches_authorized_replay !== false) throw new Error(`${row?.row_id ?? "<missing>"}: not a replay mismatch`);
  if (typeof row.row_id !== "string" || typeof row.input_text !== "string" || typeof row.input_binding_hash !== "string" || !Array.isArray(row.permitted_edits)) throw new Error(`${row?.row_id ?? "<missing>"}: invalid judge request`);
  const authorized = replayPermittedEdits(row.input_text, row.permitted_edits);
  const authorizedHash = sha256(authorized);
  if (row.authorized_replay_output_hash !== authorizedHash) throw new Error(`${row.row_id}: authorized replay hash drift`);
  return {
    schema_version: "heardright.l1_authorized_teacher_request.v1",
    request_id: `authorized-teacher:${row.row_id}`,
    row_id: row.row_id,
    input_binding_hash: row.input_binding_hash,
    prompt_version: "heardright.l1.minimax_authorized_teacher.v1",
    input_text: row.input_text,
    bounded_context: row.bounded_context ?? {},
    permitted_edits: row.permitted_edits,
    authorized_replay_output_hash: authorizedHash,
  };
}

export function prepareAuthorizedTeacherQueue({ inputFile, outputFile }) {
  const input = readJsonl(inputFile);
  const mismatches = input.filter((row) => row.proposed_output_matches_authorized_replay === false);
  const requests = mismatches.map(createAuthorizedTeacherRequest);
  if (new Set(requests.map((row) => row.row_id)).size !== requests.length) throw new Error("duplicate row IDs in authorized teacher queue");
  atomicWrite(outputFile, `${requests.map(JSON.stringify).join("\n")}\n`);
  return { schema_version: "heardright.l1_authorized_teacher_queue_summary.v1", status: "prepared_not_run", input_rows: input.length, queue_rows: requests.length, output: path.resolve(outputFile), sha256: sha256(readFileSync(outputFile)) };
}

function cli() {
  const args = process.argv.slice(2);
  const valueAfter = (flag) => { const index = args.indexOf(flag); return index >= 0 ? args[index + 1] : undefined; };
  const inputFile = valueAfter("--input"); const outputFile = valueAfter("--out");
  if (!inputFile || !outputFile) throw new Error("usage: --input <judge-request-queue.v2.jsonl> --out <authorized-teacher-queue.jsonl>");
  console.log(JSON.stringify(prepareAuthorizedTeacherQueue({ inputFile, outputFile })));
}

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) {
  try { cli(); } catch (error) { console.error(error.stack ?? error.message); process.exitCode = 1; }
}
