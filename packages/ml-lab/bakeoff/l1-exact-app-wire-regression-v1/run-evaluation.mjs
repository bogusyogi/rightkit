#!/usr/bin/env node
import crypto from "node:crypto";
import { execFileSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(new URL(import.meta.url)));
const manifestPath = path.join(here, "candidate-manifest.json");
const probesPath = path.join(here, "context-probes.jsonl");
const priorCorpusPath = path.resolve(here, "../l1-work-email-notes-route-v1/candidate-corpus.jsonl");
const promptSourcePath = path.resolve(here, "../../heardright-engine/src/l3_cleanup_sections/section01.rs");
const resultsPath = path.join(here, "openrouter-results.json");
const endpoint = "https://openrouter.ai/api/v1/chat/completions";

const manifestBytes = fs.readFileSync(manifestPath);
const probesBytes = fs.readFileSync(probesPath);
const priorCorpusBytes = fs.readFileSync(priorCorpusPath);
const manifest = JSON.parse(manifestBytes);
const rows = [
  ...parseJsonl(priorCorpusBytes),
  ...parseJsonl(probesBytes),
];
if (rows.length !== 12) fail(`expected 12 rows, received ${rows.length}`);

const promptSource = fs.readFileSync(promptSourcePath, "utf8");
const wire = rustRegularString("APP_WIRE_CONTEXT_PREFIX");
const fieldInstruction = rustRegularString("APP_FIELD_CONTEXT_INSTRUCTION");
const general = rustRawString("APP_SYS_13_2");
const insertAfter = "Preserve every statement, question, request, constraint, condition, qualification, quantity, and meaningful detail.";
const task = general.replace(insertAfter, `${insertAfter}\n\n${manifest.task_overlay}`);
const semanticBySurface = {
  work: task,
  email: general,
  notes: task,
};

const manifestSha = sha256(manifestBytes);
const probesSha = sha256(probesBytes);
const priorCorpusSha = sha256(priorCorpusBytes);
const previous = fs.existsSync(resultsPath) ? JSON.parse(fs.readFileSync(resultsPath, "utf8")) : null;
if (previous && (
  previous.manifest_sha256 !== manifestSha ||
  previous.probes_sha256 !== probesSha ||
  previous.prior_corpus_sha256 !== priorCorpusSha
)) fail("existing results belong to another frozen run");

const results = previous?.results ?? [];
let lastLaunch = 0;
let consecutiveFailures = 0;
const apiKey = execFileSync(
  "security",
  ["find-generic-password", "-s", "openrouter", "-a", process.env.USER, "-w"],
  { encoding: "utf8" },
).trim();

for (const row of rows.sort((a, b) => sha256(`exact-wire:${a.id}`).localeCompare(sha256(`exact-wire:${b.id}`)))) {
  if (results.some((result) => result.row_id === row.id)) continue;
  const wait = 6_000 - (Date.now() - lastLaunch);
  if (wait > 0) await sleep(wait);
  lastLaunch = Date.now();
  try {
    const result = await callProvider(row);
    results.push(result);
    consecutiveFailures = 0;
    writeResults();
    process.stderr.write(`${results.length}/12 ${row.id} ${result.elapsed_ms}ms\n`);
  } catch (error) {
    consecutiveFailures += 1;
    process.stderr.write(`unavailable ${row.id}: ${error.message}\n`);
    if (consecutiveFailures >= 2) fail("two consecutive unavailable cells");
  }
}

if (results.length !== 12) fail(`incomplete run: ${results.length}/12`);
writeResults();
process.stdout.write(`${resultsPath}\n`);

