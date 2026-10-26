// Consumer input contracts: three app-check rules (CONSUMER_INPUT_RULES, appended to the app check) for inputs a committed tree
// declares but a later step cannot reach. Each reads committed files only (workflow YAML through the strict workflow-yaml.mjs
// parser, JSON and Markdown as data), never runs repository code, never touches the network, and never throws on odd input.
//
//   checkout-step-input  CA-07  a workflow step names a tracked repository path (a script, a source file, a working-directory)
//                         that the job's own sparse checkout (cone mode) does not materialize. SwiftRight factory-pairs.yml at
//                         80c9be45d compiled factory/native/windows/wgc-record/wgc-record.cpp on a windows job whose sparse
//                         checkout listed packages, probes/sr0-s/fresh-runtime, factory/pipeline and factory/tools only; the
//                         recorder source was absent on the runner until 2a67fc182 added its directory to the cone.
//   source-relation      CA-09  a value in one committed file names a row of another committed file that does not exist. The
//                         relations are data: SOURCE_RELATIONS below (the ones RightKit knows) plus an app's own
//                         .rightkit/source-relations.json of the same shape. SwiftRight commit 7655d1d87 rewrote
//                         docs/sr1-status.md as one line with literal "\n" escapes, so its status table has no rows and
//                         factory/tools/proven-controls/hand-entries.json's statusDocBlock "I2" names nothing (the
//                         proven-controls drift check failed on every push after it, e.g. run 39701428805).
//   checkout-longpaths   CA-11  a job on a GitHub-hosted Windows runner checks out tracked paths that exceed the Win32 path limit
//                         under the runner workspace (D:\a\<repo>\<repo>\) without enabling Git long paths before the checkout.
//                         SwiftRight run 38363145934 (windows-2025, plain actions/checkout): "Filename too long", 829 tracked paths
//                         over the limit. Long paths are legitimate: a job passes when a step before its checkout provably runs
//                         `git config --system|--global core.longpaths true` (or GIT_CONFIG_COUNT covers a KEY_n/VALUE_n pair), or when its
//                         sparse checkout leaves every long path out, or when it runs on a non-Windows runner.
//
// Known limits, deliberate for an offline check: a non-cone sparse checkout (sparse-checkout-cone-mode: false) is not modelled, a
// job with more than one checkout of its own repository is skipped, a path assembled from variables is matched on its literal
// text only, and a self-hosted Windows runner (label self-hosted) is skipped because its system Git configuration is not in the
// tree. The workspace name is the GitHub repository name from the checked repository's remote origin (git config, no network),
// never the checker's own directory; with no GitHub origin, paths that fail under any name fail and the rest are UNSUPPORTED.
// A pre-checkout step counts only when it provably runs and its whole script is the git config line; any other step or
// environment that touches core.longpaths is reported as UNSUPPORTED, never passed. A workflow the strict parser refuses is a finding only
// when it mentions a sparse checkout or a Windows runner (the two things these rules read); any other is left alone.
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { parseYaml } from "./workflow-yaml.mjs";

const readText = (root, rel) => { try { return fs.readFileSync(path.join(root, rel), "utf8").replace(/^\uFEFF/, ""); } catch { return null; } };
const WORKFLOW = /^\.github\/workflows\/[^/]+\.ya?ml$/;
const CHECKOUT = /^actions\/checkout@/;

// ---- workflow model ------------------------------------------------------------------------------------------------------------

const asList = (value) => (Array.isArray(value) ? value : value === undefined || value === null || value === "" ? [] : [value]);
const lines = (value) => (typeof value === "string" ? value.split("\n").map((l) => l.trim()).filter(Boolean) : asList(value).map(String));

/** The parsed workflows of the tree: [{ file, doc }] and [{ file, error }] for one the strict parser refuses. */
function workflows(root, files) {
  return files.filter((f) => WORKFLOW.test(f)).sort().map((file) => {
    const text = readText(root, file);
    if (text === null) return { file, error: "unreadable", text: "" };
    try { return { file, doc: parseYaml(text), text }; } catch (error) { return { file, error: error.message, text }; }
  });
}

/** The single self checkout of a job: { index, step, path, sparse: string[] | null, cone } or null when there is none or more
 * than one (a checkout of another repository, `with.repository`, is not the job's own tree). */
