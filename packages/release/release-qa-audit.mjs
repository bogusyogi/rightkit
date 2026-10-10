// Warn-and-record QA admission plus the ad hoc `right-release qa audit` scan.
//
// A missing or invalid QA receipt never blocks a release. The gates in release-qa-receipt.mjs still verify
// receipts exactly as before; this module turns a refusal into a loud stderr warning and a durable record
// (the sealed manifest `qa` field for the source stage, `state/<releaseId>/<platformDir>/qa-installed.json`
// for the installed stage). `qa audit` reads those records so a weekly run can flag released builds
// that lack passing QA.
import { existsSync, mkdirSync, readdirSync, readFileSync, writeFileSync } from "node:fs";
import path from "node:path";
import { spawnSync } from "./background-process.mjs";
import { assertInstalledQaAdmission, assertSourceQaAdmission, qaReceiptPath, readQaReceipt } from "./release-qa-receipt.mjs";

const platformDirOf = (platform) => (platform === "win" ? "windows" : platform);

/** "missing" when no receipt exists; "rejected" for anything else the gate refused (bad receipt, no key, wrong commit). */
function refusalStatus(error) {
  return /^no (?:source|installed) QA receipt/.test(error.message) ? "missing" : "rejected";
}
const refusalReason = (error) => String(error.message).split("\n  run:")[0];

export function formatQaWarning({ status, reason, command }) {
  return `[right-release] QA warning: ${reason}; release continues and is recorded as QA ${status} for audit.${command ? ` Run: ${command}` : ""}`;
}

function admit(check, { warn = (line) => console.error(line) } = {}) {
  try { return { status: "passed", result: check() }; }
  catch (error) {
    if (error?.code !== "QA_ADMISSION_REFUSED") throw error;
    const outcome = { status: refusalStatus(error), reason: refusalReason(error), command: error.command ?? null };
    warn(formatQaWarning(outcome));
    return outcome;
  }
}

/** Build stage. Returns the `qa` block sealed into the release manifest. */
export function admitSourceQa(opts, hooks) {
  const outcome = admit(() => assertSourceQaAdmission(opts), hooks);
  if (outcome.status === "passed") return { status: "passed", sourceReceiptDigest: outcome.result.digest, floorDigest: outcome.result.floorDigest };
  return { status: outcome.status, reason: outcome.reason };
}

export const installedQaRecordPath = ({ repoRoot, releaseId, platform }) =>
  path.join(repoRoot, ".right-release", "state", releaseId, platformDirOf(platform), "qa-installed.json");

/** Publication stage (upload, publish-update, github). Records the outcome durably and never blocks. */
export function admitInstalledQa(opts, { provider, now = () => new Date(), ...hooks } = {}) {
  const outcome = admit(() => assertInstalledQaAdmission(opts), hooks);
  const record = {
    status: outcome.status,
    ...(outcome.status === "passed" ? { receiptDigest: outcome.result.digest } : { reason: outcome.reason }),
    checkedAt: now().toISOString(),
    provider,
  };
  const file = installedQaRecordPath({ repoRoot: opts.repoRoot, releaseId: opts.manifest.releaseId, platform: opts.platform });
  try {
    mkdirSync(path.dirname(file), { recursive: true });
    writeFileSync(file, `${JSON.stringify(record, null, 2)}\n`);
  } catch (error) {
    (hooks.warn ?? ((line) => console.error(line)))(`[right-release] QA warning: could not record installed QA status at ${file} (${error.message}); release continues.`);
  }
  return record;
}

const readJson = (file) => { try { return JSON.parse(readFileSync(file, "utf8")); } catch { return null; } };
const subdirs = (dir) => (existsSync(dir) ? readdirSync(dir, { withFileTypes: true }).filter((entry) => entry.isDirectory()).map((entry) => entry.name) : []);

function sourceStatus(manifest) {
  const qa = manifest.qa;
  if (qa?.status === "passed" || (!qa?.status && qa?.sourceReceiptDigest)) return { status: "passed" };
  if (qa?.status) return { status: qa.status, ...(qa.reason ? { reason: qa.reason } : {}) };
  return { status: "missing", reason: "sealed manifest records no source QA" };
}

