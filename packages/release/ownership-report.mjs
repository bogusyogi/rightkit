import { existsSync, readFileSync } from "node:fs";
import path from "node:path";
import { checkApp, EXCEPTIONS_FILE } from "./ownership.mjs";
import { DEFAULT_APPS } from "./standalone-clone-verify.mjs";
import { sha256 } from "./ownership-approval.mjs";

export const OWNERSHIP_APPS = Object.freeze([
  ...DEFAULT_APPS.map(({ key, appDir }) => ({ key, root: key === "coderight" ? key : path.posix.join(key, appDir === "." ? "" : appDir), scope: key === "coderight" ? "repository" : appDir })),
  { key: "cutright", root: "cutright" },
  { key: "genright", root: "genright" },
  { key: "rightsites", root: "rightsites" },
  { key: "voiceright", root: "voiceright" },
  { key: "membrane", root: "coderight/context/apps/membrane-hub", scope: "context/apps/membrane-hub" },
]);

const canonicalJson = (value) => JSON.stringify(value, (_key, item) => item && !Array.isArray(item) && typeof item === "object"
  ? Object.fromEntries(Object.keys(item).sort().map((key) => [key, item[key]])) : item);

function catalogVersion(manifest) {
  return String(manifest.catalogVersion ?? manifest.version ?? manifest.schema ?? "unknown");
}

export function publishedAvailability(capability, versions) {
  const npmPackage = capability.npmPackage ?? (capability.package?.startsWith("@rightkit/") ? capability.package : undefined);
  const cargoPackage = capability.cargoPackage ?? (capability.package?.startsWith("rightkit-") ? capability.package : undefined);
  const npmVersion = npmPackage && (versions.npm?.[npmPackage] ?? versions.stagedNpm?.[npmPackage]);
  const cargoVersion = cargoPackage && (versions.cargo?.[cargoPackage] ?? versions.stagedCargo?.[cargoPackage]);
  const swift = capability.package === "rightkit-swift" ? (versions.swift?.["rightkit-swift"] ?? versions.stagedSwift?.["rightkit-swift"]) : undefined;
  const published = [];
  if (npmVersion) published.push({ registry: "npm", package: npmPackage, version: npmVersion, state: versions.npm?.[npmPackage] ? "registry-pinned" : "staged-unverified" });
  if (cargoVersion) published.push({ registry: "crates.io", package: cargoPackage, version: cargoVersion, state: versions.cargo?.[cargoPackage] ? "registry-pinned" : "staged-unverified" });
  if (swift) published.push({ registry: "SwiftPM", package: "rightkit-swift", version: swift.version, url: swift.url, state: versions.swift?.["rightkit-swift"] ? "registry-pinned" : "staged-unverified" });
  return published;
}

function statusFor(row, report) {
  if (report.enrollmentPending && (row.status === "blocked" || row.status === "blocked-by-findings")) return "enrollment-pending";
  if (row.status === "blocked" || row.status === "blocked-by-findings") return "blocked";
  if (row.status === "excepted") return "excepted";
  if (row.status === "verified-integration") return "verified-integration";
  if (row.status === "declared-dependency" || row.uses === true) return "declared-dependency";
  if (report.coverage?.length) return "unscanned";
  return "not-detected";
}

function exceptionRequests(root, app, report) {
  const file = path.join(root, EXCEPTIONS_FILE);
  if (!existsSync(file)) return [];
  let records;
  try { records = JSON.parse(readFileSync(file, "utf8")).exceptions ?? []; } catch { return []; }
  const payload = (record) => record?.payload ?? record;
  const confirmed = new Set((report.covered ?? []).map((finding) => payload(finding.exception)?.requestId).filter(Boolean));
  return records.filter((record) => !confirmed.has(payload(record)?.requestId)).map((record) => {
    const details = payload(record);
    const request = { app, requestId: details.requestId ?? null, fingerprint: details.fingerprint ?? null, paths: details.paths ?? [], reason: details.reason ?? null, owner: details.owner ?? null, expires: details.expires ?? null, tracking: details.trackingRef ?? details.tracking ?? null, state: "request-pending-verification" };
    return { ...request, contentHash: sha256(canonicalJson(request)) };
  });
}

