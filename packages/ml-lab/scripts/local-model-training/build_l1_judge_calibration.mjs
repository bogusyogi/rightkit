#!/usr/bin/env node
import { readFileSync, renameSync, writeFileSync } from "node:fs";
import path from "node:path";
import { pathToFileURL } from "node:url";

export function buildCalibration({ queue, cases }) {
  const byId = new Map(queue.map((row) => [row.row_id, row]));
  if (byId.size !== queue.length) throw new Error("calibration source queue contains duplicate row IDs");
  const ids = new Set();
  const publicQueue = [];
  const privateManifest = [];
  for (const item of cases) {
    if (!item.row_id || ids.has(item.row_id) || !["accept", "reject"].includes(item.expected_verdict)) throw new Error(`invalid calibration case: ${item.row_id ?? "<missing>"}`);
    const row = byId.get(item.row_id);
    if (!row) throw new Error(`calibration row missing from queue: ${item.row_id}`);
    ids.add(item.row_id);
    publicQueue.push(row);
    privateManifest.push({ row_id: item.row_id, expected_verdict: item.expected_verdict });
  }
  return { publicQueue, privateManifest };
}

const readJsonl = (file) => readFileSync(file, "utf8").split(/\r?\n/u).filter(Boolean).map(JSON.parse);
const atomicWrite = (file, value) => { const temporary = `${file}.tmp-${process.pid}`; writeFileSync(temporary, value, "utf8"); renameSync(temporary, file); };
const valueAfter = (args, flag) => args[args.indexOf(flag) + 1];

function cli() {
  const args = process.argv.slice(2);
  const queueFile = valueAfter(args, "--queue");
  const casesFile = valueAfter(args, "--cases");
  const publicFile = valueAfter(args, "--public-out");
  const privateFile = valueAfter(args, "--private-out");
  if (![queueFile, casesFile, publicFile, privateFile].every(Boolean)) throw new Error("usage: --queue Q --cases C --public-out P --private-out M");
  const result = buildCalibration({ queue: readJsonl(queueFile), cases: JSON.parse(readFileSync(casesFile, "utf8")) });
  atomicWrite(publicFile, `${result.publicQueue.map(JSON.stringify).join("\n")}\n`);
  atomicWrite(privateFile, `${result.privateManifest.map(JSON.stringify).join("\n")}\n`);
  console.log(JSON.stringify({ status: "ok", rows: result.publicQueue.length, public_out: path.resolve(publicFile), private_out: path.resolve(privateFile) }));
}

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) cli();