function installedStatus({ repoRoot, releaseId, platformDir }) {
  const stateDir = path.join(repoRoot, ".right-release", "state", releaseId, platformDir);
  const record = readJson(path.join(stateDir, "qa-installed.json"));
  const published = existsSync(stateDir) && readdirSync(stateDir).some((name) => /^verified-.*\.json$/.test(name));
  if (record) return { uploaded: true, status: record.status, ...(record.reason ? { reason: record.reason } : {}), ...(record.checkedAt ? { checkedAt: record.checkedAt } : {}) };
  if (!published) return { uploaded: false, status: "not-uploaded" };
  // Uploaded before outcomes were recorded: fall back to the receipt file the old gate required.
  const receipt = readQaReceipt(qaReceiptPath({ repoRoot, releaseId, platform: platformDir === "windows" ? "win" : platformDir, stage: "installed" }));
  if (receipt?.status === "passed") return { uploaded: true, status: "passed", reason: "legacy upload; receipt file present, not re-verified" };
  return { uploaded: true, status: "missing", reason: "uploaded with no recorded installed QA outcome" };
}

/** Scan the app repo's sealed manifests and state. Returns rows plus `ok` (false when any uploaded build lacks passing QA). */
export function auditQa({ repoRoot, since } = {}) {
  const sealedRoot = path.join(repoRoot, ".right-release", "sealed");
  const rows = [];
  for (const releaseId of subdirs(sealedRoot).sort()) {
    for (const platformDir of subdirs(path.join(sealedRoot, releaseId)).sort()) {
      const manifest = readJson(path.join(sealedRoot, releaseId, platformDir, "release-manifest.json"));
      if (!manifest) continue;
      if (since && String(manifest.sealedAt ?? "") < since) continue;
      const installed = installedStatus({ repoRoot, releaseId, platformDir });
      const source = sourceStatus(manifest);
      const { uploaded, ...installedRest } = installed;
      rows.push({
        releaseId, platform: platformDir, version: manifest.version ?? null, sealedAt: manifest.sealedAt ?? null, uploaded,
        source, installed: installedRest,
        ok: !uploaded || (source.status === "passed" && installedRest.status === "passed"),
      });
    }
  }
  return { repoRoot, since: since ?? null, rows, ok: rows.every((row) => row.ok) };
}

export function formatAuditTable(report) {
  if (!report.rows.length) return "right-release qa audit: no sealed releases found";
  const header = ["release", "platform", "source", "installed", "result"];
  const body = report.rows.map((row) => [row.releaseId, row.platform, row.source.status, row.installed.status, row.ok ? (row.uploaded ? "ok" : "ok (not uploaded)") : "FLAG"]);
  const widths = header.map((_, column) => Math.max(header[column].length, ...body.map((line) => line[column].length)));
  const line = (cells) => cells.map((cell, column) => cell.padEnd(widths[column])).join("  ").trimEnd();
  const flagged = report.rows.filter((row) => !row.ok);
  return [
    line(header), ...body.map(line),
    "",
    flagged.length
      ? `${flagged.length} uploaded build(s) lack passing QA:\n${flagged.map((row) => `  ${row.releaseId} ${row.platform}: ${[row.source.status !== "passed" && `source ${row.source.status}${row.source.reason ? ` (${row.source.reason})` : ""}`, row.installed.status !== "passed" && `installed ${row.installed.status}${row.installed.reason ? ` (${row.installed.reason})` : ""}`].filter(Boolean).join("; ")}`).join("\n")}`
      : "all uploaded builds have passing QA",
  ].join("\n");
}

function repoRootOf(dir) {
  const result = spawnSync("git", ["-C", dir, "rev-parse", "--show-toplevel"], { encoding: "utf8", windowsHide: true });
  const top = result.status === 0 ? String(result.stdout ?? "").trim() : "";
  return top && existsSync(path.join(top, ".right-release")) ? top : path.resolve(dir);
}

export function auditMain(args, { stdout = (text) => console.log(text), stderr = (text) => console.error(text) } = {}) {
  let app = process.cwd(), json = false, since;
  for (let index = 0; index < args.length; index += 1) {
    const arg = args[index];
    if (arg === "--app") app = args[++index];
    else if (arg === "--json") json = true;
    else if (arg === "--since") since = args[++index];
    else if (arg === "-h" || arg === "--help") { stdout("usage: right-release qa audit [--app <dir>] [--json] [--since <YYYY-MM-DD>]"); return 0; }
    else { stderr(`right-release qa audit: unknown argument: ${arg}`); return 2; }
  }
  if (!app) { stderr("right-release qa audit: --app requires a directory"); return 2; }
  if (since !== undefined && !/^\d{4}-\d{2}-\d{2}$/.test(since)) { stderr("right-release qa audit: --since must be YYYY-MM-DD"); return 2; }
  const report = auditQa({ repoRoot: repoRootOf(app), since });
  stdout(json ? JSON.stringify(report, null, 2) : formatAuditTable(report));
  return report.ok ? 0 : 1;
}
