// Deterministic generator for the L1 app-aware "already_clean" bucket (250 rows).
// Anti-overedit anchor: teaches the model to return already-correct transcript text
// unchanged, and to apply ONLY trivial casing/terminal-punctuation fixes when needed.
//
// Contract: schema "heardright.l1_appaware_pair.v1" — see
// docs/plans/2026-08-02-granite-l1-appaware-training.md ("Already-clean identity", 250 rows).
//
// Mirrors the lineage/replay discipline of generate_precision_corpus.mjs (read-only
// reference in this directory) but uses its own simpler pending-target-free schema:
// this bucket ships direct training pairs, not adjudication candidates.

import { createHash } from "node:crypto";
import { readFileSync, mkdirSync, writeFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const scriptPath = fileURLToPath(import.meta.url);
const here = dirname(scriptPath);
const outputDir = resolve(here, "corpus/l1-appaware-v1");
const corpusPath = resolve(outputDir, "already_clean.jsonl");
const summaryPath = resolve(outputDir, "already_clean.summary.json");

const SCHEMA = "heardright.l1_appaware_pair.v1";
const BUCKET = "already_clean";
const PRISTINE_COUNT = 150;
const TRIVIAL_COUNT = 100;
const TOTAL = PRISTINE_COUNT + TRIVIAL_COUNT;
const PRISTINE_INSTANCES = 15; // 10 templates x 15 = 150
const TRIVIAL_INSTANCES = 10; // 10 templates x 10 = 100

const sha = (value) => `sha256:${createHash("sha256").update(value).digest("hex")}`;
const pad = (n, width = 3) => String(n).padStart(width, "0");
const pad2 = (n) => String(n).padStart(2, "0");

const scriptHash = sha(readFileSync(scriptPath));

// ---- protected-span value helpers (globally unique across seq 1..250) ----
const emailFor = (seq) => `user${seq}@example-${seq}.test`;
const urlFor = (seq) => `https://docs.example-${seq}.test/guide-${seq}`;
const ticketFor = (seq) => `SX-${7000 + seq}`;
const fileFor = (seq) => `report_2026-08-${pad2(1 + ((seq - 1) % 28))}_${seq}.csv`;
const digitsFor = (seq) => `${800000 + seq * 7}`;
const identifierFor = (seq) => `session_id_${seq}`;

const TOPICS = [
  "design review", "incident retro", "budget check", "client demo", "backlog grooming",
  "architecture review", "security audit", "release rehearsal", "customer call", "sprint planning",
  "postmortem", "hiring sync", "roadmap review", "onboarding session", "morning standup",
];
const DAYS = ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday"];

const DOMAIN_CONTEXT = {
  chat: { app_name: "Slack", window_title: "Team Chat" },
  email: { app_name: "Outlook", window_title: "Inbox" },
  notes: { app_name: "Notion", window_title: "Daily Notes" },
  code: { app_name: "VS Code", window_title: "main.rs" },
  terminal: { app_name: "Terminal", window_title: "zsh" },
};

// ---- deterministic diff: LCS-based char replace-range ops, mirrors the op shape
// (op_id/op/unit/start/end/expected_text/replacement_text/authority_tags/op_hash)
// used by generate_precision_corpus.mjs's buildPermittedEditOps/replayPermittedEditOps.
function buildDeterministicDiff(input, output) {
  if (input === output) return [];
  const rows = input.length + 1;
  const columns = output.length + 1;
  const lcs = new Uint16Array(rows * columns);
  const at = (i, j) => i * columns + j;
  for (let i = input.length - 1; i >= 0; i -= 1) {
    for (let j = output.length - 1; j >= 0; j -= 1) {
      lcs[at(i, j)] = input[i] === output[j] ? 1 + lcs[at(i + 1, j + 1)] : Math.max(lcs[at(i + 1, j)], lcs[at(i, j + 1)]);
    }
  }
  const ops = [];
  let i = 0;
  let j = 0;
  let start = null;
  let expected = "";
  let replacement = "";
  const flush = () => {
    if (start === null) return;
    const tag = /[a-zA-Z]/.test(expected) || /[a-zA-Z]/.test(replacement) ? "leading_capitalization" : "terminal_punctuation";
    const core = {
      op: "replace_range", unit: "utf16_code_unit", start, end: start + expected.length,
      expected_text: expected, replacement_text: replacement, authority_tags: [tag],
    };
    ops.push({ ...core, op_hash: sha(JSON.stringify(core)) });
    start = null; expected = ""; replacement = "";
  };
  while (i < input.length || j < output.length) {
    if (i < input.length && j < output.length && input[i] === output[j]) { flush(); i += 1; j += 1; continue; }
    if (start === null) start = i;
    if (j < output.length && (i === input.length || lcs[at(i, j + 1)] > lcs[at(i + 1, j)])) { replacement += output[j]; j += 1; }
    else { expected += input[i]; i += 1; }
  }
  flush();
  return ops.map((op, index) => ({ op_id: `edit-${pad(index + 1, 2)}`, ...op }));
}

function replayDeterministicDiff(input, ops) {
  if (ops.length === 0) return input;
  let output = input;
  let previousStart = input.length + 1;
  for (const op of [...ops].sort((a, b) => b.start - a.start || b.end - a.end)) {
    if (op.op !== "replace_range" || op.unit !== "utf16_code_unit" || op.start < 0 || op.end < op.start || op.end > input.length || op.end > previousStart) {
      throw new Error(`invalid diff range ${op.op_id}`);
    }
    if (input.slice(op.start, op.end) !== op.expected_text) throw new Error(`diff source mismatch ${op.op_id}`);
    output = `${output.slice(0, op.start)}${op.replacement_text}${output.slice(op.end)}`;
    previousStart = op.start;
  }
  return output;
}

function alnumWords(text) {
  return (text.toLowerCase().match(/[a-z0-9]+/g) || []).sort();
}

function digitSignature(text) {
  return (text.match(/\d+/g) || []).join("|");
}

function toInputVariant(output, variant) {
  let text = output;
  if (variant === 0 || variant === 2) text = text.charAt(0).toLowerCase() + text.slice(1);
  if (variant === 1 || variant === 2) if (text.endsWith(".")) text = text.slice(0, -1);
  return text;
}

// ---- pristine templates: input_text === output_text exactly (no edit needed) ----
const PRISTINE_TEMPLATES = [
  {
    id: "pristine-chat-ticket", domain: "chat",
    spans: (seq) => [ticketFor(seq)],
    text: (seq, i) => `Can you check ticket ${ticketFor(seq)} before the ${TOPICS[i]}?`,
  },
  {
    id: "pristine-email-file-email", domain: "email",
    spans: (seq) => [fileFor(seq), emailFor(seq)],
    text: (seq, i) => `Please review ${fileFor(seq)} and reply to ${emailFor(seq)} by ${DAYS[i % 7]}.`,
  },
  {
    id: "pristine-notes-digits", domain: "notes",
    spans: (seq) => [digitsFor(seq)],
    text: (seq, i) => `The order confirmation number is ${digitsFor(seq)}; ship it by ${DAYS[i % 7]}.`,
  },
  {
    id: "pristine-code-url", domain: "code",
    spans: (seq) => [urlFor(seq)],
    text: (seq) => `The build log for this run is posted at ${urlFor(seq)}; check the failing step first.`,
  },
  {
    id: "pristine-terminal-plain", domain: "terminal",
    spans: () => [],
    text: (seq, i) => `Restart the service once the ${TOPICS[i]} finishes.`,
  },
  {
    id: "pristine-chat-plain", domain: "chat",
    spans: () => [],
    text: (seq, i) => `Thanks for the update, see you at the ${TOPICS[i]}.`,
  },
  {
    id: "pristine-email-ticket-url", domain: "email",
    spans: (seq) => [ticketFor(seq), urlFor(seq)],
    text: (seq) => `Reference ${ticketFor(seq)} is linked from ${urlFor(seq)} for your review.`,
  },
  {
    id: "pristine-notes-file", domain: "notes",
    spans: (seq) => [fileFor(seq)],
    text: (seq) => `Backup for last night is saved to ${fileFor(seq)}.`,
  },
  {
    id: "pristine-code-identifier", domain: "code",
    spans: (seq) => [identifierFor(seq)],
    text: (seq) => `Update the config key ${identifierFor(seq)} before the next deploy.`,
  },
  {
    id: "pristine-terminal-email", domain: "terminal",
    spans: (seq) => [emailFor(seq)],
    text: (seq) => `Send the crash report to ${emailFor(seq)} once you reproduce it.`,
  },
];

// ---- trivial_fix templates: output is the "clean" sentence; input differs ONLY
// by leading-capital and/or a single terminal period ----
const TRIVIAL_TEMPLATES = [
  {
    id: "trivial-chat-ticket", domain: "chat",
    spans: (seq) => [ticketFor(seq)],
    text: (seq, i) => `Check ticket ${ticketFor(seq)} before the ${TOPICS[i]}.`,
  },
  {
    id: "trivial-email-file-email", domain: "email",
    spans: (seq) => [fileFor(seq), emailFor(seq)],
    text: (seq) => `Attach ${fileFor(seq)} and send it to ${emailFor(seq)}.`,
  },
  {
    id: "trivial-notes-digits", domain: "notes",
    spans: (seq) => [digitsFor(seq)],
    text: (seq) => `The tracking number is ${digitsFor(seq)}.`,
  },
  {
    id: "trivial-code-url", domain: "code",
    spans: (seq) => [urlFor(seq)],
    text: (seq) => `Review the pipeline at ${urlFor(seq)}.`,
  },
  {
    id: "trivial-terminal-plain", domain: "terminal",
    spans: () => [],
    text: (seq, i) => `Restart the service after the ${TOPICS[i]} completes.`,
  },
  {
    id: "trivial-chat-plain", domain: "chat",
    spans: () => [],
    text: (seq, i) => `See you at the ${TOPICS[i]} tomorrow.`,
  },
  {
    id: "trivial-email-ticket-url", domain: "email",
    spans: (seq) => [ticketFor(seq), urlFor(seq)],
    text: (seq) => `Ticket ${ticketFor(seq)} is tracked at ${urlFor(seq)}.`,
  },
  {
    id: "trivial-notes-file", domain: "notes",
    spans: (seq) => [fileFor(seq)],
    text: (seq) => `Notes for today are stored in ${fileFor(seq)}.`,
  },
  {
    id: "trivial-code-identifier", domain: "code",
    spans: (seq) => [identifierFor(seq)],
    text: (seq) => `Update the config key ${identifierFor(seq)}.`,
  },
  {
    id: "trivial-terminal-email", domain: "terminal",
    spans: (seq) => [emailFor(seq)],
    text: (seq) => `Escalate to ${emailFor(seq)} if it recurs.`,
  },
];

function makeRow({ seq, subBucket, templateId, domain, inputText, outputText, spans, hasContext }) {
  const id = `already_clean-${pad(seq)}`;
  const bounded_context = hasContext ? { ...DOMAIN_CONTEXT[domain] } : {};
  const must_preserve = [...spans];
  const deterministic_diff = subBucket === "pristine" ? [] : buildDeterministicDiff(inputText, outputText);
  const seed = `${subBucket}-${templateId}-${pad(seq)}`;
  const template_hash = sha(JSON.stringify({ bucket: BUCKET, sub_bucket: subBucket, template_id: templateId }));
  const source_hash = sha(JSON.stringify({ input_text: inputText, bounded_context, seed }));
  const deterministic_diff_hash = sha(JSON.stringify(deterministic_diff));
  const row = {
    schema: SCHEMA,
    id,
    bucket: BUCKET,
    sub_bucket: subBucket,
    input_text: inputText,
    output_text: outputText,
    bounded_context,
    must_preserve,
    deterministic_diff,
    source: "builder_deterministic_v1",
    construction: "target_first_deterministic_v1",
    template_id: templateId,
    seed,
    template_hash,
    source_hash,
    row_hash: "",
    split: "train",
  };
  row.row_hash = sha(JSON.stringify({ id, source_hash, deterministic_diff_hash, template_hash }));
  return row;
}

// 12 of every 25 rows (by global seq) carry bounded_context -> exactly 120 of 250.
const hasContextFor = (seq) => (seq - 1) % 25 < 12;

function buildRows() {
  const rows = [];
  let seq = 0;
  for (const tmpl of PRISTINE_TEMPLATES) {
    for (let i = 0; i < PRISTINE_INSTANCES; i += 1) {
      seq += 1;
      const text = tmpl.text(seq, i);
      rows.push(makeRow({
        seq, subBucket: "pristine", templateId: tmpl.id, domain: tmpl.domain,
        inputText: text, outputText: text, spans: tmpl.spans(seq), hasContext: hasContextFor(seq),
      }));
    }
  }
  for (const tmpl of TRIVIAL_TEMPLATES) {
    for (let i = 0; i < TRIVIAL_INSTANCES; i += 1) {
      seq += 1;
      const output = tmpl.text(seq, i);
      const input = toInputVariant(output, i % 3);
      rows.push(makeRow({
        seq, subBucket: "trivial_fix", templateId: tmpl.id, domain: tmpl.domain,
        inputText: input, outputText: output, spans: tmpl.spans(seq), hasContext: hasContextFor(seq),
      }));
    }
  }
  return rows;
}

function validate(rows) {
  const errors = [];
  const ids = new Set();
  const inputs = new Set();
  const subCounts = { pristine: 0, trivial_fix: 0 };
  const templateCounts = {};
  const hashPattern = /^sha256:[0-9a-f]{64}$/;
  let spanCarryingRows = 0;
  let contextRows = 0;

  for (const r of rows) {
    if (r.schema !== SCHEMA || r.bucket !== BUCKET || r.split !== "train") errors.push(`${r.id}: contract fields`);
    if (!["pristine", "trivial_fix"].includes(r.sub_bucket)) errors.push(`${r.id}: sub_bucket`);
    if (r.source !== "builder_deterministic_v1" || r.construction !== "target_first_deterministic_v1") errors.push(`${r.id}: provenance fields`);
    if (ids.has(r.id)) errors.push(`${r.id}: duplicate id`);
    ids.add(r.id);
    if (inputs.has(r.input_text)) errors.push(`${r.id}: duplicate input_text`);
    inputs.add(r.input_text);
    subCounts[r.sub_bucket] = (subCounts[r.sub_bucket] ?? 0) + 1;
    templateCounts[r.template_id] = (templateCounts[r.template_id] ?? 0) + 1;
    if (r.must_preserve.length > 0) spanCarryingRows += 1;
    if (Object.keys(r.bounded_context).length > 0) contextRows += 1;

    const expectedTemplateHash = sha(JSON.stringify({ bucket: BUCKET, sub_bucket: r.sub_bucket, template_id: r.template_id }));
    const expectedSourceHash = sha(JSON.stringify({ input_text: r.input_text, bounded_context: r.bounded_context, seed: r.seed }));
    const expectedDiffHash = sha(JSON.stringify(r.deterministic_diff));
    const expectedRowHash = sha(JSON.stringify({ id: r.id, source_hash: expectedSourceHash, deterministic_diff_hash: expectedDiffHash, template_hash: expectedTemplateHash }));
    if (r.template_hash !== expectedTemplateHash) errors.push(`${r.id}: template_hash`);
    if (r.source_hash !== expectedSourceHash) errors.push(`${r.id}: source_hash`);
    if (r.row_hash !== expectedRowHash) errors.push(`${r.id}: row_hash`);
    for (const h of [r.template_hash, r.source_hash, r.row_hash]) if (!hashPattern.test(h)) errors.push(`${r.id}: malformed hash`);

    for (const spanText of r.must_preserve) {
      if (!r.input_text.includes(spanText)) errors.push(`${r.id}: span missing from input ${spanText}`);
      if (!r.output_text.includes(spanText)) errors.push(`${r.id}: span missing from output ${spanText}`);
    }

    if (digitSignature(r.input_text) !== digitSignature(r.output_text)) errors.push(`${r.id}: digit signature drift`);

    if (r.sub_bucket === "pristine") {
      if (r.input_text !== r.output_text) errors.push(`${r.id}: pristine not exact`);
      if (r.deterministic_diff.length !== 0) errors.push(`${r.id}: pristine diff not no-op`);
    } else {
      try {
        const replayed = replayDeterministicDiff(r.input_text, r.deterministic_diff);
        if (replayed !== r.output_text) errors.push(`${r.id}: replay mismatch`);
      } catch (error) {
        errors.push(`${r.id}: ${error.message}`);
      }
      const inWords = alnumWords(r.input_text);
      const outWords = alnumWords(r.output_text);
      if (JSON.stringify(inWords) !== JSON.stringify(outWords)) errors.push(`${r.id}: word multiset drift`);
      if (r.input_text === r.output_text) errors.push(`${r.id}: trivial_fix has no diff`);
      if (r.deterministic_diff.length === 0) errors.push(`${r.id}: trivial_fix missing diff ops`);
      for (const op of r.deterministic_diff) {
        if (!["leading_capitalization", "terminal_punctuation"].includes(op.authority_tags[0])) errors.push(`${r.id}: unexpected diff authority ${op.authority_tags[0]}`);
      }
    }
  }

  if (rows.length !== TOTAL) errors.push(`total row count ${rows.length}`);
  if (subCounts.pristine !== PRISTINE_COUNT) errors.push(`pristine count ${subCounts.pristine}`);
  if (subCounts.trivial_fix !== TRIVIAL_COUNT) errors.push(`trivial_fix count ${subCounts.trivial_fix}`);

  const pristineCap = Math.ceil(PRISTINE_COUNT * 0.2);
  const trivialCap = Math.ceil(TRIVIAL_COUNT * 0.2);
  for (const [templateId, count] of Object.entries(templateCounts)) {
    const cap = templateId.startsWith("pristine-") ? pristineCap : trivialCap;
    if (count > cap) errors.push(`template cap exceeded ${templateId}=${count}`);
  }

  if (spanCarryingRows < 140) errors.push(`must_preserve coverage ${spanCarryingRows} < 140`);
  if (ids.size !== TOTAL) errors.push(`id uniqueness ${ids.size}`);
  if (inputs.size !== TOTAL) errors.push(`input_text uniqueness ${inputs.size}`);

  if (errors.length) throw new Error(errors.slice(0, 40).join("\n"));

  return {
    row_count: rows.length,
    sub_bucket_counts: subCounts,
    template_counts: templateCounts,
    rows_with_must_preserve: spanCarryingRows,
    rows_with_bounded_context: contextRows,
  };
}

function main() {
  const rows = buildRows();
  const metrics = validate(rows);
  mkdirSync(outputDir, { recursive: true });
  const corpusText = `${rows.map((r) => JSON.stringify(r)).join("\n")}\n`;
  writeFileSync(corpusPath, corpusText, "utf8");
  const corpusHash = sha(corpusText);
  const summary = {
    schema: "heardright.l1_appaware_already_clean_validation.v1",
    status: "pass",
    corpus_file: "already_clean.jsonl",
    corpus_sha256: corpusHash,
    generator_file: "tauri-app-next/scripts/local-model-training/generate_already_clean_corpus.mjs",
    generator_sha256: scriptHash,
    ...metrics,
    checks: [
      "pristine input_text === output_text exact",
      "pristine deterministic_diff is a no-op",
      "trivial_fix replay(input_text, deterministic_diff) === output_text",
      "trivial_fix alnum word multiset identical between input and output",
      "trivial_fix diff ops limited to casing/terminal-punctuation authority tags",
      "every must_preserve span verbatim in input_text and output_text",
      "digit signature identical between input_text and output_text for all rows",
      "sub-bucket counts exactly 150/100, total 250",
      "template_id share capped at 20% of its sub-bucket",
      "must_preserve coverage >= 140 rows",
      "unique ids and input_text, well-formed sha256 lineage hashes",
    ],
  };
  writeFileSync(summaryPath, `${JSON.stringify(summary, null, 2)}\n`, "utf8");
  console.log(JSON.stringify(summary, null, 2));
}

const isMain = process.argv[1] && pathToFileURL(process.argv[1]).href === import.meta.url;
if (isMain) main();

export {
  SCHEMA, BUCKET, PRISTINE_COUNT, TRIVIAL_COUNT, TOTAL,
  outputDir, corpusPath, summaryPath,
  sha, buildRows, validate, buildDeterministicDiff, replayDeterministicDiff,
  alnumWords, digitSignature,
};
