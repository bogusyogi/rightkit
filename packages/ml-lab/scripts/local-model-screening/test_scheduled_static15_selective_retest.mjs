#!/usr/bin/env node
import assert from "node:assert/strict";
import {
  appendWithOverlap,
  cloudReferenceOutput,
  durationQuantileSample,
  longestCommonPrefix,
  percentile,
  promptForRequest,
  scheduledTranscriptChunks,
  scheduledFieldText,
  stableScheduledPrefixes,
  wordErrorRate,
} from "./run_scheduled_static15_selective_retest.mjs";
import { buildRequest, maxTokensFor } from "./contextual_repair_core.mjs";

const rows = Array.from({ length: 97 }, (_, index) => ({
  id: `c${index}`,
  duration_s: 15 + index,
}));
const sample = durationQuantileSample(rows, 15);
assert.equal(sample.length, 15);
assert.equal(sample[0].id, "c0");
assert.equal(sample.at(-1).id, "c96");
assert.equal(new Set(sample.map((row) => row.id)).size, 15);

assert.equal(percentile([1, 2, 3, 4, 5], 0.5), 3);
assert.equal(percentile([1, 2, 3, 4, 5], 0.95), 5);
assert.equal(longestCommonPrefix("abc def", "abc xyz"), "abc ");

const prefixes = stableScheduledPrefixes(
  "one two three four five six seven eight nine ten eleven twelve",
  { duration_s: 60, tail_audio_s: 10, scheduled_background_windows: 3 },
);
assert.equal(prefixes.length, 3);
assert.ok(prefixes.every((prefix) => prefix.length > 0));
assert.ok(prefixes.every((prefix, index) => index === 0 || prefix.startsWith(prefixes[index - 1])));
assert.ok(prefixes.every((prefix) => !prefix.endsWith(" ")));

const chunkSource = "one two three four five six seven eight nine ten eleven twelve";
const scheduledChunks = scheduledTranscriptChunks(
  chunkSource,
  { duration_s: 60, tail_audio_s: 10, scheduled_background_windows: 3 },
);
assert.equal(scheduledChunks.length, 4);
assert.ok(scheduledChunks.every((chunk) => chunk.length > 0));
assert.equal(scheduledChunks.join(" "), chunkSource);
assert.equal(appendWithOverlap("in the focus of Anderson", "County was hit hardest"), "in the focus of Anderson County was hit hardest");
assert.equal(appendWithOverlap("photo descrip", "descrip is here"), "photo descrip is here");
assert.equal(appendWithOverlap("photo descrip", "ription ready"), "photo description ready");

assert.equal(wordErrorRate("one two three", "one two three"), 0);
assert.equal(wordErrorRate("one two three", "one four three"), 1 / 3);
assert.equal(cloudReferenceOutput({ status: "cleaned", hypothesis: "cleaned text" }), "cleaned text");
assert.equal(cloudReferenceOutput({ status: "skipped_local_fallback", hypothesis: "unchanged text" }), "unchanged text");
assert.equal(scheduledFieldText({ scheduled_context_policy: "none" }, "prior repaired text"), "");
assert.equal(scheduledFieldText({}, "prior repaired text"), "prior repaired text");

const sottoRequest = buildRequest(
  { id: "sotto-350m-q8_0" },
  {
    message_layout: "raw_prompt",
    system: "",
    user: "### Input:\n{transcript}\n\n### Output:\n",
    generation: { max_tokens_policy: "product_full_text" },
  },
  { transcript: "send the invoice", context: {} },
);
assert.equal(sottoRequest.raw_prompt, "### Input:\nsend the invoice\n\n### Output:\n");
assert.equal(
  await promptForRequest(12345, sottoRequest, async () => { throw new Error("chat templating must not run"); }),
  sottoRequest.raw_prompt,
);
assert.equal(maxTokensFor("short transcript", "sotto_full_text"), 900);
assert.equal(maxTokensFor(Array(784).fill("word").join(" "), "sotto_full_text"), 1176);

const fewShotRequest = buildRequest(
  { id: "candidate" },
  {
    message_layout: "few_shot",
    system: "Copy edit.",
    examples: [{ user: "St say hello.", assistant: "Say hello." }],
    user: "Transcript: {transcript}",
    generation: { max_tokens_policy: "product_full_text" },
  },
  { transcript: "real input", context: {} },
);
assert.deepEqual(fewShotRequest.messages, [
  { role: "system", content: "Copy edit." },
  { role: "user", content: "St say hello." },
  { role: "assistant", content: "Say hello." },
  { role: "user", content: "Transcript: real input" },
]);

console.log("scheduled-static15 selective retest unit checks: ok");
