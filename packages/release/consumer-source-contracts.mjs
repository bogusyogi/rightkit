// Consumer source contracts: files-only check rules for mechanisms that failed consumer CI minutes into a run, read as data before
// any build (SellRight's historical Class A rows of the consumer audit). Like the shell contract, every rule is
// { id, gap, run({ root, files }) } and returns findings [{ file, subject, problem, fix }]: `root` holds the exported tree, `files`
// is the posix path of every tracked file, `problem` starts with "<file>:<line>", and rule|file|subject (no line number) is the
// identity an app baseline names. Node builtins only (preflight runs with no node_modules); no repository code, workflow, script,
// Vite or Caddy config is executed, imported or evaluated: workflow YAML, package.json and Caddyfiles are read as text.
//
//   workflow-action-pair     CA-04  one job runs github/codeql-action init/autobuild/analyze at different refs (SellRight dependabot
//                                   PRs #263/#264 bumped init or analyze alone to v4.39.0: the job fails at analyze)
//   referenced-input         CA-07  a command input a checkout does not have: a package.json script or workflow `run:` that runs
//                                   `node|sh|bash|python <file>` absent from the commit or outside the repository, a `pnpm --dir`
//                                   / `npm --prefix` directory, workflow `working-directory` or `cache-dependency-path` that is absent
//   caddy-global-options     CA-08  a Caddyfile whose first block is (or, with its {$VAR} address empty, becomes) the global options
//                                   block while it holds site directives such as `encode` (Caddy refuses "unrecognized global option")
//
// Legitimate exceptions stay accepted: build outputs (dist/, build/, out/, target/, node_modules/ ...), anything spelled with a
// variable, glob or expression, directories a workflow checks out or downloads into (`path:` of actions/checkout or
// download-artifact), fixtures (a fixtures/ or testdata/ tree), and Caddy global options this rule does not know (plugins add them).
import path from "node:path";
import fs from "node:fs";

const SKIPPED_TREE = /(^|\/)(node_modules|vendor|target)\//;
const FIXTURE_TREE = /(^|\/)(fixtures?|__fixtures__|testdata|test-fixtures)\//;
const scanned = (file) => !SKIPPED_TREE.test(file);
const readText = (root, rel) => { try { return fs.readFileSync(path.join(root, rel), "utf8"); } catch { return null; } };
const lineOf = (text, offset) => text.slice(0, offset).split("\n").length;
const workflowFiles = (files) => files.filter((f) => /^\.github\/workflows\/[^/]+\.ya?ml$/.test(f)).sort();

// ---- a line reader for GitHub workflow YAML (block style, the form workflows are written in) ------------------------------------
const unquote = (value) => {
  const v = value.trim();
  if ((v.startsWith('"') && v.endsWith('"') && v.length > 1) || (v.startsWith("'") && v.endsWith("'") && v.length > 1)) return v.slice(1, -1);
  return v;
};
/** `value` without a trailing ` # comment` (outside quotes). */
const stripComment = (value) => {
  let quote = null;
  for (let i = 0; i < value.length; i += 1) {
    const c = value[i];
    if (quote) { if (c === quote) quote = null; continue; }
    if (c === '"' || c === "'") quote = c;
    else if (c === "#" && (i === 0 || /\s/.test(value[i - 1]))) return value.slice(0, i).trimEnd();
  }
  return value;
};

/**
 * Every `key: value` of a block-style YAML document: { line, key, value, block, path, items }. `path` is the chain of keys above and
 * including this one; `items[i]` is the sequence-item number of path[i] (null outside a sequence item), so two keys of one step share
 * the item number at the step's depth. A block scalar (`|`, `>`) is `block: [{ line, text }]` and its lines are not read as keys.
 * Flow collections and anchors are not followed (workflows rarely use them; a key inside one is simply not seen).
 */
