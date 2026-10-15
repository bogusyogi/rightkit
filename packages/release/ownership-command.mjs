#!/usr/bin/env node
// right-release ownership: check an app against RightKit's capability ownership, or rebuild
// the shipped manifest from each package's declaration.
//
//   right-release ownership [--app <dir>] [--json]
//   right-release ownership [--app <dir>] --audit-json   (Legion governance.capability-ownership provider)
//   right-release ownership trust-compare [--json]   (print this host's protected-trust digest; policy-bundle drift is a warning, never a block)
//   right-release ownership trust-sync               (retired: prints why and exits 0)
//   right-release ownership --write-manifest [--rightkit <rightkit>] [--approve-new-capabilities]
// --approve-new-capabilities permits new ids during local generation only.
// Existing owner changes are always refused; installation needs protected authorization.
import { realpathSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { SCANNER_VERSION, formatTable } from "./ownership.mjs";
import { writeOwnershipCatalog } from "./ownership-catalog.mjs";
import { createOwnershipEvidence } from "./ownership-evidence.mjs";
import { spawnSync } from "./background-process.mjs";
import { main as stagedMain } from "./ownership-staged.mjs";
import { loadTrustedExternal } from "./ownership-approval.mjs";
import { diffPolicyBundles, runningPolicyBundle, trustComparison } from "./ownership-policy-bundle.mjs";
import { POLICY_BUNDLE_DRIFT_WARNING, TRUST_SYNC_RETIRED } from "./ownership-approval.mjs";
import { isBlockingFinding } from "./ownership-verdict.mjs";

const packageRoot = path.dirname(fileURLToPath(import.meta.url));

function flag(args, name) {
  const i = args.indexOf(name);
  return i >= 0 ? args[i + 1] : undefined;
}

/** Semantic drift of the bundle pinned in protected trust against the bundle this checkout runs; null when none is pinned or they match. */
export function policyBundleDrift(trust, { runningBundle = () => runningPolicyBundle({ scannerVersion: SCANNER_VERSION }) } = {}) {
  const pinned = trust?.policyBundle;
  if (!pinned?.digest) return null;
  let running;
  try { running = runningBundle(); } catch (error) { return { warning: `policy bundle could not be computed: ${error.message}`, diff: null }; }
  if (running.digest === pinned.digest) return null;
  return { warning: POLICY_BUNDLE_DRIFT_WARNING, pinnedDigest: pinned.digest, runningDigest: running.digest, diff: diffPolicyBundles(pinned, running) };
}

function driftLines(drift) {
  const lines = [`WARNING: ${drift.warning}`];
  if (drift.diff) {
    const { filesAdded, filesRemoved, filesChanged, scannerVersion, catalogChanged, registryChanged, allowlistChanged, qaFloorChanged } = drift.diff;
    if (scannerVersion) lines.push(`  scanner version: ${scannerVersion.from} -> ${scannerVersion.to}`);
    for (const [label, list] of [["files added", filesAdded], ["files removed", filesRemoved], ["files changed", filesChanged]]) if (list.length) lines.push(`  ${label}: ${list.join(", ")}`);
    const moved = Object.entries({ catalog: catalogChanged, registry: registryChanged, allowlist: allowlistChanged, "qa floor": qaFloorChanged }).filter(([, changed]) => changed).map(([name]) => name);
    if (moved.length) lines.push(`  changed digests: ${moved.join(", ")}`);
  }
  return lines;
}

/** Print this host's protected-trust digest. The Mac's digest is the reference; see docs/OWNERSHIP-ENFORCEMENT.md. */
export function trustCompare(args, { print, trustLoader = loadTrustedExternal, runningBundle } = {}) {
  let state;
  try { state = trustLoader(); } catch (error) { print(`[right-release] trust-compare: protected trust unreadable (${error.message})`); return 1; }
  if (!state?.trust) { print("[right-release] trust-compare: no protected trust on this host (not enrolled)"); return 1; }
  const { digest, components } = trustComparison(state.trust);
  const drift = policyBundleDrift(state.trust, runningBundle ? { runningBundle } : undefined);
  if (args.includes("--json")) print(JSON.stringify({ platform: process.platform, digest, components, ...(drift ? { warnings: [drift] } : {}) }, null, 2));
  else {
    print(`trust digest (${process.platform}): ${digest}`);
    for (const [name, value] of Object.entries(components)) print(`  ${name}: ${typeof value === "object" && value !== null ? JSON.stringify(value) : value ?? "none"}`);
    if (drift) for (const line of driftLines(drift)) print(line);
  }
  return 0;
}

export async function main(args, { evidenceFactory = createOwnershipEvidence, gitRunner = spawnSync, print = (value) => console.log(value), stderr = (line) => console.error(line), trustLoader = loadTrustedExternal } = {}) {
  if (args.includes("--help")) {
    print("right-release ownership [--app <dir>] [--json]\nright-release ownership --staged [--app <dir>] [--json]  (pre-commit gate: scans the index, not the working tree)\nright-release ownership --write-manifest [--rightkit <dir>] [--approve-new-capabilities]\n--approve-new-capabilities permits new capability ids during local generation only. Existing owner conflicts are always refused. Protected installation authorization remains separate.");
    return 0;
  }
  if (args[0] === "trust-compare") return trustCompare(args, { print, trustLoader });
  if (args.includes("--staged")) return stagedMain(args, { print });
  if (args[0] === "trust-sync") { print(`[right-release] ${TRUST_SYNC_RETIRED}`); return 0; }
  if (["enroll", "approve-key", "revoke-key"].includes(args[0])) {
    const { main: keyMain } = await import("./ownership-key-command.mjs");
    return keyMain(args, { runner: gitRunner, print });
  }
  if (args.includes("--write-manifest")) {
    const root = path.resolve(flag(args, "--rightkit") ?? path.join(packageRoot, "..", ".."));
    const manifest = writeOwnershipCatalog(root, { approveNewCapabilities: args.includes("--approve-new-capabilities") });
    const { syncConsumerCopies } = await import("./rightkit-sync.mjs");
    syncConsumerCopies(root);
    print(`[right-release] ownership manifest: ${manifest.capabilities.length} owner rows -> ${path.join(root, "packages/release/rightkit-ownership.json")}`);
    return 0;
  }
  const app = path.resolve(flag(args, "--app") ?? process.cwd());
  const top = gitRunner("git", ["-C", app, "rev-parse", "--show-toplevel"], { encoding: "utf8", windowsHide: true });
  if (top.status !== 0) return refuse(args, { type: "repository", message: "could not resolve --app to a Git repository" }, { print, stderr });
  const repoRoot = path.resolve(String(top.stdout).trim());
  let checked;
  try { checked = evidenceFactory(repoRoot, app); }
  catch (error) {
    // A refusal from the protected checkout carries its check name and offending paths.
    const blocked = error?.blocked ? [blockedCheck(error.blocked.check, error.message, error.blocked.paths)] : undefined;
    return refuse(args, { type: "protected-evidence", message: error.message, blocked }, { print, stderr });
  }
  const result = checked.result;
  if (args.includes("--audit-json")) return printAuditJson(result, print);
  // A refusal names every blocking check on stderr and in the JSON `blocked` field. Advise-tier findings never refuse.
  const blocked = checked.enrollmentPending || checked.ok ? undefined : blockingChecks(checked);
  if (blocked) reportBlocked(blocked, stderr);
  if (args.includes("--json")) {
    const error = checked.enrollmentPending ? undefined : !checked.floor.ok
      ? { type: "ownership-floor", message: checked.floor.problems.join("; ") }
      : result.coverage.length
        ? { type: "ownership-coverage", message: `${result.coverage.length} ownership coverage gap(s)` }
        : undefined;
    print(JSON.stringify({ ok: checked.ok, ...(checked.enrollmentPending ? { enrollmentPending: true, warning: checked.warning } : {}), ...(error ? { error } : {}), ...(blocked ? { blocked } : {}), rows: result.rows, violations: result.violations, covered: result.covered, problems: [...result.problems, ...checked.floor.problems], ...(checked.warnings ? { warnings: checked.warnings } : {}), coverage: result.coverage, metadata: result.metadata }, null, 2));
  } else {
    print(`RightKit ownership: ${path.basename(app)}`);
    print(formatTable(result.rows));
    for (const v of result.violations) {
      print(`  OWN COPY ${v.fingerprint} (${v.package}) ${v.file}${v.line ? `:${v.line}` : ""}  ${v.text}`);
    }
    for (const p of result.problems) print(`  ${checked.enrollmentPending ? "DIAGNOSTIC" : "EXCEPTION"} ${p}`);
    print(verdictLine(checked, result, blocked));
    for (const warning of checked.warnings ?? []) print(`  WARNING ${warning}`);
  }
  // CI lanes may pass `--advisory-when-pending`: before approval keys are enrolled the scan asserts
  // nothing, so findings print as diagnostics and the step exits 0. Release lanes never pass it.
  if (checked.enrollmentPending && args.includes("--advisory-when-pending")) return 0;
  return checked.ok ? 0 : 1;
}

// Audit provider output: every scan violation (block and advise tiers) as a repo-relative finding.
// Exit 0 whenever the scan completes; findings never change the exit code.
export function auditFindings(result) {
  return {
    schemaVersion: 1,
    findings: result.violations.map((v) => {
      const tier = v.tier ?? "block";
      const file = String(v.file ?? "").replaceAll("\\", "/");
      if (!file || file.startsWith("/") || /^[A-Za-z]:/.test(file) || file === ".." || file.startsWith("../") || file.split("/").includes("..")) throw new Error(`audit finding path is outside the audited root: ${v.file}`);
      const body = v.text ?? `${v.ecosystem ?? "dependency"} ${v.name ?? ""}`.trim();
      const finding = { rule: String(v.fingerprint), path: file };
      if (Number.isSafeInteger(v.line) && v.line > 0) finding.line = v.line;
      finding.message = `${tier === "advise" ? "advise: " : ""}${v.capability} duplicates RightKit owner ${v.package} (${tier}): ${body}`;
      return finding;
    }),
  };
}

function printAuditJson(result, print) {
  try {
    print(JSON.stringify(auditFindings(result)));
    return 0;
  } catch (error) {
    console.error(`[right-release] ownership audit failed: ${error.message}`);
    return 1;
  }
}

// Each refusal names its blocking check and the first offending paths. Stderr and the JSON `blocked` field carry the same list.
const BLOCKED_PATH_LIMIT = 10;

function blockedCheck(check, message, paths = []) {
  return { check, message, count: paths.length, paths: paths.slice(0, BLOCKED_PATH_LIMIT) };
}

// The checks that refuse a scan that did not pass. Advise-tier findings are never listed here.
function blockingChecks(checked) {
  const result = checked.result;
  const checks = [];
  const blocking = (result.violations ?? []).filter(isBlockingFinding);
  if (blocking.length) checks.push(blockedCheck("ownership-violation", `${blocking.length} block-tier finding(s) duplicate RightKit owners`, blocking.map((entry) => entry.file).filter(Boolean)));
  const floorProblems = checked.floor?.ok === false ? checked.floor.problems ?? [] : [];
  if (floorProblems.length) checks.push(blockedCheck("ownership-floor", floorProblems.join("; ")));
  const problems = (result.problems ?? []).filter((problem) => !floorProblems.includes(problem));
  if (problems.length) checks.push(blockedCheck("ownership-problem", problems.join("; ")));
  const gaps = result.coverage ?? [];
  if (gaps.length) checks.push(blockedCheck("ownership-coverage", `${gaps.length} coverage gap(s): ${[...new Set(gaps.map((gap) => gap.reason))].join(", ")}`, gaps.map((gap) => gap.path)));
  if (!checks.length) checks.push(blockedCheck("ownership-unknown", "scan reported not ok without a listed cause"));
  return checks;
}

function reportBlocked(checks, stderr) {
  for (const check of checks) {
    stderr(`[right-release] refused by check ${check.check}: ${check.message}`);
    for (const file of check.paths) stderr(`  - ${file}`);
    if (check.count > check.paths.length) stderr(`  ... and ${check.count - check.paths.length} more`);
  }
}

function tierCounts(violations) {
  const counts = { block: 0, advise: 0 };
  for (const violation of violations) counts[isBlockingFinding(violation) ? "block" : "advise"] += 1;
  return counts;
}

// The human verdict line. Advise-only results pass and say so; a refusal names its checks.
function verdictLine(checked, result, blocked) {
  if (checked.enrollmentPending) return `[right-release] ${checked.warning}`;
  if (checked.ok) {
    const counts = tierCounts(result.violations ?? []);
    return counts.advise
      ? `[right-release] ownership ADVISE: block ${counts.block}, advise ${counts.advise} (reported, not refused)`
      : "[right-release] ownership passed";
  }
  const hint = blocked.some((entry) => entry.check === "ownership-violation") ? "; move these to RightKit or add a reviewed, dated exception" : "";
  return `[right-release] ownership FAILED: ${blocked.map((entry) => `${entry.check}: ${entry.message}`).join("; ")}${hint}`;
}

// Refusal before any verdict: unresolved repository, protected evidence, or a dirty protected checkout.
function refuse(args, { type, message, blocked }, { print = (value) => console.log(value), stderr = (line) => console.error(line) } = {}) {
  const checks = blocked ?? [blockedCheck(type, message)];
  reportBlocked(checks, stderr);
  if (args.includes("--audit-json")) { stderr(`[right-release] ownership audit failed: ${message}`); return 1; }
  if (args.includes("--json")) print(JSON.stringify({ ok: false, error: { type, message }, blocked: checks, rows: [], violations: [], covered: [], problems: [message], coverage: [], metadata: null }, null, 2));
  else print(`[right-release] ownership FAILED: ${message}`);
  return 1;
}

// Compare real paths: a symlinked spelling of this file (pnpm links, macOS /var -> /private/var)
// must still run the command.
const isEntry = (() => {
  if (!process.argv[1]) return false;
  try { return realpathSync(process.argv[1]) === realpathSync(fileURLToPath(import.meta.url)); }
  catch { return path.resolve(process.argv[1]) === fileURLToPath(import.meta.url); }
})();
if (isEntry) {
  main(process.argv.slice(2)).then((code) => {
    process.exitCode = code;
  }).catch((error) => {
    console.error(`[right-release] ownership failed: ${error.message}`);
    process.exitCode = 1;
  });
}
