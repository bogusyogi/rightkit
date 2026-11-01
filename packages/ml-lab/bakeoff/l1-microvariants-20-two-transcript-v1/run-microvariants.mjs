#!/usr/bin/env node
import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";

const argv = process.argv.slice(2);
const renderOnly = argv.includes("--render-only");
const args = new Map();
for (let index = 0; index < argv.length; index++) {
  if (argv[index] === "--render-only") continue;
  const key = argv[index];
  const value = argv[++index];
  if (!["--source", "--baseline-controls", "--results", "--report"].includes(key) || !value) {
    fail(`invalid argument near ${key || "<end>"}`);
  }
  args.set(key, value);
}

const sourcePath = path.resolve(args.get("--source") || "");
const baselinePath = path.resolve(args.get("--baseline-controls") || "");
const resultPath = path.resolve(args.get("--results") || "results.json");
const reportPath = path.resolve(args.get("--report") || "comparison.md");
const model = "qwen/qwen3.6-27b";
const endpoint = "https://api.groq.com/openai/v1/chat/completions";
const apiKey = process.env.GROQ_API_KEY;

if (!fs.existsSync(sourcePath)) fail(`source unavailable: ${sourcePath}`);
if (!fs.existsSync(baselinePath)) fail(`baseline controls unavailable: ${baselinePath}`);
if (!renderOnly && !apiKey) fail("GROQ_API_KEY unavailable");
if (!renderOnly && (fs.existsSync(resultPath) || fs.existsSync(reportPath))) fail("refusing to overwrite result artifacts");
if (renderOnly && !fs.existsSync(resultPath)) fail(`results unavailable: ${resultPath}`);

const source = fs.readFileSync(sourcePath, "utf8");
const candidates = parseCandidates(source);
const fixtures = parseFixtures(source);
const baselineControls = JSON.parse(fs.readFileSync(baselinePath, "utf8"));
const baseline = Object.fromEntries(baselineControls.map(row => [row.id, row.production_v13_2]));
const results = renderOnly ? JSON.parse(fs.readFileSync(resultPath, "utf8")).results : [];
const sleep = milliseconds => new Promise(resolve => setTimeout(resolve, milliseconds));
let lastLaunch = 0;

if (!renderOnly) {
  for (const candidate of candidates) {
    for (const fixture of fixtures) {
      const paceWait = 4_000 - (Date.now() - lastLaunch);
      if (paceWait > 0) await sleep(paceWait);
      lastLaunch = Date.now();
      const started = Date.now();
      const provider = await callGroq(candidate.prompt, fixture.raw, candidate.id);
      results.push({
        candidate_id: candidate.id,
        fixture_id: fixture.id,
        prompt_sha256: sha256(candidate.prompt),
        output: provider.output,
        usage: provider.usage,
        attempts: provider.attempts,
        elapsed_ms: Date.now() - started,
      });
      fs.writeFileSync(resultPath, `${JSON.stringify(resultDocument(), null, 2)}\n`);
    }
  }
}

fs.writeFileSync(reportPath, renderReport());
console.log(JSON.stringify({
  status: renderOnly ? "report_rendered" : "complete",
  candidates: candidates.length,
  fixtures: fixtures.length,
  calls: renderOnly ? 0 : results.length,
  model,
  report: reportPath,
  results: resultPath,
}, null, 2));

function fail(message) {
  console.error(message);
  process.exit(1);
}

function sha256(value) {
  return crypto.createHash("sha256").update(value).digest("hex");
}

function textBlock(section, label) {
  const match = section.match(/```text\n([\s\S]*?)\n```/);
  if (!match) fail(`missing text block: ${label}`);
  return match[1].trim();
}