export function yamlEntries(text) {
  const lines = String(text ?? "").replace(/\r\n?/g, "\n").split("\n");
  const out = [];
  const stack = [];
  let nextItem = 0;
  for (let i = 0; i < lines.length; i += 1) {
    const raw = lines[i];
    if (/^\s*(#.*)?$/.test(raw) || /^\s*(---|\.\.\.)\s*$/.test(raw)) continue;
    let indent = raw.match(/^ */)[0].length;
    let rest = raw.slice(indent);
    let item = null;
    while (/^-(\s|$)/.test(rest)) {
      const dash = indent;
      while (stack.length && stack.at(-1).indent > dash) stack.pop();
      item = (nextItem += 1);
      const after = rest.slice(1);
      const gap = after.match(/^\s*/)[0].length;
      indent += 1 + gap;
      rest = after.slice(gap);
      if (rest === "") break;
    }
    const m = /^([A-Za-z0-9_.$/-]+|"[^"]*"|'[^']*')\s*:(?:\s+(.*)|\s*)$/.exec(rest);
    if (!m) continue;
    const key = unquote(m[1]);
    let value = m[2] === undefined ? "" : stripComment(m[2]);
    while (stack.length && stack.at(-1).indent >= indent) {
      const top = stack.pop();
      if (top.indent === indent && item === null) item = top.item;
    }
    const entry = { line: i + 1, key, value: unquote(value), block: null, path: [...stack.map((s) => s.key), key], items: [...stack.map((s) => s.item), item] };
    if (/^[|>][+-]?\d*$/.test(value.trim())) {
      const block = [];
      let j = i + 1;
      for (; j < lines.length; j += 1) {
        const l = lines[j];
        if (/^\s*$/.test(l)) { block.push({ line: j + 1, text: "" }); continue; }
        if (l.match(/^ */)[0].length <= indent) break;
        block.push({ line: j + 1, text: l.trim() });
      }
      while (block.length && block.at(-1).text === "") block.pop();
      entry.block = block;
      entry.value = "";
      i = j - 1;
    }
    out.push(entry);
    stack.push({ indent, key, item });
  }
  return out;
}

/** The steps of each job: Map(job -> [{ item, entries }]) and the job-level and workflow-level defaults.run.working-directory. */
function workflowJobs(entries) {
  const jobs = new Map();
  const stepOf = new Map();
  for (const e of entries) {
    if (e.path[0] !== "jobs" || e.path.length < 2) continue;
    const job = e.path[1];
    if (!jobs.has(job)) jobs.set(job, { steps: [], defaultsDir: null, line: e.line });
    const j = jobs.get(job);
    if (e.path.length === 5 && e.path[2] === "defaults" && e.path[3] === "run" && e.path[4] === "working-directory") j.defaultsDir = e;
    if (e.path[2] !== "steps" || e.path.length < 4 || e.items[3] === null) continue;
    const id = `${job}#${e.items[3]}`;
    if (!stepOf.has(id)) { const step = { item: e.items[3], entries: [] }; stepOf.set(id, step); j.steps.push(step); }
    stepOf.get(id).entries.push({ ...e, rel: e.path.slice(3) });
  }
  const topDefaultsDir = entries.find((e) => e.path.join(".") === "defaults.run.working-directory") ?? null;
  return { jobs, topDefaultsDir };
}
const stepGet = (step, ...rel) => step.entries.find((e) => e.rel.length === rel.length && e.rel.every((k, i) => k === rel[i])) ?? null;

// ---- CA-04: paired actions in one job run one version --------------------------------------------------------------------------
// CodeQL's init writes the database and configuration that autobuild and analyze read; the action refuses a job whose steps come from
// different releases. A ref is compared exactly: `@v4` beside `@<sha> # v4.38.3` is also two refs (the tag moves).
const PAIRED_ACTIONS = [{ repo: "github/codeql-action", parts: new Set(["init", "autobuild", "analyze", "resolve-environment"]) }];

function workflowActionPair({ root, files }) {
  const out = [];
  for (const file of workflowFiles(files)) {
    const text = readText(root, file);
    if (text === null || !text.includes("@")) continue;
    const { jobs } = workflowJobs(yamlEntries(text));
    for (const [job, { steps }] of jobs) {
      for (const family of PAIRED_ACTIONS) {
        const uses = [];
        for (const step of steps) {
          const u = stepGet(step, "uses");
          const m = u && /^([^@\s]+)@(\S+)/.exec(u.value);
          if (!m || !m[1].startsWith(`${family.repo}/`)) continue;
          const part = m[1].slice(family.repo.length + 1);
          if (!family.parts.has(part)) continue;
          const comment = /#\s*(\S+)/.exec(text.split("\n")[u.line - 1].slice(text.split("\n")[u.line - 1].indexOf(m[2]) + m[2].length));
          uses.push({ part, ref: m[2], label: comment ? `${m[2].slice(0, 12)} (${comment[1]})` : m[2], line: u.line });
        }
        const refs = [...new Set(uses.map((u) => u.ref))];
        if (refs.length < 2) continue;
        const first = uses[0];
        const odd = uses.find((u) => u.ref !== first.ref);
        out.push({
          file, subject: `${job}:${family.repo}`,
          problem: `${file}:${odd.line} job ${job} runs ${family.repo}/${odd.part}@${odd.label} but ${family.repo}/${first.part}@${first.label} (line ${first.line}): its steps must come from one release, or the job fails at ${odd.part === "init" ? first.part : odd.part}`,
          fix: `pin every ${family.repo} step of job ${job} (${[...new Set(uses.map((u) => u.part))].join(", ")}) to the same commit; bump them together`,
        });
      }
    }
  }
  return out;
}

// ---- CA-07: a command input the checkout does not have -------------------------------------------------------------------------
// Directories a build or install writes: a path through one of them is produced, not committed.
const PRODUCED_SEGMENT = /^(dist|dist-[^/]*|build|out|\.output|target|node_modules|coverage|\.next|\.nuxt|\.svelte-kit|\.turbo|\.cache)$/;
const DYNAMIC = /[$*?[\]{}%`~<>]|^\(|\)$/;
const SCRIPT_EXT = /\.(?:mjs|cjs|js|mts|cts|ts|tsx|sh|bash|py)$/;
// Interpreter flags that take the next word as their value.
const FLAG_VALUE = new Set(["--import", "--require", "-r", "--loader", "--experimental-loader", "--conditions", "-C", "--input-type", "--test-reporter", "--test-reporter-destination", "--env-file", "--title", "--stack-size", "-c", "-o", "-m", "-W", "-X"]);
const EVAL_FLAG = /^(-e|--eval|-p|--print|-c|-m)$/;

/** Shell words of one command line: quotes kept together, `&&`, `||`, `;`, `|` and newlines split commands. */
export function shellCommands(text) {
  const commands = [];
  let words = [];
  let word = "";
  let quote = null;
  let started = false;
  const endWord = () => { if (started) words.push(word); word = ""; started = false; };
  const endCommand = () => { endWord(); if (words.length) commands.push(words); words = []; };
  for (let i = 0; i < text.length; i += 1) {
    const c = text[i];
    if (quote) { if (c === quote) quote = null; else word += c; continue; }
    if (c === '"' || c === "'") { quote = c; started = true; continue; }
    if (c === "\\" && text[i + 1] === "\n") { i += 1; continue; }
    if (c === "#" && !started) { while (i < text.length && text[i] !== "\n") i += 1; endCommand(); continue; }
    if (c === "\n" || c === ";" || c === "&" || c === "|" || c === "(" || c === ")") { endCommand(); continue; }
    if (/\s/.test(c)) { endWord(); continue; }
    word += c; started = true;
  }
  endCommand();
  return commands;
}

/**
 * The repository inputs one command names, given its words: [{ kind: "file" | "dir", value }]. `cd` is returned as { kind: "cd" }.
 * Only shapes that are certainly paths are read: the script an interpreter runs, and the directory pnpm/npm/yarn is pointed at.
 */
export function commandInputs(words) {
  const w = words;
  let i = 0;
  while (i < w.length && /^[A-Za-z_][A-Za-z0-9_]*=/.test(w[i])) i += 1;
  while (i < w.length && ["env", "exec", "time", "nohup", "sudo", "command"].includes(w[i])) i += 1;
  const cmd = w[i];
  if (cmd === undefined) return [];
  if (cmd === "cd" && w[i + 1] !== undefined) return [{ kind: "cd", value: w[i + 1] }];
  const out = [];
  if (/^(node|bash|sh|python|python3)$/.test(cmd)) {
    for (let k = i + 1; k < w.length; k += 1) {
      const a = w[k];
      if (EVAL_FLAG.test(a) && cmd !== "sh" && cmd !== "bash") return out;
      if (a === "-c" && (cmd === "sh" || cmd === "bash")) return out;
      if (a === "--test" || a === "--") continue;
      if (a.startsWith("-")) { if (!a.includes("=") && FLAG_VALUE.has(a)) k += 1; continue; }
      if (SCRIPT_EXT.test(a) || a.includes("/")) out.push({ kind: "file", value: a });
      return out;
    }
    return out;
  }
  if (/^(pnpm|npm|yarn)$/.test(cmd)) {
    for (let k = i + 1; k < w.length; k += 1) {
      const a = w[k];
      const eq = /^(--dir|--prefix|--cwd)=(.+)$/.exec(a);
      if (eq) out.push({ kind: "dir", value: eq[2] });
      else if (["--dir", "-C", "--prefix", "--cwd"].includes(a) && w[k + 1] !== undefined) { out.push({ kind: "dir", value: w[k + 1] }); k += 1; }
    }
  }
  return out;
}

/** A path matcher for the tracked .gitignore files: a path (or a directory above it) the repository ignores is produced, not committed. */
export function gitignoreMatcher(root, files) {
  const rules = new Map();
  for (const f of files.filter((x) => /(^|\/)\.gitignore$/.test(x))) {
    const base = path.posix.dirname(f);
    const list = [];
    for (let line of String(readText(root, f) ?? "").replace(/\r\n?/g, "\n").split("\n")) {
      line = line.replace(/(?<!\\)\s+$/, "");
      if (!line || line.startsWith("#")) continue;
      const negate = line.startsWith("!");
      if (negate) line = line.slice(1);
      const dirOnly = line.endsWith("/");
      if (dirOnly) line = line.slice(0, -1);
      const anchored = line.includes("/");
      line = line.replace(/^\//, "");
      const body = line.replace(/[.+^${}()|[\]\\]/g, "\\$&").replace(/\*\*\//g, "\u0000").replace(/\/\*\*/g, "\u0001").replace(/\*/g, "[^/]*").replace(/\?/g, "[^/]").replace(/\u0000/g, "(?:.*/)?").replace(/\u0001/g, "/.*");
      list.push({ negate, dirOnly, re: new RegExp(anchored ? `^${body}$` : `(^|/)${body}$`) });
    }
    rules.set(base, list);
  }
  return (rel) => {
    const parts = rel.split("/");
    for (let n = 1; n <= parts.length; n += 1) {
      const candidate = parts.slice(0, n).join("/");
      const isDir = n < parts.length;
      let ignored = false;
      for (const [base, list] of rules) {
        if (base !== "." && !(candidate.startsWith(`${base}/`))) continue;
        const local = base === "." ? candidate : candidate.slice(base.length + 1);
        for (const r of list) if ((!r.dirOnly || isDir || n === parts.length) && r.re.test(local)) ignored = !r.negate;
      }
      if (ignored) return true;
    }
    return false;
  };
}

function trackedIndex(files) {
  const fileSet = new Set(files);
  const dirSet = new Set(["."]);
  for (const f of files) { let d = path.posix.dirname(f); while (d !== "." && !dirSet.has(d)) { dirSet.add(d); d = path.posix.dirname(d); } }
  return { fileSet, dirSet };
}
/** What `value` (relative to `cwd`, both posix and repository-relative) is: "outside", "skip" (dynamic or produced) or its normalized path. */
function resolveInput(cwd, value, produced = [], ignored = () => false) {
  if (DYNAMIC.test(value) || value === "" || value.startsWith("/")) return { state: "skip" };
  const joined = path.posix.normalize(path.posix.join(cwd, value));
  if (joined === ".." || joined.startsWith("../")) return { state: "outside", path: joined };
  const rel = joined.replace(/\/$/, "") || ".";
  if (rel.split("/").some((s) => PRODUCED_SEGMENT.test(s))) return { state: "skip" };
  if (produced.some((p) => rel === p || rel.startsWith(`${p}/`))) return { state: "skip" };
  if (rel !== "." && ignored(rel)) return { state: "skip" };
  return { state: "check", path: rel };
}

function checkInputs({ file, line, cwd, commands, index, produced, ignored, add, where }) {
  let dir = cwd;
  for (const words of commands) {
    for (const input of commandInputs(words)) {
      if (input.kind === "cd") {
        const r = resolveInput(dir, input.value, produced, ignored);
        if (r.state === "check" && index.dirSet.has(r.path)) dir = r.path; else dir = null;
        continue;
      }
      if (dir === null) continue;
      const r = resolveInput(dir, input.value, produced, ignored);
      if (r.state === "skip") continue;
      if (r.state === "outside") {
        add(`${input.kind}:${input.value}`, `${file}:${line} ${where} runs ${words.join(" ")}: ${input.value} resolves to ${r.path}, outside the repository, which no checkout of this commit has`, `commit the ${input.kind === "dir" ? "directory" : "file"} into this repository, or depend on the published package instead of a sibling checkout`);
        continue;
      }
      const present = input.kind === "dir" ? index.dirSet.has(r.path) : index.fileSet.has(r.path) || index.dirSet.has(r.path);
      if (!present) add(`${input.kind}:${r.path}`, `${file}:${line} ${where} runs ${words.join(" ")}: ${r.path} is not in this commit`, `commit ${r.path}, or point the command at the path that exists`);
    }
  }
}

function referencedInput({ root, files }) {
  const out = [];
  const index = trackedIndex(files.filter(scanned));
  const ignored = gitignoreMatcher(root, files);
  for (const file of files.filter((f) => /(^|\/)package\.json$/.test(f) && scanned(f) && !FIXTURE_TREE.test(f)).sort()) {
    const text = readText(root, file);
    let pkg;
    try { pkg = JSON.parse(String(text ?? "").replace(/^﻿/, "")); } catch { continue; }
    const scripts = pkg && typeof pkg.scripts === "object" && !Array.isArray(pkg.scripts) ? pkg.scripts : {};
    const dir = path.posix.dirname(file);
    for (const [name, command] of Object.entries(scripts)) {
      if (typeof command !== "string") continue;
      const offset = Math.max(0, text.indexOf(`"${name}"`));
      const add = (subject, problem, fix) => out.push({ file, subject: `script:${name}:${subject}`, problem, fix });
      checkInputs({ file, line: lineOf(text, offset), cwd: dir, commands: shellCommands(command), index, produced: [], ignored, add, where: `script "${name}"` });
    }
  }
  for (const file of workflowFiles(files)) {
    const text = readText(root, file);
    if (text === null) continue;
    const entries = yamlEntries(text);
    const { jobs, topDefaultsDir } = workflowJobs(entries);
    // Directories a step checks out or downloads into exist only at run time.
    const produced = [];
    for (const { steps } of jobs.values()) for (const step of steps) {
      const uses = stepGet(step, "uses")?.value ?? "";
      const p = stepGet(step, "with", "path");
      if (p && /^actions\/(checkout|download-artifact|cache)(\/|@)/.test(uses)) for (const v of (p.block ? p.block.map((b) => b.text) : [p.value])) {
        const r = resolveInput(".", v); if (r.state === "check") produced.push(r.path);
      }
    }
    const seen = new Set();
    const add = (subject, problem, fix) => { if (!seen.has(subject)) { seen.add(subject); out.push({ file, subject, problem, fix }); } };
    for (const [job, { steps, defaultsDir }] of jobs) {
      const jobDir = defaultsDir ?? topDefaultsDir;
      for (const step of steps) {
        const uses = stepGet(step, "uses")?.value ?? "";
        const wd = stepGet(step, "working-directory") ?? jobDir;
        let cwd = ".";
        if (wd) {
          const r = resolveInput(".", wd.value, produced, ignored);
          if (r.state === "outside") { add(`working-directory:${wd.value}`, `${file}:${wd.line} job ${job}: working-directory ${wd.value} is outside the repository`, "use a directory of this repository"); continue; }
          if (r.state === "skip") continue;
          if (!index.dirSet.has(r.path)) { add(`working-directory:${r.path}`, `${file}:${wd.line} job ${job}: working-directory ${r.path} is not in this commit (the step fails before its command runs)`, `commit ${r.path}, or point working-directory at the directory that exists`); continue; }
          cwd = r.path;
        }
        const cache = stepGet(step, "with", "cache-dependency-path");
        if (cache && /^actions\/setup-(node|python|go|java|dotnet)@/.test(uses)) {
          const values = cache.block ? cache.block.filter((b) => b.text).map((b) => ({ v: b.text, line: b.line })) : [{ v: cache.value, line: cache.line }];
          for (const { v, line } of values) {
            if (v.startsWith("!")) continue;
            const r = resolveInput(".", v, produced, ignored);
            if (r.state === "outside") { add(`cache-dependency-path:${v}`, `${file}:${line} job ${job}: cache-dependency-path ${v} is outside the repository`, "name a lock file of this repository"); continue; }
            if (r.state === "skip") {
              if (!/[*?]/.test(v) || /[${}]/.test(v)) continue;
              const glob = new RegExp(`^${v.replace(/[.+^()|[\]\\]/g, "\\$&").replace(/\*\*\/?/g, "\u0000").replace(/\*/g, "[^/]*").replace(/\?/g, "[^/]").replace(/\u0000/g, "(?:.*/)?")}$`);
              if (!files.some((f) => glob.test(f))) add(`cache-dependency-path:${v}`, `${file}:${line} job ${job}: cache-dependency-path ${v} matches no file of this commit (setup refuses: the dependency lock is not found)`, "point cache-dependency-path at the lock file that exists");
              continue;
            }
            if (!index.fileSet.has(r.path)) add(`cache-dependency-path:${r.path}`, `${file}:${line} job ${job}: cache-dependency-path ${r.path} is not in this commit (setup refuses: the dependency lock is not found)`, `commit ${r.path}, or point cache-dependency-path at the lock file that exists`);
          }
        }
        const run = stepGet(step, "run");
        if (run) {
          const lines = run.block ? run.block : [{ line: run.line, text: run.value }];
          // One shell per step: a `cd` carries to the lines after it.
          let dir = cwd;
          for (const { line, text: l } of lines) {
            const commands = shellCommands(l);
            const cds = commands.flatMap(commandInputs).filter((x) => x.kind === "cd");
            checkInputs({ file, line, cwd: dir, commands, index, produced, ignored, add: (s, p, f) => add(`run:${s}`, p, f), where: `job ${job} step` });
            for (const cd of cds) { if (dir === null) break; const r = resolveInput(dir, cd.value, produced, ignored); dir = r.state === "check" && index.dirSet.has(r.path) ? r.path : null; }
            if (dir === null) break;
          }
        }
      }
    }
  }
  return out;
}

// ---- CA-08: Caddyfile global options ---------------------------------------------------------------------------------------------
// HTTP handler and site directives: never a global option. Names that are both (log, metrics) and unknown names (a plugin's global
// option) are not judged.
const SITE_DIRECTIVES = new Set(["abort", "acme_server", "basic_auth", "basicauth", "bind", "encode", "error", "file_server", "forward_auth", "fs", "handle", "handle_errors", "handle_path", "header", "intercept", "invoke", "log_append", "log_name", "log_skip", "map", "method", "php_fastcgi", "push", "redir", "request_body", "request_header", "respond", "reverse_proxy", "rewrite", "root", "route", "skip_log", "templates", "tls", "tracing", "try_files", "uri", "vars"]);
const caddyFiles = (files) => files.filter((f) => /(^|\/)(Caddyfile(\.[^/]*)?|[^/]+\.caddyfile)$/i.test(f) && scanned(f) && !FIXTURE_TREE.test(f)).sort();

/** Top-level blocks of a Caddyfile: [{ header, line, directives: [{ name, line }] }] (directives at depth 1 only). */
export function caddyBlocks(text) {
  const blocks = [];
  let depth = 0;
  let current = null;
  String(text ?? "").replace(/\r\n?/g, "\n").split("\n").forEach((raw, i) => {
    const tokens = [];
    for (const m of raw.matchAll(/"(?:[^"\\]|\\.)*"|`[^`]*`|\S+/g)) { if (m[0].startsWith("#")) break; tokens.push(m[0]); }
    if (!tokens.length) return;
    if (depth === 0) {
      if (tokens.at(-1) === "{") {
        current = { header: tokens.slice(0, -1), line: i + 1, directives: [] };
        blocks.push(current);
        depth = 1;
      }
      return;
    }
    if (depth === 1 && tokens[0] !== "}" && tokens[0] !== "{") current.directives.push({ name: tokens[0], line: i + 1 });
    for (const t of tokens) { if (t === "{") depth += 1; else if (t === "}") depth -= 1; }
    if (depth <= 0) { depth = 0; current = null; }
  });
  return blocks;
}

