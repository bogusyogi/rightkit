#!/usr/bin/env node
import { pathToFileURL } from "node:url";

export function parseAttemptedJsonl(text) {
  return String(text ?? "")
    .split(/\r?\n/)
    .filter((line) => line.length > 0)
    .map((line, index) => {
      try {
        return JSON.parse(line);
      } catch {
        throw new Error(`corrupt_jsonl_line_${index + 1}`);
      }
    });
}

export function validateResume({ manifestRows, attemptedRows }) {
  const manifestIds = new Set(manifestRows.map((row) => row.row_id));
  const seen = new Set();
  for (const row of attemptedRows) {
    if (!manifestIds.has(row.row_id)) return { ok: false, error: "row_not_in_manifest", row_id: row.row_id };
    if (seen.has(row.row_id)) return { ok: false, error: "duplicate_row", row_id: row.row_id };
    seen.add(row.row_id);
  }
  const next = manifestRows.find((row) => !seen.has(row.row_id)) ?? null;
  return { ok: true, next_row_id: next?.row_id ?? null };
}

function selfTest() {
  const manifestRows = [{ row_id: "a" }, { row_id: "b" }, { row_id: "c" }];
  if (validateResume({ manifestRows, attemptedRows: [{ row_id: "a" }] }).next_row_id !== "b") {
    throw new Error("next row self-test failed");
  }
  if (validateResume({ manifestRows, attemptedRows: [{ row_id: "x" }] }).error !== "row_not_in_manifest") {
    throw new Error("unknown row self-test failed");
  }
  if (validateResume({ manifestRows, attemptedRows: [{ row_id: "a" }, { row_id: "a" }] }).error !== "duplicate_row") {
    throw new Error("duplicate row self-test failed");
  }
  const parsed = parseAttemptedJsonl('{"row_id":"a"}\n{"row_id":"b"}\n');
  if (parsed.length !== 2 || parsed[1].row_id !== "b") {
    throw new Error("jsonl parse self-test failed");
  }
  try {
    parseAttemptedJsonl('{"row_id":"a"}\n{"row_id"');
    throw new Error("corrupt jsonl self-test failed");
  } catch (error) {
    if (!String(error.message).includes("corrupt_jsonl_line_2")) throw error;
  }
  console.log("l1 resume integrity self-test ok");
}

if (process.argv.includes("--self-test") && import.meta.url === pathToFileURL(process.argv[1]).href) selfTest();
