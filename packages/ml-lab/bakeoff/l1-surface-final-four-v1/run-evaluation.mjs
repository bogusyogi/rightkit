#!/usr/bin/env node
import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(new URL(import.meta.url)));
const packetPath = "/Volumes/D/Downloads/HeardRight_Multi_Surface_Prompt_Eval_Packet.md";
const resultsPath = path.join(here, "results.json");
const reportPath = path.join(here, "outputs.md");
const endpoint = "https://api.groq.com/openai/v1/chat/completions";
const model = "qwen/qwen3.6-27b";
const apiKey = process.env.GROQ_API_KEY;

if (!apiKey) fail("GROQ_API_KEY unavailable");

const packet = fs.readFileSync(packetPath, "utf8");
const promptsSection = between(packet, "# Prompts", "# Raw fixtures");
const fixturesSection = between(packet, "# Raw fixtures", "# Output order");
const base = Object.fromEntries(["EXP-28", "EXP-29", "EXP-30", "EXP-31"].map(id => [
  id,
  textBlockAfter(promptsSection, `## ${id}`),
]));

const notesOld = "Do not invent headings, categories, decisions, conclusions, or action items that the speaker did not communicate.";
const notesNew = "You may create concise descriptive headings or organizational labels when they faithfully describe the speaker’s own content and introduce no new claim. Do not invent decisions, conclusions, categories with new meaning, or action items the speaker did not communicate.";
if (!base["EXP-30"].includes(notesOld)) fail("EXP-30 replacement sentence unavailable");

const prompts = [
  {
    id: "EXP-32",
    base: "EXP-31",
    prompt: `${base["EXP-31"]}\n\nPreserve meaningful uncertainty even when it appears as casual wording such as I think, I guess, probably, maybe, I'm not sure, or approximate quantities. Do not rewrite uncertain information as established fact.`,
  },
  {
    id: "EXP-33",
    base: "EXP-28",
    prompt: `${base["EXP-28"]}\n\nExplicit self-corrections override all other preservation rules. Once a word, value, name, clause, or idea has been corrected or replaced, never restore or blend the superseded version back into the output. Preserve meaningful hedges and approximations; do not make a casual or uncertain statement more definite.`,
  },
  {
    id: "EXP-34",
    base: "EXP-29",
    prompt: `${base["EXP-29"]}\n\nPreserve the force of requests and preferences exactly. Wording such as if possible, preferably, could you, would you, I'd prefer, and similar qualifiers must not be removed when doing so would make the speaker more demanding or certain.`,
  },
  {
    id: "EXP-35",
    base: "EXP-30",
    prompt: base["EXP-30"].replace(notesOld, notesNew),
  },
];

const fixtures = [...fixturesSection.matchAll(/^## ([A-Z]+-\d{2}): (.+)$/gm)].map(match => ({
  id: match[1],
  label: match[2].trim(),
  raw: textBlockAfter(fixturesSection, match[0]),
}));
if (prompts.length !== 4 || fixtures.length !== 12) fail(`expected 4 prompts × 12 fixtures; found ${prompts.length} × ${fixtures.length}`);

const existing = fs.existsSync(resultsPath) ? JSON.parse(fs.readFileSync(resultsPath, "utf8")) : null;
const results = existing?.packet_sha256 === sha256(packet) ? existing.results : [];
const sleep = milliseconds => new Promise(resolve => setTimeout(resolve, milliseconds));
let lastLaunch = 0;

for (const fixture of fixtures) {
  for (const arm of prompts) {
    if (results.some(row => row.fixture_id === fixture.id && row.prompt_id === arm.id)) continue;
    const wait = 4_000 - (Date.now() - lastLaunch);
    if (wait > 0) await sleep(wait);
    lastLaunch = Date.now();
    const started = Date.now();
    const provider = await callGroq(arm, fixture);
    results.push({
      fixture_id: fixture.id,
      prompt_id: arm.id,
      output: provider.output,
      attempts: provider.attempts,
      elapsed_ms: Date.now() - started,
      usage: provider.usage,
    });
    writeResults();
    process.stderr.write(`${results.length}/48 ${fixture.id} ${arm.id}\n`);
  }
}

