// The gate's acceptance test: twelve lines that must be true of every run (GATE-CONTRACT GC-A27; Adrian, 2026-11-20: "An acceptance test
// should be a single list of things that has to pass any run").
//
// It judges ONE run, and takes nothing from the gate's own reporting (Adrian, 2026-11-21: "remove all dependency of gate reporting anything").
// Its evidence is what the gate does not write: git (the selected commit, its tree and files), the operating system's record of every program
// the run started and when it exited (Windows Security events 4688/4689, macOS eslogger), the broker's signed receipts (each job's command,
// folder, times, commit, tree and test programs), the gate's state on disk as the launcher copied it before the gate started (each row's
// last inputs and result) and as it stands after (the results the cache store took during the run), and the launcher's own record (commit,
// run id, the gate's pid, its clock, its exit, the lanes and time limit pinned at launch, the run lock). The test works out itself what the
// run owes, from the files each row last read and the selected tree. It stages nothing and knows no scenario. A line with no record to judge
// it from is "not measured", never a pass.
//
// Rules every check keeps (gatecodex reviews and Council CV-1 and CV-2, 2026-11-20):
//  * Missing proof is not the absence of work. "Nothing was sent", "nothing repeated" and "no other compile" are shown only from a process
//    record that covers the whole run; otherwise the line is not measured.
//  * What the run owed (the test's own reckoning) and what it did (programs, signed jobs, stored results) are compared in both directions.
//  * What a record cannot tell apart (a wrapped command with no child, a feature-set variant, a tie in time) is not measured, not guessed.
//
// The lines are not written here. They are read from the one table in docs/GATE-CONTRACT.md [GC-A27] between
// `<!-- acceptance-lines:begin -->` and `<!-- acceptance-lines:end -->`, and nothing is judged unless every line there has a check here and
// every check here has a line there.
//
//   node acceptance-lines.mjs --run <run dir> --repo <dir> --cache-dir <dir> --receipts <dir> [--broker-key <hex> --broker-key-id <hex>]
//                             [--successor <run dir>] [--live] [--contract <file>] [--json]
import fs from "node:fs";
import path from "node:path";
import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { fileURLToPath } from "node:url";
import { sidecarBoundCrate } from "./deterministic-scans.mjs";
import { historyFileFor } from "./acceptance-launch.mjs";

export const CONTRACT_FILE = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..", "..", "docs", "GATE-CONTRACT.md");
const BEGIN = "<!-- acceptance-lines:begin -->"; const END = "<!-- acceptance-lines:end -->";

/**
 * The twelve lines as the contract states them: the rows `| \`NNxx\` | sentence |` between the two markers. Returns { lines: [{ id, text }],
 * problems }. Anything that is not a clean table there (no markers, a row of another shape, a repeated id, an empty sentence) is a problem.
 */
export function parseAcceptanceLines(text) {
  const src = String(text ?? ""); const problems = [];
  const b = src.indexOf(BEGIN); const e = src.indexOf(END);
  if (b < 0 || e < 0 || e < b) return { lines: [], problems: [`the contract has no ${BEGIN} ... ${END} table`] };
  if (src.indexOf(BEGIN, b + 1) >= 0 || src.indexOf(END, e + 1) >= 0) problems.push("the contract has more than one acceptance-lines table");
  const lines = [];
  for (const raw of src.slice(b + BEGIN.length, e).split(/\r?\n/)) {
    const row = raw.trim();
    if (!row || /^\|\s*Id\s*\|\s*Line\s*\|$/.test(row) || /^\|\s*-+\s*\|\s*-+\s*\|$/.test(row)) continue;
    const m = /^\|\s*`(NN\d\d)`\s*\|\s*(.*?)\s*\|$/.exec(row);
    if (!m) { problems.push(`a row of the acceptance-lines table is not \`| \\\`NNxx\\\` | sentence |\`: ${row.slice(0, 80)}`); continue; }
    if (!m[2]) problems.push(`${m[1]} has no sentence`);
    if (lines.some((l) => l.id === m[1])) problems.push(`${m[1]} appears more than once`);
    lines.push(Object.freeze({ id: m[1], text: m[2] }));
  }
  if (!lines.length) problems.push("the acceptance-lines table has no lines");
  return { lines: Object.freeze(lines), problems };
}

/** Both directions: a line the contract states with no check here, and a check here with no line in the contract. */
export function linkProblems(lines, checkIds = Object.keys(JUDGES)) {
  const stated = new Set(lines.map((l) => l.id)); const checked = new Set(checkIds); const problems = [];
  for (const id of stated) if (!checked.has(id)) problems.push(`${id} is a line in the contract and has no check`);
  for (const id of checked) if (!stated.has(id)) problems.push(`${id} has a check and is not a line in the contract`);
  return problems;
}

/** The contract's lines, linked to the checks; throws, naming every problem, when the table is malformed or the two do not match. */
export function readAcceptanceLines(text) {
  const { lines, problems } = parseAcceptanceLines(text);
  const all = [...problems, ...linkProblems(lines)];
  if (all.length) throw Object.assign(new Error(`the contract and the acceptance test do not match: ${all.join("; ")}`), { problems: all });
  return lines;
}

const PASS = "pass"; const FAIL = "fail"; const NOT_MEASURED = "not measured";
const result = (verdict, why) => ({ verdict, why });
/** Any failure fails the line; else any gap leaves it not measured; else it passes. Each list is reported in full, the first few shown. */
const decide = (fails, gaps, passWhy) => {
  const uniq = (xs) => [...new Set(xs)];
  const show = (xs) => `${uniq(xs).slice(0, 4).join("; ")}${uniq(xs).length > 4 ? `; and ${uniq(xs).length - 4} more` : ""}`;
  if (fails.length) return result(FAIL, show(fails));
  if (gaps.length) return result(NOT_MEASURED, show(gaps));
  return result(PASS, passWhy);
};
const short = (s) => String(s).slice(0, 12);

// ---- reading the records ---------------------------------------------------------------------------------------------------------------------------
const ISO = /^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d(?:\.\d+)?Z$/;
const timeOf = (s) => (typeof s === "string" && ISO.test(s) ? Date.parse(s) : NaN);

/**
 * A process start as the program it really ran (Council CV-1 F-4, F-22). A Windows `cmd /c <string>` start is decoded by cmd's own rules
 * (https://learn.microsoft.com/windows-server/administration/windows-commands/cmd): with `/s`, the first and last quote of the string are
 * removed and the rest is left as it is; without `/s`, a string that begins with a quote loses it and its closing partner. Then the program
 * is the quoted head or the first word, the arguments are the words after it, and a caret outside quotes escapes the next character. A
 * string this reader cannot resolve to one plain command (several commands, redirection, variables, quotes left in the arguments) is marked
 * `ambiguous`, never guessed. A shell-string start (execSync: the whole line in cmd, no args) is read the same way.
 * Returns { program, args, cwd, wrapped, ambiguous }.
 */
