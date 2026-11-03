#!/usr/bin/env node
import crypto from "node:crypto";
import fs from "node:fs";
import { fileURLToPath } from "node:url";

const SCHEMA = "heardright.sotto_tuning_pair.v1";
const SPLITS = new Set(["train", "dev", "sealed"]);
const SOURCES = new Set(["synthetic_target_first", "sotto_replay", "real_human"]);
const AUTHORITIES = new Set(["construction", "public_dataset", "human"]);
const ROUND_ONE_TRAIN_BUCKETS = {
  self_correction: 500,
  protected_literal: 250,
  minimal_edit: 200,
  long_intentional_repetition: 150,
  sotto_replay: 100,
};
const ROUND_ONE_DEV_BUCKETS = {
  self_correction: 60,
  protected_literal: 35,
  minimal_edit: 30,
  long_intentional_repetition: 25,
};

function requireString(row, field) {
  if (typeof row[field] !== "string" || row[field].trim() === "") {
    throw new Error(`${row.id ?? "unknown"}: ${field} must be a non-empty string`);
  }
}

function includesFolded(value, needle) {
  return value.toLocaleLowerCase("en-US").includes(needle.toLocaleLowerCase("en-US"));
}

function fingerprint(row) {
  const normalize = (value) => value.toLocaleLowerCase("en-US").replace(/[^\p{L}\p{N}]+/gu, " ").trim();
  return crypto.createHash("sha256").update(`${normalize(row.input)}\n${normalize(row.target)}`).digest("hex");
}

function normalizedTokens(value) {
  return value.toLocaleLowerCase("en-US").match(/[\p{L}\p{N}]+/gu) ?? [];
}

function targetFingerprint(row) {
  return crypto.createHash("sha256").update(normalizedTokens(row.target).join(" ")).digest("hex");
}

function repeatedNgram(value, size = 5, limit = 3) {
  const tokens = normalizedTokens(value);
  const counts = new Map();
  for (let index = 0; index <= tokens.length - size; index += 1) {
    const ngram = tokens.slice(index, index + size).join(" ");
    const count = (counts.get(ngram) ?? 0) + 1;
    if (count >= limit) return ngram;
    counts.set(ngram, count);
  }
  return null;
}