function asRecord(app, root, scope, report, manifest, versions, scannedAt) {
  const meta = report.metadata ?? {};
  const findings = (report.findings ?? []).map((f) => ({ fingerprint: f.fingerprint, capability: f.capability, file: f.file, line: f.line ?? 0, occurrenceHash: f.occurrenceHash ?? sha256(canonicalJson(f)) }));
  const files = [...new Set(findings.map((f) => f.file))].sort();
  const contentDigestSource = meta.contentDigests ? "scanner-inputs" : "finding-occurrences-only";
  const contentDigests = meta.contentDigests ?? findings.map((f) => ({ path: f.file, digest: f.occurrenceHash, kind: "finding-occurrence" }));
  const rows = (report.rows ?? []).map((row) => {
    const catalogEntry = manifest.capabilities.find((c) => c.capability === row.capability || c.package === row.owner);
    return { ...row, status: statusFor(row, report), available: catalogEntry ? publishedAvailability(catalogEntry, versions) : [] };
  });
  const enrollmentIsPending = report.enrollmentPending === true;
  const record = {
    app,
    scope: scope ?? ".",
    status: enrollmentIsPending ? "warning" : (report.violations?.length || report.problems?.length) ? "blocked" : report.coverage?.length ? "unscanned" : "scanned",
    enrollmentPending: enrollmentIsPending,
    repositoryRoot: path.resolve(root),
    repositoryIdentity: meta.repositoryIdentity ?? null,
    commit: meta.commit ?? null,
    host: meta.host ?? null,
    scannedAt: meta.scannedAt ?? scannedAt,
    scanAgeSeconds: Math.max(0, Math.floor((Date.parse(scannedAt) - Date.parse(meta.scannedAt ?? scannedAt)) / 1000)),
    catalogVersion: meta.catalogVersion ?? catalogVersion(manifest),
    scannerVersion: meta.scannerVersion ?? null,
    catalogDigest: meta.catalogDigest ?? sha256(canonicalJson(manifest)),
    contentDigestSource,
    contentDigests,
    scannedFilesWithFindings: files,
    coverage: report.coverage ?? [],
    rows,
    findings,
    violations: report.violations ?? [],
    covered: report.covered ?? [],
    problems: report.problems ?? [],
    warnings: report.warnings ?? [],
  };
  record.resultDigest = sha256(canonicalJson({ app: record.app, commit: record.commit, catalogDigest: record.catalogDigest, contentDigests: record.contentDigests, coverage: record.coverage, rows: record.rows, problems: record.problems, warnings: record.warnings, enrollmentPending: record.enrollmentPending }));
  record.exceptionRequests = exceptionRequests(root, app, report);
  return record;
}

