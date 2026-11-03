import { createHash } from "node:crypto";
import { readFileSync, writeFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const OUTPUT_DEFAULT = new URL("./short.synthetic.jsonl", import.meta.url);
const SUMMARY_DEFAULT = new URL("./short.synthetic.validation.json", import.meta.url);
const CREATED_AT = "2026-07-21T00:00:00Z";
const EXPECTED = {
  short_noop: 180,
  short_single_repair: 150,
  short_punctuation: 120,
  short_filler_or_restart: 90,
  short_dictated_format: 60,
};
const NAMES = [
  "Amina", "Bevan", "Celia", "Darin", "Elora", "Faris", "Gina", "Hector", "Inez", "Jules",
  "Kira", "Loren", "Marta", "Nolan", "Orla", "Pavel", "Quinn", "Rina", "Soren", "Talia",
];

function sha256(value) {
  return `sha256:${createHash("sha256").update(value).digest("hex")}`;
}

const GENERATOR_PATH = fileURLToPath(import.meta.url);
const GENERATOR_HASH = sha256(readFileSync(GENERATOR_PATH));
const STRUCTURAL_CAP = { max_rows: 24, max_corpus_share: 0.04, max_sub_bucket_share: 0.20 };
const REPO_ROOT = resolve(dirname(GENERATOR_PATH), "../../../..");
const DEFAULT_DENY_PATH = resolve(dirname(GENERATOR_PATH), "short.benchmark-deny-manifest.json");
const DENY_PATH = option("--deny-manifest") ?? DEFAULT_DENY_PATH;
const DENY_TEXT = readFileSync(DENY_PATH, "utf8");
const DENY = JSON.parse(DENY_TEXT);
const DENY_HASH = sha256(DENY_TEXT);

function verifyDeniedSources() {
  if (DENY.schema !== "heardright.l1_short_benchmark_deny_manifest.v1" || DENY.status !== "fail_closed" || !Array.isArray(DENY.source_files)) throw new Error("denied manifest schema/status");
  for (const source of DENY.source_files) {
    let content;
    try { content = readFileSync(resolve(REPO_ROOT, source.path)); } catch (error) { throw new Error(`denied source missing: ${source.path}: ${error.message}`); }
    const actual = sha256(content).slice(7);
    if (actual !== source.sha256) throw new Error(`denied source changed: ${source.path}`);
  }
}

verifyDeniedSources();

function normalized(value) {
  return value.toLowerCase().replace(/[^a-z0-9]+/g, " ").trim().replace(/\s+/g, " ");
}

function tokenJaccard(left, right) {
  const a = new Set(normalized(left).split(" ").filter(Boolean));
  const b = new Set(normalized(right).split(" ").filter(Boolean));
  const intersection = [...a].filter((token) => b.has(token)).length;
  return intersection / Math.max(1, new Set([...a, ...b]).size);
}

function editSimilarity(left, right) {
  const a = normalized(left);
  const b = normalized(right);
  let prior = Array.from({ length: b.length + 1 }, (_value, index) => index);
  for (let row = 1; row <= a.length; row += 1) {
    const current = [row];
    for (let column = 1; column <= b.length; column += 1) current[column] = Math.min(current[column - 1] + 1, prior[column] + 1, prior[column - 1] + (a[row - 1] === b[column - 1] ? 0 : 1));
    prior = current;
  }
  return 1 - prior[b.length] / Math.max(a.length, b.length, 1);
}

function deniedMatch(value) {
  const text = normalized(value);
  for (const entity of DENY.denied_entities ?? []) if (new RegExp(`\\b${normalized(entity).replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}\\b`, "iu").test(text)) return `entity:${entity}`;
  for (const phrase of DENY.denied_phrases ?? []) {
    const phraseText = normalized(phrase);
    if (text.includes(phraseText)) return `phrase:${phrase}`;
    if (phraseText.split(" ").length >= DENY.near_match_policy.minimum_phrase_tokens && (tokenJaccard(text, phraseText) > DENY.near_match_policy.token_jaccard_max || editSimilarity(text, phraseText) > DENY.near_match_policy.normalized_edit_similarity_max)) return `near:${phrase}`;
  }
  return null;
}

function tokenCount(value) {
  return value.trim().split(/\s+/u).filter(Boolean).length;
}

function emptyContext() {
  return {
    app_name: "",
    window_title: "",
    field_text: "",
    selected_text: "",
    vocabulary: [],
    sound_alikes: [],
    writing_region: "",
  };
}

function protectedSpan(kind, inputSurface, targetSurface = inputSurface) {
  return { kind, input_surface: inputSurface, target_surface: targetSurface, required: true };
}

function deterministicDiff(input, output) {
  let prefix = 0;
  while (prefix < input.length && prefix < output.length && input[prefix] === output[prefix]) prefix += 1;
  let suffix = 0;
  while (suffix < input.length - prefix && suffix < output.length - prefix && input[input.length - 1 - suffix] === output[output.length - 1 - suffix]) suffix += 1;
  return {
    authority: "single_contiguous_diff_v1",
    input_hash: sha256(input),
    output_hash: sha256(output),
    prefix_length_utf16: prefix,
    suffix_length_utf16: suffix,
    removed_text: input.slice(prefix, input.length - suffix),
    inserted_text: output.slice(prefix, output.length - suffix),
  };
}

function replayDiff(input, diff) {
  const tailStart = input.length - diff.suffix_length_utf16;
  if (sha256(input) !== diff.input_hash || input.slice(diff.prefix_length_utf16, tailStart) !== diff.removed_text) throw new Error("deterministic diff input mismatch");
  const output = `${input.slice(0, diff.prefix_length_utf16)}${diff.inserted_text}${input.slice(tailStart)}`;
  if (sha256(output) !== diff.output_hash) throw new Error("deterministic diff output mismatch");
  return output;
}

function row({ sequence, subBucket, templateId, input, candidateOutput, context = emptyContext(), protectedSpans, failureTags, removableSpans = [], dictatedFormatting = null }) {
  const seed = 710000 + sequence;
  const inputHash = sha256(input);
  const templateHash = sha256(`short-v1|${subBucket}|${templateId}`);
  return {
    schema_version: "heardright.l1_training_candidate.v1",
    lifecycle: "candidate",
    row_id: `short-v1-synth-${String(sequence).padStart(6, "0")}`,
    bucket: "short_v1",
    sub_bucket: subBucket,
    cluster_id: sha256(`short-v1|${subBucket}|${inputHash}`),
    split: "adjudication_candidate",
    source_kind: "synthetic_text",
    consent: "synthetic",
    license: "internal_synthetic",
    source_ref: `generator:short-v1/${subBucket}/${templateId}/seed-${seed}`,
    source_session_group: `synthetic:${subBucket}:seed-${seed}`,
    generator_seed: seed,
    input_text: input,
    bounded_context: context,
    cloud_l1_output: null,
    cloud_l1_status: "not_requested_synthetic_candidate",
    candidate_output: candidateOutput,
    gold_output: null,
    target_basis: "pending_human_adjudication",
    adjudication_status: "pending",
    training_eligible: false,
    adjudicator: null,
    adjudication_reason: null,
    removable_spans: removableSpans,
    failure_tags: failureTags,
    dictated_formatting: dictatedFormatting,
    source_hash: inputHash,
    generator_hash: GENERATOR_HASH,
    template_id: templateId,
    template_hash: templateHash,
    template_lineage: { id: templateId, hash: templateHash, authority: "persisted_canonical_template_hash" },
    denied_manifest_path: "tauri-app-next/bakeoff/l1-four-family-training-v1/corpus-candidates/short.benchmark-deny-manifest.json",
    denied_manifest_hash: DENY_HASH,
    structured_spans: {
      protected: protectedSpans,
      permitted_changes: [],
      deletion_permissions: removableSpans.map((text) => ({ text, reason: "candidate-authored removable span" })),
      context_resolution: [],
    },
    deterministic_diff: deterministicDiff(input, candidateOutput),
    teacher_prompt_hash: null,
    serializer_version: "l1_local_student_v1",
    created_at: CREATED_AT,
  };
}

function buildRows() {
  const rows = [];
  let sequence = 1;
  const add = (value) => rows.push(row({ sequence: sequence++, ...value }));

  const noopTemplates = [
    (name, code) => `${name} marked case ${code} as ready.`,
    (name, code) => `${name} confirmed case ${code} is complete.`,
    (name, code) => `Case ${code} remains assigned to ${name}.`,
    (name, code) => `${name} left case ${code} unchanged.`,
    (name, code) => `Keep case ${code} with ${name}.`,
    (name, code) => `${name} reviewed case ${code} this morning.`,
    (name, code) => `Case ${code} is ready for ${name}.`,
    (name, code) => `${name} saved the note for case ${code}.`,
    (name, code) => `Please retain ${name} on case ${code}.`,
    (name, code) => `${name} owns the next step for case ${code}.`,
    (name, code) => `The status for case ${code} is clear to ${name}.`,
    (name, code) => `${name} verified the label on case ${code}.`,
  ];
  for (let index = 0; index < EXPECTED.short_noop; index += 1) {
    const name = NAMES[index % NAMES.length];
    const code = `S-${String(index + 1).padStart(3, "0")}`;
    const templateIndex = index % noopTemplates.length;
    const input = noopTemplates[templateIndex](name, code);
    add({
      subBucket: "short_noop",
      templateId: `noop-${String(templateIndex + 1).padStart(2, "0")}`,
      input,
      candidateOutput: input,
      protectedSpans: [protectedSpan("name", name), protectedSpan("identifier", code)],
      failureTags: ["no_op", "protected_identifier"],
    });
  }

  const repairs = [
    ["calender", "calendar", "Please check the calender entry for record"],
    ["reciept", "receipt", "Please attach the reciept to record"],
    ["adress", "address", "Please update the adress for record"],
    ["notifcation", "notification", "Please send the notifcation for record"],
    ["invoie", "invoice", "Please file the invoie under record"],
    ["reviw", "review", "Please complete the reviw for record"],
    ["schedual", "schedule", "Please confirm the schedual for record"],
    ["attatch", "attach", "Please attatch the note to record"],
    ["seperate", "separate", "Please use the seperate folder for record"],
    ["enviroment", "environment", "Please verify the enviroment label for record"],
    ["commitee", "committee", "Please update the commitee list for record"],
    ["referance", "reference", "Please check the referance code for record"],
    ["occurance", "occurrence", "Please log the occurance for record"],
    ["recieve", "receive", "Please recieve approval for record"],
    ["availble", "available", "Please confirm the availble amount for record"],
  ];
  for (let index = 0; index < EXPECTED.short_single_repair; index += 1) {
    const [wrong, right, stem] = repairs[index % repairs.length];
    const code = `R-${String(index + 1).padStart(3, "0")}`;
    const input = `${stem} ${code}.`;
    const proposed = input.replace(wrong, right);
    add({
      subBucket: "short_single_repair",
      templateId: `repair-${String((index % repairs.length) + 1).padStart(2, "0")}`,
      input,
      candidateOutput: proposed,
      context: {
        app_name: "Records",
        window_title: "Entry review",
        field_text: "",
        selected_text: "",
        vocabulary: [right],
        sound_alikes: [[wrong, right]],
        writing_region: "",
      },
      protectedSpans: [protectedSpan("identifier", code)],
      failureTags: ["single_local_repair", "controlled_spelling", "context_disambiguation"],
    });
  }

  const punctuationTemplates = [
    (name, code) => [`${name} moved item ${code} then filed the note`, `${name} moved item ${code}, then filed the note.`],
    (name, code) => [`can ${name} review item ${code}`, `Can ${name} review item ${code}?`],
    (_name, code) => [`please save item ${code} before noon`, `Please save item ${code} before noon.`],
    (name, code) => [`${name} said item ${code} is ready`, `${name} said item ${code} is ready.`],
    (name, code) => [`after lunch ${name} will close item ${code}`, `After lunch, ${name} will close item ${code}.`],
    (_name, code) => [`should item ${code} stay open`, `Should item ${code} stay open?`],
    (name, code) => [`${name} checked item ${code} yes it is complete`, `${name} checked item ${code}. Yes, it is complete.`],
    (_name, code) => [`for item ${code} use the current note`, `For item ${code}, use the current note.`],
  ];
  for (let index = 0; index < EXPECTED.short_punctuation; index += 1) {
    const name = NAMES[index % NAMES.length];
    const code = `P-${String(index + 1).padStart(3, "0")}`;
    const templateIndex = index % punctuationTemplates.length;
    const [input, proposed] = punctuationTemplates[templateIndex](name, code);
    add({
      subBucket: "short_punctuation",
      templateId: `punctuation-${String(templateIndex + 1).padStart(2, "0")}`,
      input,
      candidateOutput: proposed,
      protectedSpans: input.includes(name)
        ? [protectedSpan("name", name), protectedSpan("identifier", code)]
        : [protectedSpan("identifier", code)],
      failureTags: ["punctuation_only", "casing_only", "protected_identifier"],
    });
  }

  const fillerTemplates = [
    (code) => [`I think uh item ${code} should wait.`, `I think item ${code} should wait.`, "uh", []],
    (code) => [`Please um keep item ${code} open.`, `Please keep item ${code} open.`, "um", []],
    (code) => [`We can er move item ${code} tomorrow.`, `We can move item ${code} tomorrow.`, "er", []],
    (code) => [`The team uh approved item ${code}.`, `The team approved item ${code}.`, "uh", []],
    (code) => [`I mean um item ${code} is ready.`, `I mean item ${code} is ready.`, "um", []],
    (code) => [`Move item ${code} to April no May.`, `Move item ${code} to May.`, "to April no", ["May"]],
    (code) => [`Send item ${code} on Monday no Tuesday.`, `Send item ${code} on Tuesday.`, "on Monday no", ["Tuesday"]],
    (code) => [`Mark item ${code} as draft no final.`, `Mark item ${code} as final.`, "as draft no", ["final"]],
    (code) => [`Assign item ${code} to Amina no Bevan.`, `Assign item ${code} to Bevan.`, "to Amina no", ["Bevan"]],
    (code) => [`Use option two no option three for item ${code}.`, `Use option three for item ${code}.`, "option two no", ["option three"]],
  ];
  for (let index = 0; index < EXPECTED.short_filler_or_restart; index += 1) {
    const code = `F-${String(index + 1).padStart(3, "0")}`;
    const templateIndex = index % fillerTemplates.length;
    const [input, proposed, removable, extraProtected] = fillerTemplates[templateIndex](code);
    const isRestart = templateIndex >= 5;
    add({
      subBucket: "short_filler_or_restart",
      templateId: `${isRestart ? "restart" : "filler"}-${String((templateIndex % 5) + 1).padStart(2, "0")}`,
      input,
      candidateOutput: proposed,
      protectedSpans: [protectedSpan("identifier", code), ...extraProtected.map((value) => protectedSpan("literal", value))],
      removableSpans: [removable],
      failureTags: isRestart ? ["explicit_self_correction", "restart", "minimal_deletion"] : ["nonsemantic_filler", "minimal_deletion"],
    });
  }

  const formatTemplates = [
    (code) => ["topic colon item", `topic: item ${code} is ready`, "colon", "colon", ":"],
    (code) => ["subject colon item", `subject: item ${code} is ready`, "colon", "colon", ":"],
    (code) => ["first line new line item", `first line\nitem ${code} is ready`, "new_line", "new line", "\n"],
    (code) => ["status line new line item", `status line\nitem ${code} is ready`, "new_line", "new line", "\n"],
    (code) => ["opening note new paragraph item", `opening note\n\nitem ${code} is ready`, "new_paragraph", "new paragraph", "\n\n"],
    (code) => ["summary note new paragraph item", `summary note\n\nitem ${code} is ready`, "new_paragraph", "new paragraph", "\n\n"],
    (code) => ["bullet item", `- item ${code} is ready`, "bullet", "bullet", "- "],
    (code) => ["next bullet item", `next - item ${code} is ready`, "bullet", "bullet", "- "],
    (code) => ["numbered item", `1. item ${code} is ready`, "numbered_item", "numbered", "1. "],
    (code) => ["next numbered item", `next 1. item ${code} is ready`, "numbered_item", "numbered", "1. "],
  ];
  for (let index = 0; index < EXPECTED.short_dictated_format; index += 1) {
    const code = `D-${String(index + 1).padStart(3, "0")}`;
    const templateIndex = index % formatTemplates.length;
    const [sourcePrefix, proposed, directive, sourceSpan, renderedForm] = formatTemplates[templateIndex](code);
    add({
      subBucket: "short_dictated_format",
      templateId: `format-${String(templateIndex + 1).padStart(2, "0")}`,
      input: `${sourcePrefix} ${code} is ready`,
      candidateOutput: proposed,
      protectedSpans: [protectedSpan("identifier", code)],
      failureTags: ["dictated_formatting", "no_action_execution"],
      dictatedFormatting: { directive, source_span: sourceSpan, rendered_form: renderedForm },
    });
  }
  return rows;
}

function structuralSignature(candidate) {
  let masked = candidate.input_text;
  for (const span of [...candidate.structured_spans.protected].sort((a, b) => b.input_surface.length - a.input_surface.length)) {
    masked = masked.replaceAll(span.input_surface, `<${span.kind}>`);
  }
  masked = masked.toLowerCase().replace(/\b[a-z]-\d+\b/giu, "<identifier>").replace(/\b\d+(?:[.:]\d+)?\b/gu, "<number>").replace(/\s+/gu, " ").trim();
  return `${candidate.sub_bucket}|${masked}`;
}

function structuralMetrics(rows) {
  const all = new Map();
  const bySubBucketMaps = new Map();
  for (const candidate of rows) {
    const signature = structuralSignature(candidate);
    all.set(signature, (all.get(signature) ?? 0) + 1);
    if (!bySubBucketMaps.has(candidate.sub_bucket)) bySubBucketMaps.set(candidate.sub_bucket, new Map());
    const bucket = bySubBucketMaps.get(candidate.sub_bucket);
    bucket.set(signature, (bucket.get(signature) ?? 0) + 1);
  }
  const maxRows = Math.max(0, ...all.values());
  const bySubBucket = {};
  for (const [subBucket, signatures] of bySubBucketMaps.entries()) {
    const bucketRows = [...signatures.values()].reduce((sum, count) => sum + count, 0);
    const bucketMax = Math.max(...signatures.values());
    bySubBucket[subBucket] = { rows: bucketRows, unique: signatures.size, max_rows: bucketMax, max_share: bucketMax / bucketRows };
  }
  return { cap: STRUCTURAL_CAP, unique: all.size, max_rows: maxRows, max_corpus_share: rows.length === 0 ? 0 : maxRows / rows.length, by_sub_bucket: bySubBucket };
}

function assert(condition, message) {
  if (!condition) throw new Error(message);
}

function validate(rows, { full }) {
  const ids = new Set();
  const clusters = new Set();
  const normalizedPairs = new Set();
  const counts = Object.fromEntries(Object.keys(EXPECTED).map((key) => [key, 0]));
  for (const candidate of rows) {
    assert(candidate.schema_version === "heardright.l1_training_candidate.v1", `${candidate.row_id}: schema`);
    assert(candidate.lifecycle === "candidate", `${candidate.row_id}: lifecycle`);
    assert(candidate.bucket === "short_v1", `${candidate.row_id}: bucket`);
    assert(Object.hasOwn(EXPECTED, candidate.sub_bucket), `${candidate.row_id}: sub_bucket`);
    assert(candidate.split === "adjudication_candidate", `${candidate.row_id}: split`);
    assert(candidate.source_kind === "synthetic_text" && candidate.consent === "synthetic", `${candidate.row_id}: provenance`);
    assert(candidate.gold_output === null && candidate.adjudicator === null && candidate.adjudication_status === "pending" && candidate.training_eligible === false, `${candidate.row_id}: human fields`);
    assert(candidate.cloud_l1_output === null && candidate.cloud_l1_status === "not_requested_synthetic_candidate", `${candidate.row_id}: cloud fields`);
    assert(typeof candidate.candidate_output === "string" && candidate.candidate_output.length > 0 && !Object.hasOwn(candidate, "proposed_output"), `${candidate.row_id}: candidate_output`);
    assert(/^sha256:[a-f0-9]{64}$/u.test(candidate.generator_hash) && /^sha256:[a-f0-9]{64}$/u.test(candidate.template_hash), `${candidate.row_id}: lineage hashes`);
    assert(candidate.template_lineage?.id === candidate.template_id && candidate.template_lineage?.hash === candidate.template_hash, `${candidate.row_id}: template lineage mismatch`);
    assert(candidate.denied_manifest_hash === DENY_HASH, `${candidate.row_id}: denied manifest hash`);
    assert(tokenCount(candidate.input_text) >= 1 && tokenCount(candidate.input_text) <= 40 && candidate.input_text.length <= 320, `${candidate.row_id}: short bound`);
    assert(!/[\u2013\u2014]/u.test(candidate.candidate_output), `${candidate.row_id}: dash`);
    assert(!/[{[]\s*["']?(old|new|replacement)["']?\s*[:}]/iu.test(candidate.candidate_output), `${candidate.row_id}: edit wrapper`);
    assert(deniedMatch(`${candidate.input_text}\n${candidate.candidate_output}\n${JSON.stringify(candidate.bounded_context)}`) === null, `${candidate.row_id}: benchmark text`);
    assert(!ids.has(candidate.row_id), `${candidate.row_id}: duplicate id`);
    assert(!clusters.has(candidate.cluster_id), `${candidate.row_id}: duplicate cluster`);
    const pair = `${normalized(candidate.input_text)}\n${normalized(candidate.candidate_output)}`;
    assert(!normalizedPairs.has(pair), `${candidate.row_id}: duplicate input/output`);
    ids.add(candidate.row_id);
    clusters.add(candidate.cluster_id);
    normalizedPairs.add(pair);
    for (const span of candidate.structured_spans.protected) {
      assert(typeof span.kind === "string" && typeof span.input_surface === "string" && typeof span.target_surface === "string" && span.required === true, `${candidate.row_id}: structured protected span`);
      assert(candidate.input_text.includes(span.input_surface), `${candidate.row_id}: protected source span ${span.input_surface}`);
      assert(candidate.candidate_output.includes(span.target_surface), `${candidate.row_id}: protected output span ${span.target_surface}`);
    }
    assert(replayDiff(candidate.input_text, candidate.deterministic_diff) === candidate.candidate_output, `${candidate.row_id}: deterministic diff replay`);
    if (candidate.sub_bucket === "short_noop") assert(candidate.input_text === candidate.candidate_output, `${candidate.row_id}: no-op delta`);
    if (candidate.sub_bucket === "short_punctuation") assert(normalized(candidate.input_text) === normalized(candidate.candidate_output), `${candidate.row_id}: punctuation lexical delta`);
    if (candidate.sub_bucket === "short_dictated_format") assert(candidate.dictated_formatting !== null, `${candidate.row_id}: format directive`);
    if (candidate.sub_bucket !== "short_dictated_format") assert(candidate.dictated_formatting === null, `${candidate.row_id}: unexpected format directive`);
    counts[candidate.sub_bucket] += 1;
  }
  if (full) {
    assert(rows.length === 600, `row count ${rows.length}`);
    for (const [subBucket, expected] of Object.entries(EXPECTED)) assert(counts[subBucket] === expected, `${subBucket}: ${counts[subBucket]} !== ${expected}`);
  }
  const structures = structuralMetrics(rows);
  if (full) {
    assert(structures.max_rows <= STRUCTURAL_CAP.max_rows, `structural signature max rows ${structures.max_rows}`);
    assert(structures.max_corpus_share <= STRUCTURAL_CAP.max_corpus_share, `structural signature corpus share ${structures.max_corpus_share}`);
    for (const [subBucket, metric] of Object.entries(structures.by_sub_bucket)) assert(metric.max_share <= STRUCTURAL_CAP.max_sub_bucket_share, `${subBucket}: structural signature share ${metric.max_share}`);
  }
  return { row_count: rows.length, sub_bucket_counts: counts, unique_ids: ids.size, unique_clusters: clusters.size, unique_normalized_pairs: normalizedPairs.size, structural_signatures: structures };
}

function option(name) {
  const index = process.argv.indexOf(name);
  return index === -1 ? null : process.argv[index + 1];
}

const limitRaw = option("--limit");
const limit = limitRaw === null ? null : Number.parseInt(limitRaw, 10);
assert(limit === null || (Number.isInteger(limit) && limit >= 1 && limit <= 600), "--limit must be 1..600");
const output = option("--out") ?? fileURLToPath(OUTPUT_DEFAULT);
const summary = option("--summary") ?? fileURLToPath(SUMMARY_DEFAULT);
const allRows = buildRows();
const rows = limit === null ? allRows : allRows.slice(0, limit);
const validation = validate(rows, { full: limit === null });
const corpusContent = `${rows.map((entry) => JSON.stringify(entry)).join("\n")}\n`;
const report = {
  schema: "heardright.l1_synthetic_candidate_validation.v1",
  corpus: "short.synthetic.jsonl",
  generated_from: "generate_short_synthetic_corpus.mjs",
  generator_sha256: GENERATOR_HASH,
  corpus_sha256: sha256(corpusContent),
  generation_mode: limit === null ? "full" : "smoke",
  expected_full_counts: EXPECTED,
  denied_manifest_file: "short.benchmark-deny-manifest.json",
  denied_manifest_hash: DENY_HASH,
  denied_source_count: DENY.source_files.length,
  validation,
  structural_signatures: validation.structural_signatures,
  checks: ["schema", "candidate_lifecycle", "counts", "unique_ids", "unique_clusters", "duplicate_input_output", "structured_spans.protected", "deterministic_diff_replay", "denied_source_hashes", "denied_entity_phrase_near_match", "pending_human_fields", "lineage_hashes", "short_bounds", "dash_ban", "structural_signature_cap"],
};
writeFileSync(output, corpusContent, "utf8");
writeFileSync(summary, `${JSON.stringify(report, null, 2)}\n`, "utf8");
console.log(`OK mode=${report.generation_mode} rows=${validation.row_count} counts=${JSON.stringify(validation.sub_bucket_counts)} output=${output}`);
