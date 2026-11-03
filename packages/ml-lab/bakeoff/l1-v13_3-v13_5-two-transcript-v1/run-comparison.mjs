#!/usr/bin/env node
import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
const args = new Map();
const renderOnly = process.argv.includes("--render-only");
const positionalArgs = process.argv.slice(2).filter(value => value !== "--render-only");
for (let index = 0; index < positionalArgs.length; index += 2) {
  const key = positionalArgs[index];
  const value = positionalArgs[index + 1];
  if (!["--prompt-source", "--inputs", "--results", "--report"].includes(key) || !value) {
    fail(`invalid argument near ${key || "<end>"}`);
  }
  args.set(key, value);
}

const promptSourcePath = path.resolve(args.get("--prompt-source") || "");
const inputPath = path.resolve(args.get("--inputs") || path.join(here, "inputs.json"));
const resultPath = path.resolve(args.get("--results") || path.join(here, "results.json"));
const reportPath = path.resolve(args.get("--report") || path.join(here, "comparison.md"));
const apiKey = process.env.GROQ_API_KEY;
const endpoint = "https://api.groq.com/openai/v1/chat/completions";
const model = "qwen/qwen3.6-27b";
const versions = ["1.3.3", "1.3.4", "1.3.5"];

if (!renderOnly && !apiKey) fail("GROQ_API_KEY unavailable");
if (!fs.existsSync(promptSourcePath)) fail(`prompt source unavailable: ${promptSourcePath}`);
if (!renderOnly && (fs.existsSync(resultPath) || fs.existsSync(reportPath))) fail("refusing to overwrite existing result artifacts");
if (renderOnly && !fs.existsSync(resultPath)) fail(`results unavailable: ${resultPath}`);

const promptSource = fs.readFileSync(promptSourcePath, "utf8");
const inputs = JSON.parse(fs.readFileSync(inputPath, "utf8"));
const prompts = Object.fromEntries(versions.map(version => [version, extractPrompt(promptSource, version)]));
const sleep = milliseconds => new Promise(resolve => setTimeout(resolve, milliseconds));
const results = renderOnly ? JSON.parse(fs.readFileSync(resultPath, "utf8")).results : [];
let lastLaunch = 0;

if (!renderOnly) {
  for (const input of inputs) {
    for (const version of versions) {
      const paceWait = 4_000 - (Date.now() - lastLaunch);
      if (paceWait > 0) await sleep(paceWait);
      lastLaunch = Date.now();
      const started = Date.now();
      const provider = await callGroq(prompts[version], input.raw, version);
      results.push({
        input_id: input.id,
        prompt_version: version,
        prompt_sha256: sha256(prompts[version]),
        elapsed_ms: Date.now() - started,
        output: provider.output,
        usage: provider.usage,
        attempts: provider.attempts,
      });
      fs.writeFileSync(resultPath, `${JSON.stringify(buildResultDocument(), null, 2)}\n`);
    }
  }
}

fs.writeFileSync(reportPath, renderReport());
console.log(JSON.stringify({
  status: renderOnly ? "report_rendered" : "complete",
  calls: renderOnly ? 0 : results.length,
  model,
  results: resultPath,
  report: reportPath,
}, null, 2));

function fail(message) {
  console.error(message);
  process.exit(1);
}

function sha256(value) {
  return crypto.createHash("sha256").update(value).digest("hex");
}

function extractPrompt(source, version) {
  const heading = `### v${version}:`;
  const sectionStart = source.indexOf(heading);
  if (sectionStart < 0) fail(`missing prompt heading: ${heading}`);
  const nextHeading = source.indexOf("\n### v", sectionStart + heading.length);
  const section = source.slice(sectionStart, nextHeading < 0 ? source.length : nextHeading);
  const promptStart = section.indexOf("You polish dictated speech");
  if (promptStart < 0) fail(`missing prompt body for ${version}`);
  let prompt = section.slice(promptStart);
  if (version === "1.3.5") prompt = prompt.split("\nI’d test all three")[0];
  return prompt.trim();
}

function maxTokens(source) {
  const words = source.trim().split(/\s+/).length;
  const chars = [...source].length;
  return Math.max(512, Math.min(1_200, Math.max(words + 220, Math.floor(chars / 3) + 220)));
}

async function callGroq(system, user, version) {
  const body = {
    model,
    messages: [
      { role: "system", content: system },
      { role: "user", content: user },
    ],
    temperature: 0,
    max_tokens: maxTokens(user),
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
          "x-heardright-ai-prompt-version": `v${version}`,
        },
        body: JSON.stringify(body),
        signal: AbortSignal.timeout(60_000),
      });
    } catch (error) {
      if (attempt === 3) fail(`${version}: ${error.name}: ${error.message}`);
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
      fail(`${version}: Groq HTTP ${response.status}`);
    }
    const parsed = JSON.parse(responseText);
    const output = (parsed.choices?.[0]?.message?.content || "").trim();
    if (!output) fail(`${version}: empty Groq output`);
    return { output, usage: parsed.usage || null, attempts: attempt };
  }
  fail(`${version}: provider attempts exhausted`);
}

function buildResultDocument() {
  return {
    schema: "heardright.prompt_delta_comparison.v1",
    created_at: new Date().toISOString(),
    provider: "groq",
    model,
    temperature: 0,
    reasoning_effort: "none",
    prompt_source: promptSourcePath,
    prompt_source_sha256: sha256(promptSource),
    prompts: Object.fromEntries(versions.map(version => [version, {
      sha256: sha256(prompts[version]),
      text: prompts[version],
    }])),
    inputs,
    results,
  };
}

function fenced(value) {
  return `~~~text\n${value}\n~~~`;
}

function renderReport() {
  const lines = [
    "# HeardRight prompt comparison: v1.3.2 to v1.3.5",
    "",
    `- Model: \`${model}\``,
    "- Temperature: `0`",
    "- Reasoning: `none`",
    `- Groq candidate calls: \`${results.length}\``,
    "- v1.3.2: user-supplied production output",
  ];
  for (const input of inputs) {
    lines.push(
      "",
      `## ${input.label}`,
      "",
      "### Raw",
      "",
      fenced(input.raw),
      "",
      "### v1.3.2 (production)",
      "",
      fenced(input.production_v13_2),
    );
    for (const version of versions) {
      const result = results.find(row => row.input_id === input.id && row.prompt_version === version);
      lines.push(
        "",
        `### v${version}`,
        "",
        fenced(result.output),
      );
    }
  }
  return `${lines.join("\n")}\n`;
}
