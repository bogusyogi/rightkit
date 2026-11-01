#!/usr/bin/env node
import { createHash } from "node:crypto";
import { readFileSync, renameSync, writeFileSync } from "node:fs";
import path from "node:path";
import { pathToFileURL } from "node:url";

const CANONICAL = "heardright.l1_training_candidate.v1";
const PRECISION_V2 = "heardright.l1_precision_candidate.v2";
const LONG_V2 = "heardright.l1.scheduled-static15.candidate.v2";
const SCHEMAS = new Set([CANONICAL, PRECISION_V2, LONG_V2]);
const HASH = /^sha256:[a-f0-9]{64}$/u;
const BARE_HASH = /^[a-f0-9]{64}$/u;

function hash(value) {
  return `sha256:${createHash("sha256").update(value).digest("hex")}`;
}

function requireString(value, field, rowId) {
  if (typeof value !== "string" || value.trim() === "") throw new Error(`REJECTED ${rowId}: ${field} must be a non-empty string`);
  return value;
}

function requireHash(value, field, rowId, allowBare = false) {
  if (typeof value === "string" && HASH.test(value)) return value;
  if (allowBare && typeof value === "string" && BARE_HASH.test(value)) return `sha256:${value}`;
  throw new Error(`REJECTED ${rowId}: ${field} must be sha256:<64 lowercase hex>`);
}

function detectSchema(row) {
  return row.schema_version ?? row.schema;
}

function clone(value) {
  return value === undefined ? undefined : structuredClone(value);
}

function lineageFor(row, schema, rowId) {
  if (schema === PRECISION_V2) {
    const generatorHash = requireHash(row.generator_hash, "generator_hash", rowId);
    const scriptHash = requireHash(row.generator?.script_sha256, "generator.script_sha256", rowId);
    if (generatorHash !== scriptHash) throw new Error(`REJECTED ${rowId}: generator lineage mismatch`);
    return {
      generator_hash: generatorHash,
      template_lineage: {
        id: requireString(row.generator?.structural_frame_id, "generator.structural_frame_id", rowId),
        hash: requireHash(row.structural_signature, "structural_signature", rowId),
        authority: "persisted_structural_signature",
      },
    };
  }
  if (schema === LONG_V2) {
    return {
      generator_hash: requireHash(row.generator_sha256, "generator_sha256", rowId, true),
      template_lineage: {
        id: requireString(row.source_template_id, "source_template_id", rowId),
        hash: requireHash(row.source_hash, "source_hash", rowId),
        authority: "persisted_source_template_and_source_hash",
      },
    };
  }
  const generatorHash = requireHash(row.generator_hash, "generator_hash", rowId);
  const directTemplateHash = requireHash(row.template_hash, "template_hash", rowId);
  const persisted = row.template_lineage;
  const lineage = persisted ?? { id: row.template_id, hash: directTemplateHash, authority: "persisted_canonical_template_hash" };
  const lineageHash = requireHash(lineage?.hash, "template_lineage.hash", rowId);
  if (lineageHash !== directTemplateHash) throw new Error(`REJECTED ${rowId}: template lineage mismatch`);
  if (row.generator?.script_sha256 !== undefined && requireHash(row.generator.script_sha256, "generator.script_sha256", rowId) !== generatorHash) throw new Error(`REJECTED ${rowId}: generator lineage mismatch`);
  return {
    generator_hash: generatorHash,
    template_lineage: {
      id: requireString(lineage?.id, "template_lineage.id", rowId),
      hash: lineageHash,
      authority: requireString(lineage?.authority, "template_lineage.authority", rowId),
    },
  };
}

function structuredSpansFor(row, schema, rowId) {
  if (schema === PRECISION_V2) {
    if (!Array.isArray(row.protected_spans)) throw new Error(`REJECTED ${rowId}: protected_spans must be an array`);
    return {
      protected: clone(row.protected_spans),
      permitted_changes: clone(row.permitted_edit_ops ?? []),
      deletion_permissions: [],
      context_resolution: [],
    };
  }
  if (!row.structured_spans || !Array.isArray(row.structured_spans.protected)) throw new Error(`REJECTED ${rowId}: structured_spans.protected must be an array`);
  return clone(row.structured_spans);
}

function adjudicationFor(row, schema) {
  if (schema === PRECISION_V2) {
    return {
      adjudicator: row.adjudication?.adjudicator_ids?.[0] ?? null,
      reason: row.adjudication?.reason ?? null,
      ledger_ref: row.adjudication?.ledger_ref ?? null,
    };
  }
  if (schema === LONG_V2) {
    return {
      adjudicator: row.adjudication?.primary_reviewer ?? null,
      reason: row.adjudication?.rationale ?? null,
      ledger_ref: row.adjudication?.ledger_ref ?? null,
    };
  }
  return {
    adjudicator: row.adjudicator ?? row.adjudication?.adjudicator ?? null,
    reason: row.adjudication_reason ?? row.adjudication?.reason ?? null,
    ledger_ref: row.adjudication_ledger_ref ?? row.adjudication?.ledger_ref ?? null,
  };
}

