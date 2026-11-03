#!/usr/bin/env node
import { createHash } from "node:crypto";
import { existsSync, readFileSync, readdirSync, renameSync, writeFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { replayPermittedEdits } from "./replay_permitted_edits.mjs";

const sha256 = (value) => `sha256:${createHash("sha256").update(value).digest("hex")}`;
const readJsonl = (file) => existsSync(file) ? readFileSync(file, "utf8").split(/\r?\n/u).filter(Boolean).map(JSON.parse) : [];
const unique = (rows, label) => { const map = new Map(); for (const row of rows) { if (!row.row_id || map.has(row.row_id)) throw new Error(`${label}: duplicate/missing row ${row.row_id ?? "<missing>"}`); map.set(row.row_id, row); } return map; };
const validate = (result, row, label) => {
  if (!result || result.schema_version !== "heardright.l1_cloud_result.v1" || result.row_id !== row.row_id || result.input_binding_hash !== row.input_binding_hash || result.status !== "ok" || !Array.isArray(result.critical_flags) || result.critical_flags.length || result.output_hash !== sha256(result.output)) throw new Error(`${row.row_id}: invalid ${label} result`);
};

export function createDeterministicFallback(row) {
  const output = replayPermittedEdits(row.input_text, row.permitted_edits ?? []);
  const engineFile = fileURLToPath(new URL("./replay_permitted_edits.mjs", import.meta.url));
  const request = { row_id: row.row_id, input_binding_hash: row.input_binding_hash, input_text: row.input_text, permitted_edits: row.permitted_edits ?? [], authorized_replay_output_hash: row.authorized_replay_output_hash };
  return {
    schema_version: "heardright.l1_cloud_result.v1",
    row_id: row.row_id,
    input_binding_hash: row.input_binding_hash,
    model: "heardright-authorized-replay-engine",
    model_artifact_hash: sha256(readFileSync(engineFile)),
    model_artifact_basis: "sha256 of the exact deterministic replay engine source; no provider model was called for this fallback",
    prompt_hash: sha256("heardright.l1.deterministic_authorized_replay.v1"),
    params_hash: sha256(JSON.stringify({ deterministic: true, provider_called: false })),
    request_hash: sha256(JSON.stringify(request)),
    output,
    output_hash: sha256(output),
    critical_flags: [],
    status: "ok",
    provider_route: "none:deterministic-authority",
    result_provenance: "deterministic_authorized_replay_v1",
    authorized_replay_output_hash: sha256(output),
    authorized_replay_match: true,
    captured_at: new Date().toISOString(),
    metrics: { wall_ms: 0, ttft_ms: null, duration_api_ms: null, input_tokens: null, output_tokens: null },
  };
}

export function mergeTeacherResults({ queue, prior, authorized, deterministicFallback = false }) {
  const priorById = unique(prior, "prior"); const authorizedById = unique(authorized, "authorized");
  const requiredAuthorized = queue.filter((row) => row.proposed_output_matches_authorized_replay === false);
  const requiredIds = new Set(requiredAuthorized.map((row) => row.row_id));
  if ([...authorizedById.keys()].some((rowId) => !requiredIds.has(rowId)) || (!deterministicFallback && (authorizedById.size !== requiredAuthorized.length || requiredAuthorized.some((row) => !authorizedById.has(row.row_id))))) throw new Error("authorized result coverage mismatch");
  const merged = queue.map((row) => {
    const expected = replayPermittedEdits(row.input_text, row.permitted_edits ?? []);
    if (sha256(expected) !== row.authorized_replay_output_hash) throw new Error(`${row.row_id}: queue replay hash drift`);
    if (row.proposed_output_matches_authorized_replay === true) {
      const priorResult = priorById.get(row.row_id); validate(priorResult, row, "prior");
      if (priorResult.output !== expected) throw new Error(`${row.row_id}: prior output mismatch`);
      return priorResult;
    }
    if (row.proposed_output_matches_authorized_replay !== false) throw new Error(`${row.row_id}: missing replay disposition`);
    const replacement = authorizedById.get(row.row_id) ?? (deterministicFallback ? createDeterministicFallback(row) : undefined); validate(replacement, row, "authorized");
    if (!["minimax_generated_exact_authorized_replay_v1", "deterministic_authorized_replay_v1"].includes(replacement.result_provenance) || replacement.authorized_replay_match !== true || replacement.authorized_replay_output_hash !== row.authorized_replay_output_hash || replacement.output !== expected) throw new Error(`${row.row_id}: authorized output mismatch`);
    return replacement;
  });
  if (merged.length !== queue.length) throw new Error("merged result coverage mismatch");
  return merged;
}

function cli() {
  const args = process.argv.slice(2); const valueAfter = (flag) => { const i = args.indexOf(flag); return i >= 0 ? args[i + 1] : undefined; };
  const queueFile = valueAfter("--queue"); const priorFile = valueAfter("--prior"); const authorizedRoot = valueAfter("--authorized-root"); const outputFile = valueAfter("--out"); const deterministicFallback = args.includes("--deterministic-fallback");
  if (!queueFile || !priorFile || !authorizedRoot || !outputFile) throw new Error("usage: --queue <judge-v2> --prior <reused-results> --authorized-root <shards> --out <merged-results>");
  const authorized = readdirSync(authorizedRoot, { withFileTypes: true }).filter((entry) => entry.isDirectory() && /^shard-\d+$/u.test(entry.name)).flatMap((entry) => readJsonl(path.join(authorizedRoot, entry.name, "cloud-results.jsonl")));
  const merged = mergeTeacherResults({ queue: readJsonl(queueFile), prior: readJsonl(priorFile), authorized, deterministicFallback });
  const content = `${merged.map(JSON.stringify).join("\n")}\n`; const temp = `${outputFile}.tmp-${process.pid}`; writeFileSync(temp, content, "utf8"); renameSync(temp, outputFile);
  console.log(JSON.stringify({ schema_version: "heardright.l1_teacher_merge_summary.v1", status: "complete", rows: merged.length, authorized_provider_rows: merged.filter((row) => row.result_provenance === "minimax_generated_exact_authorized_replay_v1").length, deterministic_authority_rows: merged.filter((row) => row.result_provenance === "deterministic_authorized_replay_v1").length, prior_exact_rows: merged.filter((row) => !["minimax_generated_exact_authorized_replay_v1", "deterministic_authorized_replay_v1"].includes(row.result_provenance)).length, output: path.resolve(outputFile), sha256: sha256(content) }));
}

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) { try { cli(); } catch (error) { console.error(error.stack ?? error.message); process.exitCode = 1; } }
