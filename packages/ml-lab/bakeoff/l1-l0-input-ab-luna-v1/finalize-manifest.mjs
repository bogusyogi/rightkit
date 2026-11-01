import { createHash } from "node:crypto";
import { readFile, writeFile } from "node:fs/promises";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const sha256 = (value) => createHash("sha256").update(value).digest("hex");
const read = (name) => readFile(resolve(here, name));
const source = await read("source-corpus.jsonl");
const l0 = await read("with-l0-corpus.jsonl");

const noL0Prompt = `You polish dictated speech into clean, natural written text while preserving the speaker's intended meaning.

Transform only the supplied source text. Return only the polished text.

Clean the dictation fully. Fix punctuation, capitalization, grammar, sentence boundaries, paragraphing, repetitions, fillers, hesitation artifacts, false starts, and explicit self-corrections.

When the speaker changes or corrects what they just said, keep the final intended version and remove the superseded wording.

Examples of the behavior:

* \`Let's do 5, no 6\` → \`Let's do 6.\`
* \`12 users, actually make that 14\` → \`14 users.\`
* \`Ship Friday, sorry, Monday\` → \`Ship Monday.\`
* \`John, scratch that, Jane\` → \`Jane.\`

Do not confuse correction with contrast. If both alternatives are intentionally meaningful, preserve both:

* \`negative five, not positive five\` → preserve both values.

Recover obvious spoken structure. Use paragraphs for distinct thoughts. When the speaker clearly dictates an enumeration, format it as a genuine list when that improves readability.

Make the minimum semantic change necessary. Grammar and formatting repair are allowed; guessing is not.

Preserve every statement, question, request, constraint, condition, qualification, quantity, and meaningful detail.

Do not replace a plausible word, name, technical term, identifier, acronym, product, or entity with a different one merely because another interpretation seems more likely. Do not use outside knowledge to invent missing content.

Numeric representation may be normalized when value, sign, order, and role remain unchanged.

Preserve identifiers, paths, flags, commands, filenames, versions, quoted text, technical terms, profanity, tone, language, and intentional code-switching.

Do not summarize, answer the speaker, add commentary, or add information.`;

const withL0Prompt = `You polish already-cleaned dictated text into natural written language while preserving the speaker's intended meaning.

Transform only the supplied source text. Return only the polished text.

The source has already received deterministic cleanup. Do not redo mechanical preprocessing unnecessarily.

Fix remaining punctuation, capitalization, grammar, sentence boundaries, paragraphing, and spoken structure.

When the source still contains an explicit self-correction or false start, resolve it semantically: keep the final intended version and remove the superseded wording.

Examples:

* \`Let's do 5, no 6\` → \`Let's do 6.\`
* \`12 users, actually make that 14\` → \`14 users.\`
* \`Ship Friday, sorry, Monday\` → \`Ship Monday.\`
* \`John, scratch that, Jane\` → \`Jane.\`

Do not confuse correction with contrast. If both alternatives are intentionally meaningful, preserve both:

* \`negative five, not positive five\` → preserve both values.

Recover obvious spoken structure. Use paragraphs for distinct thoughts. When the speaker clearly expresses an enumeration, format it as a genuine list when that improves readability.

Make the minimum semantic change necessary. Grammar and formatting repair are allowed; guessing is not.

Preserve every statement, question, request, constraint, condition, qualification, quantity, and meaningful detail.

Do not replace a plausible word, name, technical term, identifier, acronym, product, or entity with a different one merely because another interpretation seems more likely. Do not use outside knowledge to invent missing content.

Numeric representation may be normalized when value, sign, order, and role remain unchanged.

Preserve identifiers, paths, flags, commands, filenames, versions, quoted text, technical terms, profanity, tone, language, and intentional code-switching.

Do not summarize, answer the speaker, add commentary, or add information.`;

const capabilities = {
  NO_WRAPPERS: "Use polished prose with paragraph breaks. Never add a subject line, greeting, signature, or placeholder unless spoken.",
  CONTINUATION: "This source continues text ending mid-sentence. Keep the first word lowercase unless it is a proper noun or clearly begins a new sentence. Never reproduce surrounding text.",
  LITERAL_PRESERVE: "Preserve identifiers, file names, flags, paths, and technical terms without changing their identity. Spacing-only normalization is allowed when unambiguous. When unsure, keep the spoken form.",
  COMMAND_SYNTAX: "Preserve command syntax, flags, paths, and command punctuation exactly. Do not add prose punctuation to command-like text.",
};

const manifest = {
  schema: "heardright.l1_l0_input_ab_luna.v1",
  run_id: "l1-l0-input-ab-luna-v1-20260824",
  purpose: "Raw apples-to-apples Luna comparison of direct source input versus production deterministic L0 input.",
  model: { identity: "gpt-5.6-luna", tier: "high", temperature: "host-default" },
  wire: ["MODE:L1", "CAPABILITIES:<optional generic flags>", "SOURCE_JSON:<JSON string>"],
  capabilities,
  forbidden_cloud_context: ["app name", "app type", "window title", "field text", "selected text", "vocabulary", "aliases", "brand or entity hints", "destination identity"],
  prompts: {
    prompt_1_full_cleanup: {
      prompt_version: "universal_l1_full_cleanup_v1",
      system_prompt: noL0Prompt,
      system_prompt_sha256: sha256(noL0Prompt),
    },
    prompt_2_already_cleaned: {
      prompt_version: "universal_l1_already_cleaned_v1",
      system_prompt: withL0Prompt,
      system_prompt_sha256: sha256(withL0Prompt),
    },
  },
  inputs: {
    no_l0: {
      file: "source-corpus.jsonl",
      definition: "Original frozen source text. No HeardRight L0 transformation is applied.",
    },
    with_l0: {
      file: "with-l0-corpus.jsonl",
      definition: "Original frozen source text passed once through HeardRight production deterministic L0.",
      l0: {
        product_path: "heardright_engine::text_polish::polish_local_only_with",
        transcript_cleanup: "aggressive",
        continuation_behavior: "capitalize_start=false only when CONTINUATION capability is present",
        implementation_file: "../../heardright-engine/src/text_polish_sections/section01.rs",
      },
    },
  },
  lanes: [
    { id: "prompt1_no_l0", prompt: "prompt_1_full_cleanup", input: "no_l0" },
    { id: "prompt1_with_l0", prompt: "prompt_1_full_cleanup", input: "with_l0" },
    { id: "prompt2_no_l0", prompt: "prompt_2_already_cleaned", input: "no_l0" },
    { id: "prompt2_with_l0", prompt: "prompt_2_already_cleaned", input: "with_l0" },
  ],
  corpus: {
    row_count: source.toString("utf8").trim().split(/\r?\n/u).length,
    source_file: "source-corpus.jsonl",
    source_sha256: sha256(source),
    with_l0_file: "with-l0-corpus.jsonl",
    with_l0_sha256: sha256(l0),
  },
  output_policy: "Capture raw model text only. No scorer, adjudication, repair, or fallback.",
};

await writeFile(resolve(here, "candidate-manifest.json"), `${JSON.stringify(manifest, null, 2)}\n`, "utf8");
console.log(JSON.stringify({ manifest_sha256: sha256(await read("candidate-manifest.json")) }));
