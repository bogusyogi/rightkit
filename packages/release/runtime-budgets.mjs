// Runtime budget gate for long-running shared crates (docs/RUNTIME-BUDGETS.md).
//
// Membership is fail-closed: every crate that reaches `right-release publish cargo` must carry an
// explicit declaration, either
//   * `runtime-budget.json` (a continuous crate: ceilings, status pointer, measurements with raw
//     bench evidence), or
//   * `[package.metadata.rightkit] continuous = false` plus a one-line `continuous_reason`.
// A crate with neither is refused. A crate declared `continuous = false` is refused when its non-test
// `src` matches a continuous-work pattern (CONTINUOUS_PATTERNS) that the reason does not name, so a
// false positive is a reviewed decision, never a silent one. The 10 crates in RUNTIME_BUDGET_CRATES
// can never declare `continuous = false`.
//
// runtime-budget.json (schema 1):
//   {
//     "schema": 1,
//     "crate": "rightkit-fsindex",
//     "hostPolicy": "darwin-only",            // optional; must agree with the release evidence hostPolicy
//     "hostPolicyReason": "no Windows backend",
//     "ceilings": { "residentMemoryBytes": N, "idleCpuPercent": N, "wakeupsPerSecond": N, "diskOpsPerSecond": N },  // null = undeclared, refused
//     "ceilingSources": { "residentMemoryBytes": "path:line" },
//     "ceilingBasis": { "wakeupsPerSecond": "delta-over-bare" },   // optional; see "Delta basis" below
//     "deltaCeilings": { "wakeupsPerSecond": N },                  // required for, and only for, a figure with a delta basis
//     "requiredScale": { "unit": "entries", "minCount": 1000000, "source": "path:line" },  // null = undeclared, refused
//     "status": { "api": "Index::status()", "file": "src/index.rs", "exposes": ["residentMemoryBytes", ...] },
//     "measurements": [ {
//       "host": { "os": "darwin", "arch": "arm64", "model": ".." }, "crateVersion": "0.2.0",
//       "commit": "<40-hex>", "date": "2026-10-11", "scope": "synthetic" | "real" | "installed",
//       "app": { "name": "..", "version": ".." },                      // installed scope only
//       "dataScale": { "description": "..", "unit": "entries", "count": 1000000 },
//       "command": "..", "observed": { "residentMemoryBytes": N, "idleCpuPercent": N, "wakeupsPerSecond": N, "diskOpsPerSecond": N },
//       "evidence": { "file": "runtime-budget.evidence/darwin-arm64-0.2.0-synthetic.txt", "sha256": "<64-hex>" }
//     } ]
//   }
// Measurements are written by `node runtime-budgets.mjs record`, never typed: the raw bench output is
// stored under runtime-budget.evidence/, and the gate re-parses it and compares every value.
//
// Delta basis. A whole-process wake-up rate on a desktop host is dominated by host and window-server
// activity the crate does not own. A crate may declare `ceilingBasis.wakeupsPerSecond = "delta-over-bare"`:
// its synthetic bench then measures the crate's workload and a bare runtime loop back to back (separate
// processes), and the gate compares (wake-ups - bare wake-ups), computed here from the two raw figures,
// to `deltaCeilings.wakeupsPerSecond`. A synthetic measurement of that crate must carry `baseline` and
// `delta` (`{ wakeupsPerSecond }`); one without a baseline is refused, and a reported delta that differs
// from raw minus bare is refused (the gate never trusts it). `ceilings.wakeupsPerSecond` stays the
// whole-process ceiling and keeps governing every other scope (real, installed) and app adoption, so an
// installed app is never compared to a delta ceiling and nothing weakens for apps. Measurements of
// other versions are history and are not re-gated, so they need no baseline.
//
// Bench output the `record` command accepts (exactly one such line in the file):
//   RESULT budget crate=<name> version=<semver> resident_bytes=<int> idle_cpu_percent=<n> wakeups_per_s=<n> disk_ops_per_s=<n> scale_unit=<word> scale_count=<int>
// optionally followed by `bare_wakeups_per_s=<n> wakeups_delta_per_s=<n>` (both or neither; delta may be negative).
//
// Check objects use the preflight shape ({ name, status, detail, fix }) so formatPreflight() in
// preflight.mjs prints them as `[FAIL] runtime-budget: ...`.

