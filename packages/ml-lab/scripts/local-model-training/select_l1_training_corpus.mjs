#!/usr/bin/env node
import { createHash } from "node:crypto";
import { readFileSync, renameSync, writeFileSync } from "node:fs";
import path from "node:path";

const QUOTAS = Object.freeze({ short: { short_noop: 135, short_single_repair: 113, short_punctuation: 90, short_filler_or_restart: 67, short_dictated_format: 45 }, precision: { names: 87, numbers_dates_money: 35, urls_emails_paths: 82, code_identifiers: 82, negation_qualifiers: 87, profanity_tone: 77, deliberate_repetition: 77, semantic_asr_repair: 97, compound_protected_spans: 84 }, long: { contextual_semantic_repair: 59, false_start_or_self_correction: 52, no_op_or_intentional_repetition: 31, misleading_context: 48, multi_error_repair: 68, protected_span: 73, truncation_or_deletion_adversarial: 11, boundary_or_overlap: 0, loop_adversarial: 0 } });
const hash = (value) => `sha256:${createHash("sha256").update(value).digest("hex")}`;
function option(name, required = true) { const i = process.argv.indexOf(name); const value = i < 0 ? null : process.argv[i + 1]; if (required && (!value || value.startsWith("--"))) throw new Error(`REJECTED CLI: ${name} is required`); return value; }
function rows(file) { return readFileSync(file, "utf8").split(/\r?\n/u).filter(Boolean).map((line, i) => { try { return JSON.parse(line); } catch (error) { throw new Error(`REJECTED ${file}:${i + 1}: invalid JSON (${error.message})`); } }); }
function quotas(raw) { if (raw === null) return structuredClone(QUOTAS); try { const value = JSON.parse(raw); if (!value || typeof value !== "object") throw new Error(); return value; } catch { throw new Error("REJECTED CLI: --quota-json must be a JSON object"); } }
function classify(row) {
  if (typeof row.sub_bucket === "string") return ["short", row.sub_bucket];
  if (typeof row.primary_bucket === "string") return ["precision", row.primary_bucket];
  const tags = Array.isArray(row.failure_tags) ? row.failure_tags : [];
  for (const bucket of Object.keys(QUOTAS.long)) if (tags.includes(bucket)) return ["long", bucket];
  throw new Error(`REJECTED ${row.row_id}: no selection bucket`);
}
function rank(row) { return hash(JSON.stringify({ row_id: row.row_id, source_hash: row.source_hash ?? "", cluster_id: row.session_cluster_id ?? row.logical_cluster_id ?? row.cluster_id ?? "" })); }
function atomic(file, value) { const target = path.resolve(file), temporary = `${target}.tmp-${process.pid}`; writeFileSync(temporary, value, "utf8"); renameSync(temporary, target); }
function main() {
  const input = option("--input"), out = option("--out"), manifest = option("--manifest"), config = quotas(option("--quota-json", false));
  const grouped = new Map(), ids = new Set();
  for (const row of rows(input)) {
    if (typeof row.row_id !== "string" || row.row_id === "") throw new Error("REJECTED input: missing row_id"); if (ids.has(row.row_id)) throw new Error(`REJECTED input: duplicate row_id ${row.row_id}`); ids.add(row.row_id);
    if (row.lifecycle !== "accepted" || row.split !== "train" || row.training_eligible !== true) throw new Error(`REJECTED ${row.row_id}: not accepted training data`);
    const [lane, bucket] = classify(row); if (!Object.hasOwn(config[lane] ?? {}, bucket)) throw new Error(`REJECTED ${row.row_id}: unconfigured quota ${lane}/${bucket}`);
    const key = `${lane}/${bucket}`; if (!grouped.has(key)) grouped.set(key, []); grouped.get(key).push(row);
  }
  const selected = [], usedLongClusters = new Set(), actual = {};
  for (const lane of Object.keys(config).sort()) for (const [bucket, wanted] of Object.entries(config[lane])) {
    if (!Number.isInteger(wanted) || wanted < 0) throw new Error(`REJECTED quota ${lane}/${bucket}: non-negative integer required`);
    const pool = (grouped.get(`${lane}/${bucket}`) ?? []).sort((a, b) => rank(a).localeCompare(rank(b)) || a.row_id.localeCompare(b.row_id)); const picks = [];
    for (const row of pool) { if (picks.length === wanted) break; const cluster = row.session_cluster_id ?? row.logical_cluster_id ?? row.cluster_id ?? row.row_id; if (lane === "long" && usedLongClusters.has(cluster)) continue; picks.push(row); if (lane === "long") usedLongClusters.add(cluster); }
    if (picks.length !== wanted) throw new Error(`REJECTED quota ${lane}/${bucket}: need ${wanted}, have ${picks.length}`);
    actual[lane] ??= {}; actual[lane][bucket] = picks.length; selected.push(...picks);
  }
  const expected = Object.values(config).flatMap(Object.values).reduce((total, value) => total + value, 0); if (selected.length !== expected) throw new Error(`REJECTED selection: expected ${expected}, got ${selected.length}`);
  selected.sort((a, b) => a.row_id.localeCompare(b.row_id)); const content = `${selected.map(JSON.stringify).join("\n")}\n`;
  atomic(out, content); atomic(manifest, `${JSON.stringify({ schema_version: "heardright.l1_training_selection.v1", selected_rows: selected.length, quotas: actual, input_sha256: hash(readFileSync(input)), output_sha256: hash(content), row_ids_sha256: hash(selected.map((row) => row.row_id).join("\n")) }, null, 2)}\n`);
  console.log(JSON.stringify({ status: "ok", selected_rows: selected.length, out, manifest }));
}
try { main(); } catch (error) { console.error(error.message.startsWith("REJECTED") ? error.message : `REJECTED: ${error.message}`); process.exitCode = 1; }
