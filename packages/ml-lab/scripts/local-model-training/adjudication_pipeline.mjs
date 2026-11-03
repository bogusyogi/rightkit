#!/usr/bin/env node
/** Local-only L1 adjudication contract. This module never invokes a provider. */
import { createHash } from "node:crypto";
import { existsSync, mkdirSync, readFileSync, readdirSync, renameSync, writeFileSync } from "node:fs";
import path from "node:path";
import { pathToFileURL } from "node:url";
import { replayPermittedEdits } from "./replay_permitted_edits.mjs";

const HASH = /^sha256:[a-f0-9]{64}$/u;
const BARE_HASH = /^[a-f0-9]{64}$/u;
const POLICY_VERSION = "heardright.l1.adjudication-policy.v1";
const RESOLVER_SCHEMA = "heardright.l1_adjudication_resolver.v1";
const CLOUD_SCHEMA = "heardright.l1_cloud_result.v1";
const JUDGE_SCHEMA = "heardright.l1_judge_receipt.v1";
const SEMANTIC_SCHEMA = "heardright.l1_human_audio_evidence.v1";

export const ADJUDICATION_SCHEMAS = {
  schema_version: "heardright.l1_adjudication_receipt_schemas.v1",
  required_binding_fields: ["input_binding_hash", "model", "model_artifact_hash", "prompt_hash", "params_hash", "request_hash", "output_hash", "critical_flags"],
  cloud_result: { schema_version: CLOUD_SCHEMA, required: ["row_id", "input_binding_hash", "model", "model_artifact_hash", "prompt_hash", "params_hash", "request_hash", "output", "output_hash", "critical_flags", "status"] },
  judge_receipt: { schema_version: JUDGE_SCHEMA, required: ["receipt_id", "row_id", "input_binding_hash", "provider_id", "judge_model", "judge_model_artifact_hash", "prompt_hash", "params_hash", "config_hash", "request_hash", "output_hash", "verdict", "critical_flags"] },
  human_audio: { schema_version: SEMANTIC_SCHEMA, required: ["evidence_id", "row_id", "input_binding_hash", "evidence_type", "evidence_source", "evidence_hash", "reviewer_id", "audio_ref_hash", "transcript_ref_hash", "decision", "rationale", "reviewed_at"] },
};

export const sha256 = (value) => `sha256:${createHash("sha256").update(value).digest("hex")}`;
const canonical = (value) => JSON.stringify(value);
const hashJson = (value) => sha256(canonical(value));
const text = (value) => typeof value === "string" ? value : "";
const hasTag = (row, tag) => Array.isArray(row.failure_tags) && row.failure_tags.includes(tag);
const handoff = (row) => /\bhandoff\s+\d+\b/iu.test(`${text(row.input_text)}\n${text(row.candidate_output)}`);
const normalizedHash = (value) => HASH.test(value ?? "") ? value : BARE_HASH.test(value ?? "") ? `sha256:${value}` : value;

function deterministicReasons(row) {
  const reasons = [];
  if (row.primary_bucket === "numbers_dates_money" && row.failure_tags?.[0] !== "exact_noop") reasons.push("iso_date_non_noop");
  if ((row.source_template_id === "synthetic-authored.no_op_or_intentional_repetition.v3" && row.template_lineage?.descriptor?.core_variant === 1) || hasTag(row, "loop_adversarial")) reasons.push("repetition_policy");
  if (handoff(row)) reasons.push("synthetic_handoff_marker");
  return reasons;
}

export function classifyDisposition(row) {
  const deterministic = deterministicReasons(row);
  if (deterministic.length) return { kind: "deterministic_reject", rule: deterministic.join("+"), reasons: deterministic };
  if ((row.primary_bucket === "semantic_asr_repair" && row.semantic_action === "repair") || hasTag(row, "contextual_semantic_repair")) return { kind: "cloud_human_audio_semantic", rule: "semantic_repair_requires_human_audio", reasons: [] };
  if (row.sub_bucket === "short_dictated_format" && ["new_line", "new_paragraph", "bullet", "numbered_item"].includes(row.dictated_formatting?.directive)) return { kind: "cloud_dual_terminal_format", rule: "layout_terminal_format_requires_dual_judge", reasons: [] };
  return { kind: "cloud_dual_standard", rule: "standard_candidate_requires_cloud_and_dual_judge", reasons: [] };
}