function selfCheckout(job) {
  const steps = Array.isArray(job?.steps) ? job.steps : [];
  const found = [];
  steps.forEach((step, index) => {
    if (!step || typeof step !== "object" || typeof step.uses !== "string" || !CHECKOUT.test(step.uses)) return;
    const w = step.with && typeof step.with === "object" ? step.with : {};
    if (w.repository && !/^\$\{\{\s*github\.repository\s*\}\}$/.test(String(w.repository))) return;
    const sparse = w["sparse-checkout"] === undefined ? null : lines(w["sparse-checkout"]).map((d) => d.replace(/^\/+|\/+$/g, ""));
    found.push({ index, step, path: typeof w.path === "string" ? w.path.replace(/^\.\/|\/+$/g, "") : "", sparse, cone: String(w["sparse-checkout-cone-mode"] ?? "true") !== "false" });
  });
  return found.length === 1 ? found[0] : null;
}

/** The runner labels a job can run on: its runs-on labels, with `${{ matrix.<key> }}` expanded from strategy.matrix. */
function runnerLabels(job) {
  const matrix = job?.strategy?.matrix && typeof job.strategy.matrix === "object" ? job.strategy.matrix : {};
  const expand = (label) => {
    const m = /^\$\{\{\s*matrix\.([A-Za-z0-9_-]+)\s*\}\}$/.exec(String(label).trim());
    if (!m) return [String(label)];
    const values = asList(matrix[m[1]]).map(String);
    for (const entry of asList(matrix.include)) if (entry && typeof entry === "object" && entry[m[1]] !== undefined) values.push(String(entry[m[1]]));
    return values.length ? values : [String(label)];
  };
  const runsOn = job?.["runs-on"];
  if (runsOn && typeof runsOn === "object" && !Array.isArray(runsOn)) return asList(runsOn.labels).flatMap(expand); // { group, labels }
  return asList(runsOn).flatMap(expand);
}

// ---- cone-mode sparse checkout -------------------------------------------------------------------------------------------------

/** Whether a cone-mode sparse checkout of directories `dirs` materializes the tracked file `file`: a root file, a file under a
 * listed directory, or a file directly in a parent directory of one (git's cone rules). */
export function coneIncludesFile(dirs, file) {
  const parent = path.posix.dirname(file);
  if (parent === ".") return true;
  return dirs.some((d) => file.startsWith(`${d}/`) || d === parent || d.startsWith(`${parent}/`));
}
/** Whether the cone materializes anything of the tracked directory `dir` that a step would read as that directory. */
const coneIncludesDir = (dirs, dir) => dirs.some((d) => dir === d || dir.startsWith(`${d}/`) || d.startsWith(`${dir}/`));

// ---- CA-07 checkout-step-input ---------------------------------------------------------------------------------------------------

