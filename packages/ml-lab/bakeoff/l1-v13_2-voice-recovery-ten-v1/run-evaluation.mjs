#!/usr/bin/env node
import crypto from "node:crypto";
import { execFileSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(new URL(import.meta.url)));
const basePath = path.join(here, "base-prompt.txt");
const manifestPath = path.join(here, "candidate-manifest.json");
const corpusPath = path.join(here, "candidate-corpus.jsonl");
const resultsPath = path.join(here, "stage1-results.json");
const endpoint = "https://openrouter.ai/api/v1/chat/completions";
const expected = {
  base: "e6dbb228173316e81861e567e2e76de3b85a41f50d060d1f54489a84e033ba3b",
  manifest: "83702979a995cd0c122676da30e13727578d00b06f3db96c6daabb7fa5731a54",
  corpus: "2d7aefa2242e3f300705c45912fa9891eb683456379d3b72969bfa1cbf1307b5",
};
const base = fs.readFileSync(basePath, "utf8").trim();
const manifestText = fs.readFileSync(manifestPath, "utf8");
const corpusText = fs.readFileSync(corpusPath, "utf8");

for (const [label, file, hash] of [
  ["base", basePath, expected.base],
  ["manifest", manifestPath, expected.manifest],
  ["corpus", corpusPath, expected.corpus],
]) {
  const actual = sha256(fs.readFileSync(file));
  if (actual !== hash) fail(`${label} hash mismatch: ${actual}`);
}

const manifest = JSON.parse(manifestText);
const corpus = corpusText
  .split(/\r?\n/)
  .filter(Boolean)
  .map((line) => JSON.parse(line));
if (manifest.arms.length !== 11 || corpus.length !== 3) fail("expected 11 arms and 3 rows");
if (sha256(base) !== "09d79b9e3a8b091d1551f79e9672ec3d63df50821dede18b9daef2536f664f88") {
  fail("effective 13.2 hash mismatch");
}

const prompts = manifest.arms.map((arm) => ({
  id: arm.id,
  prompt: composePrompt(arm),
}));
const cells = prompts
  .flatMap((arm) => corpus.map((row) => ({ arm, row })))
  .sort((left, right) =>
    sha256(`heardright-voice-v1:${left.arm.id}:${left.row.id}`).localeCompare(
      sha256(`heardright-voice-v1:${right.arm.id}:${right.row.id}`),
    ),
  );

const previous = fs.existsSync(resultsPath) ? JSON.parse(fs.readFileSync(resultsPath, "utf8")) : null;
if (previous && (
  previous.manifest_sha256 !== expected.manifest ||
  previous.corpus_sha256 !== expected.corpus ||
  previous.base_sha256 !== expected.base
)) {
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

for (const [index, cell] of cells.entries()) {
  if (results.some((row) => row.arm_id === cell.arm.id && row.row_id === cell.row.id)) continue;
  const wait = 2_000 - (Date.now() - lastLaunch);
  if (wait > 0) await sleep(wait);
  lastLaunch = Date.now();
  try {
    const result = await callProvider(cell);
    results.push(result);
    cumulativeCost += Number(result.usage?.cost ?? 0);
    if (cumulativeCost > 0.25) fail(`cost ceiling exceeded: ${cumulativeCost}`);
    consecutiveFailures = 0;
    writeResults();
    process.stderr.write(`${results.length}/33 ${cell.row.id} ${cell.arm.id}\n`);
  } catch (error) {
    consecutiveFailures += 1;
    process.stderr.write(`unavailable ${cell.row.id} ${cell.arm.id}: ${error.message}\n`);
    if (consecutiveFailures >= 2) fail("two consecutive unavailable cells");
    if (index + 1 === cells.length) throw error;
  }
}

if (results.length !== 33) fail(`incomplete stage 1: ${results.length}/33`);
writeResults();
process.stdout.write(`${resultsPath}\n`);

function composePrompt(arm) {
  const additions = arm.components.map((name) => {
    const component = manifest.components[name];
    if (!component) fail(`unknown component: ${name}`);
    return component;
  });
  const semantic = additions.length
    ? base.replace(manifest.insert_after, `${manifest.insert_after}\n\n${additions.join("\n\n")}`)
    : base;
  return `${manifest.wire}\n\n${semantic}`;
}

async function callProvider(cell) {
  const messages = [
    { role: "system", content: cell.arm.prompt },
    {
      role: "user",
      content: `SCHEMA:RIGHTKIT_ML_CLOUD_C2\nMODE:L1\nSOURCE_JSON:${JSON.stringify(cell.row.raw)}`,
    },
  ];
  const body = {
    model: manifest.model,
    messages,
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
          "x-heardright-ai-prompt-version": cell.arm.id,
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
    if (parsed.model && parsed.model !== manifest.model) {
      throw new Error(`wrong model: ${parsed.model}`);
    }
    const output = parsed.choices?.[0]?.message?.content?.trim() ?? "";
    if (!output) throw new Error("empty output");
    return {
      arm_id: cell.arm.id,
      row_id: cell.row.id,
      prompt_sha256: sha256(cell.arm.prompt),
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
    base_sha256: expected.base,
    manifest_sha256: expected.manifest,
    corpus_sha256: expected.corpus,
    model: manifest.model,
    temperature: manifest.temperature,
    reasoning_effort: manifest.reasoning_effort,
    cumulative_cost: cumulativeCost,
    prompts: prompts.map((arm) => ({ id: arm.id, sha256: sha256(arm.prompt) })),
    results,
  }, null, 2)}\n`);
}

function maxTokens(source) {
  const words = source.trim().split(/\s+/).length + 180;
  const chars = Math.floor([...source].length / 3) + 180;
  return Math.max(256, Math.min(1_200, Math.max(words, chars)));
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
