#!/usr/bin/env node
import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

import { numericAtomSignature, repairNumericMaterial } from "./numeric-span-repair.mjs";

const here = path.dirname(fileURLToPath(import.meta.url));
const args = new Map();
for (let index = 2; index < process.argv.length; index++) {
  const argument = process.argv[index];
  if (argument === "--dry-run") continue;
  if (!["--manifest", "--corpus", "--output"].includes(argument)) {
    fail(`unknown argument: ${argument}`);
  }
  const value = process.argv[++index];
  if (!value) fail(`missing value for ${argument}`);
  args.set(argument, value);
}
const manifestPath = path.resolve(args.get("--manifest") || path.join(here, "candidate-manifest.json"));
const corpusPath = path.resolve(args.get("--corpus") || path.join(here, "candidate-corpus.jsonl"));
const outputPath = args.get("--output") ? path.resolve(args.get("--output")) : null;
const dryRun = process.argv.includes("--dry-run");

function sha256File(file) {
  return crypto.createHash("sha256").update(fs.readFileSync(file)).digest("hex");
}

function sha256Text(value) {
  return crypto.createHash("sha256").update(value).digest("hex");
}

function readJsonl(file) {
  return fs.readFileSync(file, "utf8").split(/\r?\n/).filter(Boolean).map(JSON.parse);
}

function fail(message) {
  console.error(message);
  process.exit(1);
}

const manifest = JSON.parse(fs.readFileSync(manifestPath, "utf8"));
const corpus = readJsonl(corpusPath);
const implementation = manifest.implementation || {};
const repairPath = path.resolve(here, implementation.numeric_span_repair_file || "numeric-span-repair.mjs");
const runnerPath = fileURLToPath(import.meta.url);

for (const [label, actual, expected] of [
  ["numeric span repair", sha256File(repairPath), implementation.numeric_span_repair_sha256],
  ["evaluation runner", sha256File(runnerPath), implementation.runner_sha256],
  ["candidate corpus", sha256File(corpusPath), manifest.corpus.sha256],
]) {
  if (!expected || actual !== expected) fail(`${label} hash mismatch: expected ${expected || "<missing>"}, got ${actual}`);
}
if (corpus.length !== manifest.corpus.row_count) {
  fail(`corpus row count mismatch: expected ${manifest.corpus.row_count}, got ${corpus.length}`);
}
if (new Set(corpus.map(row => row.row_id)).size !== corpus.length) fail("duplicate row_id in corpus");

if (dryRun) {
  console.log(JSON.stringify({
    status: "dry_run_ok",
    rows: corpus.length,
    manifest: manifestPath,
    corpus: corpusPath,
    implementation: {
      numeric_span_repair_sha256: sha256File(repairPath),
      runner_sha256: sha256File(runnerPath),
    },
  }, null, 2));
  process.exit(0);
}

if (!outputPath) fail("--output is required for provider execution");
const apiKey = process.env.GROQ_API_KEY;
if (!apiKey) fail("GROQ_API_KEY unavailable");
if (fs.existsSync(outputPath)) fail(`refusing to overwrite existing output: ${outputPath}`);

const provider = manifest.provider;
const prompt = manifest.prompt;
for (const [label, actual, expected] of [
  ["prompt core", sha256Text(prompt.system_core), prompt.core_sha256],
  ["capability map", sha256Text(JSON.stringify(prompt.capabilities)), prompt.capabilities_sha256],
]) {
  if (!expected || actual !== expected) fail(`${label} hash mismatch: expected ${expected || "<missing>"}, got ${actual}`);
}
const sleep = milliseconds => new Promise(resolve => setTimeout(resolve, milliseconds));

function systemFor(labels) {
  return [prompt.system_core, ...labels.map(label => prompt.capabilities[label])].join("\n\n");
}

function userFor(inputText, labels) {
  const lines = ["SCHEMA:RIGHTKIT_ML_CLOUD_C1", "MODE:L1"];
  if (labels.length) lines.push(`CAPABILITIES:${labels.join(",")}`);
  lines.push(`SOURCE_JSON:${JSON.stringify(inputText)}`);
  return lines.join("\n");
}

function maxTokens(inputText) {
  const words = inputText.trim() ? inputText.trim().split(/\s+/).length : 0;
  const chars = [...inputText].length;
  return Math.max(128, Math.min(1200, Math.max(words + 160, Math.floor(chars / 3) + 160)));
}

