#!/usr/bin/env node
import { createHash } from "node:crypto";
import { readFileSync, renameSync, writeFileSync } from "node:fs";
import path from "node:path";
import { pathToFileURL } from "node:url";

const canonical = (value) => JSON.stringify(value);
const sha256 = (value) => `sha256:${createHash("sha256").update(value).digest("hex")}`;
const visiblePayload = (row) => ({ prompt_version: row.prompt_version, input_text: row.input_text, bounded_context: row.bounded_context ?? {} });
const normalizeOldRequest = (row) => row.schema_version === "heardright.l1_judge_request.v1" ? {
  schema_version: "heardright.l1_cloud_request.v1",
  request_id: `cloud-${row.row_id}`,
  row_id: row.row_id,
  input_binding_hash: row.input_binding_hash,
  prompt_version: "heardright.l1.cloud_request.v1",
  input_text: row.input_text,
  bounded_context: row.bounded_context ?? {},
} : row;
const unique = (rows, label) => {
  const map = new Map();
  for (const row of rows) {
    if (!row.row_id || map.has(row.row_id)) throw new Error(`${label}: duplicate or missing row_id ${row.row_id ?? "<missing>"}`);
    map.set(row.row_id, row);
  }
  return map;
};

export function rebindTeacherResults({ oldQueue, oldResults, newQueue }) {
  const oldRequests = unique(oldQueue.map(normalizeOldRequest), "old queue");
  const results = unique(oldResults, "old results");
  unique(newQueue, "new queue");
  const reused = [];
  const fresh = [];
  for (const request of newQueue) {
    const priorRequest = oldRequests.get(request.row_id);
    const priorResult = results.get(request.row_id);
    const sameVisiblePayload = priorRequest && canonical(visiblePayload(priorRequest)) === canonical(visiblePayload(request));
    if (!priorRequest || !priorResult || !sameVisiblePayload) { fresh.push(request); continue; }
    if (priorResult.input_binding_hash !== priorRequest.input_binding_hash || priorResult.request_hash !== sha256(canonical(priorRequest))) throw new Error(`${request.row_id}: prior request binding/hash mismatch`);
    if (priorResult.status !== "ok" || priorResult.output_hash !== sha256(priorResult.output) || priorResult.critical_flags?.length) throw new Error(`${request.row_id}: prior teacher result invalid`);
    reused.push({
      ...priorResult,
      input_binding_hash: request.input_binding_hash,
      request_hash: sha256(canonical(request)),
      result_provenance: "rebound_identical_teacher_visible_payload_v1",
      rebound_from_input_binding_hash: priorRequest.input_binding_hash,
      rebound_from_request_hash: priorResult.request_hash,
      teacher_visible_payload_hash: sha256(canonical(visiblePayload(request))),
    });
  }
  return { reused, fresh };
}

const readJsonl = (file) => readFileSync(file, "utf8").split(/\r?\n/u).filter(Boolean).map(JSON.parse);
const atomicWrite = (file, content) => { const temporary = `${file}.tmp-${process.pid}`; writeFileSync(temporary, content, "utf8"); renameSync(temporary, file); };
const writeJsonl = (file, rows) => atomicWrite(file, rows.length ? `${rows.map(canonical).join("\n")}\n` : "");
const valueAfter = (args, flag) => args[args.indexOf(flag) + 1];

function cli() {
  const args = process.argv.slice(2);
  const oldQueueFile = valueAfter(args, "--old-queue");
  const oldResultsFile = valueAfter(args, "--old-results");
  const newQueueFile = valueAfter(args, "--new-queue");
  const reusedFile = valueAfter(args, "--reused-out");
  const freshFile = valueAfter(args, "--fresh-out");
  const summaryFile = valueAfter(args, "--summary-out");
  if (![oldQueueFile, oldResultsFile, newQueueFile, reusedFile, freshFile, summaryFile].every(Boolean)) throw new Error("usage: --old-queue Q --old-results R --new-queue Q --reused-out R --fresh-out Q --summary-out S");
  const result = rebindTeacherResults({ oldQueue: readJsonl(oldQueueFile), oldResults: readJsonl(oldResultsFile), newQueue: readJsonl(newQueueFile) });
  writeJsonl(reusedFile, result.reused);
  writeJsonl(freshFile, result.fresh);
  const summary = { schema_version: "heardright.l1_teacher_rebind_summary.v1", status: result.fresh.length ? "fresh_teacher_calls_required" : "complete", reused_rows: result.reused.length, fresh_rows: result.fresh.length, new_queue_rows: result.reused.length + result.fresh.length, hashes: { old_queue: sha256(readFileSync(oldQueueFile)), old_results: sha256(readFileSync(oldResultsFile)), new_queue: sha256(readFileSync(newQueueFile)), reused_results: sha256(readFileSync(reusedFile)), fresh_queue: sha256(readFileSync(freshFile)) } };
  atomicWrite(summaryFile, `${JSON.stringify(summary, null, 2)}\n`);
  console.log(JSON.stringify({ ...summary, reused_out: path.resolve(reusedFile), fresh_out: path.resolve(freshFile) }));
}

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) cli();