function parseCandidates(markdown) {
  const headingRegex = /^## (\d{2})\. (HR-EXP-\d{2}) — (.+)$/gm;
  const headings = [...markdown.matchAll(headingRegex)];
  const parsed = headings.map((match, index) => {
    const sectionEnd = headings[index + 1]?.index ?? markdown.indexOf("\n# Test fixtures", match.index);
    const section = markdown.slice(match.index, sectionEnd < 0 ? markdown.length : sectionEnd);
    const hypothesis = section.match(/\*\*Hypothesis:\*\* ([^\n]+)/)?.[1] || "";
    return {
      number: Number(match[1]),
      id: match[2],
      title: match[3].trim(),
      hypothesis,
      prompt: textBlock(section, match[2]),
    };
  });
  if (parsed.length !== 20) fail(`expected 20 candidates, found ${parsed.length}`);
  return parsed;
}

function parseFixtures(markdown) {
  const fixtures = [
    ["faran-thailand-story", "Fixture A: Faran in Thailand", "## Fixture A: Faran in Thailand", "## Fixture B:"],
    ["premium-listing-redesign", "Fixture B: Premium listing redesign", "## Fixture B: Premium listing redesign", "# Output format"],
  ];
  return fixtures.map(([id, label, startHeading, endHeading]) => {
    const start = markdown.indexOf(startHeading);
    const end = markdown.indexOf(endHeading, start + startHeading.length);
    if (start < 0 || end < 0) fail(`missing fixture section: ${label}`);
    return { id, label, raw: textBlock(markdown.slice(start, end), label) };
  });
}

function maxTokens(sourceText) {
  const words = sourceText.trim().split(/\s+/).length;
  const chars = [...sourceText].length;
  return Math.max(512, Math.min(1_200, Math.max(words + 220, Math.floor(chars / 3) + 220)));
}

async function callGroq(system, user, candidateId) {
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
          "x-heardright-ai-prompt-version": candidateId,
        },
        body: JSON.stringify(body),
        signal: AbortSignal.timeout(60_000),
      });
    } catch (error) {
      if (attempt === 3) fail(`${candidateId}: ${error.name}: ${error.message}`);
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
      fail(`${candidateId}: Groq HTTP ${response.status}`);
    }
    const parsed = JSON.parse(responseText);
    const output = (parsed.choices?.[0]?.message?.content || "").trim();
    if (!output) fail(`${candidateId}: empty Groq output`);
    return { output, usage: parsed.usage || null, attempts: attempt };
  }
  fail(`${candidateId}: provider attempts exhausted`);
}

function resultDocument() {
  return {
    schema: "heardright.microvariant_comparison.v1",
    created_at: new Date().toISOString(),
    provider: "groq",
    model,
    temperature: 0,
    reasoning_effort: "none",
    source: sourcePath,
    source_sha256: sha256(source),
    candidates: candidates.map(candidate => ({
      id: candidate.id,
      title: candidate.title,
      hypothesis: candidate.hypothesis,
      prompt_sha256: sha256(candidate.prompt),
      prompt: candidate.prompt,
    })),
    fixtures,
    results,
  };
}

function fenced(value) {
  return `~~~text\n${value}\n~~~`;
}

function renderReport() {
  const lines = [
    "# HeardRight AI Polish: 20 Microvariants × 2 Raw Transcripts",
    "",
    `- Model: \`${model}\``,
    "- Temperature: `0`",
    "- Reasoning: `none`",
    `- Fresh Groq calls: \`${results.length}\``,
    "- Baseline: user-supplied production v1.3.2 output",
    "- Candidate outputs: 20 prompts × 2 transcripts",
    "",
    "## Production baseline: v1.3.2",
  ];
  for (const fixture of fixtures) {
    lines.push("", `### ${fixture.label}`, "", fenced(baseline[fixture.id]));
  }
  for (const candidate of candidates) {
    lines.push(
      "",
      `## ${String(candidate.number).padStart(2, "0")}. ${candidate.id}: ${candidate.title}`,
    );
    for (const fixture of fixtures) {
      const result = results.find(row => row.candidate_id === candidate.id && row.fixture_id === fixture.id);
      lines.push("", `### ${fixture.label}`, "", fenced(result?.output || "MISSING OUTPUT"));
    }
  }
  return `${lines.join("\n")}\n`;
}