function rowHashes(row) {
  const protectedSpans = row.structured_spans?.protected ?? row.protected_spans ?? [];
  const permittedOps = row.structured_spans?.permitted_changes ?? row.permitted_edit_ops ?? [];
  return {
    source: row.source_hash,
    input: sha256(text(row.input_text)),
    candidate: sha256(text(row.candidate_output)),
    context: hashJson(row.bounded_context ?? {}),
    protected: hashJson(protectedSpans),
    permitted_ops: hashJson(permittedOps),
    expected_l0_output: sha256(text(row.candidate_output)),
    generator: normalizedHash(row.generator_hash ?? row.generator_sha256 ?? null),
    template: normalizedHash(row.template_hash ?? row.generator?.template_sha256 ?? row.template_lineage?.template_sha256 ?? null),
  };
}

export function createResolverForRow(row, corpusHash) {
  const hashes = rowHashes(row);
  for (const [field, value] of Object.entries(hashes)) if (!HASH.test(value ?? "")) throw new Error(`REJECTED ${row.row_id}: ${field} hash missing or invalid`);
  const disposition = classifyDisposition(row);
  if (row.deterministic_diff?.input_hash !== undefined && (row.deterministic_diff.input_hash !== hashes.source || hashes.source !== hashes.input)) throw new Error(`REJECTED ${row.row_id}: declared source hash does not match input_text`);
  const binding = hashJson({ corpus_hash: corpusHash, row_id: row.row_id, source_hash: hashes.source, input_hash: hashes.input, candidate_hash: hashes.candidate, context_hash: hashes.context, protected_hash: hashes.protected, permitted_ops_hash: hashes.permitted_ops, generator_hash: hashes.generator, template_hash: hashes.template, policy_version: POLICY_VERSION, policy_rule: disposition.rule, expected_l0_output_hash: hashes.expected_l0_output });
  return { schema_version: RESOLVER_SCHEMA, corpus_hash: corpusHash, row_id: row.row_id, input_binding_hash: binding, row_hashes: hashes, policy_version: POLICY_VERSION, disposition };
}

function queueFor(row, resolver) {
  return { schema_version: "heardright.l1_cloud_request.v1", request_id: `cloud-${row.row_id}`, row_id: row.row_id, input_binding_hash: resolver.input_binding_hash, prompt_version: "heardright.l1.cloud_request.v1", input_text: row.input_text, bounded_context: row.bounded_context ?? {} };
}

function countsFor(resolvers) {
  const all = (kind) => resolvers.filter((resolver) => resolver.disposition.kind === kind).length;
  const rejected = resolvers.filter((resolver) => resolver.disposition.kind === "deterministic_reject");
  const countRule = (rule) => rejected.filter((resolver) => resolver.disposition.reasons.includes(rule)).length;
  const overlap = rejected.filter((resolver) => resolver.disposition.reasons.length > 1).length;
  return { total: resolvers.length, deterministic_rejects: { total: rejected.length, iso_date: countRule("iso_date_non_noop"), repetition_policy: countRule("repetition_policy"), handoff_marker: countRule("synthetic_handoff_marker"), overlaps: overlap }, cloud_dual_terminal_format: all("cloud_dual_terminal_format"), cloud_human_audio_semantic: all("cloud_human_audio_semantic"), cloud_dual_standard: all("cloud_dual_standard"), unclassified: 0 };
}

function writeAtomic(file, content) { const temp = `${file}.tmp-${process.pid}`; writeFileSync(temp, content, "utf8"); renameSync(temp, file); }
function readJsonl(file) { return readFileSync(file, "utf8").split(/\r?\n/u).filter(Boolean).map((line, index) => { try { return JSON.parse(line); } catch (error) { throw new Error(`REJECTED ${file}:${index + 1}: ${error.message}`); } }); }

