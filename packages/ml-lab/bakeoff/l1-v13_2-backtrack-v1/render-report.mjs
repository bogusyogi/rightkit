#!/usr/bin/env node
import crypto from "node:crypto";
import fs from "node:fs";

const args = new Map();
for (let index = 2; index < process.argv.length; index += 2) args.set(process.argv[index], process.argv[index + 1]);
const required = ["--manifest", "--corpus", "--scoring", "--results", "--score", "--groq-replay", "--luna-replay", "--old-groq", "--output"];
for (const key of required) if (!args.get(key)) throw new Error(`${key} is required`);
if (fs.existsSync(args.get("--output"))) throw new Error(`refusing to overwrite ${args.get("--output")}`);

const readJson = file => JSON.parse(fs.readFileSync(file, "utf8"));
const readJsonl = file => fs.readFileSync(file, "utf8").split(/\r?\n/).filter(Boolean).map(JSON.parse);
const hash = file => crypto.createHash("sha256").update(fs.readFileSync(file)).digest("hex");
const manifest = readJson(args.get("--manifest"));
const corpus = readJsonl(args.get("--corpus"));
const scoring = new Map(readJsonl(args.get("--scoring")).map(row => [row.row_id, row]));
const results = readJsonl(args.get("--results"));
const score = readJson(args.get("--score"));
const groqReplay = readJsonl(args.get("--groq-replay"));
const lunaReplay = readJsonl(args.get("--luna-replay"));
const oldGroq = new Map(readJsonl(args.get("--old-groq")).map(row => [row.row_id, row.provider?.raw_output]));
const resultById = new Map(results.map(row => [row.row_id, row]));
const scoreById = new Map(score.row_scores.map(row => [row.row_id, row]));
const attempts = results.reduce((total, row) => total + (row.provider?.attempts || 0), 0);
const promptTokens = results.reduce((total, row) => total + (row.provider?.usage?.prompt_tokens || 0), 0);
const completionTokens = results.reduce((total, row) => total + (row.provider?.usage?.completion_tokens || 0), 0);
const oldIdentical = results.filter(row => row.provider?.raw_output === oldGroq.get(row.row_id)).length;
const cleanupRules = {};
for (const row of results) for (const edit of row.v13_2?.edits || []) cleanupRules[edit.rule] = (cleanupRules[edit.rule] || 0) + 1;