export function buildOwnershipReport({ workspaceRoot, manifest, versions, apps = OWNERSHIP_APPS, now = new Date(), scanner = checkApp } = {}) {
  if (!workspaceRoot || !manifest?.capabilities || !versions) throw new TypeError("workspaceRoot, ownership manifest, and version registry are required");
  const scannedAt = now.toISOString();
  const records = apps.map(({ key, root: relativeRoot, scope }) => {
    const root = path.resolve(workspaceRoot, relativeRoot);
    if (!existsSync(root)) return { app: key, scope: scope ?? ".", repositoryRoot: root, status: "unscanned", reason: "configured app root is missing", catalogVersion: catalogVersion(manifest), catalogDigest: sha256(canonicalJson(manifest)), scannedAt, host: null, commit: null, scanAgeSeconds: 0, rows: [], findings: [], violations: [], covered: [], problems: [], coverage: [{ path: ".", status: "unscanned", reason: "configured app root is missing" }], contentDigests: [], resultDigest: sha256(`${key}:missing`) };
    try { return asRecord(key, root, scope, scanner(root, manifest), manifest, versions, scannedAt); }
    catch (error) { return { app: key, scope: scope ?? ".", repositoryRoot: root, status: "unscanned", reason: error.message, catalogVersion: catalogVersion(manifest), catalogDigest: sha256(canonicalJson(manifest)), scannedAt, host: null, commit: null, scanAgeSeconds: 0, rows: [], findings: [], violations: [], covered: [], problems: [error.message], coverage: [{ path: ".", status: "unscanned", reason: error.message }], contentDigests: [], resultDigest: sha256(`${key}:${error.message}`) }; }
  });
  const blockMap = new Map(), requestMap = new Map(), warnings = [];
  for (const record of records) {
    for (const request of record.exceptionRequests ?? []) if (!requestMap.has(request.contentHash)) requestMap.set(request.contentHash, request);
    if (record.enrollmentPending) {
      warnings.push({ app: record.app, message: "approval enrollment pending; ownership findings remain backlog diagnostics", findings: record.violations, problems: record.problems, scannerWarnings: record.warnings });
      continue;
    }
    for (const item of [...record.violations, ...record.problems.map((message) => ({ kind: "scanner-problem", message }))]) {
      const normalized = { app: record.app, ...item };
      const contentHash = item.occurrenceHash ?? sha256(canonicalJson(item));
      const existing = blockMap.get(contentHash);
      if (existing) existing.apps = [...new Set([...existing.apps, record.app])].sort();
      else blockMap.set(contentHash, { ...normalized, apps: [record.app], contentHash });
    }
  }
  return { schema: 1, generatedAt: scannedAt, catalogVersion: catalogVersion(manifest), catalogDigest: sha256(canonicalJson(manifest)), hostCoverage: [...new Set(records.map((x) => x.host).filter(Boolean))], apps: records, blocks: [...blockMap.values()], warnings, exceptionRequests: [...requestMap.values()] };
}

export function renderOwnershipMarkdown(report) {
  const lines = ["# RightKit ownership report", "", `Generated: ${report.generatedAt} · Catalog: ${report.catalogVersion} · Digest: ${report.catalogDigest}`, `Host coverage: ${report.hostCoverage.join(", ") || "none"}`, "", "| App | Scan scope | Status | Commit | Host | Scan age | Findings | Coverage gaps |", "|---|---|---|---|---|---:|---:|---:|"];
  for (const app of report.apps) {
    const states = new Set(app.rows.map((row) => row.status));
    const state = app.status ?? (states.has("blocked") ? "blocked" : states.has("unscanned") || app.coverage.length ? "unscanned" : "scanned");
    lines.push(`| ${app.app} | ${app.scope ?? "."} | ${state} | ${app.commit ?? "unknown"} | ${app.host ?? "unknown"} | ${app.scanAgeSeconds}s | ${app.findings.length} | ${app.coverage.length} |`);
  }
  lines.push("", "## Capability status", "", "| App | Capability | Status | Published availability |", "|---|---|---|---|");
  for (const app of report.apps) for (const row of app.rows) lines.push(`| ${app.app} | ${row.capability} | ${row.status} | ${row.available.map((x) => `${x.package}@${x.version} (${x.state})`).join(", ") || "none recorded"} |`);
  lines.push("", `Blocks: ${report.blocks.length} · Exception requests awaiting verification: ${report.exceptionRequests.length}`, "", "Exception requests are unverified records; only scanner-validated signed exceptions are reported as excepted.", "");
  if (report.warnings?.length) {
    lines.splice(-2, 0, "## Warnings", "", ...report.warnings.map((warning) => `- ${warning.app}: WARNING: ${warning.message}; ${warning.findings.length} finding(s), ${warning.problems.length} problem(s) retained as diagnostics`), "");
  }
  return lines.join("\n");
}

export function formatOwnershipJson(report) { return `${JSON.stringify(report, null, 2)}\n`; }
