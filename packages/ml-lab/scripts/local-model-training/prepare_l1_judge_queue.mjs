#!/usr/bin/env node
import { createHash } from "node:crypto";
import { readFileSync, writeFileSync, renameSync } from "node:fs";
import path from "node:path";
import { pathToFileURL } from "node:url";
import { buildReplayEvidence } from "./replay_permitted_edits.mjs";

const sha256 = (value) => `sha256:${createHash("sha256").update(value).digest("hex")}`;
const readJsonl = (file) => readFileSync(file, "utf8").split(/\r?\n/u).filter(Boolean).map(JSON.parse);
const valueAfter = (args, flag) => args[args.indexOf(flag) + 1];
const atomicWrite = (file, content) => { const temp = `${file}.tmp-${process.pid}`; writeFileSync(temp, content, "utf8"); renameSync(temp, file); };

export function createJudgeRequest({ item, cloud, resolver, row }) {
  if (!cloud || !resolver || !row) throw new Error(`missing binding material: ${item.row_id}`);
  if (cloud.input_binding_hash !== item.input_binding_hash || resolver.input_binding_hash !== item.input_binding_hash) throw new Error(`input binding mismatch: ${item.row_id}`);
  if (cloud.output_hash !== sha256(cloud.output) || cloud.status !== "ok" || cloud.critical_flags?.length) throw new Error(`invalid teacher result: ${item.row_id}`);
  if (row.input_text !== item.input_text || JSON.stringify(row.bounded_context ?? {}) !== JSON.stringify(item.bounded_context ?? {})) throw new Error(`current source drift: ${item.row_id}`);
  const permittedEdits = row.structured_spans?.permitted_changes ?? row.permitted_edit_ops ?? [];
  const replay = buildReplayEvidence({ input: item.input_text, proposedOutput: cloud.output, edits: permittedEdits });
  return {
    schema_version: "heardright.l1_judge_request.v2",
    row_id: item.row_id,
    input_binding_hash: item.input_binding_hash,
    input_text: item.input_text,
    bounded_context: item.bounded_context ?? {},
    proposed_output: cloud.output,
    output_hash: cloud.output_hash,
    protected_spans: row.structured_spans?.protected ?? row.protected_spans ?? [],
    permitted_edits: permittedEdits,
    deletion_permissions: row.structured_spans?.deletion_permissions ?? row.removable_spans ?? [],
    authorized_replay_output_hash: replay.authorized_output_hash,
    proposed_output_matches_authorized_replay: replay.matches,
    disposition: resolver.disposition,
  };
}

export function prepareJudgeQueue({ queueFile, cloudFile, resolverFile, outputFile, corpusFiles }) {
  const queue = readJsonl(queueFile);
  const clouds = readJsonl(cloudFile);
  const resolvers = readJsonl(resolverFile);
  const corpus = corpusFiles.flatMap(readJsonl);
  const uniqueMap = (rows, label) => {
    const map = new Map();
    for (const row of rows) { if (!row.row_id || map.has(row.row_id)) throw new Error(`${label}: duplicate/missing row_id ${row.row_id}`); map.set(row.row_id, row); }
    return map;
  };
  const cloudMap = uniqueMap(clouds, "cloud");
  const resolverMap = uniqueMap(resolvers, "resolver");
  const corpusMap = uniqueMap(corpus, "corpus");
  if (queue.length !== clouds.length) throw new Error(`coverage mismatch queue=${queue.length} cloud=${clouds.length}`);
  const requests = queue.map((item) => createJudgeRequest({ item, cloud: cloudMap.get(item.row_id), resolver: resolverMap.get(item.row_id), row: corpusMap.get(item.row_id) }));
  atomicWrite(outputFile, `${requests.map(JSON.stringify).join("\n")}\n`);
  return { status: "ok", rows: requests.length, output: path.resolve(outputFile), sha256: sha256(readFileSync(outputFile)) };
}

function cli() {
  const args = process.argv.slice(2);
  const queueFile = valueAfter(args, "--queue");
  const cloudFile = valueAfter(args, "--cloud-results");
  const resolverFile = valueAfter(args, "--resolvers");
  const outputFile = valueAfter(args, "--out");
  const corpusFiles = args.filter((_, index) => args[index - 1] === "--corpus");
  if (!queueFile || !cloudFile || !resolverFile || !outputFile || !corpusFiles.length) throw new Error("usage: --queue Q --cloud-results C --resolvers R --corpus F [...] --out O");
  console.log(JSON.stringify(prepareJudgeQueue({ queueFile, cloudFile, resolverFile, outputFile, corpusFiles })));
}

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) cli();