fs.writeFileSync(reportPath, renderReport());
process.stdout.write(`${reportPath}\n`);

function fail(message) {
  console.error(message);
  process.exit(1);
}

function between(markdown, startHeading, endHeading) {
  const start = markdown.indexOf(startHeading);
  const end = markdown.indexOf(endHeading, start + startHeading.length);
  if (start < 0 || end < 0) fail(`missing section: ${startHeading} or ${endHeading}`);
  return markdown.slice(start, end);
}

function textBlockAfter(markdown, heading) {
  const start = markdown.indexOf(heading);
  if (start < 0) fail(`missing heading: ${heading}`);
  const match = markdown.slice(start).match(/```text\n([\s\S]*?)\n```/);
  if (!match) fail(`missing text block after: ${heading}`);
  return match[1].trim();
}

function sha256(value) {
  return crypto.createHash("sha256").update(value).digest("hex");
}

function maxTokens(source) {
  const words = source.trim().split(/\s+/).length;
  const chars = [...source].length;
  return Math.max(512, Math.min(1_200, Math.max(words + 220, Math.floor(chars / 3) + 220)));
}

async function callGroq(arm, fixture) {
  const body = {
    model,
    messages: [
      { role: "system", content: arm.prompt },
      { role: "user", content: fixture.raw },
    ],
    temperature: 0,
    max_tokens: maxTokens(fixture.raw),
    stream: false,
    reasoning_effort: "none",
  };
  for (let attempt = 1; attempt <= 4; attempt++) {
    let response;
    try {
      response = await fetch(endpoint, {
        method: "POST",
        headers: {
          "content-type": "application/json",
          accept: "application/json",
          authorization: `Bearer ${apiKey}`,
          "x-heardright-ai-prompt-version": arm.id,
        },
        body: JSON.stringify(body),
        signal: AbortSignal.timeout(60_000),
      });
    } catch (error) {
      if (attempt === 4) fail(`${fixture.id}/${arm.id}: ${error.name}: ${error.message}`);
      await sleep(10_000);
      continue;
    }
    const responseText = await response.text();
    if (!response.ok) {
      if ((response.status === 429 || response.status >= 500) && attempt < 4) {
        const retryAfter = Number(response.headers.get("retry-after"));
        await sleep(Number.isFinite(retryAfter) && retryAfter > 0
          ? Math.min(65_000, (retryAfter + 1) * 1_000)
          : 10_000);
        continue;
      }
      fail(`${fixture.id}/${arm.id}: Groq HTTP ${response.status}`);
    }
    const parsed = JSON.parse(responseText);
    const output = (parsed.choices?.[0]?.message?.content || "").trim();
    if (!output) fail(`${fixture.id}/${arm.id}: empty output`);
    return { output, attempts: attempt, usage: parsed.usage || null };
  }
  fail(`${fixture.id}/${arm.id}: attempts exhausted`);
}

function writeResults() {
  fs.writeFileSync(resultsPath, `${JSON.stringify({
    schema: "heardright.surface_final_four.v1",
    created_at: new Date().toISOString(),
    provider: "groq",
    model,
    temperature: 0,
    reasoning_effort: "none",
    packet_sha256: sha256(packet),
    prompts: prompts.map(row => ({ id: row.id, base: row.base, prompt: row.prompt, prompt_sha256: sha256(row.prompt) })),
    fixtures,
    results,
  }, null, 2)}\n`);
}

function fenced(value) {
  return `~~~text\n${value}\n~~~`;
}

function renderReport() {
  const lines = [];
  for (const fixture of fixtures) {
    lines.push(`# ${fixture.id}: ${fixture.label}`);
    for (const arm of prompts) {
      const result = results.find(row => row.fixture_id === fixture.id && row.prompt_id === arm.id);
      if (!result) fail(`missing result: ${fixture.id}/${arm.id}`);
      lines.push("", `## ${arm.id}`, "", fenced(result.output));
    }
    lines.push("");
  }
  return `${lines.join("\n").trim()}\n`;
}
