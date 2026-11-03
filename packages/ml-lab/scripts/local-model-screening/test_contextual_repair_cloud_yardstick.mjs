#!/usr/bin/env node
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { computeCloudYardstick } from "./compute_contextual_repair_cloud_yardstick.mjs";
import { nearestRankPercentile } from "./contextual_repair_core.mjs";

function payload(at, providerMs) {
  return {
    at,
    model: "qwen/qwen3.6-27b",
    prompt_version: "l1_app_polish_prompt_v5",
    response: JSON.stringify({ usage: { total_time: providerMs / 1000 }, choices: [{ message: { content: "ok" } }] }),
  };
}

function event(at, wallMs) {
  return {
    message: `${at} INFO phase0_polish_timing l1_app_polish_ms=${wallMs} ai_used=true outcome="l1_cleaned"`,
  };
}

const result = computeCloudYardstick(
  [payload("2026-07-21T00:00:00.000Z", 100), payload("2026-07-21T00:00:01.000Z", 200), payload("2026-07-21T00:00:02.000Z", 300)],
  [event("2026-07-21T00:00:00.001Z", 150), event("2026-07-21T00:00:01.002Z", 250), event("2026-07-21T00:00:20.000Z", 999)],
  { matchWindowMs: 5_000 },
);

assert.deepEqual(result.provider_total_ms, { sample_count: 3, p50: 200, p95: 300, max: 300 });
assert.deepEqual(result.cleanup_stage_wall_ms, { sample_count: 2, p50: 150, p95: 250, max: 250 });
assert.equal(result.matches.length, 2);
assert.equal(result.matches[0].delta_ms, 1);

const evidenceFile = path.resolve(path.dirname(fileURLToPath(new URL(import.meta.url))), "../../../docs/evidence/l1-local-model-screening-setup-2026-07-20/cloud-yardstick-current.json");
const evidence = JSON.parse(fs.readFileSync(evidenceFile, "utf8"));
for (const [key, summary] of [["provider_total_ms", evidence.provider_total_ms], ["cleanup_stage_wall_ms", evidence.cleanup_stage_wall_ms]]) {
  const samples = evidence.frozen_samples[key];
  assert.equal(samples.length, summary.sample_count);
  assert.equal(nearestRankPercentile(samples, 0.5), summary.p50);
  assert.equal(nearestRankPercentile(samples, 0.95), summary.p95);
  assert.equal(Math.max(...samples), summary.max);
}

console.log(JSON.stringify({ status: "ok", exact_cloud_yardstick: true }));