const lines = [];
const push = (...values) => lines.push(...values);
push(
  "# HeardRight v13.2 Backtrack: Groq external-review results",
  "",
  `Generated: ${new Date().toISOString()}`,
  "",
  "## Decision",
  "",
  "**HOLD v13.2 as production replacement.** Deterministic cleanup is safe, but short-prompt Qwen Backtrack is not reliable enough: three of five explicit-correction rows retain superseded wording.",
  "",
  "Next experiment should compare formatter models or specialized training. Do not add deterministic semantic correction logic.",
  "",
  "Verification: focused cleanup/isolation/replay/score reproduction passed; full `pnpm verify` exited 0. Command-level receipt is `tauri-app-next/bakeoff/l1-v13_2-backtrack-v1/verification.json`.",
  "",
  "## Frozen candidate",
  "",
  `- Manifest SHA-256: \`${hash(args.get("--manifest"))}\``,
  `- Corpus SHA-256: \`${hash(args.get("--corpus"))}\``,
  `- Prompt core SHA-256: \`${manifest.prompt.core_sha256}\``,
  `- Capabilities SHA-256: \`${manifest.prompt.capabilities_sha256}\``,
  `- Numeric cleanup SHA-256: \`${manifest.implementation.numeric_format_cleanup_sha256}\``,
  `- Provider runner SHA-256: \`${manifest.implementation.runner_sha256}\``,
  `- Corrected scorer SHA-256: \`${hash(args.get("--scoring"))}\``,
  `- Score runner SHA-256: \`${hash("tauri-app-next/bakeoff/l1-v13_2-backtrack-v1/score-results.mjs")}\``,
  `- Provider result SHA-256: \`${hash(args.get("--results"))}\``,
  `- Implementation commit: \`${manifest.implementation.git_commit}\``,
  `- Scorer commit: \`${manifest.evaluation_scorer.git_commit}\``,
  `- Provider/model: ${manifest.provider.id} \`${manifest.provider.model}\`; temperature 0; reasoning effort none.`,
  "- Cloud fields: `SCHEMA`, `MODE`, behavioral `CAPABILITIES`, `SOURCE_JSON` only.",
  "- No app/window/destination identity, field text, vocabulary, aliases, entity hints, brand bias or hardcoded corrections.",
  "",
  "## v13.2 prompt",
  "",
  "```text",
  manifest.prompt.system_core,
  "```",
  "",
  "Capabilities:",
  "",
);
for (const [label, value] of Object.entries(manifest.prompt.capabilities)) push(`- \`${label}\`: ${value}`);
push(
  "",
  "## Results",
  "",
  "| Measure | Result |",
  "|---|---:|",
  `| Provider completion | ${score.provider_ok}/52 |`,
  `| Automated target checks | ${score.passed}/52 pass; ${score.failed}/52 fail |`,
  `| Explicit self-corrections | ${score.categories.self_corrections.passed}/5 pass; ${score.categories.self_corrections.failed}/5 fail |`,
  `| Contrast | ${score.categories.contrast.passed}/1 pass |`,
  `| False starts | ${score.categories.false_starts.passed}/1 pass |`,
  `| Identifier rows | ${score.categories.identifiers.passed}/5 pass |`,
  `| Capability rows | ${score.categories.capability.passed}/4 pass |`,
  `| Fresh rows changed by deterministic cleanup | ${score.changed_by_cleanup}/52 |`,
  `| Captured Groq replay | 52/52; ${groqReplay.filter(row => row.v13_2_cleanup.status !== "unchanged").length} changed; 0 failures |`,
  `| Captured Luna replay | 52/52; ${lunaReplay.filter(row => row.v13_2_cleanup.status !== "unchanged").length} changed; 0 failures |`,
  `| Fresh v13.2 vs captured v13 raw equality | ${oldIdentical} identical; ${52 - oldIdentical} different |`,
  `| Attempts | ${attempts} |`,
  `| Tokens | ${promptTokens.toLocaleString()} prompt + ${completionTokens.toLocaleString()} completion |`,
  "",
  `Fresh cleanup edits by rule: ${Object.keys(cleanupRules).length ? Object.entries(cleanupRules).map(([rule, count]) => `\`${rule}\` ${count}`).join(", ") : "none"}.`,
  "",
  "Captured Luna rows used the frozen v13/v13.1 prompt. They test v13.2 deterministic cleanup only; no v13.2 Luna provider call was requested or made.",
  "",
  "The first scorer pass was 44/52. It incorrectly treated capitalization and digit-to-word rendering as failures. Correcting only those pre-authorized representation rules produced 46/52; provider output did not change.",
  "",
  "## Automated failures",
  "",
  "| Row | Category | Failure |",
  "|---|---|---|",
);
for (const failure of score.failures) push(`| \`${failure.row_id}\` | ${failure.category} | ${failure.failures.join("; ").replaceAll("|", "\\|")} |`);
push(
  "",
  "## Key correction & contrast evidence",
  "",
  "| Row | Source behavior | Result |",
  "|---|---|---|",
  "| `r_0261c09f9ddc` | 12 → 14 | FAIL: retained both |",
  "| `r_05c7e31c2f67` | 7 → 10; retain timeout 30 | FAIL: retained 7 |",
  "| `r_544483fbcd0c` | Friday → Monday | TARGET PASS: removed Friday, but repeated Monday remains for external minimality review |",
  "| `r_b0be591aa321` | Tuesday → Wednesday; time remains 3 | FAIL: retained Tuesday |",
  "| `r_e5dd3957334d` | John → Jane | PASS: kept only Jane |",
  "| `r_60f032f3382e` | -5 contrasted with +5 | PASS: retained both as `-5` & `+5` |",
  "",
  "## All 52 rows",
  "",
  "Automated checks do not replace external semantic review for fidelity, minimality, fluency, identity preservation or over-paraphrase.",
  "",
);

for (const source of corpus) {
  const result = resultById.get(source.row_id);
  const rowScore = scoreById.get(source.row_id);
  const rowScoring = scoring.get(source.row_id);
  push(
    `### ${source.row_id}`,
    "",
    `- Category: \`${rowScoring.category}\``,
    `- Capabilities: ${source.capabilities.length ? source.capabilities.map(value => `\`${value}\``).join(", ") : "none"}`,
    `- Automated result: **${rowScore.pass ? "PASS" : `FAIL — ${rowScore.failures.join("; ")}`}**`,
    `- Provider: ${result.provider.ok ? `OK, attempt ${result.provider.attempts}, ${result.elapsed_ms} ms` : `FAIL — ${result.provider.error}`}`,
    `- Cleanup: \`${result.v13_2?.status || "unavailable"}\`${result.v13_2?.edits?.length ? `; edits ${result.v13_2.edits.map(edit => `${JSON.stringify(edit.before)} → ${JSON.stringify(edit.after)}`).join(", ")}` : ""}`,
    "",
    "Source",
    "",
    "```text",
    source.input_text,
    "```",
    "",
    "Raw Groq output",
    "",
    "```text",
    result.provider?.raw_output || "<unavailable>",
    "```",
    "",
    "Delivered v13.2 output",
    "",
    "```text",
    result.v13_2?.delivered || "<unavailable>",
    "```",
    "",
  );
}

fs.writeFileSync(args.get("--output"), `${lines.join("\n")}\n`);
console.log(JSON.stringify({ output: args.get("--output"), rows: corpus.length, failures: score.failed }, null, 2));