async function callProvider(row) {
  const body = {
    model: provider.model,
    messages: [
      { role: "system", content: systemFor(row.capabilities) },
      { role: "user", content: userFor(row.input_text, row.capabilities) },
    ],
    temperature: provider.temperature,
    max_tokens: maxTokens(row.input_text),
    stream: provider.stream,
    reasoning_effort: provider.reasoning_effort,
  };
  for (let attempt = 1; attempt <= provider.retry_limit_per_row + 1; attempt++) {
    let response;
    try {
      response = await fetch(provider.endpoint, {
        method: "POST",
        headers: {
          "content-type": "application/json",
          accept: "application/json",
          authorization: `Bearer ${apiKey}`,
          "x-heardright-ai-prompt-version": prompt.version,
        },
        body: JSON.stringify(body),
        signal: AbortSignal.timeout(provider.timeout_ms),
      });
    } catch (error) {
      if (attempt > provider.retry_limit_per_row) {
        return { ok: false, attempts: attempt, error: `${error.name}: ${error.message}` };
      }
      await sleep(provider.minimum_pace_ms);
      continue;
    }
    const responseText = await response.text();
    const retriable = response.status === 429 || response.status >= 500;
    if (!response.ok && retriable && attempt <= provider.retry_limit_per_row) {
      const retryAfter = Number(response.headers.get("retry-after"));
      await sleep(Number.isFinite(retryAfter) && retryAfter > 0
        ? Math.min(65_000, (retryAfter + 1) * 1000)
        : provider.minimum_pace_ms);
      continue;
    }
    if (!response.ok) return { ok: false, attempts: attempt, error: `HTTP ${response.status}` };
    const data = JSON.parse(responseText);
    return {
      ok: true,
      attempts: attempt,
      raw_output: (data.choices?.[0]?.message?.content || "").trim(),
      usage: data.usage || null,
    };
  }
}

function commonRawChecks(rawOutput) {
  return {
    nonempty: rawOutput.length > 0,
    no_envelope: !/SCHEMA:|MODE:|CAPABILITIES:|SOURCE_JSON:/.test(rawOutput),
    no_forbidden_format: !/```|`|—|–/.test(rawOutput),
  };
}

function v13WholeFallback(inputText, rawOutput) {
  const sourceSignature = numericAtomSignature(inputText);
  const outputSignature = numericAtomSignature(inputText, rawOutput);
  const numericEquivalent = JSON.stringify(sourceSignature) === JSON.stringify(outputSignature);
  return {
    status: numericEquivalent ? "accepted_numeric_equivalent" : "fallback_numeric_whole_output",
    delivered: numericEquivalent ? rawOutput : inputText,
  };
}

const resultRows = [];
let consecutiveFailures = 0;
let lastLaunch = 0;
for (const row of corpus) {
  const waitForPace = provider.minimum_pace_ms - (Date.now() - lastLaunch);
  if (waitForPace > 0) await sleep(waitForPace);
  lastLaunch = Date.now();
  const started = Date.now();
  const providerResult = await callProvider(row);
  if (!providerResult.ok) {
    consecutiveFailures++;
    resultRows.push({
      row_id: row.row_id,
      capabilities: row.capabilities,
      provider: providerResult,
      elapsed_ms: Date.now() - started,
    });
    fs.writeFileSync(outputPath, `${resultRows.map(result => JSON.stringify(result)).join("\n")}\n`);
    if (consecutiveFailures >= 3) fail("circuit opened after 3 consecutive provider failures");
    continue;
  }
  consecutiveFailures = 0;
  const rawChecks = commonRawChecks(providerResult.raw_output);
  const v13 = v13WholeFallback(row.input_text, providerResult.raw_output);
  const v13_1 = repairNumericMaterial(row.input_text, providerResult.raw_output);
  resultRows.push({
    row_id: row.row_id,
    capabilities: row.capabilities,
    provider: providerResult,
    raw_checks: rawChecks,
    v13,
    v13_1,
    elapsed_ms: Date.now() - started,
  });
  fs.writeFileSync(outputPath, `${resultRows.map(result => JSON.stringify(result)).join("\n")}\n`);
}

console.log(JSON.stringify({
  status: "complete",
  rows: resultRows.length,
  provider_ok: resultRows.filter(row => row.provider.ok).length,
  output: outputPath,
}, null, 2));
