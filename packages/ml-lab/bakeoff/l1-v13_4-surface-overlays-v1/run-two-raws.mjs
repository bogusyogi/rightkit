#!/usr/bin/env node
import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(new URL(import.meta.url)));
const packetPath = path.join(here, "experiment-packet.md");
const inputsPath = path.resolve(here, "../l1-v13_3-v13_5-two-transcript-v1/inputs.json");
const resultsPath = path.join(here, "two-raw-results.json");
const reportPath = path.join(here, "two-raw-outputs.md");
const endpoint = "https://api.groq.com/openai/v1/chat/completions";
const model = "qwen/qwen3.6-27b";
const apiKey = process.env.GROQ_API_KEY;

if (!apiKey) fail("GROQ_API_KEY unavailable");
if (fs.existsSync(resultsPath) || fs.existsSync(reportPath)) fail("refusing to overwrite output artifacts");

const packet = fs.readFileSync(packetPath, "utf8");
const fixtures = JSON.parse(fs.readFileSync(inputsPath, "utf8")).map(row => ({
  id: row.id,
  label: row.label,
  raw: row.raw,
}));
const core = blockAfter(packet, "## Locked core: v1.3.4");
const production = blockAfter(packet, "## Production control: v1.3.2");
const arms = [
  { id: "CONTROL-A", label: "Control A: v1.3.4", prompt: core },
  { id: "CONTROL-B", label: "Control B: v1.3.2", prompt: production },
  ...[21, 22, 23, 24, 25, 26].map(number => {
    const heading = packet.match(new RegExp(`^### EXP-${number}: (.+)$`, "m"));
    if (!heading) fail(`missing EXP-${number}`);
    const overlay = blockAfter(packet, heading[0]);
    return {
      id: `EXP-${number}`,
      label: `EXP-${number}: ${heading[1]}`,
      prompt: `${core}\n\n${overlay}`,
      overlay,
    };
  }),
];
const results = [];
const sleep = milliseconds => new Promise(resolve => setTimeout(resolve, milliseconds));
let lastLaunch = 0;

for (const arm of arms) {
  for (const fixture of fixtures) {
    const wait = 4_000 - (Date.now() - lastLaunch);
    if (wait > 0) await sleep(wait);
    lastLaunch = Date.now();
    const started = Date.now();
    const provider = await callGroq(arm, fixture);
    results.push({
      arm_id: arm.id,
      fixture_id: fixture.id,
      prompt_sha256: sha256(arm.prompt),
      output: provider.output,
      attempts: provider.attempts,
      elapsed_ms: Date.now() - started,
      usage: provider.usage,
    });
    fs.writeFileSync(resultsPath, `${JSON.stringify(resultDocument(), null, 2)}\n`);
  }
}

fs.writeFileSync(reportPath, renderReport());
console.log(JSON.stringify({ status: "complete", calls: results.length, report: reportPath }, null, 2));

function fail(message) {
  console.error(message);
  process.exit(1);
}

function blockAfter(markdown, heading) {
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
  for (let attempt = 1; attempt <= 3; attempt++) {
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
      if (attempt === 3) fail(`${arm.id}/${fixture.id}: ${error.name}: ${error.message}`);
      await sleep(10_000);
      continue;
    }
    const responseText = await response.text();
    if (!response.ok) {
      if ((response.status === 429 || response.status >= 500) && attempt < 3) {
        const retryAfter = Number(response.headers.get("retry-after"));
        await sleep(Number.isFinite(retryAfter) && retryAfter > 0
          ? Math.min(65_000, (retryAfter + 1) * 1_000)
          : 10_000);
        continue;
      }
      fail(`${arm.id}/${fixture.id}: Groq HTTP ${response.status}`);
    }
    const parsed = JSON.parse(responseText);
    const output = (parsed.choices?.[0]?.message?.content || "").trim();
    if (!output) fail(`${arm.id}/${fixture.id}: empty output`);
    return { output, attempts: attempt, usage: parsed.usage || null };
  }
  fail(`${arm.id}/${fixture.id}: attempts exhausted`);
}

function resultDocument() {
  return {
    schema: "heardright.surface_overlay_two_raws.v1",
    created_at: new Date().toISOString(),
    provider: "groq",
    model,
    temperature: 0,
    reasoning_effort: "none",
    packet_sha256: sha256(packet),
    fixtures,
    arms: arms.map(arm => ({
      id: arm.id,
      label: arm.label,
      prompt_sha256: sha256(arm.prompt),
      overlay: arm.overlay || null,
    })),
    results,
  };
}

function fenced(value) {
  return `~~~text\n${value}\n~~~`;
}

function renderReport() {
  const lines = [
    "# HeardRight v1.3.4 Surface Overlays: Two-Raw Outputs",
    "",
    `- Model: \`${model}\``,
    "- Temperature: `0`",
    "- Reasoning: `none`",
    `- Fresh Groq calls: \`${results.length}\``,
  ];
  for (const arm of arms) {
    lines.push("", `## ${arm.label}`);
    for (const fixture of fixtures) {
      const result = results.find(row => row.arm_id === arm.id && row.fixture_id === fixture.id);
      lines.push("", `### ${fixture.label}`, "", fenced(result.output));
    }
  }
  return `${lines.join("\n")}\n`;
}
