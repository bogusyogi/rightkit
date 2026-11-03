#!/usr/bin/env node
import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
const args = new Map();
for (let index = 2; index < process.argv.length; index++) {
  const argument = process.argv[index];
  if (argument === "--dry-run") continue;
  if (!["--manifest", "--corpus", "--output"].includes(argument)) fail(`unknown argument: ${argument}`);
  const value = process.argv[++index];
  if (!value) fail(`missing value for ${argument}`);
  args.set(argument, value);
}
const manifestPath = path.resolve(args.get("--manifest") || path.join(here, "candidate-manifest.json"));
const corpusPath = path.resolve(args.get("--corpus") || path.join(here, "candidate-corpus.jsonl"));
const outputPath = args.get("--output") ? path.resolve(args.get("--output")) : null;
const dryRun = process.argv.includes("--dry-run");

function fail(message) {
  console.error(message);
  process.exit(1);
}

function sha256(value) {
  return crypto.createHash("sha256").update(value).digest("hex");
}

function sha256File(file) {
  return sha256(fs.readFileSync(file));
}

function readJsonl(file) {
  return fs.readFileSync(file, "utf8").split(/\r?\n/).filter(Boolean).map(JSON.parse);
}

const manifest = JSON.parse(fs.readFileSync(manifestPath, "utf8"));
const corpus = readJsonl(corpusPath);
for (const [label, actual, expected] of [
  ["candidate corpus", sha256File(corpusPath), manifest.corpus.sha256],
  ["prompt core", sha256(manifest.prompt.system_core), manifest.prompt.core_sha256],
  ["destination profiles", sha256(JSON.stringify(manifest.prompt.destination_profiles)), manifest.prompt.destination_profiles_sha256],
  ["user wire", sha256(manifest.prompt.user_wire_template), manifest.prompt.user_wire_sha256],
]) {
  if (!expected || actual !== expected) fail(`${label} hash mismatch: expected ${expected || "<missing>"}, got ${actual}`);
}
if (corpus.length !== manifest.corpus.row_count) fail(`corpus row count mismatch: ${corpus.length}`);
if (new Set(corpus.map(row => row.row_id)).size !== corpus.length) fail("duplicate row_id in corpus");

function destinationProfile(labels) {
  if (labels.includes("COMMAND_SYNTAX")) return "TERMINAL";
  if (labels.includes("LITERAL_PRESERVE")) return "CODE";
  if (labels.includes("NO_WRAPPERS")) return "EMAIL";
  return "CORE_ONLY";
}

function systemFor(profile) {
  const block = manifest.prompt.destination_profiles[profile];
  return block ? `${manifest.prompt.system_core}\n\n${block}` : manifest.prompt.system_core;
}

function userFor(inputText) {
  return manifest.prompt.user_wire_template.replace("{{INPUT}}", inputText);
}

function maxTokens(inputText) {
  const words = inputText.trim() ? inputText.trim().split(/\s+/).length : 0;
  const chars = [...inputText].length;
  return Math.max(128, Math.min(1200, Math.max(words + 160, Math.floor(chars / 3) + 160)));
}

function rawChecks(output) {
  return {
    nonempty: output.length > 0,
    no_envelope: !/<transcript>|Polished output:|Transcript:/i.test(output),
    no_forbidden_format: !/```|`|—|–/.test(output),
  };
}

if (dryRun) {
  console.log(JSON.stringify({
    status: "dry_run_ok",
    rows: corpus.length,
    prompt_version: manifest.prompt.version,
    profile_counts: corpus.reduce((counts, row) => {
      const profile = destinationProfile(row.capabilities);
      counts[profile] = (counts[profile] || 0) + 1;
      return counts;
    }, {}),
  }, null, 2));
  process.exit(0);
}

if (!outputPath) fail("--output is required for provider execution");
const apiKey = process.env.GROQ_API_KEY;
if (!apiKey) fail("GROQ_API_KEY unavailable");
const provider = manifest.provider;
const sleep = milliseconds => new Promise(resolve => setTimeout(resolve, milliseconds));

async function callProvider(row, profile) {
  const body = {
    model: provider.model,
    messages: [
      { role: "system", content: systemFor(profile) },
      { role: "user", content: userFor(row.input_text) },
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
          "x-heardright-ai-prompt-version": manifest.prompt.version,
        },
        body: JSON.stringify(body),
        signal: AbortSignal.timeout(provider.timeout_ms),
      });
    } catch (error) {
      if (attempt > provider.retry_limit_per_row) return { ok: false, attempts: attempt, error: `${error.name}: ${error.message}` };
      await sleep(provider.minimum_pace_ms);
      continue;
    }
    const responseText = await response.text();
    const retryAfter = Number(response.headers.get("retry-after"));
    if (response.status === 429 && Number.isFinite(retryAfter) && retryAfter > 0) {
      await sleep((retryAfter + 1) * 1000);
      attempt--;
      continue;
    }
    const retriable = response.status === 429 || response.status >= 500;
    if (!response.ok && retriable && attempt <= provider.retry_limit_per_row) {
      await sleep(Number.isFinite(retryAfter) && retryAfter > 0
        ? (retryAfter + 1) * 1000
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

const existingResults = fs.existsSync(outputPath) ? readJsonl(outputPath) : [];
const corpusIds = new Set(corpus.map(row => row.row_id));
if (new Set(existingResults.map(row => row.row_id)).size !== existingResults.length) fail("duplicate row_id in resumed output");
if (existingResults.some(row => !corpusIds.has(row.row_id))) fail("resumed output contains unknown row_id");
const resultsById = new Map(existingResults.filter(row => row.provider?.ok).map(row => [row.row_id, row]));
const remaining = corpus.filter(row => !resultsById.has(row.row_id));
let consecutiveFailures = 0;
let lastLaunch = 0;
let paceChain = Promise.resolve();

function persist() {
  const ordered = corpus.filter(row => resultsById.has(row.row_id)).map(row => resultsById.get(row.row_id));
  fs.writeFileSync(outputPath, `${ordered.map(JSON.stringify).join("\n")}\n`);
}

async function waitForLaunchSlot() {
  const launch = paceChain.then(async () => {
    const waitForPace = provider.minimum_pace_ms - (Date.now() - lastLaunch);
    if (waitForPace > 0) await sleep(waitForPace);
    lastLaunch = Date.now();
  });
  paceChain = launch.catch(() => {});
  return launch;
}

let nextRow = 0;
async function worker() {
  while (nextRow < remaining.length) {
    const row = remaining[nextRow++];
    await waitForLaunchSlot();
    const started = Date.now();
    const profile = destinationProfile(row.capabilities);
    const providerResult = await callProvider(row, profile);
    if (!providerResult.ok) consecutiveFailures++;
    else consecutiveFailures = 0;
    resultsById.set(row.row_id, {
      row_id: row.row_id,
      capabilities: row.capabilities,
      destination_profile: profile,
      provider: providerResult,
      raw_checks: providerResult.ok ? rawChecks(providerResult.raw_output) : null,
      elapsed_ms: Date.now() - started,
    });
    persist();
    if (consecutiveFailures >= 3) fail("circuit opened after 3 consecutive provider failures");
  }
}

await Promise.all(Array.from({ length: Math.min(provider.maximum_in_flight, remaining.length) }, worker));
const results = corpus.map(row => resultsById.get(row.row_id));

console.log(JSON.stringify({
  status: "complete",
  rows: results.length,
  provider_ok: results.filter(row => row.provider.ok).length,
  output: outputPath,
}, null, 2));
