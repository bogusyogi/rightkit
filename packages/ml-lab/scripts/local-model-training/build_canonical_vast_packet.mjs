#!/usr/bin/env node
import { createHash } from "node:crypto";
import { cpSync, existsSync, mkdirSync, readFileSync, readdirSync, renameSync, rmSync, statSync, writeFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { serializeL1 } from "./serialize_l1.mjs";

const SCRIPT_DIR = path.dirname(fileURLToPath(import.meta.url));
const hash = (value) => `sha256:${createHash("sha256").update(value).digest("hex")}`;
const arg = (name, required = true) => { const i = process.argv.indexOf(name); const value = i < 0 ? null : process.argv[i + 1]; if (required && (!value || value.startsWith("--"))) throw new Error(`REJECTED CLI: ${name} is required`); return value; };
const rows = (file) => readFileSync(file, "utf8").split(/\r?\n/u).filter(Boolean).map((line, i) => { try { return JSON.parse(line); } catch { throw new Error(`REJECTED ${file}:${i + 1}: invalid JSON`); } });
const atomic = (file, value) => { mkdirSync(path.dirname(path.resolve(file)), { recursive: true }); const temp = `${file}.tmp-${process.pid}`; writeFileSync(temp, value, "utf8"); renameSync(temp, file); };
const tuning = (row) => ({ schema: "heardright.sotto_tuning_pair.v1", id: row.row_id, input: row.input_text, target: row.candidate_output, input_text: serializeL1(row), output_text: row.candidate_output, bucket: row.sub_bucket ?? row.primary_bucket ?? row.bucket, source: "synthetic_canonical_minimax_teacher_run_20260727", authority: "persisted_synthetic_canonical_target", construction: "target_first_deterministic_v1", template_id: row.template_id, seed: row.generator_seed, bounded_context: row.bounded_context ?? {}, must_preserve: (row.structured_spans?.protected ?? []).filter((span) => span.required).map((span) => span.target_surface), split: row.split });
const files = (root, current = root) => readdirSync(current).flatMap((name) => { const file = path.join(current, name); return statSync(file).isDirectory() ? files(root, file) : [{ path: path.relative(root, file).replaceAll("\\", "/"), bytes: statSync(file).size, sha256: hash(readFileSync(file)) }]; });

try {
  const materialized = arg("--materialized"), selectedFile = arg("--selected", false), template = arg("--template"), out = arg("--out");
  const heldoutSize = Number(arg("--heldout-size", false) ?? 150);
  if (!Number.isInteger(heldoutSize) || heldoutSize < 1) throw new Error("REJECTED CLI: --heldout-size must be a positive integer");
  const all = rows(materialized), suppliedSelected = selectedFile ? rows(selectedFile) : null, allById = new Map(all.map((row) => [row.row_id, row]));
  if (allById.size !== all.length) throw new Error("REJECTED materialized: duplicate row_id");
  const clusterOf = (row) => row.session_cluster_id ?? row.logical_cluster_id ?? row.cluster_id ?? row.row_id;
  const heldoutClusters = new Set(), heldout = [];
  for (const row of [...all].sort((a, b) => hash(a.row_id).localeCompare(hash(b.row_id)) || a.row_id.localeCompare(b.row_id))) {
    if (heldout.length === heldoutSize) break;
    const cluster = clusterOf(row);
    if (heldoutClusters.has(cluster) || all.filter((candidate) => clusterOf(candidate) === cluster).length !== 1) continue;
    heldoutClusters.add(cluster); heldout.push({ ...row, split: "heldout" });
  }
  if (heldout.length !== heldoutSize) throw new Error(`REJECTED heldout: need ${heldoutSize} singleton isolated rows, have ${heldout.length}`);
  const selected = suppliedSelected ?? all.filter((row) => !heldoutClusters.has(clusterOf(row)));
  const selectedIds = new Set(selected.map((row) => row.row_id));
  if (selectedIds.size !== selected.length || selected.some((row) => !allById.has(row.row_id))) throw new Error("REJECTED selected: invalid row binding");
  if (suppliedSelected && heldout.some((row) => selectedIds.has(row.row_id) || selected.some((candidate) => clusterOf(candidate) === clusterOf(row)))) throw new Error("REJECTED selected: overlaps canonical heldout clusters");
  if (existsSync(out)) {
    if (!process.argv.includes("--replace-existing")) throw new Error(`REJECTED output exists: ${out}`);
    rmSync(out, { recursive: true, force: true });
  }
  cpSync(template, out, { recursive: true });
  cpSync(path.join(SCRIPT_DIR, "score_evaluation.mjs"), path.join(out, "runtime", "score_evaluation.mjs"));
  const trainContent = `${selected.sort((a, b) => a.row_id.localeCompare(b.row_id)).map((row) => JSON.stringify(tuning(row))).join("\n")}\n`;
  const heldoutContent = `${heldout.map((row) => JSON.stringify(tuning(row))).join("\n")}\n`;
  atomic(path.join(out, "data", "train.jsonl"), trainContent); atomic(path.join(out, "data", "heldout-synthetic.jsonl"), heldoutContent);
  const manifest = { schema_version: "heardright.sotto.vast_packet.v2", status: "prepared_authorized", authorization: { approved: true, authority: "explicit_user_preapproval_2026-07-27" }, dataset: { train_rows: selected.length, heldout_rows: heldout.length, canonical_rows: all.length, teacher_divergences_retained: all.filter((row) => row.teacher_output_matches_canonical === false).length, training_target: "persisted_synthetic_canonical_candidate_output_v1", split_isolation: "cluster_disjoint", train_sha256: hash(trainContent), heldout_sha256: hash(heldoutContent) }, model: JSON.parse(readFileSync(path.join(template, "packet-manifest.json"), "utf8")).model, environment: JSON.parse(readFileSync(path.join(template, "packet-manifest.json"), "utf8")).environment, provenance: { materialized_sha256: hash(readFileSync(materialized)), selected_sha256: selectedFile ? hash(readFileSync(selectedFile)) : null, split_strategy: selectedFile ? "provided_selected" : "all_canonical_minus_deterministic_singleton_heldout" } };
  atomic(path.join(out, "packet-manifest.json"), `${JSON.stringify(manifest, null, 2)}\n`);
  atomic(path.join(out, "file-manifest.json"), `${JSON.stringify({ schema_version: "heardright.vast_packet_file_manifest.v1", files: files(out).sort((a, b) => a.path.localeCompare(b.path)) }, null, 2)}\n`);
  console.log(JSON.stringify({ status: "ok", train_rows: selected.length, heldout_rows: heldout.length, out }));
} catch (error) { console.error(error.message.startsWith("REJECTED") ? error.message : `REJECTED: ${error.message}`); process.exitCode = 1; }
