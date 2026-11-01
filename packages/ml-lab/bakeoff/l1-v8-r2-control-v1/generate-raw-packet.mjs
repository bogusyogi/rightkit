#!/usr/bin/env node
import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
const bakeoff = path.resolve(here, "..");
const root = path.resolve(bakeoff, "..", "..", "..");
const readJsonl = file => fs.readFileSync(file, "utf8").trim().split(/\r?\n/).filter(Boolean).map(JSON.parse);
const sha256 = file => crypto.createHash("sha256").update(fs.readFileSync(file)).digest("hex");
const relative = file => path.relative(root, file);

const files = {
  corpus: path.join(here, "candidate-corpus.jsonl"),
  v8Manifest: path.join(here, "candidate-manifest.json"),
  v8Qwen: path.join(here, "groq-partial-results-20260824.jsonl"),
  v8LunaManifest: path.join(here, "luna-manifest-20260824.json"),
  v8Luna: path.join(here, "luna-raw-20260824.jsonl"),
  v13Manifest: path.join(bakeoff, "l1-v13_2-backtrack-v1", "candidate-manifest.json"),
  v13Qwen: path.join(bakeoff, "l1-v13_2-backtrack-v1", "groq-results-20260824.jsonl"),
  v13Luna: path.join(bakeoff, "l1-v13_2-backtrack-v1", "luna-v13_2-raw-20260824.jsonl"),
};

const corpus = readJsonl(files.corpus);
const v8QwenRows = readJsonl(files.v8Qwen);
const v8Qwen = new Map(v8QwenRows.map(row => [row.row_id, row]));
const v8Luna = new Map(readJsonl(files.v8Luna).map(row => [row.row_id, row.raw_output]));
const v13Qwen = new Map(readJsonl(files.v13Qwen).map(row => [row.row_id, row.provider?.raw_output]));
const v13Luna = new Map(readJsonl(files.v13Luna).map(row => [row.row_id, row.raw_output]));
const qwenSuccesses = v8QwenRows.filter(row => row.provider?.ok).length;
const qwenFailures = v8QwenRows.length - qwenSuccesses;
const notReached = corpus.length - v8QwenRows.length;

const artifacts = [
  ["52-row corpus", files.corpus],
  ["v8 Qwen manifest", files.v8Manifest],
  ["v8 Qwen partial", files.v8Qwen],
  ["v8 Luna manifest", files.v8LunaManifest],
  ["v8 Luna raw", files.v8Luna],
  ["v13.2 manifest", files.v13Manifest],
  ["v13.2 Qwen raw/results", files.v13Qwen],
  ["v13.2 Luna raw", files.v13Luna],
];

let output = `# HeardRight raw prompt/model packet: v8 vs v13.2, Qwen vs Luna

Generated: 2026-08-24 IST

## Status

This packet contains raw outputs only. It includes no scores, pass/fail labels, cleanup results, rankings, or ship verdict.

- v8 × Qwen production Groq: ${qwenSuccesses} successful outputs, ${qwenFailures} captured HTTP 429 rows, ${notReached} rows not reached.
- v8 × Luna high: 52 raw outputs.
- v13.2 × Qwen production Groq: 52 raw outputs.
- v13.2 × Luna high: 52 raw outputs.
- Shared corpus: 52 rows in identical row order.
- Groq v8 stopped because free-tier Qwen TPD was 199,126/200,000; each v8 request required about 1,811 tokens.

Qwen model is \`qwen/qwen3.6-27b\` with temperature 0 & reasoning effort none. Luna identity is \`gpt-5.6-luna\`, tier high, generated independently from candidate-visible prompt/corpus files.

For v8, behavioral capabilities select closest R2 destination block: COMMAND_SYNTAX→TERMINAL, LITERAL_PRESERVE→CODE, NO_WRAPPERS→EMAIL, CONTINUATION→core only. No recording-time app/window/field/vocabulary snapshot exists for this frozen corpus, so v8 user wire says no active-app context.

## Frozen artifacts

| Artifact | Path | SHA-256 |
|---|---|---|
`;
for (const [label, file] of artifacts) output += `| ${label} | \`${relative(file)}\` | \`${sha256(file)}\` |\n`;
output += `
## Raw rows

Qwen v8 labels mean:

- \`captured\`: successful Groq response.
- \`HTTP 429\`: attempted but provider returned quota error.
- \`not reached\`: run stopped before that row because quota remained exhausted.

`;

const fence = value => `~~~~text\n${value}\n~~~~`;
for (const row of corpus) {
  const qwenV8 = v8Qwen.get(row.row_id);
  let qwenV8Status = "not reached";
  let qwenV8Output = "NOT CAPTURED";
  if (qwenV8?.provider?.ok) {
    qwenV8Status = "captured";
    qwenV8Output = qwenV8.provider.raw_output;
  } else if (qwenV8) {
    qwenV8Status = qwenV8.provider?.error || "provider failure";
    qwenV8Output = "NO MODEL OUTPUT";
  }
  output += `### ${row.row_id}

Source

${fence(row.input_text)}

v8 · Qwen · ${qwenV8Status}

${fence(qwenV8Output)}

v8 · Luna high

${fence(v8Luna.get(row.row_id) || "MISSING")}

v13.2 · Qwen

${fence(v13Qwen.get(row.row_id) || "MISSING")}

v13.2 · Luna high

${fence(v13Luna.get(row.row_id) || "MISSING")}

`;
}

process.stdout.write(`${output.trimEnd()}\n`);