async function callProvider(row) {
  const payload = buildPayload(row);
  for (let attempt = 1; attempt <= 2; attempt += 1) {
    const started = Date.now();
    let response;
    try {
      response = await fetch(endpoint, {
        method: "POST",
        headers: {
          Authorization: `Bearer ${apiKey}`,
          Accept: "application/json",
          "Content-Type": "application/json",
          "User-Agent": "heardright-engine/0.1",
          "X-HeardRight-AI-Prompt-Version": `l1_app_polish_candidate_${manifest.routes[row.surface]}`,
        },
        body: JSON.stringify(payload),
        signal: AbortSignal.timeout(60_000),
      });
    } catch (error) {
      if (attempt === 2) throw error;
      await sleep(8_000);
      continue;
    }
    const raw = await response.text();
    if (!response.ok) {
      if ((response.status === 429 || response.status >= 500) && attempt < 2) {
        const retryAfter = Number(response.headers.get("retry-after") ?? "8");
        await sleep(Math.max(8_000, retryAfter * 1_000 + 500));
        continue;
      }
      throw new Error(`HTTP ${response.status}: ${raw.slice(0, 180)}`);
    }
    const parsed = JSON.parse(raw);
    if (parsed.model && parsed.model !== manifest.model) throw new Error(`wrong model: ${parsed.model}`);
    const output = parsed.choices?.[0]?.message?.content?.trim() ?? "";
    const finishReason = parsed.choices?.[0]?.finish_reason ?? null;
    if (!output) throw new Error("empty output");
    if (finishReason === "length") throw new Error("truncated response");
    const elapsedMs = Date.now() - started;
    const budgetMs = productPrimaryBudgetMs(row.raw);
    return {
      row_id: row.id,
      surface: row.surface,
      length: row.length,
      route: manifest.routes[row.surface],
      field_context_present: Boolean(row.field_text),
      system_sha256: sha256(payload.messages[0].content),
      user_sha256: sha256(payload.messages[1].content),
      payload_sha256: sha256(JSON.stringify(payload)),
      user_content: payload.messages[1].content,
      output,
      finish_reason: finishReason,
      attempts: attempt,
      elapsed_ms: elapsedMs,
      production_primary_budget_reference_ms: budgetMs,
      usage: parsed.usage ?? null,
      model: parsed.model ?? manifest.model,
    };
  }
  throw new Error("attempts exhausted");
}

function buildPayload(row) {
  const semantic = semanticBySurface[row.surface];
  if (!semantic) fail(`unknown surface ${row.surface}`);
  const system = row.field_text
    ? `${wire}\n\n${fieldInstruction}\n\n${semantic}`
    : `${wire}\n\n${semantic}`;
  const capabilities = [];
  if (row.surface === "email") capabilities.push("NO_WRAPPERS");
  if (row.field_text && continuesMidSentence(row.field_text)) capabilities.push("CONTINUATION");
  const lines = ["SCHEMA:RIGHTKIT_ML_CLOUD_C3", "MODE:L1"];
  if (capabilities.length) lines.push(`CAPABILITIES:${capabilities.join(",")}`);
  if (row.field_text) {
    lines.push(`FIELD_CONTEXT_JSON:${JSON.stringify({ preceding_field_text: row.field_text })}`);
  }
  lines.push(`SOURCE_JSON:${JSON.stringify(row.raw)}`);
  return {
    model: manifest.model,
    messages: [
      { role: "system", content: system },
      { role: "user", content: lines.join("\n") },
    ],
    temperature: manifest.temperature,
    max_tokens: maxTokensFor(row.raw),
    stream: manifest.stream,
    reasoning_effort: manifest.reasoning_effort,
    provider: { data_collection: "deny" },
  };
}

function continuesMidSentence(text) {
  const trimmed = text.trimEnd();
  if (!trimmed) return false;
  const chars = [...trimmed].reverse();
  const last = chars.find((char) => !['"', "'", ")", "]", "}", "”", "’"].includes(char));
  return last != null && ![".", "!", "?", ":", "\n"].includes(last);
}

function maxTokensFor(text) {
  const wordsBudget = text.trim().split(/\s+/).filter(Boolean).length + 160;
  const charBudget = Math.floor([...text].length / 3) + 160;
  return Math.max(128, Math.min(1_200, Math.max(wordsBudget, charBudget)));
}

function productPrimaryBudgetMs(text) {
  const total = Math.max(1_500, Math.min(8_000, 1_500 + [...text].length * 2));
  return Math.max(1_500, Math.floor(total * 0.67));
}

function writeResults() {
  fs.writeFileSync(resultsPath, `${JSON.stringify({
    schema: manifest.schema,
    created_at: new Date().toISOString(),
    manifest_sha256: manifestSha,
    probes_sha256: probesSha,
    prior_corpus_sha256: priorCorpusSha,
    model: manifest.model,
    results,
  }, null, 2)}\n`);
}

function parseJsonl(bytes) {
  return bytes.toString("utf8").split(/\r?\n/).filter(Boolean).map((line) => JSON.parse(line));
}

function rustRegularString(name) {
  const match = promptSource.match(new RegExp(`const ${name}: &str =\\s*\n?\\s*"([^"]*)";`));
  if (!match) fail(`missing Rust string ${name}`);
  return JSON.parse(`"${match[1]}"`);
}

function rustRawString(name) {
  const match = promptSource.match(new RegExp(`const ${name}: &str = r#"([\\s\\S]*?)"#;`));
  if (!match) fail(`missing Rust raw string ${name}`);
  return match[1];
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