import crypto from "node:crypto";
import fs, { existsSync, readFileSync, realpathSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "./background-process.mjs";
import { parse } from "smol-toml";
import { singleHostOfPolicy } from "./publication-gate.mjs";
import { findWorkspaceRoot, lockClosure, pathDependencyDirs, readCargoPackage } from "./crate-graph.mjs";

export const RUNTIME_BUDGET_CHECK = "runtime-budget";
export const RUNTIME_BUDGET_CODE = "RUNTIME_BUDGET_MISSING";
export const RUNTIME_BUDGET_FILE = "runtime-budget.json";
export const RUNTIME_BUDGET_EVIDENCE_DIR = "runtime-budget.evidence";
export const RUNTIME_BUDGET_SCHEMA = 1;

export const RUNTIME_BUDGET_CRATES = Object.freeze([
  "rightkit-fsindex",
  "rightkit-file-watch",
  "rightkit-job-queue",
  "rightkit-daemon",
  "rightkit-cache",
  "rightkit-sync",
  "rightkit-logs",
  "rightkit-tauri-shell",
  "rightkit-qa",
  "rightkit-send",
]);

export const BUDGET_FIGURES = Object.freeze(["residentMemoryBytes", "idleCpuPercent", "wakeupsPerSecond", "diskOpsPerSecond"]);
const FIGURE_LABEL = Object.freeze({
  residentMemoryBytes: "resident memory",
  idleCpuPercent: "idle CPU",
  wakeupsPerSecond: "wake-ups",
  diskOpsPerSecond: "disk operations",
});
// The crate's own paths, and the paths of every workspace crate it depends on through `path`
// dependencies, whose change after a measurement makes the measurement stale (see measuredPathspecs).
export const CRATE_PATHS = Object.freeze(["src", "benches", "Cargo.toml", "build.rs"]);
export const DEPENDENCY_PATHS = Object.freeze(["src", "Cargo.toml", "build.rs"]);
// Same host rule as the publication gate (publication-gate.mjs REQUIRED_HOSTS).
const HOSTS = Object.freeze({ darwin: "macOS", win32: "Windows" });
const SHA = /^[0-9a-f]{40}$/;
const SHA256 = /^[0-9a-f]{64}$/;
const SCOPES = Object.freeze(["synthetic", "real", "installed"]);
// Figures that may be gated as a delta over a bare-runtime baseline, and the only scope that carries one.
export const CEILING_BASIS_DELTA = "delta-over-bare";
export const DELTA_FIGURES = Object.freeze(["wakeupsPerSecond"]);
const DELTA_SCOPE = "synthetic";
// Each rate is printed to 3 decimals, so raw - bare and the printed delta may differ by rounding only.
const DELTA_TOLERANCE = 0.002;
// A baseline more than this far above its own watchdog run is a mismatched or corrupt pair, not a quiet crate.
const DELTA_FLOOR = -2;

// Long-lived work a crate must declare. Each id must be named in `continuous_reason` for a `continuous = false`
// crate; for a held-resource crate every file that matches must be listed in `resources`.
export const CONTINUOUS_PATTERNS = Object.freeze({
  "thread-spawn": /\b(?:std::)?thread::(?:spawn|Builder|scope)\b|\brayon::(?:spawn|ThreadPoolBuilder)\b/,
  "task-spawn": /\b(?:tokio|async_std|smol)::(?:task::)?spawn(?:_blocking|_local)?\b|\btask::spawn(?:_blocking|_local)?\b|\bspawn_blocking\b|\bspawn_local\b|\bJoinSet\b/,
  "runtime-builder": /\btokio::runtime::Builder\b|\bRuntime::new\s*\(|\bBuilder::new_(?:multi|current)_thread\b/,
  sleep: /\b(?:thread::sleep|time::sleep|park_timeout)\s*\(|(?<![\w.:])sleep\s*\(/,
  timer: /\b(?:time::)?interval(?:_at)?\s*\(|\bTimer\b|\bDispatchSource\b|\bCFRunLoopTimer\w*|\bset_interval\b/,
  listener: /\b(?:TcpListener|UnixListener|UdpSocket|LocalSocketListener|NamedPipeServer|ServerOptions)\b|\.listen\s*\(/,
  watcher: /\b(?:RecommendedWatcher|FSEventStream\w*|ReadDirectoryChangesW|inotify|kqueue)\b|\bnotify::/,
  "event-loop": /\bkqueue\s*\(|\bepoll_(?:create1?|wait)\b|\bCreateIoCompletionPort\b|\bGetQueuedCompletionStatus(?:Ex)?\b|\bmio::(?:Poll|Events)\b|\bCFRunLoopRun\b/,
  "audio-stream": /\bcpal::|\bbuild_(?:input|output)_stream\b/,
  "onnx-session": /\bSession::builder\b|\bSessionBuilder\b|\bort::init\b|\bGlobalThreadPoolOptions\b|\bwith_(?:intra|inter)_threads\b|\bEnvironment::builder\b/,
  ggml: /\bggml_(?:init|backend_\w+|threadpool\w*|graph_compute|gallocr\w*)\b|\bthreadpool_(?:new|create)\b/,
  "conn-pool": /\br2d2\b|\bdeadpool\b|\bbb8\b|\bPooledConnection\b|\bConnectionPool\b|\bPool::builder\b|\bpool_max_idle_per_host\b|\bClient::builder\b|\breqwest::Client\b/,
  // A static holding a collection, lock, lazily built value or model: process-wide residency, for review.
  "resident-static": /\bstatic\s+(?:mut\s+)?\w+\s*:\s*[^=;]*\b(?:OnceLock|LazyLock|OnceCell|Lazy|HashMap|HashSet|BTreeMap|BTreeSet|Vec|VecDeque|Mutex|RwLock|Arc)\b|\blazy_static!|\bthread_local!/,
  // A child that outlives the caller by construction. Never behind a held value.
  "detached-child": /\bspawn_detached\b|\bDETACHED_PROCESS\b|\bprocess_group\s*\(\s*0\s*\)|\bsetsid\b/,
});

// Strongest pattern -> resource kind it requires a listed resource to hold.
const PATTERN_HOLDS = Object.freeze({ "thread-spawn": "thread", "task-spawn": "task", "runtime-builder": "task", listener: "listener" });
export const HELD_KINDS = Object.freeze(["thread", "task", "listener", "child", "memory"]);
export const CLASSES_FILE = "runtime-budget-classes.json";
export const APP_ADOPTION_FILE = "runtime-budget-adoption.json";

const isNumber = (v) => typeof v === "number" && Number.isFinite(v);
const nonEmpty = (v) => typeof v === "string" && v.trim() !== "";

/** Reads `[package.metadata.rightkit] runtime_budget` from Cargo.toml text. Undefined when unset. */
export function runtimeBudgetOptIn(manifestText) {
  const manifest = parse(manifestText);
  const value = manifest.package?.metadata?.rightkit?.runtime_budget;
  if (value !== undefined && typeof value !== "boolean") {
    throw new Error("[package.metadata.rightkit] runtime_budget must be true or false");
  }
  return value;
}

/** True when the crate is listed, or its manifest opts in with runtime_budget = true. */
export function isRuntimeBudgetCrate(crate, manifestText) {
  if (RUNTIME_BUDGET_CRATES.includes(crate)) return true;
  return manifestText !== undefined && runtimeBudgetOptIn(manifestText) === true;
}

/** Hosts the budget must be measured on. `evidence` (release evidence) is the policy authority when given. */
export function requiredBudgetHosts(budget, evidence) {
  const single = singleHostOfPolicy(evidence ?? budget);
  return single ? [single] : ["darwin", "win32"];
}

// ---------------------------------------------------------------- README view (generated from the JSON)

function sectionBounds(lines) {
  let fenced = false;
  let start = -1;
  let end = lines.length;
  for (let index = 0; index < lines.length; index += 1) {
    const line = lines[index];
    if (/^\s*(```|~~~)/.test(line)) { fenced = !fenced; continue; }
    if (fenced) continue;
    if (start < 0) {
      if (/^##\s+Runtime budget\s*$/.test(line)) start = index;
    } else if (/^#{1,2}\s/.test(line)) {
      end = index;
      break;
    }
  }
  return start < 0 ? null : { start, end };
}

/**
 * Lines of the `## Runtime budget` section (exact level-2 heading), ending at the next `#` or `##`
 * heading. `###` subsections stay inside. Headings inside fenced code blocks are ignored.
 * Returns null when the section is absent.
 */
export function runtimeBudgetSection(readmeText) {
  const lines = String(readmeText).split(/\r?\n/);
  const bounds = sectionBounds(lines);
  return bounds ? lines.slice(bounds.start + 1, bounds.end) : null;
}

const mib = (bytes) => `${(bytes / 1048576).toFixed(1)} MiB`;
const count = (n) => Number(n).toLocaleString("en-US");
const FORMAT = Object.freeze({
  residentMemoryBytes: mib,
  idleCpuPercent: (n) => `${n}%`,
  wakeupsPerSecond: (n) => `${n} per second`,
  diskOpsPerSecond: (n) => `${n} operations per second`,
});
const figure = (key, value) => (typeof value === "number" && Number.isFinite(value) ? FORMAT[key](value) : "undeclared");

/** The README section body generated from a parsed runtime-budget.json (no heading line). */
export function renderRuntimeBudgetBody(budget) {
  const out = [];
  out.push("Generated from `runtime-budget.json`; edit that file, then run `node packages/release/runtime-budgets.mjs sync <crate-dir>`.");
  out.push("");
  out.push(`- Resident memory ceiling: ${figure("residentMemoryBytes", budget.ceilings?.residentMemoryBytes)}.`);
  out.push(`- CPU ceiling (idle, steady state): ${figure("idleCpuPercent", budget.ceilings?.idleCpuPercent)}.`);
  if (budget.ceilingBasis?.wakeupsPerSecond === CEILING_BASIS_DELTA) {
    out.push(`- Wake-ups ceiling (whole process; installed and real measurements): ${figure("wakeupsPerSecond", budget.ceilings?.wakeupsPerSecond)}.`);
    out.push(`- Wake-ups gate (synthetic bench): at most ${figure("wakeupsPerSecond", budget.deltaCeilings?.wakeupsPerSecond)} over a bare runtime loop measured in the same bench run.`);
  } else out.push(`- Wake-ups ceiling: ${figure("wakeupsPerSecond", budget.ceilings?.wakeupsPerSecond)}.`);
  out.push(`- Disk operations ceiling: ${figure("diskOpsPerSecond", budget.ceilings?.diskOpsPerSecond)}.`);
  const rs = budget.requiredScale;
  out.push(rs && rs.unit && Number.isFinite(rs.minCount) ? `- Required scale: ${count(rs.minCount)} ${rs.unit} or more.` : "- Required scale: undeclared.");
  const hosts = requiredBudgetHosts(budget).map((h) => HOSTS[h]);
  out.push(`- Required hosts: ${hosts.join(" and ")}${hosts.length === 1 ? ` only (${String(budget.hostPolicyReason).trim()})` : ""}.`);
  const status = budget.status;
  out.push(status?.api
    ? `- Status exposure: \`${status.api}\` in \`${status.file}\`; exposes ${(status.exposes ?? []).map((k) => FIGURE_LABEL[k] ?? k).join(", ") || "none of the budget figures"}.`
    : "- Status exposure: none recorded.");
  const records = Array.isArray(budget.measurements) ? budget.measurements : [];
  if (records.length === 0) out.push("- Measured: not yet measured.");
  for (const m of records) {
    const host = `${m.host?.os}/${m.host?.arch}${m.host?.model ? ` (${m.host.model})` : ""}`;
    const scale = `${count(m.dataScale?.count)} ${m.dataScale?.unit} (${m.dataScale?.description})`;
    const app = m.scope === "installed" && m.app ? `, app ${m.app.name} ${m.app.version}` : "";
    const observed = BUDGET_FIGURES.map((k) => `${FIGURE_LABEL[k]} ${m.observed?.[k] === undefined || m.observed?.[k] === null ? "n/a (not measurable on this host)" : FORMAT[k](m.observed[k])}`).join(", ");
    const bare = isNumber(m.baseline?.wakeupsPerSecond) && isNumber(m.delta?.wakeupsPerSecond)
      ? `; bare baseline wake-ups ${FORMAT.wakeupsPerSecond(m.baseline.wakeupsPerSecond)}, delta ${FORMAT.wakeupsPerSecond(m.delta.wakeupsPerSecond)}` : "";
    out.push(`- Measured: ${host}, ${budget.crate} ${m.crateVersion} at ${String(m.commit).slice(0, 12)}, ${scale}, ${m.scope}${app}, \`${m.command}\`, ${m.date}, evidence ${String(m.evidence?.sha256 ?? "none").slice(0, 12)}: ${observed}${bare}.`);
  }
  return out;
}

const normalizeLines = (lines) => lines.map((line) => line.trim()).filter(Boolean);

/** README text with its `## Runtime budget` section replaced (or appended) from the JSON. */
export function syncRuntimeBudgetReadme(readmeText, budget) {
  const lines = String(readmeText).split(/\r?\n/);
  const block = ["## Runtime budget", "", ...renderRuntimeBudgetBody(budget), ""];
  const bounds = sectionBounds(lines);
  if (!bounds) {
    const trimmed = String(readmeText).replace(/\s+$/, "");
    return `${trimmed}${trimmed ? "\n\n" : ""}${block.join("\n")}`;
  }
  return [...lines.slice(0, bounds.start), ...block, ...lines.slice(bounds.end)].join("\n");
}

// ---------------------------------------------------------------- freshness (git)

function defaultGit(args, cwd) {
  const result = spawnSync("git", args, { cwd, encoding: "utf8", windowsHide: true, maxBuffer: 256 * 1024 * 1024 });
  if (result.error) return { status: 1, stdout: "" };
  return { status: result.status ?? 1, stdout: (result.stdout ?? "").trim() };
}

const posix = (p) => p.split(path.sep).join("/");

/**
 * The git pathspecs (relative to the workspace root) whose change makes a measurement stale: the
 * crate's src, benches, Cargo.toml (features) and build.rs, plus src, Cargo.toml and build.rs of every
 * workspace crate it reaches through `path` dependencies (transitively). Returns { root, specs }.
 */
export function measuredPathspecs(crateDir, deps = {}) {
  const root = findWorkspaceRoot(crateDir, deps) ?? path.resolve(crateDir);
  const specs = [];
  const add = (dir, names) => { for (const name of names) specs.push(posix(path.relative(root, path.join(dir, name)))); };
  add(path.resolve(crateDir), CRATE_PATHS);
  let dependencies = [];
  try { dependencies = pathDependencyDirs(crateDir, deps); } catch { /* unreadable manifest: the crate paths alone */ }
  for (const dir of dependencies) add(dir, DEPENDENCY_PATHS);
  return { root, specs: [...new Set(specs)] };
}

/**
 * Why a measurement no longer describes the crate, or null when it does. Fails closed on any git error.
 *
 * Stale when, against the measured commit: any measured path changed in the working tree (commits since
 * and uncommitted edits alike), a new untracked file appeared under one, the measured commit is not in
 * this checkout (a too-shallow clone) or not an ancestor of HEAD, or the Cargo.lock closure of the
 * crate changed. "In the graph" is exact: the closure starts at the crate's [[package]] entry and
 * follows every `dependencies` edge, so a lock change to a package the crate (transitively) uses
 * counts and a change to an unrelated package does not.
 */
export function measurementStaleness(commit, crateDir, git = defaultGit, deps = {}) {
  const { exists = existsSync, readFile = readFileSync } = deps;
  if (!SHA.test(String(commit ?? ""))) return "commit is not a full 40-character SHA";
  const known = git(["cat-file", "-t", commit], crateDir);
  if (known.status !== 0 || known.stdout !== "commit") return `commit ${commit.slice(0, 12)} is not a commit in this repository (shallow clone?)`;
  if (git(["merge-base", "--is-ancestor", commit, "HEAD"], crateDir).status !== 0) return `commit ${commit.slice(0, 12)} is not an ancestor of HEAD`;
  const { root, specs } = measuredPathspecs(crateDir, { exists, readFile });
  // `git diff <commit>` (no second revision) compares the working tree to the commit.
  const changed = git(["diff", "--name-only", commit, "--", ...specs], root);
  if (changed.status !== 0) return "cannot diff the crate against the measured commit";
  if (changed.stdout !== "") return `crate or workspace dependency changed since commit ${commit.slice(0, 12)}: ${changed.stdout.split("\n").slice(0, 5).join(", ")}`;
  const untracked = git(["ls-files", "--others", "--exclude-standard", "--", ...specs], root);
  if (untracked.status !== 0) return "cannot list untracked crate files";
  if (untracked.stdout !== "") return `untracked files since the measurement: ${untracked.stdout.split("\n").slice(0, 5).join(", ")}`;
  let name = null;
  try { name = readCargoPackage(crateDir, { exists, readFile }).name; } catch { /* no manifest: nothing to resolve in the lock */ }
  if (name) {
    const lockPath = path.join(root, "Cargo.lock");
    const now = exists(lockPath) ? readFile(lockPath, "utf8") : null;
    const then = git(["show", `${commit}:./Cargo.lock`], root);
    const had = then.status === 0;
    if (now === null && had) return "Cargo.lock was removed since the measurement";
    if (now !== null && !had) return "Cargo.lock did not exist at the measured commit";
    if (now !== null) {
      try {
        if (JSON.stringify(lockClosure(now, name)) !== JSON.stringify(lockClosure(then.stdout, name))) {
          return `Cargo.lock changed in the dependency graph of ${name} since commit ${commit.slice(0, 12)}`;
        }
      } catch (error) { return `cannot compare Cargo.lock: ${error.message}`; }
    }
  }
  return null;
}

// ---------------------------------------------------------------- bench evidence

// Figures a host cannot measure by design. macOS has no per-process disk-operation count (rightkit-child-process
// usage.rs: ru_inblock/ru_oublock stay 0 there), so disk_ops_per_s may be "unavailable" on darwin only.
export const HOST_UNAVAILABLE_FIGURES = Object.freeze({ darwin: Object.freeze(["diskOpsPerSecond"]) });
export const figureUnavailableOn = (os, key) => (HOST_UNAVAILABLE_FIGURES[os] ?? []).includes(key);
const RESULT_NUMBERS = Object.freeze({ resident_bytes: "residentMemoryBytes", idle_cpu_percent: "idleCpuPercent", wakeups_per_s: "wakeupsPerSecond", disk_ops_per_s: "diskOpsPerSecond", scale_count: "scaleCount" });
const RESULT_WORDS = Object.freeze(["crate", "version", "scale_unit"]);
// Optional pair emitted by a delta-basis bench: the bare-runtime wake-up rate and the bench's own reported delta.
const RESULT_DELTA = Object.freeze({ bare_wakeups_per_s: "bareWakeupsPerSecond", wakeups_delta_per_s: "wakeupsDeltaPerSecond" });

/** Parses the one `RESULT budget ...` line of a bench's raw output. */
export function parseBudgetResult(text) {
  const lines = String(text).split(/\r?\n/).filter((line) => /^RESULT budget(?:\s|$)/.test(line));
  if (lines.length !== 1) return { ok: false, problems: [`expected exactly one "RESULT budget" line, found ${lines.length}`] };
  const fields = {};
  const problems = [];
  for (const token of lines[0].replace(/^RESULT budget\s*/, "").split(/\s+/).filter(Boolean)) {
    const eq = token.indexOf("=");
    const key = eq > 0 ? token.slice(0, eq) : token;
    const value = eq > 0 ? token.slice(eq + 1) : "";
    if (!(key in RESULT_NUMBERS) && !(key in RESULT_DELTA) && !RESULT_WORDS.includes(key)) problems.push(`unknown field ${key}`);
    else if (key in fields) problems.push(`duplicate field ${key}`);
    else fields[key] = value;
  }
  const value = {};
  for (const key of RESULT_WORDS) { if (!nonEmpty(fields[key])) problems.push(`missing field ${key}`); else value[key] = fields[key]; }
  for (const [key, name] of Object.entries(RESULT_NUMBERS)) {
    const n = Number(fields[key]);
    if (fields[key] === "unavailable" && name !== "residentMemoryBytes" && name !== "scaleCount") value[name] = null;
    else if (fields[key] === undefined || fields[key] === "" || !Number.isFinite(n) || n < 0) problems.push(`field ${key} must be a non-negative number`);
    else value[name] = n;
  }
  const present = Object.keys(RESULT_DELTA).filter((key) => key in fields);
  if (present.length === 1) problems.push(`fields ${Object.keys(RESULT_DELTA).join(" and ")} must appear together`);
  for (const key of present) {
    const n = Number(fields[key]);
    // The delta is a difference and may be negative; the bare rate is a rate and may not.
    if (fields[key] === "" || !Number.isFinite(n) || (key === "bare_wakeups_per_s" && n < 0)) problems.push(`field ${key} must be a ${key === "bare_wakeups_per_s" ? "non-negative " : ""}number`);
    else value[RESULT_DELTA[key]] = n;
  }
  return problems.length ? { ok: false, problems } : { ok: true, problems: [], value };
}

const sha256Of = (bytes) => crypto.createHash("sha256").update(bytes).digest("hex");

function checkEvidence(m, index, budget, crateDir, { exists, readFile }) {
  const at = `measurement[${index}]`;
  const evidence = m.evidence;
  if (!evidence || !nonEmpty(evidence.file) || !SHA256.test(String(evidence.sha256 ?? ""))) return [`${at} has no evidence {file, sha256} (record it with runtime-budgets.mjs record)`];
  const resolved = path.resolve(crateDir, evidence.file);
  const relative = path.relative(path.resolve(crateDir, RUNTIME_BUDGET_EVIDENCE_DIR), resolved);
  if (relative.startsWith("..") || path.isAbsolute(relative)) return [`${at} evidence file must be under ${RUNTIME_BUDGET_EVIDENCE_DIR}/`];
  if (!exists(resolved)) return [`${at} evidence file ${evidence.file} is missing`];
  const bytes = readFile(resolved);
  if (sha256Of(bytes) !== evidence.sha256) return [`${at} evidence file ${evidence.file} does not match its recorded sha256`];
  const parsed = parseBudgetResult(bytes.toString("utf8"));
  if (!parsed.ok) return parsed.problems.map((p) => `${at} evidence ${evidence.file}: ${p}`);
  const v = parsed.value;
  const problems = [];
  if (v.crate !== budget.crate) problems.push(`evidence is for ${v.crate}, not ${budget.crate}`);
  if (v.version !== m.crateVersion) problems.push(`evidence version ${v.version} != recorded ${m.crateVersion}`);
  for (const key of BUDGET_FIGURES) if (v[key] !== m.observed[key]) problems.push(`evidence ${key} ${v[key]} != recorded ${m.observed[key]}`);
  if (v.scale_unit !== m.dataScale.unit || v.scaleCount !== m.dataScale.count) problems.push("evidence data scale differs from the record");
  // The baseline and delta belong to the record exactly when the raw output carries them.
  const recordedBare = m.baseline?.wakeupsPerSecond;
  const recordedDelta = m.delta?.wakeupsPerSecond;
  if (v.bareWakeupsPerSecond !== recordedBare) problems.push(`evidence bare_wakeups_per_s ${v.bareWakeupsPerSecond} != recorded baseline ${recordedBare}`);
  if (v.wakeupsDeltaPerSecond !== recordedDelta) problems.push(`evidence wakeups_delta_per_s ${v.wakeupsDeltaPerSecond} != recorded delta ${recordedDelta}`);
  if (v.bareWakeupsPerSecond !== undefined && !(isNumber(v.wakeupsPerSecond) && Math.abs(v.wakeupsPerSecond - v.bareWakeupsPerSecond - v.wakeupsDeltaPerSecond) <= DELTA_TOLERANCE)) {
    problems.push(`evidence wakeups_delta_per_s ${v.wakeupsDeltaPerSecond} disagrees with wakeups_per_s ${v.wakeupsPerSecond} minus bare_wakeups_per_s ${v.bareWakeupsPerSecond}`);
  }
  return problems.map((p) => `${at} ${p}`);
}

// ---------------------------------------------------------------- evaluation

const stripComments = (text) => text.replace(/\/\*[\s\S]*?\*\//g, "").replace(/\/\/.*$/gm, "");

// `use`, `pub use` and `pub(crate) use` declarations, including multi-line `{ ... }` lists: an import cannot start work.
const stripImports = (text) => text.replace(/^[ \t]*(?:pub(?:\([^)]*\))?\s+)?use\s[^;]*;/gm, "");

/** Source with `#[cfg(test)]` modules removed (brace-balanced), for scanning shipped code only. */
function withoutTestModules(text) {
  const out = [];
  const lines = text.split("\n");
  for (let i = 0; i < lines.length; i += 1) {
    if (!/^\s*#\[cfg\(test\)\]/.test(lines[i])) { out.push(lines[i]); continue; }
    let j = i + 1;
    while (j < lines.length && /^\s*#\[/.test(lines[j])) j += 1;
    if (j < lines.length && /\bmod\s+\w+\s*;/.test(lines[j])) { i = j; continue; }
    let depth = 0;
    let opened = false;
    for (; j < lines.length; j += 1) {
      for (const ch of lines[j]) { if (ch === "{") { depth += 1; opened = true; } else if (ch === "}") depth -= 1; }
      if (opened && depth <= 0) break;
    }
    i = j;
  }
  return out.join("\n");
}

function rustFiles(dir) {
  const files = [];
  if (!fs.existsSync(dir)) return files;
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) { if (entry.name !== "tests") files.push(...rustFiles(full)); }
    else if (entry.isFile() && entry.name.endsWith(".rs") && entry.name !== "tests.rs") files.push(full);
  }
  return files;
}

/** `{ "src/x.rs": [ids] }` for every shipped (non-test) source file that matches a CONTINUOUS_PATTERNS entry. */
export function scanContinuousFiles(crateDir) {
  const files = {};
  for (const file of rustFiles(path.join(crateDir, "src")).sort()) {
    const text = stripImports(stripComments(withoutTestModules(fs.readFileSync(file, "utf8"))));
    const ids = Object.entries(CONTINUOUS_PATTERNS).filter(([, pattern]) => pattern.test(text)).map(([id]) => id);
    if (ids.length) files[path.relative(crateDir, file).split(path.sep).join("/")] = ids;
  }
  return files;
}

/** Ids of CONTINUOUS_PATTERNS matched in the crate's shipped (non-test) src, with the first file for each. */
export function scanContinuousPatterns(crateDir) {
  const hits = {};
  for (const [file, ids] of Object.entries(scanContinuousFiles(crateDir))) for (const id of ids) hits[id] ??= file;
  return hits;
}

function validateMeasurement(m, index) {
  const at = `measurement[${index}]`;
  const problems = [];
  if (!m || typeof m !== "object") return [`${at} is not an object`];
  if (!nonEmpty(m.host?.os) || !nonEmpty(m.host?.arch)) problems.push(`${at} host must name os and arch`);
  if (!nonEmpty(m.crateVersion)) problems.push(`${at} has no crateVersion`);
  if (!SHA.test(String(m.commit ?? ""))) problems.push(`${at} commit must be a full 40-character SHA`);
  if (!/^\d{4}-\d{2}-\d{2}(?:T[\d:.]+Z?)?$/.test(String(m.date ?? "")) || Number.isNaN(Date.parse(m.date))) problems.push(`${at} date must be an ISO date`);
  if (!SCOPES.includes(m.scope)) problems.push(`${at} scope must be one of ${SCOPES.join(", ")}`);
  if (m.scope === "installed" && !(nonEmpty(m.app?.name) && nonEmpty(m.app?.version))) problems.push(`${at} installed scope must name app.name and app.version`);
  if (!nonEmpty(m.dataScale?.description) || !nonEmpty(m.dataScale?.unit) || !isNumber(m.dataScale?.count) || m.dataScale.count <= 0) {
    problems.push(`${at} dataScale must name description, unit and a positive count`);
  }
  if (!nonEmpty(m.command)) problems.push(`${at} has no command`);
  for (const key of BUDGET_FIGURES) {
    if (m.observed?.[key] === null && figureUnavailableOn(m.host?.os, key)) continue;
    if (!isNumber(m.observed?.[key]) || m.observed[key] < 0) problems.push(`${at} observed.${key} must be a non-negative number`);
  }
  if (isNumber(m.observed?.residentMemoryBytes) && m.observed.residentMemoryBytes === 0) {
    problems.push(`${at} observed.residentMemoryBytes of 0 is implausible for a running process`);
  }
  // Optional bare-runtime baseline and reported delta: shape only here; the gate recomputes the delta.
  for (const [field, signed] of [["baseline", false], ["delta", true]]) {
    if (m[field] === undefined) continue;
    const ok = m[field] && typeof m[field] === "object" && Object.keys(m[field]).every((key) => DELTA_FIGURES.includes(key) && isNumber(m[field][key]) && (signed || m[field][key] >= 0));
    if (!ok) problems.push(`${at} ${field} must map ${DELTA_FIGURES.join(", ")} to ${signed ? "numbers" : "non-negative numbers"}`);
  }
  if ((m.baseline === undefined) !== (m.delta === undefined)) problems.push(`${at} baseline and delta must be recorded together`);
  return problems;
}

/** Figures of `budget` gated as a delta over the bare baseline (validated separately; unknown bases never match). */
const deltaFiguresOf = (budget) => DELTA_FIGURES.filter((key) => budget.ceilingBasis?.[key] === CEILING_BASIS_DELTA);

const escapeRe = (s) => s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");

/** True when `api` (`Type::method()` or a bare type) is a `pub fn` / `pub struct|enum|trait` item in `source`, not in a comment. */
export function statusApiDeclared(api, source) {
  const code = stripComments(source);
  const symbol = api.replace(/\(.*$/, "").split(/::|\./).pop();
  if (!symbol) return false;
  const fn = new RegExp(`\\bpub(?:\\([^)]*\\))?\\s+(?:const\\s+)?(?:async\\s+)?(?:unsafe\\s+)?fn\\s+${escapeRe(symbol)}\\b`);
  const type = new RegExp(`\\bpub(?:\\([^)]*\\))?\\s+(?:struct|enum|trait|type)\\s+${escapeRe(symbol)}\\b`);
  return api.includes("(") || api.includes("::") ? fn.test(code) : (type.test(code) || fn.test(code));
}

/**
 * Evaluates runtime-budget.json for one crate. Returns { ok, problems }; problems name every
 * refusal so the author can fix them in one pass. `version` is the version being published and
 * `evidence` is the release gate evidence for it (the host-policy authority). `requireInstalled`
 * (adoption) additionally needs a fresh in-ceiling installed-app measurement, optionally for `app`.
 */
export function evaluateRuntimeBudget({ crate, crateDir, version, evidence, requireInstalled = false, app, heldResource = false }, { readFile = readFileSync, exists = existsSync, git = defaultGit } = {}) {
  const file = path.join(crateDir, RUNTIME_BUDGET_FILE);
  if (!exists(file)) return { ok: false, problems: [`${RUNTIME_BUDGET_FILE} is missing (docs/RUNTIME-BUDGETS.md)`] };
  let budget;
  try { budget = JSON.parse(readFile(file, "utf8")); } catch (error) {
    return { ok: false, problems: [`${RUNTIME_BUDGET_FILE} is not valid JSON: ${error.message}`] };
  }
  const problems = [];
  if (!budget || typeof budget !== "object" || Array.isArray(budget)) return { ok: false, problems: [`${RUNTIME_BUDGET_FILE} must be a JSON object`] };
  if (budget.schema !== RUNTIME_BUDGET_SCHEMA) problems.push(`${RUNTIME_BUDGET_FILE} schema must be ${RUNTIME_BUDGET_SCHEMA}`);
  if (budget.crate !== crate) problems.push(`${RUNTIME_BUDGET_FILE} is for ${budget.crate}, expected ${crate}`);
  if (!nonEmpty(version)) problems.push("publishing version is unknown, cannot bind the measurements to it");
  if (budget.hostPolicy !== undefined && budget.hostPolicy !== "darwin-only" && budget.hostPolicy !== "win32-only") problems.push('hostPolicy must be "darwin-only" or "win32-only" when present');
  if ((budget.hostPolicy === "darwin-only" || budget.hostPolicy === "win32-only") && !nonEmpty(budget.hostPolicyReason)) problems.push(`hostPolicy ${budget.hostPolicy} needs a hostPolicyReason`);
  // One source of truth for the host policy: the release evidence. The budget file may repeat it, never differ.
  if (evidence) {
    const policyLabel = (hosts) => (hosts.length === 1 ? `${hosts[0]}-only` : "macOS and Windows");
    const evidenceHosts = requiredBudgetHosts(null, evidence);
    const budgetHosts = requiredBudgetHosts(budget);
    if (evidenceHosts.join() !== budgetHosts.join()) {
      problems.push(`hostPolicy disagrees with the release evidence (evidence: ${policyLabel(evidenceHosts)}; ${RUNTIME_BUDGET_FILE}: ${policyLabel(budgetHosts)})`);
    }
  }

  // Ceilings: declared, finite, positive.
  const ceilings = {};
  for (const key of BUDGET_FIGURES) {
    const value = budget.ceilings?.[key];
    if (value === null || value === undefined) problems.push(`ceiling ${key} is undeclared`);
    else if (!isNumber(value) || value <= 0) problems.push(`ceiling ${key} must be a positive number`);
    else ceilings[key] = value;
  }

  // Ceiling basis: an explicit, closed declaration. A delta figure needs its own positive delta ceiling,
  // and a delta ceiling without the basis would be dead text, so both directions are refused.
  const basis = budget.ceilingBasis;
  if (basis !== undefined && (!basis || typeof basis !== "object" || Array.isArray(basis))) problems.push("ceilingBasis must be an object");
  else for (const [key, value] of Object.entries(basis ?? {})) {
    if (!DELTA_FIGURES.includes(key)) problems.push(`ceilingBasis.${key} is not a figure that can use a delta basis (${DELTA_FIGURES.join(", ")})`);
    else if (value !== CEILING_BASIS_DELTA) problems.push(`ceilingBasis.${key} must be "${CEILING_BASIS_DELTA}"`);
  }
  const deltaFigures = deltaFiguresOf(budget);
  const deltaCeilings = {};
  for (const key of DELTA_FIGURES) {
    const value = budget.deltaCeilings?.[key];
    if (deltaFigures.includes(key)) {
      if (!isNumber(value) || value <= 0) problems.push(`deltaCeilings.${key} must be a positive number (ceilingBasis.${key} is ${CEILING_BASIS_DELTA})`);
      else deltaCeilings[key] = value;
    } else if (value !== undefined) problems.push(`deltaCeilings.${key} is declared but ceilingBasis.${key} is not ${CEILING_BASIS_DELTA}`);
  }

  // Required scale: the unit and minimum the crate's measurement must reach.
  const scale = budget.requiredScale;
  let requiredScale = null;
  if (scale === null || scale === undefined) problems.push("requiredScale is undeclared (unit and minCount the measurement must reach)");
  else if (!nonEmpty(scale.unit) || !isNumber(scale.minCount) || scale.minCount <= 0) problems.push("requiredScale must name unit and a positive minCount");
  else requiredScale = scale;

  // Status exposure pointer: a real pub item in the named file, exposing the resident figure.
  const status = budget.status;
  if (!status || !nonEmpty(status.api) || !nonEmpty(status.file)) problems.push("status exposure is not recorded (status.api and status.file)");
  else {
    const statusFile = path.resolve(crateDir, status.file);
    const inside = path.relative(crateDir, statusFile);
    if (inside.startsWith("..") || path.isAbsolute(inside)) problems.push(`status.file ${status.file} must stay inside the crate`);
    else if (!exists(statusFile)) problems.push(`status.file ${status.file} does not exist`);
    else if (!statusApiDeclared(status.api, readFile(statusFile, "utf8"))) problems.push(`status.file ${status.file} has no pub item matching ${status.api}`);
    const exposes = Array.isArray(status.exposes) ? status.exposes : [];
    if (!exposes.includes("residentMemoryBytes")) problems.push("status does not expose residentMemoryBytes (the runtime must show the budget figure)");
    for (const key of exposes) if (!BUDGET_FIGURES.includes(key)) problems.push(`status.exposes names unknown figure ${key}`);
  }

  // Measurements.
  const records = Array.isArray(budget.measurements) ? budget.measurements : [];
  if (records.length === 0) problems.push("no measurements recorded (a declared ceiling is not evidence)");
  const valid = [];
  records.forEach((m, index) => {
    const bad = validateMeasurement(m, index);
    if (bad.length) { problems.push(...bad); return; }
    const evidenceProblems = checkEvidence(m, index, budget, crateDir, { exists, readFile });
    if (evidenceProblems.length) problems.push(...evidenceProblems); else valid.push({ m, index });
  });
  const counted = valid.filter(({ m }) => m.crateVersion === version);
  if (valid.length && nonEmpty(version) && counted.length === 0) {
    problems.push(`no measurement for version ${version} (recorded: ${[...new Set(valid.map(({ m }) => m.crateVersion))].join(", ")})`);
  }
  // Why a measurement fails one figure, or null. A delta-basis figure of a synthetic measurement is gated on
  // observed - baseline, recomputed here; the reported delta must agree and the baseline must exist. Every
  // other scope (real, installed) keeps the whole-process ceiling, so an app is never held to a delta.
  const usesDelta = (m, key) => deltaFigures.includes(key) && m.scope === DELTA_SCOPE;
  const figureFailure = (m, key) => {
    if (usesDelta(m, key)) {
      const raw = m.observed[key];
      const bare = m.baseline?.[key];
      const reported = m.delta?.[key];
      if (deltaCeilings[key] === undefined) return null; // the declaration problem is already reported
      if (!isNumber(raw)) return `${FIGURE_LABEL[key]} is not measurable here but the ${FIGURE_LABEL[key]} ceiling is a delta over a bare baseline`;
      if (!isNumber(bare) || !isNumber(reported)) return `has no bare baseline for ${FIGURE_LABEL[key]} (ceilingBasis.${key} is ${CEILING_BASIS_DELTA}; re-run the bench and record it again)`;
      const delta = Number((raw - bare).toFixed(3)); // the bench prints 3 decimals; compare at that precision
      if (Math.abs(delta - reported) > DELTA_TOLERANCE) return `reported ${FIGURE_LABEL[key]} delta ${reported} disagrees with observed ${raw} minus baseline ${bare}`;
      if (delta < DELTA_FLOOR) return `bare baseline ${bare} is more than ${-DELTA_FLOOR} ${FIGURE_LABEL[key]}/s above observed ${raw}: a mismatched pair; re-run the bench`;
      return delta > deltaCeilings[key] ? `exceeds the ${FIGURE_LABEL[key]} delta ceiling over the bare baseline: ${raw} - ${bare} = ${delta} > ${deltaCeilings[key]}` : null;
    }
    if (ceilings[key] !== undefined && m.observed[key] !== null && m.observed[key] > ceilings[key]) {
      return `exceeds the ${FIGURE_LABEL[key]} ceiling: observed ${m.observed[key]} > ${ceilings[key]}`;
    }
    return null;
  };
  for (const { m, index } of counted) {
    for (const key of BUDGET_FIGURES) {
      const failure = figureFailure(m, key);
      if (failure) problems.push(`measurement[${index}] on ${m.host.os}/${m.host.arch} ${failure}`);
    }
  }
  const withinCeilings = (m) => BUDGET_FIGURES.every((key) => ceilings[key] !== undefined && (usesDelta(m, key) ? deltaCeilings[key] !== undefined && figureFailure(m, key) === null : m.observed[key] === null ? figureUnavailableOn(m.host?.os, key) : m.observed[key] <= ceilings[key]));
  const atScale = (m) => requiredScale !== null && m.dataScale.unit === requiredScale.unit && m.dataScale.count >= requiredScale.minCount;
  const staleness = new Map();
  const stale = (m) => {
    if (!staleness.has(m.commit)) staleness.set(m.commit, measurementStaleness(m.commit, crateDir, git, { exists, readFile }));
    return staleness.get(m.commit);
  };

  // Required hosts: one fresh, in-ceiling, at-scale measurement of this version on each.
  // A held-resource crate defers measurement to adoption: no per-host publish measurement is required.
  const required = heldResource ? [] : requiredBudgetHosts(budget, evidence);
  for (const host of required) {
    const onHost = counted.filter(({ m }) => m.host.os === host);
    if (onHost.length === 0) { problems.push(`no ${HOSTS[host]} measurement for version ${version} (required hosts: ${required.map((h) => HOSTS[h]).join(" and ")}${required.length > 1 ? "; apps ship on both and the release gate requires both unless a darwin-only policy with a reason is recorded" : `; ${required[0]}-only policy: ${String((evidence ?? budget).hostPolicyReason).trim()}`})`); continue; }
    const fresh = onHost.filter(({ m }) => stale(m) === null);
    if (fresh.length === 0) { problems.push(`${HOSTS[host]} measurement is stale: ${onHost.map(({ m }) => stale(m)).join("; ")}`); continue; }
    const within = fresh.filter(({ m }) => withinCeilings(m));
    if (within.length === 0) { problems.push(`no ${HOSTS[host]} measurement within all ceilings`); continue; }
    if (!within.some(({ m }) => atScale(m))) {
      problems.push(`no ${HOSTS[host]} measurement at the required scale (${requiredScale ? `${requiredScale.minCount} ${requiredScale.unit} or more` : "requiredScale undeclared"}; recorded: ${within.map(({ m }) => `${m.dataScale.count} ${m.dataScale.unit}`).join(", ")})`);
    }
  }

  // Adoption: the figures come from the installed app, not the bench.
  if (requireInstalled) {
    if (heldResource && !nonEmpty(app)) problems.push("adoption of a held-resource crate must name the app whose installed measurement is required");
    const installed = counted.filter(({ m }) => m.scope === "installed" && (!app || m.app.name === app));
    if (installed.length === 0) problems.push(`no installed-app measurement for version ${version}${app ? ` in ${app}` : ""} (adoption needs the figures from the installed app)`);
    else if (!installed.some(({ m }) => stale(m) === null && withinCeilings(m) && atScale(m))) problems.push("no fresh installed-app measurement within all ceilings at the required scale");
  }

  // README human view must say what the JSON says.
  const readmePath = path.join(crateDir, "README.md");
  if (!exists(readmePath)) problems.push("README.md is missing");
  else {
    const section = runtimeBudgetSection(readFile(readmePath, "utf8"));
    if (section === null) problems.push('README.md has no "## Runtime budget" section (generate it from runtime-budget.json)');
    else if (JSON.stringify(normalizeLines(section)) !== JSON.stringify(normalizeLines(renderRuntimeBudgetBody(budget)))) {
      problems.push('README.md "## Runtime budget" section differs from runtime-budget.json (regenerate it with runtime-budgets.mjs sync)');
    }
  }
  return { ok: problems.length === 0, problems };
}

// ---------------------------------------------------------------- membership (fail closed)

const fail = (crate, crateDir, problems, fix) => ({
  name: RUNTIME_BUDGET_CHECK, status: "fail", detail: `${crate}: ${problems.join("; ")}`,
  fix: fix ?? `measure the crate and record it in ${path.join(crateDir, RUNTIME_BUDGET_FILE)}, then regenerate the README section (docs/RUNTIME-BUDGETS.md)`,
});

/**
 * The class a crate declares in `[package.metadata.rightkit]`: `continuous` is `false`, `true`/"always-on"
 * or "held-resource". Returns { kind: "false" | "always-on" | "held-resource" | "undeclared", error? }.
 */
export function crateClass(meta, { hasFile = false } = {}) {
  const value = meta.continuous;
  if (value === false) return { kind: "false" };
  if (value === true || value === "always-on") return { kind: "always-on" };
  if (value === "held-resource") return { kind: "held-resource" };
  if (value !== undefined) return { kind: "undeclared", error: '[package.metadata.rightkit] continuous must be false, true, "always-on" or "held-resource"' };
  return { kind: meta.runtime_budget === true || hasFile ? "always-on" : "undeclared" };
}

/**
 * Problems with a held-resource declaration (empty when it holds). Held-resource crates are checked for
 * a complete `resources` table whose entries resolve to `pub` items, and for the scanner finding no
 * continuous pattern in a file the table does not cover. Nothing is measured at crate publish.
 */
export function heldResourceProblems({ crate, crateDir, meta }, { readFile = readFileSync, exists = existsSync } = {}) {
  const problems = [];
  if (RUNTIME_BUDGET_CRATES.includes(crate)) problems.push("a listed continuous owner (RUNTIME_BUDGET_CRATES) cannot use held-resource; it stays always-on");
  const resources = meta.resources;
  if (!Array.isArray(resources) || resources.length === 0) {
    problems.push("held-resource needs a non-empty [[package.metadata.rightkit.resources]] table");
    return problems;
  }
  const covered = new Map();
  resources.forEach((entry, index) => {
    const at = `resources[${index}]`;
    if (!entry || typeof entry !== "object") { problems.push(`${at} is not a table`); return; }
    for (const key of ["api", "file", "scale", "release"]) if (!nonEmpty(entry[key])) problems.push(`${at} has no ${key}`);
    const kinds = Array.isArray(entry.holds) ? entry.holds : [entry.holds];
    if (!kinds.length || !kinds.every((k) => HELD_KINDS.includes(k))) problems.push(`${at}.holds must be one of ${HELD_KINDS.join("|")}`);
    if (nonEmpty(entry.release) && /^(none|never|n\/a|-)$/i.test(entry.release.trim())) problems.push(`${at}.release must name Drop or the function that ends it`);
    if (!nonEmpty(entry.file)) return;
    const resolved = path.resolve(crateDir, entry.file);
    const inside = path.relative(path.join(crateDir, "src"), resolved);
    if (inside.startsWith("..") || path.isAbsolute(inside)) { problems.push(`${at}.file ${entry.file} must be under src/`); return; }
    if (!exists(resolved)) { problems.push(`${at}.file ${entry.file} does not exist`); return; }
    if (nonEmpty(entry.api) && !statusApiDeclared(entry.api, readFile(resolved, "utf8"))) problems.push(`${at}.api ${entry.api} is not a pub item in ${entry.file}`);
    const rel = posix(path.relative(crateDir, resolved));
    if (!covered.has(rel)) covered.set(rel, []);
    covered.get(rel).push({ entry, kinds });
  });
  for (const [file, ids] of Object.entries(scanContinuousFiles(crateDir))) {
    const entries = covered.get(file);
    if (!entries) { problems.push(`${file} matches ${ids.join(", ")} but no resource lists it (new long-lived code cannot hide)`); continue; }
    if (ids.includes("detached-child")) problems.push(`${file} starts a detached child, which outlives any held value; the crate is always-on, or the launch moves behind a held handle`);
    for (const [pattern, kind] of Object.entries(PATTERN_HOLDS)) {
      if (ids.includes(pattern) && !entries.some(({ kinds }) => kinds.includes(kind))) problems.push(`${file} matches ${pattern} but no resource for it holds ${kind}`);
    }
    if (ids.includes("resident-static") && !entries.some(({ entry }) => nonEmpty(entry.static_note))) {
      problems.push(`${file} matches resident-static: a resource entry for it needs a static_note saying why the process-wide value is bounded or only reachable through the held value`);
    }
  }
  return problems;
}

/**
 * Preflight-shaped check for one crate directory. Never null: a crate must be declared `false` (reason
 * naming every matched pattern), "held-resource" (resources table, adoption-time proof) or always-on
 * (runtime-budget.json, fully checked). `readFile`, `exists` and `git` are injectable.
 */
export function runtimeBudgetCheck({ crate, crateDir, version, evidence, requireInstalled = false, app }, deps = {}) {
  const { readFile = readFileSync, exists = existsSync } = deps;
  let pkg;
  try { pkg = readCargoPackage(crateDir, { exists, readFile }); } catch (error) {
    return fail(crate, crateDir, [`cannot read Cargo.toml: ${error.message}`], `fix ${path.join(crateDir, "Cargo.toml")}`);
  }
  if (pkg.name !== crate) {
    return fail(crate, crateDir, [`Cargo.toml [package].name ${JSON.stringify(pkg.name)} differs from --crate ${crate}`], "publish the crate by its manifest name");
  }
  const meta = pkg.manifest.package?.metadata?.rightkit ?? {};
  if (meta.runtime_budget !== undefined && typeof meta.runtime_budget !== "boolean") return fail(crate, crateDir, ["[package.metadata.rightkit] runtime_budget must be true or false"], "set runtime_budget = true or false");
  const listed = RUNTIME_BUDGET_CRATES.includes(crate);
  const hasFile = exists(path.join(crateDir, RUNTIME_BUDGET_FILE));
  const klass = crateClass(meta, { hasFile: hasFile || listed });
  if (klass.error) return fail(crate, crateDir, [klass.error], 'set continuous = false, "always-on" or "held-resource"');

  if (klass.kind === "false") {
    if (listed) return fail(crate, crateDir, ["listed as a continuous owner in RUNTIME_BUDGET_CRATES; it cannot declare continuous = false"], "remove continuous = false and measure the crate");
    if (hasFile) return fail(crate, crateDir, [`declares continuous = false but also carries ${RUNTIME_BUDGET_FILE}`], "remove one of the two declarations");
    const reason = typeof meta.continuous_reason === "string" ? meta.continuous_reason.trim() : "";
    if (!reason) return fail(crate, crateDir, ["continuous = false needs a one-line continuous_reason"], 'add continuous_reason = "<why this crate runs no continuous work>"');
    const hits = scanContinuousPatterns(crateDir);
    // The id must appear as a whole token (hyphens included), not inside another word.
    const unnamed = Object.keys(hits).filter((id) => !new RegExp(`(?<![\\w-])${id}(?![\\w-])`).test(reason));
    if (unnamed.length) {
      return fail(crate, crateDir, [`declares continuous = false but src matches ${unnamed.map((id) => `${id} (${hits[id]})`).join(", ")}; continuous_reason must name each pattern it justifies`],
        "either measure the crate (runtime-budget.json) or write a continuous_reason that names each matched pattern after reviewing it");
    }
    return { name: RUNTIME_BUDGET_CHECK, status: "ok", detail: `${crate} declared continuous = false: ${reason}` };
  }

  if (klass.kind === "held-resource") {
    const problems = heldResourceProblems({ crate, crateDir, meta }, deps);
    if (problems.length) return fail(crate, crateDir, problems, "complete the resources table or declare the crate always-on (docs/RUNTIME-BUDGETS.md)");
    if (!requireInstalled) {
      return { name: RUNTIME_BUDGET_CHECK, status: "ok", detail: `${crate} declared held-resource (${meta.resources.length} resource(s)); measurement is required at app adoption, not at crate publish` };
    }
    const result = evaluateRuntimeBudget({ crate, crateDir, version: version ?? pkg.version, evidence, requireInstalled: true, app, heldResource: true }, deps);
    return result.ok
      ? { name: RUNTIME_BUDGET_CHECK, status: "ok", detail: `${crate} ${version ?? pkg.version} has an installed measurement for ${app}` }
      : fail(crate, crateDir, result.problems);
  }

  if (klass.kind === "undeclared") {
    return fail(crate, crateDir, [`undeclared: none of ${RUNTIME_BUDGET_FILE}, continuous = false with a continuous_reason, or continuous = "held-resource" with resources`],
      `declare the crate (docs/RUNTIME-BUDGETS.md)`);
  }
  const result = evaluateRuntimeBudget({ crate, crateDir, version: version ?? pkg.version, evidence, requireInstalled, app }, deps);
  if (result.ok) {
    return { name: RUNTIME_BUDGET_CHECK, status: "ok", detail: `${crate} ${version ?? pkg.version} has measured runtime budgets on every required host` };
  }
  return fail(crate, crateDir, result.problems);
}

/**
 * Adoption variant: the same artifact, plus a fresh installed-app measurement (optionally for `app`).
 * For a held-resource crate this is the only place its budget is proven, and `app` is required.
 */
export function runtimeBudgetAdoptionCheck(options, deps = {}) {
  return runtimeBudgetCheck({ ...options, requireInstalled: true }, deps);
}

// ---------------------------------------------------------------- app adoption (held-resource crates)

/**
 * `{ crate: { ceilings, requiredScale } }` for every held-resource crate under `cratesDir`, read from each
 * crate's manifest and (optional) runtime-budget.json. This is what ships in runtime-budget-classes.json so
 * an app, which has no access to crate source, can be checked at qualification.
 */
export function heldResourceClasses(cratesDir) {
  const out = {};
  for (const entry of fs.readdirSync(cratesDir, { withFileTypes: true }).sort((a, b) => a.name.localeCompare(b.name))) {
    const dir = path.join(cratesDir, entry.name);
    if (!entry.isDirectory() || !existsSync(path.join(dir, "Cargo.toml"))) continue;
    const pkg = readCargoPackage(dir);
    if (pkg.manifest.package?.metadata?.rightkit?.continuous !== "held-resource") continue;
    const file = path.join(dir, RUNTIME_BUDGET_FILE);
    const budget = existsSync(file) ? JSON.parse(readFileSync(file, "utf8")) : {};
    out[pkg.name] = {
      ceilings: BUDGET_FIGURES.every((k) => isNumber(budget.ceilings?.[k])) ? Object.fromEntries(BUDGET_FIGURES.map((k) => [k, budget.ceilings[k]])) : null,
      requiredScale: budget.requiredScale && nonEmpty(budget.requiredScale.unit) && isNumber(budget.requiredScale.minCount) ? { unit: budget.requiredScale.unit, minCount: budget.requiredScale.minCount } : null,
    };
  }
  return out;
}

/** The shipped classes document for `heldResourceClasses`. */
export const renderClasses = (classes) => `${JSON.stringify({ schema: 1, heldResource: classes }, null, 2)}\n`;

function lockedPackages(lockPaths, { readFile = readFileSync, exists = existsSync } = {}) {
  const seen = new Map();
  for (const lock of lockPaths) {
    if (!exists(lock)) continue;
    for (const entry of parse(readFile(lock, "utf8")).package ?? []) {
      if (!seen.has(entry.name)) seen.set(entry.name, new Set());
      seen.get(entry.name).add(entry.version);
    }
  }
  return seen;
}

/**
 * App qualification: an app whose Cargo.lock contains a held-resource crate must carry an installed-scope
 * measurement of THAT app, of the locked crate version, in `runtime-budget-adoption.json` at the app root
 * (records have the crate-measurement shape plus `crate`, with raw bench evidence under
 * `runtime-budget.evidence/`). The measurement must be within the crate's declared ceilings and at its
 * declared scale, and taken at the app's current version. Returns null when the app uses no held-resource
 * crate. Runs from preflight (`collectPreflight`), i.e. right-release doctor, --dry-run and every build.
 */
export function appRuntimeBudgetAdoptionCheck({ appRoot, app, version, cargoLockPaths = [] }, deps = {}) {
  const { readFile = readFileSync, exists = existsSync } = deps;
  let classes = deps.classes;
  if (!classes) {
    const file = new URL(`./${CLASSES_FILE}`, import.meta.url);
    try { classes = JSON.parse(readFileSync(file, "utf8")).heldResource ?? {}; } catch { classes = {}; }
  }
  let locked;
  try { locked = lockedPackages(cargoLockPaths, { readFile, exists }); } catch (error) {
    return { name: RUNTIME_BUDGET_CHECK, status: "fail", detail: `cannot read Cargo.lock: ${error.message}`, fix: "fix the app Cargo.lock" };
  }
  const used = Object.keys(classes).filter((name) => locked.has(name));
  if (used.length === 0) return null;
  const problems = [];
  // A crate whose ceilings or scale RightKit has not declared yet cannot be measured against anything, so it
  // never blocks an app: it is reported as pending (warn) until the crate declares its budget.
  const pending = used.filter((name) => !classes[name].ceilings || !classes[name].requiredScale);
  const measurable = used.filter((name) => !pending.includes(name));
  const adoptionPath = path.join(appRoot, APP_ADOPTION_FILE);
  let records = [];
  if (!exists(adoptionPath)) { if (measurable.length) problems.push(`${APP_ADOPTION_FILE} is missing`); }
  else {
    try { records = JSON.parse(readFile(adoptionPath, "utf8")).measurements ?? []; } catch (error) { problems.push(`${APP_ADOPTION_FILE} is not valid JSON: ${error.message}`); }
  }
  for (const crate of measurable) {
    const { ceilings, requiredScale } = classes[crate];
    const mine = records.map((m, index) => ({ m, index })).filter(({ m }) => m?.crate === crate && m.scope === "installed" && locked.get(crate).has(m.crateVersion));
    if (mine.length === 0) { problems.push(`${crate} ${[...locked.get(crate)].join("/")}: no installed-scope measurement of ${app}`); continue; }
    const good = mine.filter(({ m, index }) => {
      const bad = validateMeasurement(m, index);
      if (bad.length) { problems.push(`${crate}: ${bad.join("; ")}`); return false; }
      const evidenceProblems = checkEvidence(m, index, { crate }, appRoot, { exists, readFile });
      if (evidenceProblems.length) { problems.push(`${crate}: ${evidenceProblems.join("; ")}`); return false; }
      if (m.app.name !== app) { problems.push(`${crate}: measurement[${index}] is for app ${m.app.name}, not ${app}`); return false; }
      if (version && m.app.version !== version) { problems.push(`${crate}: measurement[${index}] was taken at ${app} ${m.app.version}, not the current ${version}`); return false; }
      const over = BUDGET_FIGURES.filter((k) => m.observed[k] !== null && m.observed[k] > ceilings[k]);
      if (over.length) { problems.push(`${crate}: measurement[${index}] exceeds ${over.join(", ")}`); return false; }
      if (m.dataScale.unit !== requiredScale.unit || m.dataScale.count < requiredScale.minCount) { problems.push(`${crate}: measurement[${index}] is below the required scale ${requiredScale.minCount} ${requiredScale.unit}`); return false; }
      return true;
    });
    if (good.length === 0 && !problems.some((p) => p.startsWith(`${crate}`))) problems.push(`${crate}: no qualifying measurement`);
  }
  const pendingNote = pending.length ? `; pending RightKit budget declaration (not blocking): ${pending.join(", ")}` : "";
  if (problems.length) {
    return { name: RUNTIME_BUDGET_CHECK, status: "fail", detail: `${app} uses held-resource crate(s) ${measurable.join(", ")}: ${problems.join("; ")}${pendingNote}`, fix: `record an installed measurement of ${app} for each crate in ${APP_ADOPTION_FILE} (docs/RUNTIME-BUDGETS.md)` };
  }
  if (pending.length) {
    return { name: RUNTIME_BUDGET_CHECK, status: "warn", detail: `${app} uses held-resource crate(s) whose RightKit ceilings are not declared yet: ${pending.join(", ")}${measurable.length ? `; measured: ${measurable.join(", ")}` : ""}`, fix: "RightKit declares the crate ceilings; then record installed measurements (docs/RUNTIME-BUDGETS.md)" };
  }
  return { name: RUNTIME_BUDGET_CHECK, status: "ok", detail: `${app} has installed measurements for ${used.join(", ")}` };
}

// ---------------------------------------------------------------- record (measurement provenance)

/** os/arch/model of this machine. `model` is the hardware model on macOS, else the CPU model. */
export function detectHost(deps = {}) {
  const platform = deps.platform ?? process.platform;
  const arch = deps.arch ?? process.arch;
  let model = deps.model;
  if (model === undefined) {
    if (platform === "darwin") {
      const result = (deps.spawn ?? spawnSync)("sysctl", ["-n", "hw.model"], { encoding: "utf8", windowsHide: true });
      model = result.status === 0 ? String(result.stdout).trim() : undefined;
    }
    model ??= os.cpus()[0]?.model;
  }
  return { os: platform, arch, ...(model ? { model } : {}) };
}

/**
 * Writes the measurement record for a bench run: raw output copied to runtime-budget.evidence/, its
 * sha256 stored in the record, host and commit detected (never typed). Refuses when the measured paths
 * are not at HEAD (a measurement of uncommitted code cannot be reproduced) or the output disagrees with
 * the crate. Replaces a record for the same host, version, scope and app.
 */
export function recordMeasurement({ crateDir, resultFile, scope, command, description, app, date }, deps = {}) {
  const git = deps.git ?? defaultGit;
  const host = deps.host ?? detectHost();
  const raw = fs.readFileSync(resultFile);
  const parsed = parseBudgetResult(raw.toString("utf8"));
  if (!parsed.ok) throw new Error(`bench output rejected: ${parsed.problems.join("; ")}`);
  const pkg = readCargoPackage(crateDir);
  const v = parsed.value;
  if (v.crate !== pkg.name) throw new Error(`bench output is for ${v.crate}, crate is ${pkg.name}`);
  if (v.version !== pkg.version) throw new Error(`bench output is for version ${v.version}, crate is ${pkg.version}`);
  for (const key of BUDGET_FIGURES) {
    if (v[key] === null && !figureUnavailableOn(host.os, key)) throw new Error(`bench output rejected: ${key} is unavailable, which is allowed only where the host cannot measure it (${JSON.stringify(HOST_UNAVAILABLE_FIGURES)})`);
  }
  if (!SCOPES.includes(scope)) throw new Error(`--scope must be one of ${SCOPES.join(", ")}`);
  if (scope === "installed" && !(nonEmpty(app?.name) && nonEmpty(app?.version))) throw new Error("--scope installed needs --app and --app-version");
  if (!nonEmpty(command)) throw new Error("--command (the exact bench command) is required");
  if (!nonEmpty(description)) throw new Error("--description of the data scale is required");
  const head = git(["rev-parse", "HEAD"], crateDir);
  if (head.status !== 0 || !SHA.test(head.stdout)) throw new Error("cannot read HEAD");
  const stale = measurementStaleness(head.stdout, crateDir, git);
  if (stale) throw new Error(`refusing to record: ${stale} (commit the code under test, then re-run the bench)`);
  const file = path.join(crateDir, RUNTIME_BUDGET_FILE);
  const budget = JSON.parse(fs.readFileSync(file, "utf8"));
  const hasBaseline = v.bareWakeupsPerSecond !== undefined;
  if (scope === DELTA_SCOPE && !hasBaseline && deltaFiguresOf(budget).length) {
    throw new Error(`bench output rejected: ${RUNTIME_BUDGET_FILE} gates ${deltaFiguresOf(budget).join(", ")} as ${CEILING_BASIS_DELTA}, so the RESULT line needs bare_wakeups_per_s and wakeups_delta_per_s`);
  }
  if (hasBaseline && !(isNumber(v.wakeupsPerSecond) && Math.abs(v.wakeupsPerSecond - v.bareWakeupsPerSecond - v.wakeupsDeltaPerSecond) <= DELTA_TOLERANCE)) {
    throw new Error("bench output rejected: wakeups_delta_per_s disagrees with wakeups_per_s minus bare_wakeups_per_s");
  }
  const evidenceName = `${host.os}-${host.arch}-${pkg.version}-${scope}${scope === "installed" ? `-${app.name}` : ""}.txt`.replace(/[^\w.-]/g, "_");
  const evidenceRel = `${RUNTIME_BUDGET_EVIDENCE_DIR}/${evidenceName}`;
  fs.mkdirSync(path.join(crateDir, RUNTIME_BUDGET_EVIDENCE_DIR), { recursive: true });
  fs.writeFileSync(path.join(crateDir, evidenceRel), raw);
  const record = {
    host, crateVersion: pkg.version, commit: head.stdout, date: date ?? (deps.now ?? new Date()).toISOString().slice(0, 10), scope,
    ...(scope === "installed" ? { app: { name: app.name, version: app.version } } : {}),
    dataScale: { description, unit: v.scale_unit, count: v.scaleCount },
    command,
    observed: Object.fromEntries(BUDGET_FIGURES.map((key) => [key, v[key]])),
    ...(hasBaseline ? { baseline: { wakeupsPerSecond: v.bareWakeupsPerSecond }, delta: { wakeupsPerSecond: v.wakeupsDeltaPerSecond } } : {}),
    evidence: { file: evidenceRel, sha256: sha256Of(raw) },
  };
  const same = (m) => m.host?.os === host.os && m.host?.arch === host.arch && m.crateVersion === pkg.version && m.scope === scope && (m.app?.name ?? null) === (app?.name ?? null);
  budget.measurements = [...(budget.measurements ?? []).filter((m) => !same(m)), record];
  fs.writeFileSync(file, `${JSON.stringify(budget, null, 2)}\n`);
  return record;
}

// `node runtime-budgets.mjs sync <crate-dir>` regenerates the README section from runtime-budget.json.
// `node runtime-budgets.mjs record <crate-dir> --result <file> --scope <s> --command <c> --description <d> [--app <n> --app-version <v>]`
const realOrSelf = (file) => { try { return realpathSync(file); } catch { return path.resolve(file); } };
if (process.argv[1] && realOrSelf(process.argv[1]) === realOrSelf(fileURLToPath(import.meta.url))) {
  const [command, dir, ...rest] = process.argv.slice(2);
  const flag = (name) => { const i = rest.indexOf(name); return i >= 0 ? rest[i + 1] : undefined; };
  try {
    if (!dir || !["sync", "record", "classes"].includes(command)) throw new Error("usage: runtime-budgets.mjs sync|record <crate-dir> [record flags] | classes <crates-dir>");
    const crateDir = path.resolve(dir);
    if (command === "classes") {
      writeFileSync(fileURLToPath(new URL(`./${CLASSES_FILE}`, import.meta.url)), renderClasses(heldResourceClasses(crateDir)));
      process.stdout.write(`[runtime-budgets] ${CLASSES_FILE} regenerated from ${crateDir}\n`);
    } else if (command === "sync") {
      const budget = JSON.parse(readFileSync(path.join(crateDir, RUNTIME_BUDGET_FILE), "utf8"));
      const readmePath = path.join(crateDir, "README.md");
      const readme = existsSync(readmePath) ? readFileSync(readmePath, "utf8") : `# ${budget.crate}\n`;
      writeFileSync(readmePath, syncRuntimeBudgetReadme(readme, budget));
      process.stdout.write(`[runtime-budgets] README.md ## Runtime budget regenerated for ${budget.crate}\n`);
    } else {
      const record = recordMeasurement({
        crateDir, resultFile: flag("--result"), scope: flag("--scope"), command: flag("--command"), description: flag("--description"),
        app: flag("--app") ? { name: flag("--app"), version: flag("--app-version") } : undefined,
      });
      process.stdout.write(`[runtime-budgets] recorded ${record.scope} measurement for ${record.crateVersion} on ${record.host.os}/${record.host.arch} (evidence ${record.evidence.file}); now run: runtime-budgets.mjs sync ${dir}\n`);
    }
  } catch (error) {
    process.stderr.write(`runtime-budgets: ${error.message}\n`);
    process.exitCode = 1;
  }
}
