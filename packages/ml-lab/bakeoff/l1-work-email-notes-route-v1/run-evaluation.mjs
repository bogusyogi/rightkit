#!/usr/bin/env node
import crypto from "node:crypto";
import { execFileSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(new URL(import.meta.url)));
const manifestPath = path.join(here, "candidate-manifest.json");
const corpusPath = path.join(here, "candidate-corpus.jsonl");
const promptSourcePath = path.resolve(here, "../../heardright-engine/src/l3_cleanup_sections/section01.rs");
const resultsPath = path.join(here, "results.json");
const endpoint = "https://openrouter.ai/api/v1/chat/completions";

const manifestBytes = fs.readFileSync(manifestPath);
const corpusBytes = fs.readFileSync(corpusPath);
const manifest = JSON.parse(manifestBytes);
const corpus = corpusBytes.toString("utf8").split(/\r?\n/).filter(Boolean).map((line) => JSON.parse(line));
const promptSource = fs.readFileSync(promptSourcePath, "utf8");
if (corpus.length !== 9 || manifest.arms.length !== 3) fail("run requires 9 rows and 3 arms");

const promptBodies = {
  general: rustString("APP_SYS_13_2"),
  work: rustString("APP_SYS_WORK"),
  email: rustString("APP_SYS_EMAIL"),
  notes: rustString("APP_SYS_NOTES"),
};
const taskPrompt = promptBodies.general.replace(
  manifest.insert_after,
  `${manifest.insert_after}\n\n${manifest.task_overlay}`,
);
const cells = corpus.flatMap((row) => manifest.arms.map((arm) => ({ row, arm }))).sort((left, right) =>
  sha256(`heardright-route-v1:${left.row.id}:${left.arm}`).localeCompare(
    sha256(`heardright-route-v1:${right.row.id}:${right.arm}`),
  ),
);

const previous = fs.existsSync(resultsPath) ? JSON.parse(fs.readFileSync(resultsPath, "utf8")) : null;
const manifestSha = sha256(manifestBytes);
const corpusSha = sha256(corpusBytes);
if (previous && (previous.manifest_sha256 !== manifestSha || previous.corpus_sha256 !== corpusSha)) {
  fail("existing results belong to a different frozen run");
}
const results = previous?.results ?? [];
let cumulativeCost = results.reduce((sum, row) => sum + Number(row.usage?.cost ?? 0), 0);
let consecutiveFailures = 0;
let lastLaunch = 0;
const apiKey = execFileSync(
  "security",
  ["find-generic-password", "-s", "openrouter", "-a", os.userInfo().username, "-w"],
  { encoding: "utf8" },
).trim();

for (const cell of cells) {
  if (results.some((row) => row.arm_id === cell.arm && row.row_id === cell.row.id)) continue;
  const wait = 2_000 - (Date.now() - lastLaunch);
  if (wait > 0) await sleep(wait);
  lastLaunch = Date.now();
  try {
    const result = await callProvider(cell);
    results.push(result);
    cumulativeCost += Number(result.usage?.cost ?? 0);
    if (cumulativeCost > 0.15) fail(`cost ceiling exceeded: ${cumulativeCost}`);
    consecutiveFailures = 0;
    writeResults();
    process.stderr.write(`${results.length}/27 ${cell.row.id} ${cell.arm}\n`);
  } catch (error) {
    consecutiveFailures += 1;
    process.stderr.write(`unavailable ${cell.row.id} ${cell.arm}: ${error.message}\n`);
    if (consecutiveFailures >= 2) fail("two consecutive unavailable cells");
  }
}

if (results.length !== 27) fail(`incomplete run: ${results.length}/27`);
writeResults();
process.stdout.write(`${resultsPath}\n`);

function promptFor(cell) {
  const semantic = cell.arm === "GENERAL-13.2"
    ? promptBodies.general
    : cell.arm === "TASK-13.2"
      ? taskPrompt
      : promptBodies[cell.row.surface];
  return `${manifest.wire}\n\n${semantic}`;
}