export function decodeStart(entry) {
  let program = String(entry.cmd ?? ""); let args = (entry.args ?? []).map(String);
  let ambiguous = false; let intended = null;
  const line = (() => {
    if (/(^|[\\/])cmd(\.exe)?$/i.test(program)) {
      const c = args.findIndex((a) => /^\/c$/i.test(a));
      if (c < 0) return null;
      let s = args.slice(c + 1).join(" ").trim();
      const slashS = args.slice(0, c).some((a) => /^\/s$/i.test(a));
      const a = s.indexOf('"'); const b = s.lastIndexOf('"');
      // Without /s, exactly one pair of quotes and no special character keeps its quotes (cmd's first rule); otherwise the pair is removed.
      const kept = !slashS && a === 0 && (s.match(/"/g) ?? []).length === 2 && !/[&<>()@^|]/.test(s);
      // When a removed pair does not wrap the whole string, the quotes that held a spaced program path together are consumed and the path
      // is split at its space: cmd's own reading of that is not one plain command.
      if (!kept && (slashS || a === 0) && a >= 0 && b > a && !(a === 0 && b === s.length - 1)) {
        ambiguous = true;
        // The call the string was written as: its quoted head the program and its words, as written, the arguments. NN08 holds it against the
        // run's other starts (frozen case gate 5): a call written twice is a repeat, however cmd then reads the second copy.
        const w = /^"([^"]+)"(?:\s+(.*))?$/.exec(s);
        if (w && !/["&|<>%]/.test(w[2] ?? "")) intended = { program: w[1], args: (w[2] ?? "").split(/\s+/).filter(Boolean) };
      }
      if (kept) return s;
      if (slashS) { if (a >= 0 && b > a) s = s.slice(0, a) + s.slice(a + 1, b) + s.slice(b + 1); }
      else if (s.startsWith('"')) { if (b > 0) s = s.slice(1, b) + s.slice(b + 1); }
      return s;
    }
    if (!args.length && /\s/.test(program.trim())) return program.trim();
    return null;
  })();
  if (line === null) return { program, args, cwd: String(entry.cwd ?? ""), wrapped: false, ambiguous: false };
  const m = /^"([^"]+)"(?:\s+(.*))?$/.exec(line) ?? /^([^\s"]+)(?:\s+(.*))?$/.exec(line);
  if (!m) return { program: line, args: [], cwd: String(entry.cwd ?? ""), wrapped: true, ambiguous: true };
  program = m[1]; const rest = m[2] ?? "";
  if (/["&|<>%]/.test(rest.replace(/\^./g, ""))) ambiguous = true;
  args = rest.split(/\s+/).filter(Boolean).map((a) => a.replace(/\^(.)/g, "$1"));
  return { program, args, cwd: String(entry.cwd ?? ""), wrapped: true, ambiguous, intended };
}
const baseOf = (p) => String(p).replaceAll("\\", "/").split("/").pop();
const programName = (p) => baseOf(p).replace(/\.(exe|cmd|bat|com)$/i, "").toLowerCase();

const COMPILING = new Set(["build", "check", "clippy", "doc", "test", "rustc", "rustdoc"]);
/**
 * Anything sent to the broker: a start of the rustright client, directly or through a decoded `cmd /c`, whatever it asks (a compile,
 * `cargo metadata`, a version). Looking the program up (`where rustright`) starts something else and is not a send. A start that names
 * rustright but cannot be decoded is returned with `ambiguous: true`. Returns { args, cwd, at, ms, ambiguous }, or null.
 */
export function brokerStartOf(entry) {
  const d = decodeStart(entry);
  const at = typeof entry.at === "string" ? Date.parse(entry.at) : null; const ms = typeof entry.ms === "number" ? entry.ms : null;
  if (programName(d.program) === "rustright") return { args: d.args, cwd: d.cwd, at, ms, ambiguous: d.ambiguous };
  if (d.ambiguous && /rustright/i.test([entry.cmd, ...(entry.args ?? [])].join(" "))) return { args: [], cwd: d.cwd, at, ms, ambiguous: true };
  return null;
}
/** A compile sent to the broker (`rustright cargo <sub> ...`), or null. Returns { sub, args (after the subcommand), cwd, at, ms }. */
export function brokerCompileOf(entry) {
  const b = brokerStartOf(entry);
  if (!b || b.ambiguous || b.args[0] !== "cargo" || !COMPILING.has(b.args[1])) return null;
  return { sub: b.args[1], args: b.args.slice(2), cwd: b.cwd, at: b.at, ms: b.ms };
}
/** The values a cargo flag takes in an argument list: `--package x`, `--package=x`, `-p x`, `-px`. */
const valuesOf = (args, long, short = null) => {
  const out = [];
  for (let i = 0; i < args.length; i += 1) {
    const a = args[i];
    if (a === "--") break;
    if (a === long || (short && a === short)) { if (i + 1 < args.length) out.push(args[(i += 1)]); }
    else if (a.startsWith(`${long}=`)) out.push(a.slice(long.length + 1));
    else if (short && a.startsWith(short) && a.length > short.length && !a.startsWith("--")) out.push(a.slice(short.length));
  }
  return out;
};
const cratesOf = (c) => valuesOf(c.args, "--package", "-p");
const targetsOf = (c) => valuesOf(c.args, "--target");

// ---- where a row's work happens -------------------------------------------------------------------------------------------------------------------
/**
 * A Class B row id as its stage, workspace, crate, target and feature-set variant (Council CV-1 F-20): `clippy:rk:rk-leaf@x86_64-unknown-linux-gnu`
 * is clippy of crate rk-leaf in workspace rk; `test:rust:rk:rk-leaf` a Rust test; `clippy:rightkit-a` a crate that is its own workspace;
 * `clippy:rk:rk-slow:1` the second feature set. The workspace label is never a crate name.
 */
export function rowOf(id) {
  const [head, target = null] = String(id).split("@");
  const [sub, ...rest] = head.split(":");
  const variant = rest.find((s) => /^\d+$/.test(s)) ?? null;
  const names = rest.filter((s) => s && s !== "rust" && !/^\d+$/.test(s));
  return { sub, ws: names.length >= 2 ? names[0] : null, crate: names[names.length - 1] ?? null, target, variant };
}
/** A Class B row the broker compiles: a cargo stage, and for tests only the Rust ones (`test:npm:*` suites run in the gate, not the broker). */
const isBrokerRow = (id) => { const r = rowOf(id); return COMPILING.has(r.sub) && (r.sub !== "test" || String(id).startsWith("test:rust:")); };
/** Windows paths compare without case and without a trailing separator; other paths keep their case (Council CV-1 F-22, F-23). */
const isWin = (p) => /^[a-z]:[\\/]/i.test(p) || String(p).includes("\\");
const dirKey = (p) => { const s = String(p ?? ""); const n = s.replaceAll("\\", "/").replace(/\/+$/, ""); return isWin(s) ? n.toLowerCase() : n; };
/** A folder relative to the snapshot root ("" for the root), or null when it is outside the snapshot or the snapshot is unknown. */
const relToSnap = (dir, snapPath) => {
  if (!snapPath) return null; const d = dirKey(dir); const s = dirKey(snapPath);
  return d === s ? "" : d.startsWith(`${s}/`) ? d.slice(s.length + 1) : null;
};
/** Whether a compile runs in the row's workspace: the folder its label names, or, for a crate that is its own workspace, a folder of that name. */
const inWorkspace = (c, r, snapPath) => {
  const rel = relToSnap(c.cwd, snapPath); if (rel === null) return false;
  return r.ws !== null ? rel === dirKey(r.ws) : baseOf(rel) === r.crate;
};
/**
 * Whether a broker compile does a Class B row's work: the same stage, the row's crate, in the row's workspace, and the row's target. A row
 * with no target is the host build: the compile must name no target, or name a target that no targeted row of that crate and stage claims.
 */
const covers = (c, id, snapPath, rowIds = []) => {
  const r = rowOf(id);
  if (!isBrokerRow(id) || c.sub !== r.sub || !cratesOf(c).includes(r.crate) || !inWorkspace(c, r, snapPath)) return false;
  const ts = targetsOf(c);
  if (r.target !== null) return ts.includes(r.target);
  if (!ts.length) return true;
  const claimed = new Set(rowIds.map(rowOf).filter((o) => o.sub === r.sub && o.crate === r.crate && o.ws === r.ws && o.target !== null).map((o) => o.target));
  return ts.some((t) => !claimed.has(t));
};
/** The same row in another feature set (`clippy:rk:rk-slow:0` and `clippy:rk:rk-slow:1`): one compile command cannot tell them apart. */
const otherVariant = (a, b) => { const x = rowOf(a); const y = rowOf(b); return x.variant !== y.variant && x.sub === y.sub && x.target === y.target && x.ws === y.ws && x.crate === y.crate; };

/**
 * The Class A tools whose starts are attributable to a row (Council CV-1 F-17): a row id's tool, and whether a start of that program does
 * that row's work. A version or help query is a probe, not the row's work. A row with a workspace in its id (`cargo-deny:app/engine`) is done
 * by a start in that workspace or naming its manifest; a row without one by any non-probe start of the tool.
 */
const TOOLS = { rustfmt: "rustfmt", deny: "cargo-deny", "cargo-deny": "cargo-deny", "cargo-audit": "cargo-audit", shellcheck: "shellcheck", actionlint: "actionlint", oxlint: "oxlint", tsc: "tsc" };
const toolOf = (id) => TOOLS[String(id).split(":")[0]] ?? null;
const isProbe = (args) => args.length > 0 && args.every((a) => /^(--version|-version|-V|--help|-help|-h)$/.test(a));

const RUN_PARTS = [[/(rk-probe-|rk-gate-shim-|rk-cargo-shim-)[A-Za-z0-9_-]+/g, "$1*"], [/\/var\/folders\/[^/]+\/[^/]+\/T\//g, "$TMP/"], [/\b(pid|PID)([= :]?)\d+/g, "$1$2*"]];
const runPart = (s) => RUN_PARTS.reduce((v, [re, to]) => v.replace(re, to), String(s));
/** A decoded start as program, arguments and folder; Windows folders and programs compare without case or a trailing separator. */
function callOf(d, raw) {
  const p = String(d.program); const bare = !/[\\/]/.test(p);
  const prog = runPart(p.replace(/\.(exe|cmd|bat|com)$/i, ""));
  return { name: programName(p), path: bare ? null : dirKey(prog), args: d.args.map(runPart), cwd: dirKey(runPart(d.cwd ?? "")), truncated: (raw.args ?? []).some((a) => /^\.\.\. \d+ args$/.test(String(a))), ambiguous: d.ambiguous };
}
/** The same program: one path, or a bare name (found on PATH) and a path ending in it. Two different paths are two programs. */
const sameProgram = (a, b) => a.name === b.name && (a.path === null || b.path === null || a.path === b.path);

/** The values a minimal TOML `[workspace] members = [...]` holds, or null when the manifest has no workspace table. */
const workspaceMembers = (toml) => {
  const t = String(toml ?? ""); const ws = /^\[workspace\]\s*$([\s\S]*?)(?=^\[|(?![\s\S]))/m.exec(t); if (!ws) return null;
  const m = /members\s*=\s*\[([\s\S]*?)\]/.exec(ws[1]); if (!m) return [];
  return [...m[1].matchAll(/"([^"]+)"/g)].map((x) => x[1].replace(/\/+$/, ""));
};

// ---- the evidence (none of it written by the gate) ----------------------------------------------------------------------------------------------
/**
 * A Windows command line as its program would read it (CommandLineToArgvW / the MSVC runtime rules): whitespace separates arguments outside
 * quotes; `2n` backslashes before a quote are `n` backslashes and the quote toggles quoting; `2n+1` are `n` and a literal quote; `""` inside
 * quotes is a literal quote.
 */
export function parseCommandLine(line) {
  const s = String(line ?? ""); const out = []; let cur = ""; let quoted = false; let any = false;
  for (let i = 0; i < s.length; i += 1) {
    const c = s[i];
    if (c === "\\") { let n = 0; while (s[i] === "\\") { n += 1; i += 1; } if (s[i] === '"') { cur += "\\".repeat(Math.floor(n / 2)); if (n % 2) { cur += '"'; any = true; } else { i -= 1; } } else { cur += "\\".repeat(n); i -= 1; } any = true; continue; }
    if (c === '"') { if (quoted && s[i + 1] === '"') { cur += '"'; i += 1; } else quoted = !quoted; any = true; continue; }
    if (!quoted && /\s/.test(c)) { if (any) { out.push(cur); cur = ""; any = false; } continue; }
    cur += c; any = true;
  }
  if (any) out.push(cur);
  return out;
}

/**
 * Programs as the operating system recorded them, from Windows Security events 4688 (a process created: new pid, creator pid, image path,
 * command line, time) and 4689 (a process exited: pid, image path, exit status, time). Event ids are reused by Windows, so an exit is joined
 * to the latest start of that pid before it. 4688 records no working folder, so `cwd` is null. Returns [{ pid, ppid, exe, args, cwd, start,
 * end, exit }], times in ms.
 */
export function processesFromWindowsEvents(events) {
  const procs = [];
  const sorted = [...(events ?? [])].map((e) => ({ ...e, t: timeOf(e.time) })).filter((e) => Number.isFinite(e.t)).sort((a, b) => a.t - b.t);
  for (const e of sorted) {
    if (e.id === 4688) { const argv = parseCommandLine(e.commandLine ?? ""); procs.push({ pid: e.pid, ppid: e.ppid, exe: String(e.exe ?? ""), args: argv.slice(1), cwd: null, start: e.t, end: null, exit: null }); }
    else if (e.id === 4689) { const p = [...procs].reverse().find((x) => x.pid === e.pid && x.end === null && x.start <= e.t); if (p) { p.end = e.t; p.exit = e.exit ?? null; } }
  }
  return procs;
}
/** Programs from macOS `eslogger exec exit` lines, as normalized by the collector ({ event, pid, ppid, path, args, cwd, time, status }). */
export function processesFromEslogger(lines) {
  return processesFromWindowsEvents((lines ?? []).map((l) => (l.event === "exec" ? { id: 4688, pid: l.pid, ppid: l.ppid, exe: l.path, commandLine: null, time: l.time } : { id: 4689, pid: l.pid, exit: l.status, time: l.time }))).map((p, i, all) => {
    const exec = (lines ?? []).filter((l) => l.event === "exec" && l.pid === p.pid)[all.slice(0, i).filter((q) => q.pid === p.pid).length];
    return exec ? { ...p, args: Array.isArray(exec.args) ? exec.args.slice(1) : p.args, cwd: typeof exec.cwd === "string" ? exec.cwd : null } : p;
  });
}

/**
 * The run's own programs: the gate process the launcher started (its pid, a child of the launcher, started inside the launcher's span) and
 * every program descended from it, each joined to the parent that was running when it started. Returns { root, procs, gaps }.
 */
function runTree(launch, processes, witness) {
  const gaps = [];
  if (!witness) return { root: null, procs: [], gaps: ["there is no operating-system record of the programs this run started"] };
  gaps.push(...(witness.problems ?? []).map((p) => `the operating-system process record is incomplete: ${p}`));
  const start = timeOf(launch?.startedAt); const end = timeOf(launch?.endedAt);
  if (!(timeOf(witness.from) <= start)) gaps.push("the operating-system process record does not start before the run did");
  if (!Number.isFinite(end)) gaps.push("the launcher never saw the run end, so the process record cannot be shown to cover it");
  else if (!(timeOf(witness.to) >= end)) gaps.push("the operating-system process record ends before the run did");
  const all = processes ?? [];
  const root = all.find((p) => p.pid === launch?.gatePid && p.ppid === launch?.launcherPid && p.start >= start - 1000 && (!Number.isFinite(end) || p.start <= end)) ?? null;
  if (!root) { gaps.push(`the process record has no start of the gate process ${launch?.gatePid ?? "(unknown)"} by its launcher`); return { root: null, procs: [], gaps }; }
  const inTree = new Set([root]); let grew = true;
  while (grew) {
    grew = false;
    for (const p of all) {
      if (inTree.has(p)) continue;
      const parent = [...inTree].filter((q) => q.pid === p.ppid && q.start <= p.start && (q.end === null || q.end >= p.start)).sort((a, b) => b.start - a.start)[0];
      if (parent) { inTree.add(p); grew = true; }
    }
  }
  const procs = [...inTree].filter((p) => p !== root).sort((a, b) => a.start - b.start);
  const open = procs.filter((p) => p.end === null);
  if (open.length) gaps.push(`${open.length} program(s) of the run have no recorded exit (first: ${programName(open[0].exe)})`);
  return { root, procs, gaps };
}

/** A broker job from its signed receipt: only the signed payload is read. Submission time is the signed start less the signed waits. */
function jobOf(r) {
  const p = r?.doc?.provenance?.payload ?? null; const req = p?.request ?? null;
  const argv = Array.isArray(req?.requestedArgv) ? req.requestedArgv.map(String) : null;
  const started = timeOf(p?.startedAt); const t = p?.timingsMs ?? {};
  const waits = ["admission", "queue", "bind"].map((k) => (Number.isFinite(t[k]) ? t[k] : 0)).reduce((a, b) => a + b, 0);
  return {
    file: r.file, verified: r.verified?.ok === true, problems: r.verified?.problems ?? [], jobId: p?.jobId ?? null, argv, cwd: typeof req?.workspace === "string" ? req.workspace : null,
    sub: argv?.[0] ?? null, args: argv ? argv.slice(1) : [], gate: p?.gate ?? null, started, ended: timeOf(p?.terminalAt), submitted: Number.isFinite(started) ? started - waits : NaN, queueMs: Number.isFinite(t.queue) ? t.queue : null,
    testExecutables: p?.testExecutables, testExecutablesReason: p?.testExecutablesReason,
  };
}

/** A row's recorded inputs (the gate's own history, copied by the launcher before the gate started): its files and their blob ids. */
const inputFiles = (inputs) => {
  let doc; try { doc = JSON.parse(String(inputs ?? "null").replace(/:rightkit\.[\w.-]+\/\d+$/, "")); } catch { return null; }
  return Array.isArray(doc?.files) ? doc.files.filter((f) => Array.isArray(f) && typeof f[0] === "string").map((f) => [f[0], String(f[1])]) : null;
};

/**
 * What the run owes, worked out by the test itself (rightkit#1514 step 3): every row the history before the run knows, with the blob id of
 * each input file it was last judged on, held to the selected commit's tree. A row whose files all still have those blob ids is owed no work
 * (reuse); a row with any changed file is owed a rerun of exactly those files' work. A row whose recorded inputs hold no file list cannot be
 * classified. Returns Map id -> { id, stage, expect: "reuse"|"run"|null, changed: [paths], priorKey, priorFailed, why }.
 */
function expectedRows(history, git) {
  const out = new Map();
  for (const stage of ["A", "B"]) {
    for (const [id, h] of Object.entries(history?.[stage] ?? {})) {
      const files = inputFiles(h?.last?.inputs);
      const priorKey = typeof h?.last?.key === "string" ? h.last.key : null;
      const priorFailed = Array.isArray(h?.failures) && priorKey !== null && h.failures.includes(priorKey);
      if (!files) { out.set(id, { id, stage, expect: null, changed: [], priorKey, priorFailed, why: `${id}'s recorded inputs name no files` }); continue; }
      const changed = files.filter(([p, blob]) => git.blobOf(p) !== blob).map(([p]) => p);
      out.set(id, { id, stage, expect: changed.length ? "run" : "reuse", changed, priorKey, priorFailed, why: null });
    }
  }
  return out;
}

/**
 * The work each row actually did, from evidence the gate does not write: a result the cache store took during the run (its entry's own time),
 * a program of the row's tool the operating system saw start, or a broker job the broker signed that covers the row.
 */
function workOf(ctx, id) {
  const writes = ctx.cacheWrites.filter((w) => w.row?.id === id);
  const tool = toolOf(id);
  const progs = tool ? ctx.starts.filter((d) => programName(d.program) === tool && !isProbe(d.args)) : [];
  const jobs = isBrokerRow(id) ? ctx.jobs.filter((j) => j.sub && covers(j, id, ctx.snap.path, [...ctx.expected.keys()])) : [];
  return { writes, progs, jobs, did: writes.length > 0 || jobs.length > 0 || (tool && tool !== "rustfmt" && progs.length > 0) };
}
const classOf = (ctx, id) => ctx.expected.get(id)?.stage ?? (isBrokerRow(id) ? "B" : "A");
/** Every row that did work in this run: rows with a cache write, plus rows a broker job covers. */
const rowsThatWorked = (ctx) => new Set([...ctx.cacheWrites.map((w) => w.row?.id).filter(Boolean), ...[...ctx.expected.keys()].filter((id) => { const w = workOf(ctx, id); return w.jobs.length > 0 || (toolOf(id) && toolOf(id) !== "rustfmt" && w.progs.length > 0); })]);

// ---- the twelve lines -------------------------------------------------------------------------------------------------------------------------------
function nn01(ctx) {
  const fails = []; const gaps = [...ctx.processGaps];
  if (!/^[0-9a-f]{40}$/.test(String(ctx.commit ?? ""))) return result(NOT_MEASURED, "the launcher's record names no selected commit");
  if (!ctx.tree) return result(NOT_MEASURED, `git cannot say which tree commit ${short(ctx.commit)} is`);
  // Every broker job is signed as this commit's tree, built from a snapshot the broker confirmed unchanged.
  for (const j of ctx.jobs) {
    if (!j.verified) { (ctx.verifierPinned ? fails : gaps).push(`broker job ${j.jobId ?? path.basename(j.file)}: its receipt ${ctx.verifierPinned ? `does not verify (${j.problems.join("; ") || "no reason given"})` : "cannot be verified: no broker key is pinned"}`); continue; }
    if (j.gate?.commit !== ctx.commit) fails.push(`broker job ${j.jobId} built commit ${short(j.gate?.commit)}, not the selected ${short(ctx.commit)}`);
    if (j.gate?.tree !== ctx.tree) fails.push(`broker job ${j.jobId} built tree ${short(j.gate?.tree)}, not the selected commit's ${short(ctx.tree)}`);
    if (j.gate?.snapshotUnchanged === false) fails.push(`broker job ${j.jobId}: the broker found the snapshot changed`);
    else if (j.gate?.snapshotUnchanged !== true) gaps.push(`broker job ${j.jobId}: the broker's receipt does not say whether the snapshot stayed unchanged`);
    if (j.cwd && ctx.snap.path && relToSnap(j.cwd, ctx.snap.path) === null) fails.push(`broker job ${j.jobId} built in ${j.cwd}, outside the snapshot ${ctx.snap.path}`);
  }
  const clients = ctx.starts.filter((d) => programName(d.program) === "rustright" && d.args[0] === "cargo" && COMPILING.has(d.args[1]));
  if (clients.length > ctx.jobs.length) gaps.push(`${clients.length} broker client compile start(s) and ${ctx.jobs.length} signed job(s): what the others built is not shown`);
  // A compile the gate ran itself, outside the broker, has no signed commit or tree.
  const own = ctx.starts.filter((d) => programName(d.program) === "cargo" && COMPILING.has(d.args.find((a) => !a.startsWith("-") && !a.startsWith("+"))) || programName(d.program) === "rustc");
  if (own.length) fails.push(`${own.length} compile(s) ran outside the broker (first: ${programName(own[0].program)} ${own[0].args.slice(0, 3).join(" ")}), so what they built is not shown`);
  return decide(fails, gaps, `selected commit ${short(ctx.commit)} is tree ${short(ctx.tree)}; ${ctx.jobs.length} broker job(s) signed for that tree on an unchanged snapshot; nothing compiled outside the broker`);
}

const classAStarts = (ctx) => ctx.starts.filter((d) => Object.values(TOOLS).includes(programName(d.program)) && !isProbe(d.args));
function nn02(ctx) {
  const fails = []; const gaps = [...ctx.processGaps];
  const a = classAStarts(ctx);
  // Class A failed: a Class A result the cache store took as a failure, or a Class A tool that exited non-zero.
  const failedRows = ctx.cacheWrites.filter((w) => w.row?.class === "A" && w.row?.status === "fail").map((w) => w.row.id);
  const failedProgs = a.filter((d) => d.exit !== null && d.exit !== 0);
  const failed = failedRows.length > 0 || failedProgs.length > 0;
  const clients = ctx.starts.filter((d) => programName(d.program) === "rustright" && d.args[0] === "cargo");
  if (failed) {
    if (ctx.jobs.length || clients.length) fails.push(`Class A failed (${failedRows[0] ?? programName(failedProgs[0].program)}) and ${ctx.jobs.length} broker job(s) / ${clients.length} broker client start(s) still followed`);
    return decide(fails, gaps, `Class A failed (${failedRows.slice(0, 2).join(", ") || programName(failedProgs[0].program)}) and nothing went to the broker`);
  }
  const aEnd = Math.max(-Infinity, ...a.map((d) => d.end ?? Infinity));
  if (a.some((d) => d.end === null)) gaps.push("a Class A program has no recorded exit, so when Class A finished is not shown");
  for (const j of ctx.jobs) {
    if (!Number.isFinite(j.submitted)) { gaps.push(`broker job ${j.jobId}: its receipt gives no time`); continue; }
    if (j.submitted < aEnd) fails.push(`broker job ${j.jobId} was submitted before Class A finished`);
    else if (j.submitted === aEnd) gaps.push(`broker job ${j.jobId} was submitted at the same moment Class A ended, so which came first is not shown`);
  }
  for (const c of clients) if (c.at < aEnd) fails.push(`a broker client started (rustright ${c.args.slice(0, 2).join(" ")}) before Class A finished`);
  return decide(fails, gaps, `Class A passed (${a.length} tool program(s)), then ${ctx.jobs.length} broker job(s) were submitted after it ended`);
}

function nn03(ctx) {
  const gaps = [...ctx.processGaps]; const fails = [];
  if (classAStarts(ctx).some((d) => d.end !== null && d.end < d.at)) return result(NOT_MEASURED, "the process record has a Class A program ending before it started");
  const a = classAStarts(ctx).filter((d) => Number.isFinite(d.at) && d.end !== null);
  if (!a.length) return decide([], gaps, "Class A started no tool program, so no row waited");
  const lanes = Number(ctx.launch?.lanes);
  if (!Number.isInteger(lanes) || lanes <= 0) return result(NOT_MEASURED, "the launcher's record gives no lane count for Class A on this host");
  // Class A rows are independent: every one is ready when Class A begins (its first tool program starts).
  const ready = Math.min(...a.map((d) => d.at));
  for (const d of a) {
    if (d.at === ready) continue;
    const points = [...new Set([ready, ...a.flatMap((o) => [o.at, o.end]).filter((t) => t > ready && t < d.at)])].sort((x, y) => x - y);
    let idle = 0;
    points.forEach((t, i) => { const next = points[i + 1] ?? d.at; const busy = a.filter((o) => o !== d && o.at <= t && o.end > t).length; if (busy < lanes) idle += next - t; });
    if (idle > 0) fails.push(`${programName(d.program)} waited ${Math.round(d.at - ready)} ms, ${Math.round(idle)} ms of it with a lane free of ${lanes}`);
  }
  gaps.push(...(ctx.launch?.lanesSource ? [] : ["the launcher's record does not say where its lane count came from"]));
  return decide(fails, gaps, `${a.length} Class A program(s); none waited while a lane of ${lanes} was free`);
}

function nn04(ctx) {
  // The contract's sentence is about the gate's own order; the test judges the outcome it guarantees: an expectation for every row was
  // fixed before the gate started (the history the launcher copied first), and it accounts for every row that did work.
  if (!ctx.history) return result(NOT_MEASURED, "the launcher's record has no copy of the row history taken before the gate started");
  const gaps = [...ctx.processGaps]; const fails = [];
  const copied = timeOf(ctx.history.copiedAt); const first = Math.min(...ctx.procs.map((p) => p.start));
  if (!Number.isFinite(copied)) return result(NOT_MEASURED, "the launcher's record does not say when the row history was copied");
  if (Number.isFinite(first) && copied > first) fails.push(`the row history was copied at ${new Date(copied).toISOString()}, after the run's first program started, so the expectation was not fixed before work`);
  else if (Number.isFinite(first) && copied === first) gaps.push("the row history was copied at the same moment the run's first program started, so which came first is not shown");
  if (ctx.launch?.cacheDir && ctx.launch?.historyFile && dirKey(ctx.launch.historyFile) !== dirKey(historyFileFor(path.dirname(ctx.launch.historyFile), ctx.launch.cacheDir, isWin(ctx.launch.cacheDir) ? "win32" : "linux"))) fails.push(`the history copied (${path.basename(ctx.launch.historyFile)}) is not the one the gate keeps for the cache store ${ctx.launch.cacheDir}`);
  const unclassified = [...ctx.expected.values()].filter((e) => e.expect === null);
  for (const id of rowsThatWorked(ctx)) { const e = ctx.expected.get(id); if (e && e.expect === null) gaps.push(`${id} did work and ${e.why}`); }
  return decide(fails, gaps, `${ctx.expected.size} row(s) classified before the gate started (${unclassified.length} without a file list)`);
}

function nn05(ctx) {
  if (!ctx.history) return result(NOT_MEASURED, "there is no row history from before the run, so what the run owed is not known");
  const fails = []; const gaps = [...ctx.processGaps];
  // The lookup disagrees when a row the test expects to reuse has no stored result to reuse.
  const disagree = [...ctx.expected.values()].filter((e) => e.expect === "reuse" && e.priorKey && !ctx.cacheBefore.has(e.priorKey));
  if (disagree.length) {
    if (ctx.jobs.length) fails.push(`the lookup disagreed for ${disagree[0].id} (its stored result is gone) and ${ctx.jobs.length} broker job(s) were still submitted`);
    const worked = [...rowsThatWorked(ctx)].filter((id) => !disagree.some((d) => d.id === id));
    if (worked.length) fails.push(`the lookup disagreed for ${disagree[0].id} and ${worked.slice(0, 3).join(", ")} still did work`);
    if (!Number.isInteger(ctx.exit)) gaps.push("the run's exit is not known");
    else if (ctx.exit === 0) fails.push(`the lookup disagreed for ${disagree[0].id} and the run still exited 0`);
    // Naming the row and its cause is something the gate says; its own output, kept by the launcher, is read for that and nothing else.
    for (const d of disagree) {
      const named = String(ctx.gateOutput ?? "").split(/\r?\n/).some((l) => /RK_CACHE_DIVERGENCE/.test(l) && new RegExp(`(^|[^\\w:@.-])${d.id.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}([^\\w:@.-]|$)`).test(l));
      if (!named) fails.push(`the lookup disagreed for ${d.id} and the gate's output does not name it (RK_CACHE_DIVERGENCE)`);
    }
    return decide(fails, gaps, `the lookup disagreed for ${disagree.map((d) => d.id).slice(0, 3).join(", ")}; the run stopped before sending work, named it and failed`);
  }
  // Otherwise the work that ran is the work owed: both directions.
  const owed = new Set([...ctx.expected.values()].filter((e) => e.expect === "run").map((e) => e.id));
  for (const id of owed) if (!workOf(ctx, id).did && !(toolOf(id) === "rustfmt" && rustfmtStarts(ctx).length)) (ctx.processGaps.length ? gaps : fails).push(`${id} was owed a rerun and did no work`);
  for (const id of rowsThatWorked(ctx)) {
    const e = ctx.expected.get(id);
    if (e?.expect === "reuse") fails.push(`${id} did work although nothing it reads changed`);
    else if (!e) gaps.push(`${id} did work and the history before the run does not know it, so whether it was owed cannot be shown`);
  }
  const strays = ctx.jobs.filter((j) => j.sub && ![...ctx.expected.keys()].some((id) => covers(j, id, ctx.snap.path, [...ctx.expected.keys()])));
  if (strays.length) fails.push(`${strays.length} broker job(s) do no owed row's work (first: cargo ${strays[0].sub} ${strays[0].args.slice(0, 4).join(" ")})`);
  for (const j of ctx.jobs.filter((x) => x.sub && !strays.includes(x))) {
    const extra = cratesOf(j).filter((c) => ![...owed].some((id) => covers({ ...j, args: ["-p", c, ...targetsOf(j).flatMap((t) => ["--target", t])] }, id, ctx.snap.path, [...ctx.expected.keys()])));
    if (extra.length) fails.push(`broker job ${j.jobId} also builds ${extra.join(", ")}, which no owed row needs`);
  }
  return decide(fails, gaps, `the work that ran is the ${owed.size} row(s) the test owed, no more`);
}

function nn06(ctx) {
  const fails = []; const gaps = [...ctx.processGaps];
  const reuse = [...ctx.expected.values()].filter((e) => e.expect === "reuse");
  for (const e of reuse) {
    if (!e.priorKey) gaps.push(`${e.id} is unchanged and its history names no stored result (key), so that it was reused cannot be shown`);
    const w = workOf(ctx, e.id);
    if (w.writes.length) fails.push(`${e.id} is unchanged and the cache store took a new result for it`);
    if (w.jobs.length) fails.push(`${e.id} is unchanged and broker job ${w.jobs[0].jobId} did its work`);
    if (toolOf(e.id) && toolOf(e.id) !== "rustfmt" && w.progs.length) fails.push(`${e.id} is unchanged and ${toolOf(e.id)} started`);
  }
  // A reused failure stays a failure: the run cannot end green over it.
  const failures = reuse.filter((e) => e.priorFailed);
  if (failures.length) { if (!Number.isInteger(ctx.exit)) gaps.push("the run's exit is not known"); else if (ctx.exit === 0) fails.push(`${failures[0].id} failed before and is unchanged, and the run exited 0`); }
  if (!ctx.history) gaps.push("there is no row history from before the run, so unchanged rows cannot be told");
  return decide(fails, gaps, `${reuse.length} unchanged row(s) did no work${failures.length ? `, ${failures.length} of them an earlier failure that kept the run red` : ""}`);
}

const rustfmtStarts = (ctx) => ctx.starts.filter((d) => programName(d.program) === "rustfmt" && !isProbe(d.args));
/** The file operands of a rustfmt start, as given (rustfmt is run from the snapshot root). */
const rustfmtFiles = (d) => { const out = []; for (let i = 0; i < d.args.length; i += 1) { const a = String(d.args[i]); if (["--edition", "--config", "--config-path", "--emit", "--color"].includes(a)) { i += 1; continue; } if (!a.startsWith("-")) out.push(a.replaceAll("\\", "/")); } return out; };
function nn07(ctx) {
  const fails = []; const gaps = [...ctx.processGaps];
  const run = [...ctx.expected.values()].filter((e) => e.expect === "run");
  for (const e of run) {
    if (toolOf(e.id) === "rustfmt") {
      const ed = e.id.split(":")[1]; const handed = rustfmtStarts(ctx).filter((d) => valuesOf(d.args, "--edition").includes(ed)).flatMap(rustfmtFiles);
      const owed = e.changed.filter((p) => /\.rs$/.test(p));
      for (const p of owed) if (!handed.includes(p)) (ctx.processGaps.length ? gaps : fails).push(`${e.id}: ${p} changed and was never handed to rustfmt`);
      const extra = handed.filter((p) => !owed.includes(p) && !run.some((o) => o !== e && toolOf(o.id) === "rustfmt" && o.changed.includes(p)));
      if (extra.length) fails.push(`${e.id}: rustfmt was handed ${extra.slice(0, 3).join(", ")}, which did not change`);
      for (const p of new Set(handed)) if (handed.filter((x) => x === p).length > 1) fails.push(`${e.id}: rustfmt was handed ${p} more than once`);
      continue;
    }
    const w = workOf(ctx, e.id);
    if (isBrokerRow(e.id) ? !w.jobs.length : !w.did) (ctx.processGaps.length ? gaps : fails).push(`${e.id} changed (${e.changed.slice(0, 2).join(", ")}) and ${isBrokerRow(e.id) ? "no signed broker job built it" : "did no work"}`);
  }
  return decide(fails, gaps, `${run.length} changed row(s) ran again, each with only the work its change needs`);
}

function nn08(ctx) {
  const gaps = [...ctx.processGaps];
  // A shell wrapper whose program the record shows start as its child is that child's call: the child is compared, not the wrapper.
  const resolved = new Set(ctx.procs.filter((p) => /(^|[\\/])cmd\.exe$/i.test(p.exe) && ctx.procs.some((c) => c.ppid === p.pid && c.start >= p.start && (p.end === null || c.start <= p.end))));
  const calls = []; let unclear = 0;
  ctx.procs.forEach((p, i) => {
    if (resolved.has(p)) return;
    const d = ctx.starts[i];
    if (d.ambiguous) { unclear += 1; return; }
    calls.push({ ...callOf(d, { args: p.args }), parent: ctx.parentKey(p), cwdKnown: p.cwd !== null });
  });
  const groups = new Map();
  for (const c of calls) { const k = JSON.stringify([c.name, c.path, c.args, c.cwdKnown ? c.cwd : null]); (groups.get(k) ?? groups.set(k, []).get(k)).push(c); }
  const repeats = [...groups.values()].filter((g) => g.length > 1);
  if (unclear) gaps.push(`${unclear} start(s) are command lines with no child to show what they ran, so they cannot be compared`);
  // Without a folder (Windows event 4688 records none) calls compare by program and arguments: that finds more repeats, never fewer.
  const how = ctx.procs.some((p) => p.cwd === null) ? " (compared by program and arguments: the record keeps no folder)" : "";
  const fails = repeats.map((g) => `${g.length}x ${g[0].name} ${g[0].args.join(" ").slice(0, 70)}`);
  return decide(fails.length ? [`${repeats.reduce((n, g) => n + g.length - 1, 0)} repeat start(s): ${fails.join("; ")}`] : [], fails.length ? [] : gaps, `${calls.length} program start(s), no two alike${how}`);
}

function nn10(ctx) {
  const fails = []; const gaps = [...ctx.processGaps];
  const verb = (args) => args.find((a) => !a.startsWith("-") && !a.startsWith("+"));
  const direct = ctx.starts.filter((d) => programName(d.program) === "cargo" && verb(d.args) === "test");
  if (direct.length) fails.push(`${direct.length} cargo test start(s) ran outside the broker, so the gate built tests itself`);
  const tests = ctx.jobs.filter((j) => j.sub === "test");
  if (!tests.length) return decide(fails, gaps, "the broker built no test and no cargo test ran outside it");
  let programs = 0;
  for (const j of tests) {
    const what = `broker job ${j.jobId ?? path.basename(j.file)}`;
    if (!j.verified) { (ctx.verifierPinned ? fails : gaps).push(`${what}: its receipt ${ctx.verifierPinned ? "does not verify" : "cannot be verified: no broker key is pinned"}`); continue; }
    if (!Array.isArray(j.testExecutables) && typeof j.testExecutablesReason !== "string") { fails.push(`${what}: the signed receipt does not show a compile only (no testExecutables), so the broker may have run the tests`); continue; }
    for (const exe of Array.isArray(j.testExecutables) ? j.testExecutables : []) {
      programs += 1;
      const file = typeof exe === "string" ? exe : exe?.path; const digest = typeof exe === "object" ? exe?.sha256 : null;
      if (typeof file !== "string") { fails.push(`${what}: the receipt lists a test program with no path`); continue; }
      const ran = ctx.procs.filter((p) => dirKey(p.exe) === dirKey(file));
      if (!ran.length) { (ctx.processGaps.length ? gaps : fails).push(`${what}: the broker built ${path.basename(file)} and the gate never ran it`); continue; }
      if (ran.length > 1) fails.push(`${what}: the gate ran ${path.basename(file)} ${ran.length} times`);
      const now = typeof ctx.digestOf === "function" ? ctx.digestOf(file) : null;
      if (typeof digest !== "string") gaps.push(`${what}: the receipt gives no digest for ${path.basename(file)}`);
      else if (now === null) gaps.push(`${what}: ${path.basename(file)} cannot be read now to compare with the digest the broker signed`);
      else if (now !== digest) fails.push(`${what}: ${path.basename(file)} is not the program the broker built (digest ${short(now)}, signed ${short(digest)})`);
    }
  }
  return decide(fails, gaps, `${tests.length} test build(s) by the broker, compile only and signed; the gate ran the ${programs} program(s) they list, each the one the broker built`);
}

function nn11(ctx) {
  const { launch } = ctx;
  const startMono = /^\d+$/.test(String(launch?.startMono ?? "")) ? BigInt(launch.startMono) : null; const endMono = /^\d+$/.test(String(launch?.endMono ?? "")) ? BigInt(launch.endMono) : null;
  const limitMs = Number(launch?.limitMs);
  if (startMono === null) return result(NOT_MEASURED, "the launcher's record has no start on its monotonic clock");
  if (!Number.isFinite(limitMs) || limitMs <= 0) return result(NOT_MEASURED, `no time limit was pinned at launch for ${launch?.host ?? "this host"} and run kind ${launch?.kind ?? "(none)"}`);
  // Programs the run owns that outlive the gate count too (Council CV-1 F-29), on the wall clock of the process record.
  const lastProg = Math.max(-Infinity, ...ctx.procs.map((p) => p.end ?? -Infinity));
  if (endMono === null) return result(NOT_MEASURED, `limit ${(limitMs / 1000).toFixed(1)} s, but the launcher never saw the run end`);
  if (endMono < startMono) return result(NOT_MEASURED, "the launcher's monotonic clock runs backwards between its start and end");
  let took = Number(endMono - startMono) / 1e6;
  const wallEnd = timeOf(launch.endedAt); if (Number.isFinite(lastProg) && Number.isFinite(wallEnd) && lastProg > wallEnd) took += lastProg - wallEnd;
  if (took > limitMs) return result(FAIL, `${(took / 1000).toFixed(1)} s, limit ${(limitMs / 1000).toFixed(1)} s (pinned at launch${launch.limitsSha256 ? `, limits ${short(launch.limitsSha256)}` : ""})`);
  return result(PASS, `${(took / 1000).toFixed(1)} s on the launcher's clock, limit ${(limitMs / 1000).toFixed(1)} s pinned at launch`);
}

function nn12(ctx) {
  const { live } = ctx;
  if (!live) return result(NOT_MEASURED, "what was left behind is judged right after the run ends (--live)");
  if (!ctx.snap.path) return result(NOT_MEASURED, "no broker job names the snapshot, so its lock cannot be looked for");
  if (live.exists(`${ctx.snap.path}.lock`)) return result(FAIL, `the lock ${ctx.snap.path}.lock is still there after the run`);
  if (ctx.processGaps.length) return result(NOT_MEASURED, `the run's programs cannot all be shown gone: ${ctx.processGaps[0]}`);
  const alive = ctx.procs.filter((p) => live.alive(p.pid, p.start));
  if (alive.length) return result(FAIL, `${alive.length} program(s) the run started are still alive: ${alive.slice(0, 5).map((p) => `${programName(p.exe)} ${p.pid}`).join(", ")}`);
  if (!Number.isInteger(ctx.exit) || ctx.exit !== 0) {
    // A run that did not end normally passes only on the next ordinary run: its launcher took this run's lock over and named it, and it ended 0.
    const next = ctx.successor;
    if (next && next.launch?.previous?.runId === ctx.launch?.runId && next.launch?.previous?.reclaimed === true && next.launch?.exit === 0) return result(PASS, `the run ended with ${ctx.exit ?? ctx.launch?.signal ?? "no exit"}; the next ordinary run took it over and ended 0, and nothing was left behind`);
    return result(NOT_MEASURED, `the run ended with ${ctx.exit ?? ctx.launch?.signal ?? "no exit"}; that the next ordinary run starts safely by itself needs that run's record (--successor)`);
  }
  return result(PASS, "no lock and no program of the run left behind");
}


function nn09(ctx) {
  // The compiles are the broker's signed jobs (argv, workspace, times); a broker client start with no signed job is a compile not shown.
  const fails = []; const gaps = [];
  const unverified = ctx.jobs.filter((j) => !j.verified); if (unverified.length) (ctx.verifierPinned ? fails : gaps).push(`${unverified.length} broker job(s) whose receipt ${ctx.verifierPinned ? "does not verify" : "cannot be verified: no broker key is pinned"}`);
  const compiles = ctx.jobs.filter((j) => j.verified && j.sub && COMPILING.has(j.sub)).map((j) => ({ ...j, at: j.submitted }));
  const clients = ctx.starts.filter((d) => programName(d.program) === "rustright" && d.args[0] === "cargo" && COMPILING.has(d.args[1]));
  if (clients.length > ctx.jobs.length) gaps.push(`${clients.length} broker client compile start(s) and only ${ctx.jobs.length} signed job(s), so a compile may be missing`);
  gaps.push(...ctx.processGaps.map((g) => `that every compile has its signed job cannot be shown: ${g}`));
  if (!compiles.length) return decide(fails, gaps, "nothing was compiled by the broker");
  // One group is one workspace and stage (subcommand and its settings): crate and target selectors are what one command gathers. Settings
  // compare by meaning: the same flags in another order, `--release` and `--profile release`, and a feature list in another order are the same
  // settings (Council CV-1 F-24). What follows `--` goes to the compiler in order and is kept in order.
  const VALUE_FLAGS = new Set(["--profile", "--features", "-F", "--message-format", "--manifest-path", "--config", "-Z", "--color", "-j", "--jobs", "--target-dir", "--bin", "--test", "--example", "--bench"]);
  const featureSet = (v) => v.split(/[\s,]+/).filter(Boolean).sort().join(",");
  const settings = (args) => {
    const cut = args.indexOf("--"); const head = cut < 0 ? args : args.slice(0, cut); const tail = cut < 0 ? [] : args.slice(cut);
    const units = [];
    for (let i = 0; i < head.length; i += 1) {
      const a = head[i];
      if (["-p", "--package", "--target"].includes(a)) { i += 1; continue; }
      if (/^(--package=|--target=)/.test(a) || /^-p./.test(a)) continue;
      if (a === "--release") { units.push("--profile release"); continue; }
      if (/^--profile=/.test(a)) { units.push(`--profile ${a.slice(10)}`); continue; }
      if (/^--features=/.test(a)) { units.push(`--features ${featureSet(a.slice(11))}`); continue; }
      if (VALUE_FLAGS.has(a) && i + 1 < head.length) { const v = head[(i += 1)]; units.push(`${a === "-F" ? "--features" : a} ${a === "--features" || a === "-F" ? featureSet(v) : v}`); continue; }
      units.push(a);
    }
    return [...units.sort(), ...tail].join(" ");
  };
  // The workspace a compile runs in. Two folders that nest are one workspace when the outer one's manifest, at the run's Git tree, lists the
  // inner one as a member; when that manifest cannot be read, which workspace it is stays disputed (Council CV-1 F-24).
  const rels = compiles.map((c) => relToSnap(c.cwd, ctx.snap.path));
  if (rels.some((r) => r === null)) gaps.push("a broker compile ran outside the snapshot or the snapshot folder is unknown, so its workspace cannot be placed");
  const wsOf = new Map();
  for (const r of new Set(rels.filter((x) => x !== null))) wsOf.set(r, r);
  for (const outer of new Set(rels.filter((x) => x !== null))) for (const inner of new Set(rels.filter((x) => x !== null))) {
    if (inner === outer || !(outer === "" ? inner !== "" : inner.startsWith(`${outer}/`))) continue;
    let members = null; let why = null;
    try { members = workspaceMembers(ctx.readTree(`${outer ? `${outer}/` : ""}Cargo.toml`)); } catch (e) { why = e.message; }
    if (why !== null || members === null) { if (why !== null) gaps.push(`compiles ran in ${outer || "."} and in ${inner} inside it; ${outer || "."}/Cargo.toml at the run's tree cannot be read (${why}), so whether they are one workspace is disputed`); continue; }
    const sub = outer === "" ? inner : inner.slice(outer.length + 1);
    if (members.some((m) => m === sub || (m.endsWith("/*") && sub.startsWith(m.slice(0, -1))))) wsOf.set(inner, wsOf.get(outer));
  }
  const groups = new Map();
  compiles.forEach((c, i) => { const ws = rels[i] === null ? `?${c.cwd}` : wsOf.get(rels[i]); const k = `${ws}\u0000${c.sub}\u0000${settings(c.args)}`; (groups.get(k) ?? groups.set(k, []).get(k)).push(c); });
  // A split is justified, by the check itself, only by the policy of the run's own Git tree (Council CV-1 F-25, F-26): ci/shards.json `solo`
  // names crates that always get a job of their own. Every crate and target is built once; a solo crate is alone in its command, with its
  // targets combined in one command; every other crate of the group shares one command.
  let solo; let soloProblem = null;
  const soloSet = () => { if (solo || soloProblem) return solo; try { const d = ctx.readTree("ci/shards.json"); const doc = typeof d === "string" ? JSON.parse(d) : d; solo = new Set(Array.isArray(doc?.solo) ? doc.solo.map(String) : []); } catch (e) { soloProblem = `ci/shards.json at the run's tree cannot be read (${e.message})`; } return solo; };
  // Frozen case GCX-09-7: a crate the producer's own rule (sidecarBoundCrate: an externalBin in its Tauri config) finds sidecar-bound at the
  // run's tree also keeps its own command. The crate's folder is a member of the group's workspace whose manifest names it, read at that tree.
  const treeText = (p) => { const v = ctx.readTree(p.replaceAll("\\", "/")); return typeof v === "string" ? v : JSON.stringify(v); };
  const packageName = (toml) => /^\[package\][^[]*?^name\s*=\s*"([^"]+)"/m.exec(String(toml))?.[1] ?? null;
  const boundMemo = new Map();
  const sidecarOf = (ws, crate) => {
    const key = `${ws}\u0000${crate}`; if (boundMemo.has(key)) return boundMemo.get(key);
    let out;
    try {
      const members = workspaceMembers(treeText(`${ws ? `${ws}/` : ""}Cargo.toml`)) ?? [""];
      const dirs = members.map((m) => (m.endsWith("/*") ? `${m.slice(0, -1)}${crate}` : m));
      let dir = null;
      for (const d of dirs) { const rel = [ws, d].filter(Boolean).join("/"); try { if (packageName(treeText(`${rel ? `${rel}/` : ""}Cargo.toml`)) === crate) { dir = rel; break; } } catch { /* not this member */ } }
      if (dir === null) out = { unknown: `no member of ${ws || "."}/Cargo.toml at the run's tree is package ${crate}` };
      else { const b = sidecarBoundCrate({ dir, exists: (p) => { try { treeText(p); return true; } catch { return false; } }, readFile: treeText }); out = b.bound ? { bound: true, file: String(b.file).replaceAll("\\", "/") } : { bound: false }; }
    } catch (e) { out = { unknown: `${ws || "."}/Cargo.toml at the run's tree cannot be read (${e.message})` }; }
    boundMemo.set(key, out); return out;
  };
  const justified = [];
  for (const [k, cs] of groups) {
    const where = `${cs.length}x cargo ${k.split("\u0000")[1]} in ${k.split("\u0000")[0] || "."}`;
    const seen = new Map();
    for (const c of cs) for (const crate of cratesOf(c)) for (const t of targetsOf(c).length ? targetsOf(c) : [null]) { const u = `${crate}@${t}`; seen.set(u, (seen.get(u) ?? 0) + 1); }
    const dup = [...seen].filter(([, n]) => n > 1).map(([u]) => u.replace(/@null$/, ""));
    if (dup.length) fails.push(`${where}: ${dup.join(", ")} built more than once in one workspace and stage`);
    if (cs.length < 2) continue;
    const policy = soloSet();
    if (!policy) { gaps.push(`${where} were sent as separate commands; ${soloProblem}, so the split cannot be checked`); continue; }
    const ws = k.split("\u0000")[0]; const set = new Set(policy); const reasons = new Map(); const unknown = [];
    for (const n of new Set(cs.flatMap(cratesOf))) {
      if (policy.has(n)) { reasons.set(n, "solo per the run's ci/shards.json"); continue; }
      const b = ws.startsWith("?") ? { unknown: "its workspace is not placed" } : sidecarOf(ws, n);
      if (b.bound) { set.add(n); reasons.set(n, `sidecar-bound per ${b.file} at the run's tree`); } else if (b.unknown) unknown.push(`${n}: ${b.unknown}`);
    }
    const soloCmds = cs.filter((c) => cratesOf(c).some((n) => set.has(n)));
    const mixed = soloCmds.filter((c) => cratesOf(c).length > 1);
    const ordinary = cs.filter((c) => !cratesOf(c).some((n) => set.has(n)));
    const perSolo = new Map(); for (const c of soloCmds.filter((x) => cratesOf(x).length === 1)) perSolo.set(cratesOf(c)[0], (perSolo.get(cratesOf(c)[0]) ?? 0) + 1);
    if (mixed.length) fails.push(`${where}: crate(s) that must build alone (${[...new Set(mixed.flatMap(cratesOf).filter((n) => set.has(n)))].join(", ")}) were built together with other crates`);
    if ([...perSolo.values()].some((n) => n > 1)) fails.push(`${where}: a solo crate was sent as several commands instead of one with its targets combined`);
    // NN09: "every split has a reason the check can verify"; a crate whose manifest cannot be found at the run's tree has no such reason.
    if (ordinary.length > 1) fails.push(`${where} were sent as separate commands, and only ${cs.length - ordinary.length} of them are for a crate the run's tree keeps alone (ci/shards.json solo, or sidecar-bound)${unknown.length ? `; no reason found for ${unknown.slice(0, 2).join("; ")}` : ""}`);
    if (!mixed.length && ![...perSolo.values()].some((n) => n > 1) && ordinary.length <= 1) justified.push([...perSolo.keys()].map((n) => `${n} ${reasons.get(n)}`).join(", "));
  }
  // Council CV-1 F-27: actual command intervals, not group envelopes. Commands that did not all start together cannot pass without a record of
  // when each became ready and how much capacity was free, which no record of the run carries yet.
  // Independent groups run at the same time: the gate submits each without waiting for another to end. A job the broker queued for want of
  // capacity (its signed queue wait) is the broker's, not the gate's.
  if (compiles.some((c) => !Number.isFinite(c.submitted) || !Number.isFinite(c.ended))) gaps.push("a broker job's receipt gives no submission or end time");
  else for (const c of compiles) { const before = compiles.find((o) => o !== c && o.ended <= c.submitted); if (before) fails.push(`broker job ${c.jobId} (cargo ${c.sub} ${cratesOf(c).join(" ")}) was submitted only after job ${before.jobId} ended, so independent groups did not run together`); }
  return decide(fails, gaps, `${compiles.length} compile command(s) in ${groups.size} group(s), one per workspace and stage${justified.length ? ` (split: ${justified.join("; ")})` : ""}, all submitted together`);
}


const JUDGES = Object.freeze({ NN01: nn01, NN02: nn02, NN03: nn03, NN04: nn04, NN05: nn05, NN06: nn06, NN07: nn07, NN08: nn08, NN09: nn09, NN10: nn10, NN11: nn11, NN12: nn12 });
export const CHECK_IDS = Object.freeze(Object.keys(JUDGES));

/**
 * The snapshot folder, from the broker's signed job folders and the selected tree alone: the folder each job ran in, less the part that is a
 * Cargo workspace of the tree (`ws` when `ws/Cargo.toml` is in the tree). Null when the jobs do not agree on one, or there are none.
 */
function snapshotOf(jobs, readTree) {
  const roots = new Set();
  for (const j of jobs) {
    if (!j.cwd) continue; const segs = dirKey(j.cwd).split("/"); let found = null;
    for (let k = 1; k <= segs.length; k += 1) { const rel = segs.slice(k).join("/"); try { readTree(`${rel ? `${rel}/` : ""}Cargo.toml`); found = segs.slice(0, k).join("/"); break; } catch { /* not this split */ } }
    if (found === null) return null; roots.add(found);
  }
  return roots.size === 1 ? [...roots][0] : null;
}

/**
 * Judges one run against `lines` (readAcceptanceLines of the contract; required, and checked again here) from evidence the gate does not write:
 * `launch`: the launcher's record (acceptance-launch.mjs). `processes` + `witness`: the operating system's record of every program start and
 * exit in the run's window (processesFromWindowsEvents / processesFromEslogger) and what it covers. `receipts`: the broker's receipts that
 * appeared during the run, each { file, sha256, doc, verified: { ok, problems } } (verified against the pinned broker key). `history`: the
 * row history the launcher copied before the gate started ({ copiedAt, A, B }). `cacheBefore`: the cache keys present before the run;
 * `cacheWrites`: the cache entries written during it. `git`: { tree, blobOf(path), readTree(path) } of the selected commit. `gateOutput`: the
 * gate's own output, kept by the launcher, read only to see whether the gate named a disagreeing row (NN05). `live` / `successor` (NN12),
 * `digestOf(file)` (NN10). Returns { lines: [{ id, text, verdict, why }], pass, failed, notMeasured }.
 */
export function judgeRun({ lines, launch = null, processes = null, witness = null, receipts = [], history = null, cacheBefore = null, cacheWrites = [], git = null, gateOutput = "", live = null, successor = null, verifierPinned = false, digestOf = null } = {}) {
  const unlinked = Array.isArray(lines) ? linkProblems(lines) : ["no lines were given: read them from the contract with readAcceptanceLines"];
  if (unlinked.length) throw Object.assign(new Error(`the contract and the acceptance test do not match: ${unlinked.join("; ")}`), { problems: unlinked });
  const g = git ?? { tree: null, blobOf: () => null, readTree: () => { throw new Error("no git reader was given (--repo)"); } };
  const tree = runTree(launch, processes, witness);
  const start = timeOf(launch?.startedAt); const end = timeOf(launch?.endedAt);
  const jobs = (receipts ?? []).map(jobOf).filter((j) => !Number.isFinite(j.submitted) || ((!Number.isFinite(start) || j.submitted >= start) && (!Number.isFinite(end) || j.submitted <= end)));
  const ctx = {
    launch, commit: launch?.commit ?? null, tree: g.tree, exit: Number.isFinite(end) && Number.isInteger(launch?.exit) ? launch.exit : null,
    procs: tree.procs, processGaps: tree.gaps, starts: tree.procs.map((p) => ({ ...decodeStart({ cmd: p.exe, args: p.args, cwd: p.cwd ?? "" }), at: p.start, end: p.end, exit: p.exit })),
    parentKey: (p) => { const parent = [tree.root, ...tree.procs].find((q) => q && q.pid === p.ppid && q.start <= p.start); return parent ? `${programName(parent.exe)}#${parent.start}` : "?"; },
    jobs, verifierPinned, snap: { path: snapshotOf(jobs, g.readTree) }, readTree: g.readTree,
    history, expected: expectedRows(history, g), cacheBefore: new Set(cacheBefore ?? []), cacheWrites: (cacheWrites ?? []).filter((w) => { const t = timeOf(w.at); return Number.isFinite(t) && (!Number.isFinite(start) || t >= start) && (!Number.isFinite(end) || t <= end); }),
    gateOutput, live, successor, digestOf,
  };
  const judged = lines.map(({ id, text: sentence }) => { let r; try { r = JUDGES[id](ctx); } catch (e) { r = result(FAIL, `the check itself failed: ${e.message}`); } return { id, text: sentence, verdict: r.verdict, why: r.why }; });
  const failed = judged.filter((l) => l.verdict === FAIL).map((l) => l.id); const notMeasured = judged.filter((l) => l.verdict === NOT_MEASURED).map((l) => l.id);
  return { lines: judged, pass: failed.length === 0 && notMeasured.length === 0, failed, notMeasured };
}

/**
 * A run folder's evidence as judgeRun reads it, gathered from outside the gate: the launcher's record and its before-run copies, the process
 * record the launcher exported, the broker receipts that appeared during the run, the cache entries written during it, and git.
 */
export function readRunDir(dir, { repo = null, receiptsDir = null, cacheDir = null, verify = null } = {}) {
  const text = (p) => { try { return fs.readFileSync(p, "utf8"); } catch { return null; } };
  const json = (p) => { try { return JSON.parse(text(p) ?? "null"); } catch { return null; } };
  const launch = json(path.join(dir, "launch.json"));
  const proc = json(path.join(dir, "processes.json"));
  const processes = proc?.kind === "windows-security" ? processesFromWindowsEvents(proc.events) : proc?.kind === "eslogger" ? processesFromEslogger(proc.events) : null;
  let history = null; const h = json(path.join(dir, "before", "history.json"));
  if (h) { try { const body = JSON.parse(h.body); history = { copiedAt: launch?.historyCopiedAt ?? null, A: Object.fromEntries(body?.stages?.A ?? []), B: Object.fromEntries(body?.stages?.B ?? []) }; } catch { history = null; } }
  const cacheBefore = (text(path.join(dir, "before", "cache-keys.txt")) ?? "").split(/\r?\n/).filter(Boolean);
  const receiptsBefore = new Set((text(path.join(dir, "before", "receipts.txt")) ?? "").split(/\r?\n/).filter(Boolean));
  const receipts = [];
  if (receiptsDir) for (const f of fs.readdirSync(receiptsDir).filter((n) => n.endsWith(".json") && !receiptsBefore.has(n))) {
    const file = path.join(receiptsDir, f); const body = fs.readFileSync(file);
    let doc = null; try { doc = JSON.parse(body.toString("utf8")); } catch { doc = null; }
    receipts.push({ file, sha256: createHash("sha256").update(body).digest("hex"), doc, verified: verify ? verify(file) : { ok: false, problems: ["no broker key is pinned"] } });
  }
  const before = new Set(cacheBefore); const cacheWrites = [];
  if (cacheDir) for (const f of fs.readdirSync(cacheDir).filter((n) => n.endsWith(".json"))) {
    const key = f.slice(0, -5); const st = fs.statSync(path.join(cacheDir, f));
    if (before.has(key) && !(st.mtimeMs >= timeOf(launch?.startedAt))) continue;
    const e = json(path.join(cacheDir, f)); if (e && typeof e.at === "string") cacheWrites.push({ key, at: e.at, row: e.row ?? null });
  }
  let git = null;
  if (repo && launch?.commit) {
    const run = (...a) => execFileSync("git", ["-C", repo, ...a], { encoding: "utf8", stdio: ["ignore", "pipe", "pipe"], windowsHide: true, maxBuffer: 256 * 1024 * 1024 });
    let tree = null; try { tree = run("rev-parse", `${launch.commit}^{tree}`).trim(); } catch { tree = null; }
    const blobs = new Map(); if (tree) for (const l of run("ls-tree", "-r", "-z", tree).split("\0")) { const m = /^\d+ blob ([0-9a-f]+)\t(.+)$/.exec(l); if (m) blobs.set(m[2], m[1]); }
    git = { tree, blobOf: (p) => blobs.get(String(p).replaceAll("\\", "/")) ?? null, readTree: (p) => { const t = run("cat-file", "-p", `${tree}:${p}`); try { return JSON.parse(t); } catch { return t; } } };
  }
  return { launch, processes, witness: proc ? { kind: proc.kind, from: proc.from, to: proc.to, problems: proc.problems ?? [] } : null, receipts, history, cacheBefore, cacheWrites, git, gateOutput: text(path.join(dir, "gate.log")) ?? "" };
}

// ---- command line -----------------------------------------------------------------------------------------------------------------------------------
if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const opt = (name) => { const i = process.argv.indexOf(name); return i < 0 ? null : process.argv[i + 1]; };
  const usage = "usage: acceptance-lines.mjs --run <run dir> --repo <dir> --cache-dir <dir> --receipts <dir> [--broker-key <hex> --broker-key-id <hex> [--rustright <client>]] [--successor <run dir>] [--live] [--contract <file>] [--json]";
  const runDir = opt("--run"); if (!runDir) { console.error(usage); process.exit(2); }
  const contractFile = opt("--contract") ?? CONTRACT_FILE; let lines;
  try { lines = readAcceptanceLines(fs.readFileSync(contractFile, "utf8")); } catch (e) { console.error(`NOT JUDGED: ${e.code === "ENOENT" ? `the contract ${contractFile} cannot be read` : e.message}`); process.exit(2); }
  const key = opt("--broker-key"); const keyId = opt("--broker-key-id"); const client = opt("--rustright") ?? "rustright";
  let host = null; try { host = JSON.parse(fs.readFileSync(path.join(runDir, "launch.json"), "utf8")).host; } catch { host = null; }
  // The broker's own verifier (rustright verify-receipt) with the key pinned by the caller.
  const verify = key && keyId && host ? (file) => { try { execFileSync(client, ["verify-receipt", "--receipt", file, "--public-key", key, "--key-id", keyId, "--host", host], { stdio: ["ignore", "pipe", "pipe"], windowsHide: true }); return { ok: true, problems: [] }; } catch (e) { return { ok: false, problems: [String(e.stderr ?? e.message).trim().split(/\r?\n/).pop()] }; } } : null;
  const evidence = readRunDir(runDir, { repo: opt("--repo"), receiptsDir: opt("--receipts"), cacheDir: opt("--cache-dir"), verify });
  const alive = (pid) => { try { process.kill(pid, 0); return true; } catch (e) { return e.code === "EPERM"; } };
  const live = process.argv.includes("--live") ? { exists: (p) => fs.existsSync(p), alive } : null;
  const successor = opt("--successor") ? { launch: (() => { try { return JSON.parse(fs.readFileSync(path.join(opt("--successor"), "launch.json"), "utf8")); } catch { return null; } })() } : null;
  const digestOf = (file) => { try { return createHash("sha256").update(fs.readFileSync(file)).digest("hex"); } catch { return null; } };
  const verdict = judgeRun({ lines, ...evidence, live, successor, verifierPinned: Boolean(verify), digestOf });
  if (process.argv.includes("--json")) console.log(JSON.stringify(verdict, null, 2));
  else { for (const l of verdict.lines) console.log(`${l.id} ${l.verdict.toUpperCase().padEnd(12)} ${l.text}\n      ${l.why}`); console.log(verdict.pass ? "PASS: all twelve lines" : `NOT PASSED: ${verdict.failed.length} failed, ${verdict.notMeasured.length} not measured`); }
  process.exit(verdict.pass ? 0 : 1);
}
