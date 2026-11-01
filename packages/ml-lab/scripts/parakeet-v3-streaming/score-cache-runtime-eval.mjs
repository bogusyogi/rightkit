#!/usr/bin/env node
import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

import { norm, wordErrorStats } from "../open-asr-bakeoff/score-asr-wer.mjs";

const __filename = fileURLToPath(import.meta.url);

function round(value, digits = 6) {
  if (!Number.isFinite(value)) return null;
  const factor = 10 ** digits;
  return Math.round(value * factor) / factor;
}

function percentile(values, quantile) {
  if (values.length === 0) return null;
  const sorted = [...values].sort((a, b) => a - b);
  const index = (sorted.length - 1) * quantile;
  const lower = Math.floor(index);
  const upper = Math.ceil(index);
  if (lower === upper) return sorted[lower];
  return sorted[lower] + (sorted[upper] - sorted[lower]) * (index - lower);
}

function readJsonl(file) {
  return fs
    .readFileSync(file, "utf8")
    .split(/\r?\n/)
    .filter((line) => line.trim())
    .map((line) => JSON.parse(line));
}

function rowsById(rows) {
  const mapped = new Map();
  for (const row of rows) {
    const id = String(row.id ?? row.clip_id ?? "");
    if (!id) throw new Error("hypothesis row has no id");
    if (mapped.has(id)) throw new Error(`duplicate hypothesis id: ${id}`);
    mapped.set(id, row);
  }
  return mapped;
}

function truthText(row) {
  return row.gt ?? row.ground_truth ?? row.transcript ?? row.authority ?? "";
}

function perIdStats(id, truth, baselineMap, cacheMap) {
  const truthRow = truth[id];
  if (!truthRow) throw new Error(`missing private reference row: ${id}`);
  const reference = norm(truthText(truthRow));
  const baselineRow = baselineMap.get(id);
  const cacheRow = cacheMap.get(id);
  return {
    id,
    group: String(truthRow.group ?? "unknown"),
    duration_s: Number(truthRow.duration_s),
    words: reference.split(/\s+/).filter(Boolean).length,
    baseline_errors: wordErrorStats(
      reference,
      norm(baselineRow?.hypothesis ?? ""),
    ).edits,
    cache_errors: wordErrorStats(reference, norm(cacheRow?.hypothesis ?? "")).edits,
  };
}

function aggregate(rows) {
  const words = rows.reduce((sum, row) => sum + row.words, 0);
  const baselineErrors = rows.reduce((sum, row) => sum + row.baseline_errors, 0);
  const cacheErrors = rows.reduce((sum, row) => sum + row.cache_errors, 0);
  const baselineWer = words ? (baselineErrors / words) * 100 : 0;
  const cacheWer = words ? (cacheErrors / words) * 100 : 0;
  return {
    clips: rows.length,
    words,
    baseline_errors: baselineErrors,
    cache_errors: cacheErrors,
    baseline_wer_percent: round(baselineWer),
    cache_wer_percent: round(cacheWer),
    delta_pp: round(cacheWer - baselineWer),
  };
}

function makePrng(seed) {
  let state = seed >>> 0 || 1;
  return () => {
    state ^= state << 13;
    state ^= state >>> 17;
    state ^= state << 5;
    return (state >>> 0) / 0x1_0000_0000;
  };
}

function pairedBootstrap(rows, iterations, seed) {
  if (rows.length === 0) {
    return {
      iterations,
      seed,
      lower_2_5_pp: null,
      upper_95_pp: null,
      upper_97_5_pp: null,
    };
  }
  const random = makePrng(seed);
  const deltas = [];
  for (let iteration = 0; iteration < iterations; iteration += 1) {
    let words = 0;
    let baselineErrors = 0;
    let cacheErrors = 0;
    for (let draw = 0; draw < rows.length; draw += 1) {
      const row = rows[Math.floor(random() * rows.length)];
      words += row.words;
      baselineErrors += row.baseline_errors;
      cacheErrors += row.cache_errors;
    }
    deltas.push(words ? ((cacheErrors - baselineErrors) / words) * 100 : 0);
  }
  return {
    iterations,
    seed,
    lower_2_5_pp: round(percentile(deltas, 0.025)),
    upper_95_pp: round(percentile(deltas, 0.95)),
    upper_97_5_pp: round(percentile(deltas, 0.975)),
  };
}

function tailSummary(mapped, ids) {
  const values = ids
    .map((id) => mapped.get(id)?.simulated_stop_tail_ms)
    .filter((value) => typeof value === "number" && Number.isFinite(value));
  return {
    clips: values.length,
    p50_ms: round(percentile(values, 0.5), 3),
    p90_ms: round(percentile(values, 0.9), 3),
    p95_ms: round(percentile(values, 0.95), 3),
    p99_ms: round(percentile(values, 0.99), 3),
  };
}

