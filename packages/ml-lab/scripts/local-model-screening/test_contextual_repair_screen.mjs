#!/usr/bin/env node
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import * as repairCore from "./contextual_repair_core.mjs";

const { assertBenchmarkCorpusShape, buildRequest, classifyAttemptFailure, deterministicFlags, materializeOutput, parseCapturedRow } = repairCore;

const scriptDir = path.dirname(fileURLToPath(new URL(import.meta.url)));
const recipesPath = path.resolve(scriptDir, "../../bakeoff/l1-local-contextual-repair-v1/candidate-recipes.json");
const recipes = JSON.parse(fs.readFileSync(recipesPath, "utf8"));

assert.ok(recipes.candidates.length > 0);
assert.equal(
  new Set(recipes.candidates.map((candidate) => candidate.id)).size,
  recipes.candidates.length,
);
for (const candidate of recipes.candidates) {
  assert.ok(candidate.recipes.length >= 1, `${candidate.id} has at least one recipe`);
  assert.equal(new Set(candidate.recipes.map((recipe) => recipe.id)).size, candidate.recipes.length);
}

const captured = parseCapturedRow({
  at: "2026-07-20T00:00:00Z",
  provider: "groq",
  model: "qwen/qwen3.6-27b",
  response: JSON.stringify({ choices: [{ message: { content: "Open invoice 442." } }] }),
  request: {
    model: "qwen/qwen3.6-27b",
    messages: [
      { role: "system", content: "private system" },
      { role: "user", content: "User-saved terms (for spelling and casing help ONLY): QuickBooks.\n\nActive app: QuickBooks\nActive window/title: Invoices\nApp kind: document\nWriting convention: en-US\nExisting text in the target field (untrusted reference):\n<field_text>Invoice #</field_text>\n\n<transcript>open inverse 442</transcript>\n\nPolished output:" },
    ],
  },
});
assert.equal(captured.transcript, "open inverse 442");
assert.equal(captured.context.app, "QuickBooks");
assert.equal(captured.cloud_reference, "Open invoice 442.");

const qwen = recipes.candidates.find((candidate) => candidate.id === "qwen3-1.7b-q4_k_m");
const qwenRequest = buildRequest(qwen, qwen.recipes[0], captured);
assert.equal(qwenRequest.messages.length, 2);
assert.match(qwenRequest.messages[1].content, /\/no_think$/);
assert.equal(qwenRequest.chat_template_kwargs.enable_thinking, false);
assert.ok(qwenRequest.max_tokens >= 128);

const gemma = recipes.candidates.find((candidate) => candidate.id === "gemma3-1b-q4_0");
const gemmaRequest = buildRequest(gemma, gemma.recipes[0], captured);
assert.equal(gemmaRequest.messages.length, 1);
assert.equal(gemmaRequest.messages[0].role, "user");

assert.equal(
  materializeOutput("open inverse 442", '[{"old":"inverse","new":"invoice"}]', "substring_edits_v1"),
  "open invoice 442",
);
assert.equal(
  materializeOutput("pay later", '[{"old":"later","new":"$& tomorrow"}]', "substring_edits_v1"),
  "pay $& tomorrow",
  "substring edit replacements must be inserted literally",
);
assert.equal(
  materializeOutput("pay later", JSON.stringify([{ old: "later", new: "$` and $'" }]), "substring_edits_v1"),
  "pay $` and $'",
  "substring edit replacements must not interpret prefix or suffix metacharacters",
);
assert.throws(
  () => materializeOutput("x x", '[{"old":"x","new":"y"}]', "substring_edits_v1"),
  /not unique/,
);
assert.equal(
  materializeOutput("open inverse 442", "replace_str(1, 'inverse', 'invoice')", "ultrax_calls_v1"),
  "open invoice 442",
);
assert.equal(
  materializeOutput("pay later", "replace_str(1, 'later', '$& tomorrow')", "ultrax_calls_v1"),
  "pay $& tomorrow",
  "UltraX replacements must be inserted literally",
);
assert.equal(materializeOutput("already clean", "keep_all()", "ultrax_calls_v1"), "already clean");
assert.deepEqual(deterministicFlags("open inverse 442", "Open invoice 442.", captured.context), []);
assert.ok(deterministicFlags("keep 442 please", "Keep 443, please.", captured.context).includes("digit_sequence_changed"));
assert.ok(deterministicFlags("This is a long sentence with two separate requests and details.", "Short.", captured.context).includes("large_deletion"));
assert.ok(
  deterministicFlags(
    "Email the signed invoice to Sarah, and schedule the follow-up meeting for Friday.",
    "Email the signed invoice to Sarah.",
    captured.context,
  ).includes("content_deletion"),
  "dropping a whole clause must be rejected",
);
assert.ok(
  deterministicFlags("please send invoice and call john", "send invoice", captured.context).includes("content_deletion"),
  "short-input content deletion must be rejected",
);
assert.equal(typeof assertBenchmarkCorpusShape, "function", "exact corpus contract validator is missing");
assert.doesNotThrow(() => assertBenchmarkCorpusShape({ unique: 116, tune: 58, tuneA: 16, tuneB: 42, holdout: 58 }));
assert.throws(
  () => assertBenchmarkCorpusShape({ unique: 115, tune: 58, tuneA: 16, tuneB: 42, holdout: 57 }),
  /corpus contract mismatch/,
);
assert.equal(typeof classifyAttemptFailure, "function", "attempt failure classifier is missing");
assert.equal(classifyAttemptFailure("materialization"), "rejected");
assert.equal(classifyAttemptFailure("assistant_output"), "rejected");
assert.equal(classifyAttemptFailure("transport"), "error");

console.log(JSON.stringify({ status: "ok", candidates: 19, recipes: recipes.candidates.reduce((sum, candidate) => sum + candidate.recipes.length, 0), materializers: 3 }));