async function callProvider(cell) {
  const prompt = promptFor(cell);
  const body = {
    model: manifest.model,
    messages: [
      { role: "system", content: prompt },
      { role: "user", content: `SCHEMA:RIGHTKIT_ML_CLOUD_C3\nMODE:L1\nSOURCE_JSON:${JSON.stringify(cell.row.raw)}` },
    ],
    temperature: manifest.temperature,
    max_tokens: maxTokens(cell.row.raw),
    stream: manifest.stream,
    reasoning_effort: manifest.reasoning_effort,
  };
  for (let attempt = 1; attempt <= 2; attempt += 1) {
    const started = Date.now();
    let response;
    try {
      response = await fetch(endpoint, {
        method: "POST",
        headers: {
          "content-type": "application/json",
          authorization: `Bearer ${apiKey}`,
          "x-heardright-ai-prompt-version": `${cell.arm}:${cell.row.surface}`,
        },
        body: JSON.stringify(body),
        signal: AbortSignal.timeout(60_000),
      });
    } catch (error) {
      if (attempt === 2) throw error;
      await sleep(5_000);
      continue;
    }
    const raw = await response.text();
    if (!response.ok) {
      if ((response.status === 429 || response.status >= 500) && attempt < 2) {
        const retryAfter = Number(response.headers.get("retry-after") ?? "5");
        await sleep(Math.max(5_000, retryAfter * 1_000 + 500));
        continue;
      }
      throw new Error(`HTTP ${response.status}: ${raw.slice(0, 180)}`);
    }
    const parsed = JSON.parse(raw);
    if (parsed.model && parsed.model !== manifest.model) throw new Error(`wrong model: ${parsed.model}`);
    const output = parsed.choices?.[0]?.message?.content?.trim() ?? "";
    if (!output) throw new Error("empty output");
    return {
      arm_id: cell.arm,
      row_id: cell.row.id,
      surface: cell.row.surface,
      length: cell.row.length,
      prompt_sha256: sha256(prompt),
      output,
      attempts: attempt,
      elapsed_ms: Date.now() - started,
      provider: parsed.provider ?? "openrouter",
      model: parsed.model ?? manifest.model,
      usage: parsed.usage ?? null,
    };
  }
  throw new Error("attempts exhausted");
}

function writeResults() {
  fs.writeFileSync(resultsPath, `${JSON.stringify({
    schema: manifest.schema,
    created_at: new Date().toISOString(),
    manifest_sha256: manifestSha,
    corpus_sha256: corpusSha,
    model: manifest.model,
    temperature: manifest.temperature,
    reasoning_effort: manifest.reasoning_effort,
    prompt_hashes: {
      general: sha256(`${manifest.wire}\n\n${promptBodies.general}`),
      task: sha256(`${manifest.wire}\n\n${taskPrompt}`),
      work: sha256(`${manifest.wire}\n\n${promptBodies.work}`),
      email: sha256(`${manifest.wire}\n\n${promptBodies.email}`),
      notes: sha256(`${manifest.wire}\n\n${promptBodies.notes}`),
    },
    cumulative_cost: cumulativeCost,
    results,
  }, null, 2)}\n`);
}

function rustString(name) {
  const match = promptSource.match(new RegExp(`const ${name}: &str = r#"([\\s\\S]*?)"#;`));
  if (!match) fail(`missing Rust prompt constant ${name}`);
  return match[1];
}

function maxTokens(text) {
  return Math.max(256, Math.min(1_200, Math.floor([...text].length / 3) + 180));
}

function sha256(value) {
  return crypto.createHash("sha256").update(value).digest("hex");
}

function sleep(milliseconds) {
  return new Promise((resolve) => setTimeout(resolve, milliseconds));
}

function fail(message) {
  console.error(message);
  process.exit(1);
}
