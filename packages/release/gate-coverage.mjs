// The per-scan coverage report of one gate run (rightkit#1463, ADR-001 section 8): every planned scan is listed as `hit` (answered from the
// result cache), `ran` (executed, pass or fail) or `skipped` (not run: deferred, not reached, or outside the run's classes), with the cache key
// it was looked up under and the reason. A cache that never hits is then visible in the report, not only in the timings.
import fs from "node:fs";
import path from "node:path";

export const COVERAGE_SCHEMA = "rightkit.gate-coverage/1";

const duration = (value) => typeof value === "number" && Number.isFinite(value) && value >= 0 ? value : null;
function timingOf(value) {
  if (!value || typeof value !== "object" || typeof value.invocationId !== "string" || !value.invocationId) return null;
  return { invocationId: value.invocationId, elapsedMs: duration(value.elapsedMs), cacheLookupMs: duration(value.cacheLookupMs), executionMs: duration(value.executionMs) };
}
function telemetryOf(row) {
  const attempts = Array.isArray(row?.batchAttempts) ? row.batchAttempts.filter((entry) => entry && typeof entry.id === "string" && typeof entry.status === "string").map((entry) => ({ id: entry.id, status: entry.status, timing: timingOf(entry.timing) })) : [];
  return { timing: timingOf(row?.timing), scanElapsedMs: duration(row?.scanElapsedMs), retryTiming: timingOf(row?.retried?.timing), batchAttempts: attempts };
}

/**
 * The report for `plan` (the planned scans, `plan.scans`) and `scanResult` (runScans' rows). `keys` is a Map of scan id -> the input key the cache
 * looked the scan up under (null or absent: the scan is never cached or no key could be formed). `classes` are the classes this pass runs.
 */
export function buildCoverageReport({ plan, scanResult, keys = new Map(), classes = ["A", "B"], commit = null, startedAt = null, endedAt = null }) {
  const rows = new Map((scanResult?.scans ?? []).map((row) => [row.id, row]));
  const scans = (plan?.scans ?? []).map((scan) => {
    const row = rows.get(scan.id);
    const key = keys.get(scan.id) ?? null;
    const telemetry = telemetryOf(row);
    if (row) {
      if (row.cached === true) return { id: scan.id, class: scan.class, ...telemetry, coverage: "hit", status: row.status, key, reason: row.detail ?? "answered from the result cache" };
      if (row.status === "pass" || row.status === "fail") return { id: scan.id, class: scan.class, ...telemetry, coverage: "ran", status: row.status, key, reason: key === null ? "no cache key (this scan is never cached, or no key could be formed)" : "no cached result for this key", ...(row.code ? { code: row.code } : {}) };
      return { id: scan.id, class: scan.class, ...telemetry, coverage: "skipped", status: row.status, key, reason: row.detail ?? row.status };
    }
    return { id: scan.id, class: scan.class, ...telemetry, coverage: "skipped", status: "not-in-pass", key, reason: classes.includes(scan.class) ? "planned but no result was returned" : `class ${scan.class} is not part of this pass (classes ${classes.join("+")})` };
  });
  const count = (what) => scans.filter((s) => s.coverage === what).length;
  return { schema: COVERAGE_SCHEMA, commit, startedAt, endedAt, totals: { planned: scans.length, hit: count("hit"), ran: count("ran"), skipped: count("skipped") }, scans };
}

/** Writes the report as `<dir>/<name>.json` (the directory is created); returns the path. */
export function writeCoverageReport({ dir, name, report, write = fs.writeFileSync, mkdir = fs.mkdirSync }) {
  mkdir(dir, { recursive: true });
  const file = path.join(dir, `${name}.json`);
  write(file, `${JSON.stringify(report, null, 2)}\n`);
  return file;
}

/** Wall-clock seconds as printed in a log line, one decimal. */
export const seconds = (ms) => `${(Math.max(0, ms) / 1000).toFixed(1)}s`;
