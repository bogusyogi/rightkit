#!/usr/bin/env node
// right-issues: writes issues.html at a repository root from that repository's GitHub issues.
import { execFileSync } from "node:child_process";
import { existsSync, mkdirSync, readFileSync, realpathSync, renameSync, statSync, unlinkSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { buildModel, fromRestIssues, normaliseGroups, renderHtml } from "../tracker.mjs";

const HELP = `right-issues [options]

Writes issues.html (self-contained, offline) from ALL of the repository's GitHub issues, open and
closed. The inventory is fetched with pagination; if it is incomplete or fails, an existing
issues.html is left untouched and the exit code is non-zero.

  --root <dir>         repository root (default: current directory)
  --repo <owner/name>  GitHub repository (default: gh repo view in --root)
  --out <file>         output (default: <root>/issues.html)
  --groups <file>      groups file (default: <root>/.rightkit/issue-groups.json, optional)
  --issues <file>      read issues JSON (gh REST or model shape) from a file instead of gh
  --who <name>         who decisions go to, shown as "Needs <name>" (default: Adrian)
  --closed-days <n>    the default view shows closed issues from the last n days (default: 14);
                       every closed issue stays in the page behind the Done and All filters
  --max <n>            refuse (write nothing) when the repository has more than n issues (default: no cap)
  --version, --help

Status chips come from labels: needs-decision, needs-live-run, built-needs-check,
blocked, tracking. Groups come from the groups file, then area:/component:/feature:/group:
labels, then the file's title rules, else "Other".`;

const VALUE_OPTIONS = new Set(["root", "repo", "out", "groups", "issues", "who", "closed-days", "max"]);

export function parseArgs(argv) {
  const opts = {};
  for (let i = 0; i < argv.length; i++) {
    const arg = argv[i];
    if (arg === "--help" || arg === "-h") opts.help = true;
    else if (arg === "--version") opts.version = true;
    else if (arg.startsWith("--")) {
      const eq = arg.indexOf("=");
      const key = eq > 0 ? arg.slice(2, eq) : arg.slice(2);
      if (!VALUE_OPTIONS.has(key)) throw new Error(`unknown option: --${key} (see --help)`);
      const value = eq > 0 ? arg.slice(eq + 1) : argv[++i];
      if (value === undefined || value === "" || (eq < 0 && value.startsWith("--"))) throw new Error(`--${key} needs a value`);
      opts[key] = value;
    } else throw new Error(`unexpected argument: ${arg}`);
  }
  return opts;
}

export function wholeNumber(value, name, fallback, min = 0) {
  if (value === undefined) return fallback;
  const n = /^\d+$/.test(String(value)) ? Number(value) : NaN;
  if (!Number.isInteger(n) || n < min) throw new Error(`--${name} must be a whole number of at least ${min}`);
  return n;
}

function gh(args, cwd) {
  try {
    return execFileSync("gh", args, { cwd, encoding: "utf8", maxBuffer: 1 << 28, stdio: ["ignore", "pipe", "pipe"] });
  } catch (error) {
    if (error.code === "ENOENT") throw new Error("the GitHub CLI (gh) is not installed or not on PATH; install it or pass --issues <file>");
    const detail = String(error.stderr || error.message).trim().split("\n")[0];
    throw new Error(`gh ${args.slice(0, 2).join(" ")} failed${detail ? `: ${detail}` : ""} (run gh auth status, or pass --repo and --issues)`);
  }
}

/** Default repository lookup; injectable so tests need no gh. */
export function defaultResolveRepo(cwd) {
  return parseGh(gh(["repo", "view", "--json", "nameWithOwner"], cwd), "repo view").nameWithOwner;
}

/**
 * Default fetcher: every issue (open and closed, pull requests filtered out) through the paginated
 * REST API, cross-checked against the search total. Returns { issues, total } in the model's shape.
 */
export function defaultFetchIssues({ repo, cwd }) {
  const pages = parseGh(gh(["api", "--paginate", "--slurp", `repos/${repo}/issues?state=all&per_page=100`], cwd), "issue list");
  if (!Array.isArray(pages)) throw new Error("gh returned an unexpected issue list shape");
  const raw = pages.flatMap((page) => {
    if (!Array.isArray(page)) throw new Error("gh returned an unexpected issue page shape");
    return page;
  });
  let total = null;
  const search = parseGh(gh(["api", `search/issues?q=${encodeURIComponent(`repo:${repo} is:issue`)}&per_page=1`], cwd), "issue count");
  if (Number.isInteger(search.total_count)) total = search.total_count;
  return { issues: fromRestIssues(raw), total };
}

/** Reads JSON, tolerating a UTF-8 byte order mark (Windows editors add one). */
function readJson(path, what) {
  let text;
  try {
    text = readFileSync(path, "utf8");
  } catch (error) {
    throw new Error(`cannot read ${what} ${path}: ${error.message}`);
  }
  try {
    return JSON.parse(text.replace(/^﻿/, ""));
  } catch (error) {
    throw new Error(`${what} ${path} is not valid JSON: ${error.message}`);
  }
}

function parseGh(text, what) {
  try {
    return JSON.parse(text);
  } catch {
    throw new Error(`gh returned unreadable ${what} output`);
  }
}

/**
 * @param {string[]} argv
 * @param {{ fetchIssues?: Function, resolveRepo?: Function, now?: Date }} [deps] injected for tests
 */
export function main(argv, { fetchIssues = defaultFetchIssues, resolveRepo = defaultResolveRepo, now } = {}) {
  const opts = parseArgs(argv);
  if (opts.help) return void console.log(HELP);
  if (opts.version) {
    const pkg = JSON.parse(readFileSync(join(dirname(fileURLToPath(import.meta.url)), "..", "package.json"), "utf8"));
    return void console.log(pkg.version);
  }
  // Every local argument is validated before the first gh call.
  const root = resolve(opts.root || ".");
  if (!existsSync(root) || !statSync(root).isDirectory()) throw new Error(`--root is not a directory: ${root}`);
  if (opts.repo !== undefined && !/^[\w.-]+\/[\w.-]+$/.test(opts.repo)) throw new Error(`not an owner/name repository: ${opts.repo}`);
  const max = opts.max === undefined ? Infinity : wholeNumber(opts.max, "max", undefined, 1);
  const closedDays = wholeNumber(opts["closed-days"], "closed-days", 14);
  const groupsPath = resolve(root, opts.groups || ".rightkit/issue-groups.json");
  if (opts.groups && !existsSync(groupsPath)) throw new Error(`groups file not found: ${groupsPath}`);
  const groupsFile = existsSync(groupsPath) ? readJson(groupsPath, "groups file") : undefined;
  normaliseGroups(groupsFile);
  const out = resolve(root, opts.out || "issues.html");
  if (existsSync(out) && statSync(out).isDirectory()) throw new Error(`--out is a directory: ${out}`);
  let issues = opts.issues ? fromRestIssues(readJson(resolve(root, opts.issues), "issues file")) : undefined;

  const repo = opts.repo || resolveRepo(root);
  if (!/^[\w.-]+\/[\w.-]+$/.test(String(repo))) throw new Error(`not an owner/name repository: ${repo}`);
  if (!issues) {
    const fetched = fetchIssues({ repo, cwd: root, max });
    issues = fetched.issues;
    if (!Array.isArray(issues)) throw new Error("issue retrieval returned no list; issues.html not written");
    if (Number.isInteger(fetched.total) && fetched.total !== issues.length) {
      throw new Error(`issue retrieval is incomplete: fetched ${issues.length} of ${fetched.total} issues; issues.html not written`);
    }
  }
  if (issues.length > max) throw new Error(`${repo} has ${issues.length} issues, more than --max ${max}; issues.html not written (raise --max)`);
  const model = buildModel({ issues, groupsFile, repo, closedDays, ...(now ? { now } : {}) });
  mkdirSync(dirname(out), { recursive: true });
  const temp = `${out}.${process.pid}.tmp`;
  try {
    writeFileSync(temp, renderHtml(model, { who: opts.who || "Adrian" }));
    renameSync(temp, out);
  } catch (error) {
    try { unlinkSync(temp); } catch { /* nothing to remove */ }
    throw error;
  }
  console.log(`issues.html written: ${out} ${issues.length} issues ${JSON.stringify(model.stats)}`);
}

// Run when invoked directly, including through a package-manager bin link.
const real = (path) => realpathSync.native(path).toLowerCase();
const invoked = process.argv[1] && existsSync(process.argv[1]) ? real(process.argv[1]) : "";
if (invoked === real(fileURLToPath(import.meta.url))) {
  try {
    main(process.argv.slice(2));
  } catch (error) {
    console.error(`right-issues: ${error.message}`);
    process.exitCode = 1;
  }
}