export function scorePair({
  baseline,
  cache,
  truth,
  ids,
  bootstrapIterations = 10_000,
  seed = 20_260_720,
}) {
  const baselineMap = rowsById(baseline);
  const cacheMap = rowsById(cache);
  const stats = ids.map((id) => perIdStats(id, truth, baselineMap, cacheMap));
  const longStats = stats.filter((row) => row.duration_s > 15);
  const groups = {};
  for (const group of [...new Set(stats.map((row) => row.group))].sort()) {
    groups[group] = aggregate(stats.filter((row) => row.group === group));
  }

  const overall = aggregate(stats);
  const over15 = aggregate(longStats);
  const baselineTail = tailSummary(baselineMap, ids);
  const cacheTail = tailSummary(cacheMap, ids);
  const tailReductionMs = baselineTail.p95_ms - cacheTail.p95_ms;
  const tailReductionPercent = baselineTail.p95_ms
    ? (tailReductionMs / baselineTail.p95_ms) * 100
    : 0;
  const cacheRowsComplete = ids.every(
    (id) => cacheMap.has(id) && cacheMap.get(id).status === "ok",
  );
  const cacheRowsNonempty = ids.every(
    (id) => String(cacheMap.get(id)?.hypothesis ?? "").trim().length > 0,
  );
  const groupsNonInferior = Object.values(groups).every((row) => row.delta_pp <= 0.25);
  const overallBootstrap = pairedBootstrap(stats, bootstrapIterations, seed);
  const over15Bootstrap = pairedBootstrap(longStats, bootstrapIterations, seed + 1);
  const accuracyNonInferior =
    cacheRowsComplete &&
    cacheRowsNonempty &&
    overallBootstrap.upper_95_pp <= 0.1 &&
    over15Bootstrap.upper_95_pp <= 0.25 &&
    groupsNonInferior;
  const materialStopTailWin =
    cacheTail.clips === ids.length &&
    baselineTail.clips === ids.length &&
    tailReductionMs >= 100 &&
    tailReductionPercent >= 20 &&
    cacheTail.p95_ms <= 200;

  return {
    quality: { overall, over_15s: over15, by_group: groups },
    bootstrap: {
      overall: overallBootstrap,
      over_15s: over15Bootstrap,
    },
    latency: {
      baseline: baselineTail,
      cache: cacheTail,
      p95_reduction_ms: round(tailReductionMs, 3),
      p95_reduction_percent: round(tailReductionPercent, 3),
    },
    availability: {
      required: ids.length,
      baseline_rows: ids.filter((id) => baselineMap.has(id)).length,
      cache_rows: ids.filter((id) => cacheMap.has(id)).length,
      cache_all_ok: cacheRowsComplete,
      cache_all_nonempty: cacheRowsNonempty,
    },
    gates: {
      accuracy_non_inferior: accuracyNonInferior,
      material_stop_tail_win: materialStopTailWin,
    },
    decision:
      accuracyNonInferior && materialStopTailWin
        ? "select-cache-for-product-integration"
        : "keep-current",
  };
}

function parseArgs(argv) {
  const args = { bootstrapIterations: 10_000, seed: 20_260_720 };
  for (let i = 0; i < argv.length; i += 1) {
    const flag = argv[i];
    if (flag === "--baseline") args.baseline = argv[++i];
    else if (flag === "--cache") args.cache = argv[++i];
    else if (flag === "--truth") args.truth = argv[++i];
    else if (flag === "--manifest") args.manifest = argv[++i];
    else if (flag === "--out") args.out = argv[++i];
    else if (flag === "--bootstrap-iterations") args.bootstrapIterations = Number(argv[++i]);
    else if (flag === "--seed") args.seed = Number(argv[++i]);
    else throw new Error(`unknown argument: ${flag}`);
  }
  for (const required of ["baseline", "cache", "truth", "manifest", "out"]) {
    if (!args[required]) throw new Error(`missing --${required}`);
  }
  return args;
}

function sha256File(file) {
  return crypto.createHash("sha256").update(fs.readFileSync(file)).digest("hex");
}

function main() {
  const args = parseArgs(process.argv.slice(2));
  const manifest = JSON.parse(fs.readFileSync(args.manifest, "utf8"));
  const report = scorePair({
    baseline: readJsonl(args.baseline),
    cache: readJsonl(args.cache),
    truth: JSON.parse(fs.readFileSync(args.truth, "utf8")),
    ids: manifest.map((row) => String(row.id)),
    bootstrapIterations: args.bootstrapIterations,
    seed: args.seed,
  });
  report.provenance = {
    scorer_sha256: sha256File(__filename),
    baseline_sha256: sha256File(args.baseline),
    cache_sha256: sha256File(args.cache),
    truth_sha256: sha256File(args.truth),
    manifest_sha256: sha256File(args.manifest),
  };
  fs.mkdirSync(path.dirname(path.resolve(args.out)), { recursive: true });
  fs.writeFileSync(args.out, `${JSON.stringify(report, null, 2)}\n`);
  console.log(JSON.stringify(report, null, 2));
}

if (process.argv[1] && path.resolve(process.argv[1]) === path.resolve(__filename)) {
  main();
}