const PUBLIC_FILES = new Set(["cloud-request-queue.jsonl"]);
const PRIVATE_FILES = new Set(["freeze-manifest.json", "resolver-manifest.jsonl", "receipt-schemas.json", "result-ledgers.template.json", "status.json"]);
function isNested(parent, child) { const relative = path.relative(parent, child); return relative !== "" && !relative.startsWith("..") && !path.isAbsolute(relative); }
function validateOutputLayout(outputDir, privateOutputDir) {
  const publicPath = path.resolve(outputDir); const privatePath = path.resolve(privateOutputDir);
  if (publicPath.toLocaleLowerCase("en-US") === privatePath.toLocaleLowerCase("en-US") || isNested(publicPath, privatePath) || isNested(privatePath, publicPath)) throw new Error("REJECTED: public and private output directories must be distinct and non-nested");
  for (const [directory, allowed] of [[publicPath, PUBLIC_FILES], [privatePath, PRIVATE_FILES]]) {
    if (!existsSync(directory)) continue;
    const unexpected = readdirSync(directory).filter((name) => !allowed.has(name));
    if (unexpected.length) throw new Error(`REJECTED: unexpected or stale artifact(s) in ${directory}: ${unexpected.join(", ")}`);
  }
  return { publicPath, privatePath };
}

export function buildAdjudicationArtifacts({ corpusFiles, outputDir, privateOutputDir = `${outputDir}-private`, generatorFiles = [] }) {
  const layout = validateOutputLayout(outputDir, privateOutputDir);
  const corpus = corpusFiles.map((file) => ({ file: path.resolve(file), text: readFileSync(file, "utf8") }));
  const rows = corpus.flatMap(({ text: content }) => content.split(/\r?\n/u).filter(Boolean).map(JSON.parse));
  const ids = new Set(); for (const row of rows) { if (!row.row_id || ids.has(row.row_id)) throw new Error(`REJECTED ${row.row_id ?? "<missing>"}: duplicate or missing row_id`); ids.add(row.row_id); }
  const corpusHash = hashJson(corpus.map(({ file, text: content }) => ({ file: path.basename(file), sha256: sha256(content) })));
  const frozen = { schema_version: "heardright.l1_adjudication_freeze.v1", corpus_hash: corpusHash, corpus_files: corpus.map(({ file, text: content }) => ({ file: path.basename(file), sha256: sha256(content), bytes: Buffer.byteLength(content) })), generator_files: generatorFiles.map((file) => ({ file: path.resolve(file), sha256: sha256(readFileSync(file, "utf8")) })), policy_version: POLICY_VERSION, row_count: rows.length };
  const resolvers = rows.map((row) => createResolverForRow(row, corpusHash));
  const counts = countsFor(resolvers);
  const expected = { total: 1950, deterministic_rejects: { total: 173, iso_date: 65, repetition_policy: 69, handoff_marker: 43, overlaps: 4 }, cloud_dual_terminal_format: 48, cloud_human_audio_semantic: 124, cloud_dual_standard: 1605, unclassified: 0 };
  if (rows.length === 1950 && canonical(counts) !== canonical(expected)) throw new Error(`REJECTED corpus disposition contract: ${canonical(counts)}`);
  mkdirSync(layout.publicPath, { recursive: true });
  mkdirSync(layout.privatePath, { recursive: true });
  writeAtomic(path.join(layout.privatePath, "freeze-manifest.json"), `${JSON.stringify(frozen, null, 2)}\n`);
  writeAtomic(path.join(layout.privatePath, "resolver-manifest.jsonl"), `${resolvers.map(canonical).join("\n")}\n`);
  writeAtomic(path.join(layout.privatePath, "receipt-schemas.json"), `${JSON.stringify(ADJUDICATION_SCHEMAS, null, 2)}\n`);
  const queue = rows.map((row, index) => ({ row, resolver: resolvers[index] })).filter(({ resolver }) => resolver.disposition.kind !== "deterministic_reject").map(({ row, resolver }) => queueFor(row, resolver));
  writeAtomic(path.join(layout.publicPath, "cloud-request-queue.jsonl"), `${queue.map(canonical).join("\n")}\n`);
  writeAtomic(path.join(layout.privatePath, "result-ledgers.template.json"), `${JSON.stringify({ schema_version: "heardright.l1_adjudication_ledger_templates.v1", status: "template_only_no_results", required_files: ["cloud-results.jsonl", "judge-receipts.jsonl", "human-audio-evidence.jsonl"] }, null, 2)}\n`);
  writeAtomic(path.join(layout.privatePath, "status.json"), `${JSON.stringify({ schema_version: "heardright.l1_adjudication_status.v1", corpus_hash: corpusHash, counts, external_work_remaining: ["Cloud result ledger", "two independent judge receipt ledgers", "human/audio semantic evidence ledger"] }, null, 2)}\n`);
  return { rows, resolvers, freezeManifest: frozen, counts, corpusHash, outputDir: layout.publicPath, privateOutputDir: layout.privatePath };
}

