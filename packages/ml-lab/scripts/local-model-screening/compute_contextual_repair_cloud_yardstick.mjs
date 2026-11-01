#!/usr/bin/env node
import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { nearestRankPercentile } from "./contextual_repair_core.mjs";

const scriptFile = fileURLToPath(import.meta.url);
const ANSI_ESCAPE_PATTERN = new RegExp(`${String.fromCharCode(27)}\\[[0-9;]*m`, "g");

function stats(values) {
  return {
    sample_count: values.length,
    p50: nearestRankPercentile(values, 0.5),
    p95: nearestRankPercentile(values, 0.95),
    max: values.length ? Math.max(...values) : null,
  };
}

function parsePayload(row) {
  if (row?.model !== "qwen/qwen3.6-27b" || row?.prompt_version !== "l1_app_polish_prompt_v5" || !row.response) return null;
  try {
    const response = typeof row.response === "string" ? JSON.parse(row.response) : row.response;
    const providerMs = Number(response?.usage?.total_time) * 1000;
    const at = Date.parse(row.at);
    if (!Number.isFinite(providerMs) || !Number.isFinite(at)) return null;
    return { at, at_iso: new Date(at).toISOString(), provider_total_ms: providerMs };
  } catch {
    return null;
  }
}

function parseEvent(row) {
  const message = String(row?.message ?? "").replace(ANSI_ESCAPE_PATTERN, "");
  const match = message.match(/(?<at>\d{4}-\d{2}-\d{2}T\S+).*?l1_app_polish_ms=(?<ms>\d+(?:\.\d+)?).*?ai_used=true.*?outcome=\\?"?l1_cleaned\\?"?/);
  if (!match) return null;
  const at = Date.parse(match.groups.at);
  const wallMs = Number(match.groups.ms);
  if (!Number.isFinite(at) || !Number.isFinite(wallMs)) return null;
  return { at, at_iso: new Date(at).toISOString(), cleanup_stage_wall_ms: wallMs };
}

export function computeCloudYardstick(payloadRows, eventRows, { matchWindowMs = 5_000 } = {}) {
  const payloads = payloadRows.map(parsePayload).filter(Boolean).sort((a, b) => a.at - b.at);
  const events = eventRows.map(parseEvent).filter(Boolean).sort((a, b) => a.at - b.at);
  const usedEvents = new Set();
  const matches = [];
  for (const payload of payloads) {
    let bestIndex = -1;
    let bestDistance = Number.POSITIVE_INFINITY;
    for (const [index, event] of events.entries()) {
      if (usedEvents.has(index)) continue;
      const distance = Math.abs(event.at - payload.at);
      if (distance <= matchWindowMs && distance < bestDistance) {
        bestIndex = index;
        bestDistance = distance;
      }
    }
    if (bestIndex === -1) continue;
    usedEvents.add(bestIndex);
    const event = events[bestIndex];
    matches.push({
      payload_at: payload.at_iso,
      event_at: event.at_iso,
      delta_ms: event.at - payload.at,
      provider_total_ms: payload.provider_total_ms,
      cleanup_stage_wall_ms: event.cleanup_stage_wall_ms,
    });
  }
  return {
    schema: "heardright.contextual_repair_cloud_yardstick.v1",
    model: "qwen/qwen3.6-27b",
    prompt_version: "l1_app_polish_prompt_v5",
    matching: { policy: "nearest unused successful cleanup event by absolute completion timestamp", window_ms: matchWindowMs },
    provider_total_ms: stats(payloads.map((row) => row.provider_total_ms)),
    cleanup_stage_wall_ms: stats(matches.map((row) => row.cleanup_stage_wall_ms)),
    frozen_samples: {
      provider_total_ms: payloads.map((row) => row.provider_total_ms),
      cleanup_stage_wall_ms: matches.map((row) => row.cleanup_stage_wall_ms),
    },
    matches,
  };
}

function valueAfter(flag, fallback = null) {
  const index = process.argv.indexOf(flag);
  return index === -1 ? fallback : process.argv[index + 1];
}

function readJsonl(file, limit = 0) {
  const lines = fs.readFileSync(file, "utf8").split(/\r?\n/).filter(Boolean);
  return (limit > 0 ? lines.slice(0, limit) : lines).map(JSON.parse);
}

function fileSha256(file) {
  return crypto.createHash("sha256").update(fs.readFileSync(file)).digest("hex");
}

function main() {
  const dataRoot = path.join(process.env.LOCALAPPDATA ?? path.join(os.homedir(), ".local", "share"), "HeardRightNext");
  const payloadFile = path.resolve(valueAfter("--payloads", path.join(dataRoot, "polish-payloads.jsonl")));
  const requestedEvents = valueAfter("--events", [path.join(dataRoot, "events.jsonl"), path.join(dataRoot, "events.jsonl.1")].filter(fs.existsSync).join(","));
  const eventFiles = requestedEvents.split(",").filter(Boolean).map((file) => path.resolve(file));
  const payloadRowLimit = Number(valueAfter("--payload-row-limit", "0"));
  const eventRowLimit = Number(valueAfter("--event-row-limit", "0"));
  const matchWindowMs = Number(valueAfter("--match-window-ms", "5000"));
  if (!fs.existsSync(payloadFile)) throw new Error(`payload log is missing: ${payloadFile}`);
  if (!eventFiles.length || eventFiles.some((file) => !fs.existsSync(file))) throw new Error("one or more event logs are missing");
  const payloadRows = readJsonl(payloadFile, payloadRowLimit);
  const eventRows = eventFiles.flatMap((file) => readJsonl(file, eventRowLimit));
  const result = computeCloudYardstick(payloadRows, eventRows, { matchWindowMs });
  result.created_at = new Date().toISOString();
  result.provenance = {
    calculator: path.resolve(scriptFile),
    calculator_sha256: fileSha256(scriptFile),
    payload_file: payloadFile,
    payload_file_sha256: fileSha256(payloadFile),
    payload_rows_read: payloadRows.length,
    payload_row_limit: payloadRowLimit || null,
    event_files: eventFiles.map((file) => ({ file, sha256: fileSha256(file) })),
    event_rows_read: eventRows.length,
    event_row_limit_per_file: eventRowLimit || null,
  };
  const output = valueAfter("--out");
  if (output) {
    const resolved = path.resolve(output);
    fs.mkdirSync(path.dirname(resolved), { recursive: true });
    fs.writeFileSync(resolved, `${JSON.stringify(result, null, 2)}\n`);
  }
  console.log(JSON.stringify(result, null, 2));
}

if (path.resolve(process.argv[1] ?? "") === path.resolve(scriptFile)) {
  try { main(); } catch (error) {
    console.error(error.stack ?? String(error));
    process.exitCode = 1;
  }
}
