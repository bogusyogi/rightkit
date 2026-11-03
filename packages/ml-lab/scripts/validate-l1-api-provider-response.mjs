#!/usr/bin/env node
import { pathToFileURL } from "node:url";

const required = [
  "schema",
  "run_id",
  "row_id",
  "provider",
  "model",
  "prompt_version",
  "asr_path",
  "status",
  "error_type",
  "request_started_at",
  "response_received_at",
  "provider_added_ms",
  "retry_count",
  "output_text",
  "malformed",
  "provider_api_family",
  "request_settings_hash",
  "response_schema_version",
];

export function validateResult(row) {
  for (const field of required) {
    if (!(field in row)) return { ok: false, error_type: "schema_missing_field", field };
  }
  if (row.schema !== "heardright.l1_api_result.v1") return { ok: false, error_type: "schema_version" };
  if (row.status === "rate_limited" || row.error_type === "rate_limited_429") return { ok: true, availability_failure: true };
  if (row.status === "auth_failed") return { ok: true, availability_failure: true, error_type: "availability_auth_failed" };
  if (row.status === "billing_required") return { ok: true, availability_failure: true };
  if (row.status === "timeout") return { ok: true, availability_failure: true, error_type: "availability_timeout" };
  if (row.status === "queue_or_capacity") return { ok: true, availability_failure: true, error_type: "availability_quota_or_capacity" };
  if (row.malformed === true) return { ok: false, error_type: "malformed" };
  if (row.status === "ok" && !String(row.output_text ?? "").trim()) return { ok: false, error_type: "empty_output" };
  if (!Number.isFinite(row.provider_added_ms)) return { ok: false, error_type: "latency_missing" };
  return { ok: true, availability_failure: row.status !== "ok" };
}

function base(overrides = {}) {
  return {
    schema: "heardright.l1_api_result.v1",
    run_id: "run_test",
    row_id: "row_test",
    provider: "groq",
    model: "llama-3.1-8b-instant",
    prompt_version: "asrllm_l1_context_v2",
    asr_path: "no_buffer",
    status: "ok",
    error_type: null,
    request_started_at: "2026-06-25T00:00:00.000Z",
    response_received_at: "2026-06-25T00:00:00.100Z",
    provider_added_ms: 100,
    retry_count: 0,
    output_text: "hello",
    malformed: false,
    provider_api_family: "groq",
    request_settings_hash: "abc",
    response_schema_version: "chat.completions.v1",
    ...overrides,
  };
}

function assert(condition, message) {
  if (!condition) throw new Error(message);
}

function selfTest() {
  assert(validateResult(base()).ok, "valid row rejected");
  assert(validateResult(base({ status: "rate_limited", error_type: "rate_limited_429" })).availability_failure, "429 not availability failure");
  assert(validateResult(base({ status: "auth_failed" })).error_type === "availability_auth_failed", "auth failure not typed");
  assert(validateResult(base({ status: "timeout" })).error_type === "availability_timeout", "timeout not typed");
  assert(validateResult(base({ status: "queue_or_capacity" })).error_type === "availability_quota_or_capacity", "capacity not typed");
  assert(validateResult(base({ status: "billing_required" })).availability_failure, "billing not availability failure");
  assert(validateResult(base({ output_text: "" })).error_type === "empty_output", "empty output accepted");
  assert(validateResult(base({ schema: "provider.v2" })).error_type === "schema_version", "schema drift accepted");
  assert(validateResult(base({ malformed: true })).error_type === "malformed", "malformed row accepted");
  assert(validateResult(base({ provider_added_ms: null })).error_type === "latency_missing", "missing latency accepted");
  const { response_schema_version: _omitted, ...missing } = base();
  assert(validateResult(missing).error_type === "schema_missing_field", "missing field accepted");
  console.log("l1 api provider response self-test ok");
}

if (process.argv.includes("--self-test") && import.meta.url === pathToFileURL(process.argv[1]).href) selfTest();