function caddyGlobalOptions({ root, files }) {
  const out = [];
  for (const file of caddyFiles(files)) {
    const text = readText(root, file);
    if (text === null) continue;
    const blocks = caddyBlocks(text);
    const first = blocks[0];
    if (!first) continue;
    const site = first.directives.filter((d) => SITE_DIRECTIVES.has(d.name));
    if (!site.length) continue;
    if (first.header.length === 0) {
      out.push({ file, subject: `global:${site[0].name}`, problem: `${file}:${site[0].line} \`${site[0].name}\` sits in the global options block (the first block, with no address): Caddy refuses it as an unrecognized global option`, fix: `move ${site.map((d) => d.name).join(", ")} into a site block (an address before its {)` });
      continue;
    }
    // A site address spelled only with environment placeholders and no non-empty default: an empty variable leaves the block with
    // no address, and the first block without one is the global options block.
    const fragile = first.header.every((t) => /^\{\$[A-Za-z_][A-Za-z0-9_]*(?::)?\}$/.test(t.replace(/,$/, "")));
    if (fragile) {
      const vars = first.header.map((t) => /\{\$([A-Za-z0-9_]+)/.exec(t)[1]);
      out.push({ file, subject: `address:${vars.join(",")}`, problem: `${file}:${first.line} the first block's address is only ${first.header.join(" ")}: with ${vars.join(", ")} unset or empty Caddy reads the block as global options and refuses \`${site[0].name}\` (line ${site[0].line}) as an unrecognized global option`, fix: `put a global options block ({ ... }, even empty) first, or give the placeholder a non-empty default ({$${vars[0]}:localhost})` });
    }
  }
  return out;
}

export const CONSUMER_SOURCE_RULES = Object.freeze([
  { id: "workflow-action-pair", gap: "CA-04", run: workflowActionPair },
  { id: "referenced-input", gap: "CA-07", run: referencedInput },
  { id: "caddy-global-options", gap: "CA-08", run: caddyGlobalOptions },
]);