export function validateTuningPairs(rows) {
  if (!Array.isArray(rows) || rows.length === 0) throw new Error("dataset must contain at least one row");
  const ids = new Set();
  const byFingerprint = new Map();
  const byTargetFingerprint = new Map();
  const templateSplits = new Map();
  const splits = {};
  const buckets = {};
  const sources = {};

  for (const row of rows) {
    if (!row || typeof row !== "object" || Array.isArray(row)) throw new Error("every row must be an object");
    requireString(row, "id");
    requireString(row, "input");
    requireString(row, "target");
    requireString(row, "bucket");
    if (row.schema !== SCHEMA) throw new Error(`${row.id}: wrong schema`);
    if (ids.has(row.id)) throw new Error(`${row.id}: duplicate id`);
    ids.add(row.id);
    if (!SPLITS.has(row.split)) throw new Error(`${row.id}: invalid split`);
    if (!SOURCES.has(row.source)) throw new Error(`${row.id}: invalid source`);
    if (row.source === "synthetic_target_first" && row.authority !== "construction") {
      throw new Error(`${row.id}: synthetic_target_first requires construction authority`);
    }
    if (row.source === "sotto_replay" && row.authority !== "public_dataset") {
      throw new Error(`${row.id}: sotto_replay requires public_dataset authority`);
    }
    if (row.source === "real_human" && row.authority !== "human") {
      throw new Error(`${row.id}: real_human requires human authority`);
    }
    if (!AUTHORITIES.has(row.authority)) throw new Error(`${row.id}: invalid authority`);
    if (row.source === "synthetic_target_first") {
      if (row.construction !== "target_first_deterministic_v1") {
        throw new Error(`${row.id}: synthetic construction must be target_first_deterministic_v1`);
      }
      if (typeof row.template_id !== "string" || row.template_id.trim() === "") {
        throw new Error(`${row.id}: synthetic rows require template_id`);
      }
      if (!Number.isSafeInteger(row.seed)) throw new Error(`${row.id}: synthetic rows require integer seed`);
      const priorSplit = templateSplits.get(row.template_id);
      if (priorSplit && priorSplit !== row.split) {
        throw new Error(`${row.id}: template cross-split leak from ${priorSplit}: ${row.template_id}`);
      }
      templateSplits.set(row.template_id, row.split);
    }
    if (row.source === "sotto_replay") {
      requireString(row, "source_revision");
      if (!Number.isSafeInteger(row.source_row) || row.source_row < 0) {
        throw new Error(`${row.id}: sotto_replay requires non-negative integer source_row`);
      }
    }
    if (!Array.isArray(row.must_preserve) || !row.must_preserve.every((item) => typeof item === "string" && item.trim())) {
      throw new Error(`${row.id}: must_preserve must be a string array`);
    }
    if (!Array.isArray(row.corrections)) throw new Error(`${row.id}: corrections must be an array`);

    for (const literal of row.must_preserve) {
      if (!includesFolded(row.input, literal)) {
        throw new Error(`${row.id}: must_preserve missing from input: ${literal}`);
      }
      if (!includesFolded(row.target, literal)) {
        throw new Error(`${row.id}: must_preserve missing from target: ${literal}`);
      }
    }
    for (const correction of row.corrections) {
      if (!correction || typeof correction.drop !== "string" || typeof correction.keep !== "string") {
        throw new Error(`${row.id}: correction requires string drop and keep`);
      }
      if (!includesFolded(row.input, correction.drop) || !includesFolded(row.input, correction.keep)) {
        throw new Error(`${row.id}: correction values must both occur in input`);
      }
      if (includesFolded(row.target, correction.drop)) {
        throw new Error(`${row.id}: correction drop retained in target: ${correction.drop}`);
      }
      if (!includesFolded(row.target, correction.keep)) {
        throw new Error(`${row.id}: correction keep missing from target: ${correction.keep}`);
      }
    }
    if (row.source === "synthetic_target_first") {
      const inputTokens = new Set(normalizedTokens(row.input));
      const novel = normalizedTokens(row.target).find((token) => !inputTokens.has(token));
      if (novel) throw new Error(`${row.id}: novel target token absent from input: ${novel}`);
      const loop = repeatedNgram(row.target);
      if (loop) throw new Error(`${row.id}: repeated 5-gram three times: ${loop}`);
    }

    const digest = fingerprint(row);
    const previous = byFingerprint.get(digest);
    if (previous) {
      if (previous.split !== row.split) {
        throw new Error(`${row.id}: cross-split duplicate of ${previous.id}`);
      }
      throw new Error(`${row.id}: duplicate pair of ${previous.id}`);
    }
    byFingerprint.set(digest, { id: row.id, split: row.split });
    const targetDigest = targetFingerprint(row);
    const previousTarget = byTargetFingerprint.get(targetDigest);
    if (previousTarget && previousTarget.split !== row.split) {
      throw new Error(`${row.id}: target cross-split leak from ${previousTarget.id}`);
    }
    byTargetFingerprint.set(targetDigest, { id: row.id, split: row.split });
    splits[row.split] = (splits[row.split] ?? 0) + 1;
    buckets[row.bucket] = (buckets[row.bucket] ?? 0) + 1;
    sources[row.source] = (sources[row.source] ?? 0) + 1;
  }

  return { schema: SCHEMA, rows: rows.length, splits, buckets, sources };
}

function countBuckets(rows, split) {
  const counts = {};
  for (const row of rows) {
    if (row.split === split) counts[row.bucket] = (counts[row.bucket] ?? 0) + 1;
  }
  return counts;
}

function requireExactCounts(actual, expected, label) {
  if (JSON.stringify(actual) !== JSON.stringify(expected)) {
    throw new Error(`${label} counts mismatch: expected ${JSON.stringify(expected)}, got ${JSON.stringify(actual)}`);
  }
}

export function validateRoundOneProfile(rows) {
  const summary = validateTuningPairs(rows);
  requireExactCounts(summary.splits, { train: 1200, dev: 150 }, "split");
  const trainBuckets = countBuckets(rows, "train");
  const devBuckets = countBuckets(rows, "dev");
  requireExactCounts(trainBuckets, ROUND_ONE_TRAIN_BUCKETS, "train bucket");
  requireExactCounts(devBuckets, ROUND_ONE_DEV_BUCKETS, "dev bucket");
  requireExactCounts(summary.sources, { synthetic_target_first: 1250, sotto_replay: 100 }, "source");
  return {
    rows: summary.rows,
    splits: summary.splits,
    train_buckets: trainBuckets,
    dev_buckets: devBuckets,
    sources: summary.sources,
  };
}

function readJsonl(file) {
  return fs.readFileSync(file, "utf8").split(/\r?\n/u).filter(Boolean).map((line, index) => {
    try {
      return JSON.parse(line);
    } catch (error) {
      throw new Error(`${file}:${index + 1}: ${error.message}`);
    }
  });
}

if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) {
  const inputIndex = process.argv.indexOf("--input");
  if (inputIndex < 0 || !process.argv[inputIndex + 1]) throw new Error("--input <jsonl> is required");
  const rows = readJsonl(process.argv[inputIndex + 1]);
  const result = process.argv.includes("--round-one-profile")
    ? validateRoundOneProfile(rows)
    : validateTuningPairs(rows);
  console.log(JSON.stringify(result));
}