const WORKSPACE_PREFIX = /^(?:\$\{\{\s*github\.workspace\s*\}\}|\$\{?GITHUB_WORKSPACE\}?|\$env:GITHUB_WORKSPACE|%GITHUB_WORKSPACE%)[\\/]+/i;
const PATH_TOKEN = /[A-Za-z0-9_.@${}:%-][A-Za-z0-9_.@${}:%\\/-]*[\\/][A-Za-z0-9_.@\\/-]*[A-Za-z0-9_-]/g;
const SCRIPT_COMMENT = /^\s*(?:#|REM\b|::)/i;
// git reads its object database, not the checkout: `git diff -- dir`, `git show rev:path` name no file of the working tree.
const GIT_COMMAND = /^\s*(?:[A-Za-z_][A-Za-z0-9_]*=\S*\s+)*(?:if\s+!?\s*)?git\s/;

/** The literal repository paths a step's script names: quoted strings and bare words that contain a separator, with a
 * workspace prefix or a leading ./ removed and backslashes read as separators. Comment lines, git command lines and words with a
 * glob character are skipped. */
export function scriptPaths(script) {
  const out = new Set();
  for (const line of String(script ?? "").split("\n")) {
    if (SCRIPT_COMMENT.test(line) || GIT_COMMAND.test(line)) continue;
    const pieces = [...line.matchAll(/"([^"]*)"|'([^']*)'|(\S+)/g)].map((m) => m[1] ?? m[2] ?? m[3]);
    for (const piece of pieces) {
      if (/[*?]/.test(piece)) continue; // a glob or a case pattern names no one path
      for (const raw of piece.match(PATH_TOKEN) ?? []) {
        const token = raw.trim().replace(WORKSPACE_PREFIX, "").replace(/\\+/g, "/").replace(/^\.\//, "").replace(/\/+$/, "");
        if (!token || token.startsWith("/") || token.startsWith("../") || /[${}%]|\s/.test(token)) continue;
        out.add(token);
      }
    }
  }
  return [...out];
}

function checkoutStepInput({ root, files }) {
  const out = [];
  const tracked = new Set(files);
  const trackedDirs = new Set();
  for (const f of files) for (let d = path.posix.dirname(f); d !== "."; d = path.posix.dirname(d)) trackedDirs.add(d);
  for (const wf of workflows(root, files)) {
    if (wf.error) {
      if (/sparse-checkout\s*:/.test(wf.text)) out.push({ file: wf.file, subject: "parse", problem: `${wf.file} uses a sparse checkout but cannot be read by the strict workflow parser (${wf.error}), so its step inputs cannot be checked`, fix: "write the workflow in the YAML subset packages/release/workflow-yaml.mjs reads" });
      continue;
    }
    for (const [jobId, job] of Object.entries(wf.doc?.jobs ?? {})) {
      const checkout = selfCheckout(job);
      if (!checkout || checkout.sparse === null || !checkout.cone || checkout.sparse.length === 0) continue;
      const base = checkout.path;
      const relative = (p) => (base && p.startsWith(`${base}/`) ? p.slice(base.length + 1) : base ? null : p);
      const missing = new Map(); // repo path -> step name
      job.steps.slice(checkout.index + 1).forEach((step, i) => {
        if (!step || typeof step !== "object") return;
        const name = String(step.name ?? step.uses ?? `step ${checkout.index + 2 + i}`);
        const candidates = [];
        const wd = step["working-directory"] ?? job.defaults?.run?.["working-directory"];
        if (typeof wd === "string" && step.run !== undefined) candidates.push(wd.replace(WORKSPACE_PREFIX, "").replace(/\\+/g, "/").replace(/^\.\/|\/+$/g, ""));
        if (typeof step.run === "string") candidates.push(...scriptPaths(step.run));
        for (const candidate of candidates) {
          const rel = relative(candidate);
          if (rel === null || rel === "" || rel === ".") continue;
          const isFile = tracked.has(rel);
          const isDir = !isFile && trackedDirs.has(rel);
          if (!isFile && !isDir) continue;
          const present = isFile ? coneIncludesFile(checkout.sparse, rel) : coneIncludesDir(checkout.sparse, rel);
          if (!present && !missing.has(rel)) missing.set(rel, name);
        }
      });
      for (const [rel, stepName] of missing) {
        out.push({
          file: wf.file, subject: `${jobId}:${rel}`,
          problem: `${wf.file} job ${jobId}: step "${stepName}" reads ${rel}, which its sparse checkout (${checkout.sparse.join(", ")}) does not materialize`,
          fix: `add ${path.posix.dirname(rel) === "." ? rel : tracked.has(rel) ? path.posix.dirname(rel) : rel} to the job's sparse-checkout list, or stop reading it`,
        });
      }
    }
  }
  return out;
}

// ---- CA-11 checkout-longpaths ----------------------------------------------------------------------------------------------------

// Win32 without long-path support: a full file path is at most MAX_PATH - 1 = 259 characters, and CreateDirectoryW refuses a
// directory path of 248 (MAX_PATH - 12) or more. Git for Windows applies these unless core.longpaths is true.
export const WIN_MAX_FILE = 259;
export const WIN_MAX_DIR = 247;
const HOSTED_WINDOWS = /^windows-(?:latest|\d{4}|11-arm|\d+[a-z0-9-]*)$/i;
/** A git boolean: true, false, or "unknown" for anything git would not read as one plainly. */
const gitBool = (value) => (/^(?:true|1|yes|on)$/i.test(String(value).trim()) ? true : /^(?:false|0|no|off)$/i.test(String(value).trim()) ? false : "unknown");
// The one script line that sets long paths: the whole line is the command (no echo, no condition, no substitution).
const SET_LINE = /^git\s+config\s+(--system|--global)\s+(?:--bool\s+)?(?:--(?:add|replace-all)\s+)?core\.longpaths\s+([A-Za-z0-9]+)\s*;?$/i;
// Lines a step may carry beside it without changing whether it runs: blank lines, comments, strict-mode preambles.
const NEUTRAL_LINE = /^(?:$|#|set\s+-[euxo]+(?:\s+pipefail)?\s*$|\$ErrorActionPreference\s*=\s*['"]Stop['"]\s*;?$)/i;
const STANDARD_SHELLS = new Set(["bash", "pwsh", "powershell", "sh", "cmd"]);
const MENTIONS_LONGPATHS = /core\.longpaths/i;

/** The core.longpaths value git takes from the config environment (GIT_CONFIG_COUNT, GIT_CONFIG_KEY_n, GIT_CONFIG_VALUE_n):
 * null when the environment sets none, true or false for the LAST pair below COUNT that names it (git applies pairs in index
 * order and the last value wins), or "unknown" for an expression, a COUNT git would refuse, or a value that is not a plain
 * boolean, or "error" when a pair below COUNT lacks its KEY_n or VALUE_n (git refuses to run then). Pair n counts only when
 * COUNT is a positive integer above n; an empty COUNT is 0. */
export function envLongpaths(env) {
  if (!env || typeof env !== "object") return null;
  const values = Object.entries(env).filter(([k]) => /^GIT_CONFIG_(?:COUNT|KEY_\d+|VALUE_\d+)$/.test(k));
  if (values.some(([, v]) => /\$\{\{/.test(String(v)))) return "unknown";
  if (env.GIT_CONFIG_COUNT === undefined) return null;
  const count = String(env.GIT_CONFIG_COUNT).trim();
  if (count === "" || count === "0") return null;
  if (!/^[1-9]\d*$/.test(count)) return "unknown";
  for (let i = 0; i < Number(count); i += 1) if (env[`GIT_CONFIG_KEY_${i}`] === undefined || env[`GIT_CONFIG_VALUE_${i}`] === undefined) return "error";
  let value = null;
  for (let i = 0; i < Number(count); i += 1) {
    if (String(env[`GIT_CONFIG_KEY_${i}`] ?? "").trim().toLowerCase() === "core.longpaths") value = gitBool(env[`GIT_CONFIG_VALUE_${i}`] ?? "");
  }
  return value;
}

/** Whether a step's `if:` provably lets it run on the Windows legs this rule judges: none, `true`, `always()`, `success()`, or
 * `runner.os == 'Windows'` (true on exactly those legs). Anything else cannot be judged from the file. */
function alwaysRuns(condition) {
  if (condition === undefined) return true;
  const text = String(condition).trim().replace(/^\$\{\{\s*([\s\S]*?)\s*\}\}$/, "$1").trim();
  if (/^(?:true|always\(\)|success\(\))$/.test(text)) return true;
  if (/^runner\.os\s*==\s*['"]Windows['"]$/.test(text)) return true;
  return false;
}

/** A pre-checkout step judged for long paths: "none" (it does not touch them), "unsupported" (it touches them in a way this
 * check cannot evaluate: a condition, echo or other shell text, a custom shell, an action, a value git would not read plainly),
 * or { system?, global? } with the value its git config lines leave in each file (the last line for a file wins). */
function judgeStep(step) {
  if (!step || typeof step !== "object") return "none";
  if (typeof step.uses === "string") return MENTIONS_LONGPATHS.test(JSON.stringify(step)) ? "unsupported" : "none";
  if (typeof step.run !== "string" || !MENTIONS_LONGPATHS.test(step.run)) return "none";
  const shell = step.shell === undefined ? "bash" : String(step.shell).trim();
  if (!alwaysRuns(step.if) || !STANDARD_SHELLS.has(shell) || step["continue-on-error"] !== undefined) return "unsupported";
  const sets = {};
  for (const line of step.run.split("\n").map((l) => l.trim())) {
    if (NEUTRAL_LINE.test(line)) continue;
    const m = SET_LINE.exec(line);
    if (!m) return "unsupported";
    const value = gitBool(m[2]);
    if (value === "unknown") return "unsupported";
    sets[m[1].slice(2).toLowerCase()] = value;
  }
  return Object.keys(sets).length ? sets : "unsupported";
}

/** core.longpaths as git reads it at the checkout step: the config environment overrides the files, the global file overrides
 * the system file, and within a source the last value wins; Git for Windows on a hosted runner defaults to false. Returns
 * true, false, "unknown", or "error" (an incomplete GIT_CONFIG pair set: git refuses to run). */
export function effectiveLongpaths(steps, env) {
  const files = { system: null, global: null };
  let unknown = false;
  for (const step of steps) {
    const judged = judgeStep(step);
    if (judged === "unsupported") unknown = true;
    else if (judged !== "none") Object.assign(files, judged);
  }
  const fromEnv = envLongpaths(env);
  if (fromEnv === "unknown" || fromEnv === "error") return fromEnv;
  if (fromEnv !== null) return fromEnv;
  if (unknown) return "unknown";
  return files.global ?? files.system ?? false;
}

/** The workspace prefix a GitHub-hosted Windows runner checks a repository out under: D:\a\<name>\<name>\ (plus with.path). */
export const windowsWorkspace = (name, checkoutPath = "") => `D:\\a\\${name}\\${name}\\${checkoutPath ? `${checkoutPath.replace(/\//g, "\\")}\\` : ""}`;

/** The GitHub repository name the runner names its workspace after, from the checked repository's remote `origin` URL (read with
 * git config, no network); null when there is none or it is not a GitHub URL. Never the checker's own directory name. */
export function repositoryName(repo) {
  if (!repo) return null;
  const r = spawnSync("git", ["-C", String(repo), "config", "--get", "remote.origin.url"], { encoding: "utf8", windowsHide: true });
  const m = /github\.com[:/]+[^/\s]+\/([^/\s]+?)(?:\.git)?\/?$/i.exec(String(r.stdout ?? "").trim());
  return r.status === 0 && m ? m[1] : null;
}

/** The tracked paths a checkout materializes that are too long for Win32 under `prefix`: { files, dirs, longest }. */
export function overLongPaths(files, prefix, sparse = null) {
  const kept = sparse ? files.filter((f) => coneIncludesFile(sparse, f)) : files;
  const tooLong = [];
  const dirs = new Set();
  for (const f of kept) {
    if (prefix.length + f.length > WIN_MAX_FILE) tooLong.push(f);
    for (let d = path.posix.dirname(f); d !== "."; d = path.posix.dirname(d)) if (prefix.length + d.length > WIN_MAX_DIR) dirs.add(d);
  }
  const longest = [...tooLong, ...dirs].sort((a, b) => b.length - a.length || (a < b ? -1 : 1))[0] ?? null;
  return { files: tooLong, dirs: [...dirs], longest };
}

function checkoutLongpaths({ root, files, repo, repositoryName: given }) {
  const out = [];
  const name = given !== undefined ? given : repositoryName(repo);
  for (const wf of workflows(root, files)) {
    if (wf.error) {
      if (/windows/i.test(wf.text) && /actions\/checkout@/.test(wf.text)) out.push({ file: wf.file, subject: "parse", problem: `UNSUPPORTED: ${wf.file} checks out on a Windows runner but cannot be read by the strict workflow parser (${wf.error}), so its long-path capability cannot be checked`, fix: "write the workflow in the YAML subset packages/release/workflow-yaml.mjs reads" });
      continue;
    }
    for (const [jobId, job] of Object.entries(wf.doc?.jobs ?? {})) {
      const labels = runnerLabels(job);
      if (labels.some((l) => /^self-hosted$/i.test(l))) continue;
      const windows = labels.filter((l) => HOSTED_WINDOWS.test(l.trim()));
      if (!windows.length) continue;
      const checkout = selfCheckout(job);
      if (!checkout || (checkout.sparse !== null && !checkout.cone)) continue;
      // Unknown repository: judge with the shortest possible name (one character); what fails there fails under any name, and
      // what fails only under a longer one is reported as UNSUPPORTED rather than passed.
      const prefix = windowsWorkspace(name ?? "x", checkout.path);
      const long = overLongPaths(files, prefix, checkout.sparse);
      const unknownWide = name ? null : overLongPaths(files, windowsWorkspace("x".repeat(100), checkout.path), checkout.sparse);
      if (!long.longest && !unknownWide?.longest) continue;
      const env = { ...(wf.doc.env ?? {}), ...(job.env ?? {}), ...(checkout.step.env ?? {}) };
      const effective = effectiveLongpaths(job.steps.slice(0, checkout.index), env);
      if (effective === true) continue;
      const unsupported = effective === "unknown";
      if (effective === "error") {
        out.push({ file: wf.file, subject: jobId, problem: `${wf.file} job ${jobId} (${windows.join(", ")}) sets GIT_CONFIG_COUNT with a pair below it missing its GIT_CONFIG_KEY_n or GIT_CONFIG_VALUE_n: git refuses to run, so the checkout of ${long.longest ? long.files.length : unknownWide.files.length} over-long tracked path(s) has no Git long paths`, fix: "give every index below GIT_CONFIG_COUNT both a GIT_CONFIG_KEY_n and a GIT_CONFIG_VALUE_n, or lower the count" });
        continue;
      }
      const after = job.steps.slice(checkout.index + 1).some((s) => s && typeof s.run === "string" && MENTIONS_LONGPATHS.test(s.run));
      if (!long.longest) {
        out.push({ file: wf.file, subject: jobId, problem: `UNSUPPORTED: ${wf.file} job ${jobId} (${windows.join(", ")}) checks out ${unknownWide.files.length} tracked path(s) that exceed the Win32 limit under D:\\a\\<repo>\\<repo>\\ for a long enough repository name, and the repository name is unknown (no GitHub remote origin), without proven Git long paths`, fix: "enable core.longpaths before the checkout, or check a clone whose origin names the GitHub repository" });
        continue;
      }
      out.push({
        file: wf.file, subject: jobId,
        problem: `${unsupported ? "UNSUPPORTED: " : ""}${wf.file} job ${jobId} (${windows.join(", ")}) checks out ${long.files.length} tracked file path(s) longer than ${WIN_MAX_FILE} characters under ${prefix}${name ? "" : " (repository name unknown; shortest possible)"} (longest: ${prefix.length + long.longest.length} characters, ${long.longest}) without ${unsupported ? "provable " : ""}Git long paths${unsupported ? " (a step or environment before the checkout touches core.longpaths in a way this check cannot prove runs: a condition, echo or other shell text, a custom shell, an action, an expression, or a GIT_CONFIG_COUNT git would refuse)" : ""}${after ? " (core.longpaths is enabled only after the checkout)" : ""}: git fails with "Filename too long"`,
        fix: "add an unconditional step before the checkout whose whole script is `git config --system core.longpaths true`, or leave the long paths out of the job's sparse checkout",
      });
    }
  }
  return out;
}

// ---- CA-09 source-relation -------------------------------------------------------------------------------------------------------

/** The relations RightKit knows: each names a source file, a selector of string values in it, and the target file whose rows they
 * must name. An app adds its own in .rightkit/source-relations.json ({ "schema": 1, "relations": [ ... ] }). */
export const SOURCE_RELATIONS = Object.freeze([
  Object.freeze({
    id: "proven-controls-status-row",
    source: "factory/tools/proven-controls/hand-entries.json",
    select: "[*].statusDocBlock",
    target: "docs/sr1-status.md",
    adapter: "markdown-table-first-cell",
    about: "SwiftRight: each hand entry's statusDocBlock names a row of docs/sr1-status.md (factory/tools/proven-controls/generate.py status_doc_rows)",
  }),
]);
export const APP_RELATIONS_FILE = ".rightkit/source-relations.json";
const ADAPTERS = new Set(["markdown-table-first-cell"]);

/** The string values `select` picks from parsed JSON: dot-separated keys, `[*]` for every element of an array. Returns
 * [{ at, value }] with `at` the concrete path; a missing key yields { at, value: undefined }. */
export function selectValues(doc, select) {
  const steps = String(select).match(/\[\*\]|[^.[\]]+/g) ?? [];
  let current = [{ at: "", value: doc }];
  for (const step of steps) {
    const next = [];
    for (const { at, value } of current) {
      if (step === "[*]") { if (Array.isArray(value)) value.forEach((v, i) => next.push({ at: `${at}[${i}]`, value: v })); else next.push({ at: `${at}[*]`, value: undefined }); }
      else next.push({ at: at ? `${at}.${step}` : step, value: value && typeof value === "object" && !Array.isArray(value) ? value[step] : undefined });
    }
    current = next;
  }
  return current;
}

/** The row ids of a Markdown status document: the first word of the first cell of each table row before the first `---` line
 * (the same reading as SwiftRight's status_doc_rows). A document whose lines are escaped into one line has no rows. */
export function markdownFirstCells(text) {
  const ids = new Set();
  for (const line of String(text).replace(/\r\n/g, "\n").split("\n---\n")[0].split("\n")) {
    if (!line.startsWith("|")) continue;
    const cell = line.trim().replace(/^\|+|\|+$/g, "").split("|")[0].trim();
    const m = /^([A-Za-z0-9][\w.-]*)\s/.exec(`${cell} `);
    if (m) ids.add(m[1]);
  }
  return ids;
}

function relationsOf(root, files) {
  const relations = SOURCE_RELATIONS.map((r) => ({ ...r, origin: "RightKit" }));
  const problems = [];
  if (files.includes(APP_RELATIONS_FILE)) {
    let doc = null;
    try { doc = JSON.parse(readText(root, APP_RELATIONS_FILE) ?? ""); } catch (error) { problems.push(`${APP_RELATIONS_FILE} is not JSON: ${error.message}`); }
    if (doc) {
      if (doc.schema !== 1 || !Array.isArray(doc.relations)) problems.push(`${APP_RELATIONS_FILE} is not { "schema": 1, "relations": [...] }`);
      else doc.relations.forEach((r, i) => {
        const ok = r && typeof r === "object" && ["id", "source", "select", "target", "adapter"].every((k) => typeof r[k] === "string" && r[k]);
        if (!ok) problems.push(`${APP_RELATIONS_FILE} relations[${i}] needs id, source, select, target and adapter strings`);
        else if (!ADAPTERS.has(r.adapter)) problems.push(`${APP_RELATIONS_FILE} relations[${i}] adapter "${r.adapter}" is not one of ${[...ADAPTERS].join(", ")}`);
        else relations.push({ ...r, origin: APP_RELATIONS_FILE });
      });
    }
  }
  return { relations, problems };
}

function sourceRelation({ root, files }) {
  const out = [];
  const { relations, problems } = relationsOf(root, files);
  for (const problem of problems) out.push({ file: APP_RELATIONS_FILE, subject: "declaration", problem, fix: "fix the declaration" });
  const present = new Set(files);
  for (const rel of relations) {
    if (!present.has(rel.source)) continue; // the relation applies only to a tree that has its source
    let doc;
    try { doc = JSON.parse(readText(root, rel.source) ?? ""); } catch (error) {
      out.push({ file: rel.source, subject: `${rel.id}:parse`, problem: `${rel.source} is not JSON (${error.message}), so its ${rel.select} references to ${rel.target} cannot be checked`, fix: "commit valid JSON" });
      continue;
    }
    const text = present.has(rel.target) ? readText(root, rel.target) : null;
    if (text === null) {
      out.push({ file: rel.source, subject: `${rel.id}:target`, problem: `${rel.source} references rows of ${rel.target} (${rel.id}), which is not in the tree`, fix: `commit ${rel.target} or remove the references` });
      continue;
    }
    const rows = markdownFirstCells(text);
    for (const { at, value } of selectValues(doc, rel.select)) {
      if (typeof value !== "string" || !value) {
        out.push({ file: rel.source, subject: `${rel.id}:${at}`, problem: `${rel.source} ${at} is ${value === undefined ? "missing" : JSON.stringify(value)}; it must name a row of ${rel.target}`, fix: `set it to the id in the first cell of a ${rel.target} table row` });
      } else if (!rows.has(value)) {
        out.push({ file: rel.source, subject: `${rel.id}:${value}`, problem: `${rel.source} ${at} names row "${value}", which ${rel.target} does not have (${rows.size} table row id(s) before its first --- line${rows.size === 0 ? "; a document escaped into one line has none" : ""})`, fix: `restore the "${value}" row in ${rel.target}, or point ${at} at a row that exists` });
      }
    }
  }
  return out;
}

export const CONSUMER_INPUT_RULES = Object.freeze([
  { id: "checkout-step-input", gap: "CA-07", run: checkoutStepInput },
  { id: "source-relation", gap: "CA-09", run: sourceRelation },
  { id: "checkout-longpaths", gap: "CA-11", run: checkoutLongpaths },
]);