function requireHash(value, label) { if (!HASH.test(value ?? "")) throw new Error(`REJECTED: ${label} must be a sha256 hash`); }
function oneByRow(items, rowId, label) { const matched = items.filter((item) => item.row_id === rowId); if (matched.length !== 1) throw new Error(`REJECTED ${rowId}: ${label} coverage must be exactly one`); return matched[0]; }
function validateCurrentRow(row, resolver) {
  const current = rowHashes(row);
  for (const key of ["source", "input", "candidate", "context", "protected", "permitted_ops"]) if (current[key] !== resolver.row_hashes?.[key]) throw new Error(`REJECTED ${resolver.row_id}: current row hash mismatch for ${key}`);
  if (row.deterministic_diff?.input_hash !== undefined && (row.deterministic_diff.input_hash !== current.source || current.source !== current.input)) throw new Error(`REJECTED ${resolver.row_id}: declared source hash does not match input_text`);
}
function validateFreezeManifest(freezeManifest, rowCount) {
  if (freezeManifest?.schema_version !== "heardright.l1_adjudication_freeze.v1" || freezeManifest.policy_version !== POLICY_VERSION || freezeManifest.row_count !== rowCount) throw new Error("REJECTED: frozen manifest schema, policy, or row coverage mismatch");
  requireHash(freezeManifest.corpus_hash, "frozen corpus hash");
  if (!Array.isArray(freezeManifest.corpus_files) || freezeManifest.corpus_files.length === 0) throw new Error("REJECTED: frozen corpus files missing");
  for (const item of freezeManifest.corpus_files) { if (!text(item.file) || !Number.isInteger(item.bytes) || item.bytes < 0) throw new Error("REJECTED: invalid frozen corpus file"); requireHash(item.sha256, "frozen corpus file hash"); }
  const recomputed = hashJson(freezeManifest.corpus_files.map(({ file, sha256: digest }) => ({ file, sha256: digest })));
  if (recomputed !== freezeManifest.corpus_hash) throw new Error("REJECTED: frozen corpus hash mismatch");
}
function validateResolverAuthenticity(row, resolver, freezeManifest) {
  if (resolver?.schema_version !== RESOLVER_SCHEMA || resolver.policy_version !== POLICY_VERSION || resolver.corpus_hash !== freezeManifest.corpus_hash || resolver.row_id !== row.row_id) throw new Error(`REJECTED ${row.row_id}: resolver schema, policy, corpus, or row mismatch`);
  const intrinsic = classifyDisposition(row);
  if (canonical(resolver.disposition) !== canonical(intrinsic)) throw new Error(`REJECTED ${row.row_id}: resolver disposition mismatch`);
  const authentic = createResolverForRow(row, freezeManifest.corpus_hash);
  if (resolver.input_binding_hash !== authentic.input_binding_hash || canonical(resolver.row_hashes) !== canonical(authentic.row_hashes)) throw new Error(`REJECTED ${row.row_id}: resolver binding authenticity failure`);
  requireHash(resolver.input_binding_hash, "resolver input binding hash");
}
function requiredProtectedTargets(row) { return (row.structured_spans?.protected ?? row.protected_spans ?? []).filter((span) => span.required !== false).map((span) => span.target_surface ?? span.text).filter((span) => text(span)); }
function contentTokens(row) {
  const permitted = row.structured_spans?.permitted_changes ?? row.permitted_edit_ops ?? [];
  const deletion = row.structured_spans?.deletion_permissions ?? row.removable_spans ?? [];
  const mutable = [...permitted.map((op) => op.input_span?.text ?? op.expected_text ?? op.from).filter((value) => text(value)), ...deletion.map((entry) => typeof entry === "string" ? entry : entry.text).filter((value) => text(value))];
  return text(row.input_text).match(/[\p{L}\p{N}]+/gu)?.filter((token) => !mutable.some((part) => new RegExp(`(?:^|[^\\p{L}\\p{N}])${token.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}(?:$|[^\\p{L}\\p{N}])`, "iu").test(part))) ?? [];
}
function safeOutput(value, row) {
  const rowId = row.row_id;
  if (typeof value !== "string" || !value.trim()) throw new Error(`REJECTED ${rowId}: empty cloud output`);
  if (/[\u2013\u2014]/u.test(value) || /(?:^|\n)\s*[{[]\s*["']?(?:old|new|replacement)["']?\s*:/iu.test(value)) throw new Error(`REJECTED ${rowId}: unsafe cloud output`);
  for (const target of requiredProtectedTargets(row)) if (!value.includes(target)) throw new Error(`REJECTED ${rowId}: protected target span missing: ${target}`);
  const outputTokens = new Set(value.toLocaleLowerCase("en-US").match(/[\p{L}\p{N}]+/gu) ?? []);
  for (const token of contentTokens(row)) if (!outputTokens.has(token.toLocaleLowerCase("en-US"))) throw new Error(`REJECTED ${rowId}: content preservation failure: ${token}`);
  return value;
}

function validateCloud(cloud, resolver) {
  if (cloud.schema_version !== CLOUD_SCHEMA || cloud.status !== "ok") throw new Error(`REJECTED ${resolver.row_id}: invalid Cloud result`);
  if (cloud.input_binding_hash !== resolver.input_binding_hash) throw new Error(`REJECTED ${resolver.row_id}: Cloud binding mismatch`);
  if (!text(cloud.model)) throw new Error(`REJECTED ${resolver.row_id}: Cloud model missing`);
  requireHash(cloud.model_artifact_hash, "Cloud model artifact hash");
  for (const key of ["prompt_hash", "params_hash", "request_hash", "output_hash"]) requireHash(cloud[key], `Cloud ${key}`);
  if (!Array.isArray(cloud.critical_flags) || cloud.critical_flags.length) throw new Error(`REJECTED ${resolver.row_id}: Cloud critical flags`);
  const output = safeOutput(cloud.output, resolver.row); if (sha256(output) !== cloud.output_hash) throw new Error(`REJECTED ${resolver.row_id}: Cloud output hash mismatch`); return output;
}

function validateJudges(receipts, resolver, outputHash) {
  if (receipts.length !== 2) throw new Error(`REJECTED ${resolver.row_id}: judge coverage must be exactly two`);
  for (const receipt of receipts) if (!text(receipt.receipt_id)) throw new Error(`REJECTED ${resolver.row_id}: judge receipt id missing`);
  if (new Set(receipts.map((receipt) => receipt.receipt_id)).size !== 2 || new Set(receipts.map((receipt) => receipt.judge_model)).size !== 2) throw new Error(`REJECTED ${resolver.row_id}: duplicate judge receipt`);
  for (const receipt of receipts) {
    if (receipt.schema_version !== JUDGE_SCHEMA || receipt.input_binding_hash !== resolver.input_binding_hash || receipt.output_hash !== outputHash) throw new Error(`REJECTED ${resolver.row_id}: judge binding mismatch`);
    if (!text(receipt.provider_id) || !text(receipt.judge_model)) throw new Error(`REJECTED ${resolver.row_id}: judge provider or model missing`);
    for (const key of ["judge_model_artifact_hash", "prompt_hash", "params_hash", "config_hash", "request_hash", "output_hash"]) requireHash(receipt[key], `judge ${key}`);
    if (receipt.verdict !== "accept") throw new Error(`REJECTED ${resolver.row_id}: judge consensus failure`);
    if (!Array.isArray(receipt.critical_flags) || receipt.critical_flags.length) throw new Error(`REJECTED ${resolver.row_id}: judge critical flags`);
  }
  if (new Set(receipts.map((receipt) => receipt.provider_id)).size !== 2 || new Set(receipts.map((receipt) => receipt.judge_model_artifact_hash)).size !== 2) throw new Error(`REJECTED ${resolver.row_id}: judges are not independent provider/model artifacts`);
  const lineages = receipts.map((receipt) => hashJson({ provider_id: receipt.provider_id, model_artifact_hash: receipt.judge_model_artifact_hash, prompt_hash: receipt.prompt_hash, config_hash: receipt.config_hash }));
  if (new Set(lineages).size !== 2) throw new Error(`REJECTED ${resolver.row_id}: aliased judge lineage is not independent`);
}

function validateSemantic(evidence, resolver, output) {
  const row = resolver.row;
  const permitted = row.structured_spans?.permitted_changes ?? row.permitted_edit_ops ?? [];
  if (/^synthetic(?:_|$)/u.test(row.source_kind ?? "") && permitted.length > 0 && replayPermittedEdits(row.input_text, permitted) === output) return;
  const item = oneByRow(evidence, resolver.row_id, "human/audio evidence");
  if (item.schema_version !== SEMANTIC_SCHEMA || item.input_binding_hash !== resolver.input_binding_hash || item.decision !== "accept" || !text(item.evidence_id) || !text(item.evidence_type) || !text(item.evidence_source) || !text(item.reviewer_id) || !text(item.rationale) || !text(item.reviewed_at)) throw new Error(`REJECTED ${resolver.row_id}: human/audio evidence incomplete`);
  requireHash(item.evidence_hash, "human/audio evidence_hash"); requireHash(item.audio_ref_hash, "human/audio audio_ref_hash"); requireHash(item.transcript_ref_hash, "human/audio transcript_ref_hash");
}

export function promoteAcceptedRows({ rows, resolvers, freezeManifest, cloudResults, judgeReceipts, semanticEvidence }) {
  const byId = new Map(rows.map((row) => [row.row_id, row]));
  if (byId.size !== rows.length || new Set(resolvers.map((resolver) => resolver.row_id)).size !== resolvers.length || resolvers.length !== rows.length) throw new Error("REJECTED: resolver row coverage mismatch");
  validateFreezeManifest(freezeManifest, rows.length);
  for (const resolver of resolvers) { const row = byId.get(resolver.row_id); if (!row) throw new Error(`REJECTED ${resolver.row_id}: missing source row`); validateResolverAuthenticity(row, resolver, freezeManifest); }
  const promotable = resolvers.filter((resolver) => resolver.disposition.kind !== "deterministic_reject");
  const permitted = new Set(promotable.map((resolver) => resolver.row_id));
  for (const collection of [cloudResults, judgeReceipts, semanticEvidence]) for (const item of collection) if (!permitted.has(item.row_id)) throw new Error(`REJECTED ${item.row_id}: extra or deterministic evidence`);
  const semanticRows = new Set(promotable.filter((resolver) => resolver.disposition.kind === "cloud_human_audio_semantic").map((resolver) => resolver.row_id));
  for (const item of semanticEvidence) if (!semanticRows.has(item.row_id)) throw new Error(`REJECTED ${item.row_id}: extra human/audio evidence`);
  const accepted = [];
  for (const resolver of promotable) {
    const row = byId.get(resolver.row_id); if (!row) throw new Error(`REJECTED ${resolver.row_id}: missing source row`);
    validateCurrentRow(row, resolver); const boundResolver = { ...resolver, row };
    const cloud = oneByRow(cloudResults, resolver.row_id, "Cloud result"); const output = validateCloud(cloud, boundResolver);
    const judges = judgeReceipts.filter((receipt) => receipt.row_id === resolver.row_id); validateJudges(judges, resolver, cloud.output_hash);
    if (resolver.disposition.kind === "cloud_human_audio_semantic") validateSemantic(semanticEvidence, boundResolver, output);
    accepted.push({ ...row, lifecycle: "accepted", split: "train", cloud_l1_output: output, candidate_output: row.candidate_output, gold_output: output, adjudication_status: "accepted", adjudicator: "cloud+dual-judge", adjudication_reason: resolver.disposition.rule, adjudication_ledger_ref: resolver.input_binding_hash, training_eligible: true });
  }
  return { accepted };
}

function cli() {
  const args = process.argv.slice(2); const output = args[args.indexOf("--out") + 1]; const inputs = args.filter((_, index) => args[index - 1] === "--input");
  if (!output || !inputs.length) throw new Error("usage: node adjudication_pipeline.mjs --input <corpus.jsonl> [--input <corpus.jsonl>] --out <adjudication-dir>");
  const result = buildAdjudicationArtifacts({ corpusFiles: inputs, outputDir: output }); console.log(JSON.stringify({ status: "ok", corpus_hash: result.corpusHash, counts: result.counts, output_dir: result.outputDir }));
}
if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) { try { cli(); } catch (error) { console.error(error.message); process.exitCode = 1; } }