export function normalizeCandidate(row) {
  const schema = detectSchema(row);
  const rowId = row.row_id ?? "<missing-row-id>";
  if (!SCHEMAS.has(schema)) throw new Error(`REJECTED ${rowId}: unsupported schema ${String(schema)}`);
  const inputText = row.input_text ?? row.current_l0_chunk;
  const clusterId = row.cluster_id ?? row.logical_cluster_id ?? row.session_cluster_id;
  const lineage = lineageFor(row, schema, rowId);
  const adjudication = adjudicationFor(row, schema);
  return {
    schema_version: CANONICAL,
    source_schema: schema,
    lifecycle: row.lifecycle ?? "candidate",
    row_id: requireString(row.row_id, "row_id", rowId),
    cluster_id: requireString(clusterId, "cluster_id", rowId),
    split: row.split,
    source_kind: requireString(row.source_kind, "source_kind", rowId),
    consent: requireString(row.consent, "consent", rowId),
    license: requireString(row.license, "license", rowId),
    input_text: requireString(inputText, "input_text", rowId),
    bounded_context: row.bounded_context && typeof row.bounded_context === "object" ? clone(row.bounded_context) : {},
    cloud_l1_output: row.cloud_l1_output ?? row.cloud_l1_draft ?? null,
    candidate_output: row.candidate_output ?? null,
    gold_output: row.gold_output ?? null,
    adjudication_status: row.adjudication_status ?? row.adjudication?.status ?? "pending",
    adjudicator: adjudication.adjudicator,
    adjudication_reason: adjudication.reason,
    adjudication_ledger_ref: adjudication.ledger_ref,
    training_eligible: row.training_eligible ?? false,
    structured_spans: structuredSpansFor(row, schema, rowId),
    failure_tags: Array.isArray(row.failure_tags) ? clone(row.failure_tags) : [],
    source_hash: requireHash(row.source_hash, "source_hash", rowId),
    generator_hash: lineage.generator_hash,
    template_lineage: lineage.template_lineage,
    serializer_version: requireString(row.serializer_version, "serializer_version", rowId),
  };
}

export function toTrainingRow(candidate) {
  const rowId = candidate.row_id;
  if (candidate.lifecycle !== "accepted") throw new Error(`REJECTED ${rowId}: lifecycle must be accepted`);
  if (candidate.split !== "train") throw new Error(`REJECTED ${rowId}: split must be train for training export`);
  if (candidate.adjudication_status !== "accepted") throw new Error(`REJECTED ${rowId}: adjudication_status must be accepted`);
  if (candidate.training_eligible !== true) throw new Error(`REJECTED ${rowId}: training_eligible must be true`);
  const gold = requireString(candidate.gold_output, "gold_output", rowId);
  const adjudicator = requireString(candidate.adjudicator, "adjudicator", rowId);
  const reason = requireString(candidate.adjudication_reason, "adjudication_reason", rowId);
  const ledgerRef = requireString(candidate.adjudication_ledger_ref, "adjudication_ledger_ref", rowId);
  if (/[\u2013\u2014]/u.test(gold)) throw new Error(`REJECTED ${rowId}: gold_output contains forbidden dash`);
  for (const span of candidate.structured_spans.protected) {
    const target = span.target_surface ?? span.text;
    const required = span.required ?? true;
    if (required && (typeof target !== "string" || !gold.includes(target))) throw new Error(`REJECTED ${rowId}: gold_output misses protected span ${String(target)}`);
  }
  return {
    schema_version: "heardright.l1_training_row.v1",
    row_id: candidate.row_id,
    cluster_id: candidate.cluster_id,
    split: "train",
    source_kind: candidate.source_kind,
    input_text: candidate.input_text,
    bounded_context: candidate.bounded_context,
    output_text: gold,
    structured_spans: clone(candidate.structured_spans),
    failure_tags: candidate.failure_tags,
    serializer_version: candidate.serializer_version,
    source_hash: candidate.source_hash,
    generator_hash: candidate.generator_hash,
    template_lineage: clone(candidate.template_lineage),
    adjudication: { status: candidate.adjudication_status, adjudicator, reason, ledger_ref: ledgerRef },
  };
}

function optionValues(name) {
  const values = [];
  for (let index = 2; index < process.argv.length; index += 1) if (process.argv[index] === name) values.push(process.argv[index + 1]);
  return values;
}

function main() {
  const inputs = optionValues("--input");
  const outputs = optionValues("--out");
  if (inputs.length === 0 || outputs.length !== 1) throw new Error("REJECTED CLI: use --input <jsonl> [--input <jsonl> ...] --out <jsonl>");
  const seenRows = new Set();
  const trainingRows = [];
  const sourceSchemas = new Set();
  for (const input of inputs) {
    const lines = readFileSync(input, "utf8").split(/\r?\n/u).filter((line) => line.trim() !== "");
    for (const [lineIndex, line] of lines.entries()) {
      let raw;
      try { raw = JSON.parse(line); } catch (error) { throw new Error(`REJECTED ${input}:${lineIndex + 1}: invalid JSON: ${error.message}`); }
      const normalized = normalizeCandidate(raw);
      if (seenRows.has(normalized.row_id)) throw new Error(`REJECTED ${normalized.row_id}: duplicate row_id`);
      seenRows.add(normalized.row_id);
      sourceSchemas.add(normalized.source_schema);
      trainingRows.push(toTrainingRow(normalized));
    }
  }
  const output = path.resolve(outputs[0]);
  const content = `${trainingRows.map((row) => JSON.stringify(row)).join("\n")}\n`;
  const temporary = `${output}.tmp-${process.pid}`;
  writeFileSync(temporary, content, "utf8");
  renameSync(temporary, output);
  console.log(JSON.stringify({ status: "ok", rows: trainingRows.length, source_schemas: [...sourceSchemas].sort(), output, sha256: hash(content) }));
}

const invokedPath = process.argv[1] ? pathToFileURL(path.resolve(process.argv[1])).href : null;
if (invokedPath !== null && import.meta.url === invokedPath) {
  try { main(); } catch (error) { console.error(error.message.startsWith("REJECTED") ? error.message : `REJECTED: ${error.message}`); process.exitCode = 1; }
}
