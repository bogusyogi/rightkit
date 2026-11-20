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
//   node acceptance-lines.mjs --run <run dir> --repo <dir> --receipts <dir> [--inventory <file>] [--broker-key <hex> --broker-key-id <hex>]
//                             [--successor <run dir>] [--live] [--contract <file>] [--json]
import fs from "node:fs";
import path from "node:path";
import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { enumerateRows } from "./acceptance-rows.mjs";
import os from "node:os";
import { fileURLToPath } from "node:url";
import { sidecarBoundCrate } from "./deterministic-scans.mjs";

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
  const gaps = []; const all = processes ?? [];
  if (!witness) return { root: null, procs: [], gaps: ["there is no operating-system record of the programs this run started"] };
  gaps.push(...(witness.problems ?? []).map((p) => `the operating-system process record is incomplete: ${p}`));
  const start = timeOf(launch?.startedAt); const end = timeOf(launch?.endedAt);
  if (!(timeOf(witness.from) <= start)) gaps.push("the operating-system process record does not start before the run did");
  if (!Number.isFinite(end)) gaps.push("the launcher never saw the run end, so the process record cannot be shown to cover it");
  else if (!(timeOf(witness.to) >= end)) gaps.push("the operating-system process record ends before the run did");
  // The log itself must have advanced past the run's end when it was read (its newest event), so events cannot be missing from the tail.
  if (!Number.isFinite(timeOf(witness.newest))) gaps.push("the process record does not say how far the Security log had advanced when it was read");
  else if (Number.isFinite(end) && timeOf(witness.newest) < end) gaps.push("the Security log's newest event is older than the run's end, so events may not have been written yet");
  if (all.some((p) => !Number.isInteger(p.pid) || !Number.isInteger(p.ppid) || !Number.isFinite(p.start))) gaps.push("the process record holds a start with no usable process id, parent or time");
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
  if (root.end === null) gaps.push("the gate process the launcher started has no recorded exit, so that nothing else ran after it cannot be shown");
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

/**
 * What the run owes, worked out from git and an independent row inventory (rightkit#1514 step 3; chat gate 2026-11-21: no gate history). `inventory`:
 * Map row id -> { stage, files: [paths the row reads] }, from a source that is not the gate's reporting (the planner's entry point once it takes plain
 * data; null until then). The prior state is the tree of the launcher's baseline commit (the previous launched run's selected commit), never a gate
 * file. A row none of whose files changed between the two trees is owed reuse; a row with a changed, added or removed file is owed a rerun of exactly
 * those files' work. Returns Map id -> { id, stage, expect: "reuse"|"run", changed: [paths], priorFailed, isNew }.
 */
function expectedRows(inventory, git, prior) {
  const out = new Map();
  if (!inventory || typeof git?.priorBlobOf !== "function") return out;
  for (const [id, row] of inventory) {
    const files = Array.isArray(row?.files) ? row.files.map((p) => String(p).replaceAll("\\", "/")) : [];
    const changed = files.filter((p) => (git.blobOf(p) ?? null) !== (git.priorBlobOf(p) ?? null));
    const isNew = files.length > 0 && files.every((p) => (git.priorBlobOf(p) ?? null) === null);
    out.set(id, { id, stage: row?.stage === "B" ? "B" : "A", expect: changed.length || !files.length ? "run" : "reuse", changed: changed.length ? changed : (files.length ? [] : ["(no input files)"]), priorFailed: prior?.failedRows?.has(id) === true, isNew });
  }
  return out;
}

/**
 * The work each row actually did, from evidence the gate does not write: a result the cache store took during the run (its entry's own time),
 * a program of the row's tool the operating system saw start, or a broker job the broker signed that covers the row.
 */
function workOf(ctx, id) {
  const tool = toolOf(id);
  let progs = tool ? ctx.starts.filter((d) => programName(d.program) === tool && !isProbe(d.args)) : [];
  if (tool === "rustfmt") { const ed = String(id).split(":")[1]; progs = progs.filter((d) => valuesOf(d.args, "--edition").includes(ed)); }
  const jobs = isBrokerRow(id) ? ctx.vjobs.filter((j) => j.sub && covers(j, id, ctx.snap.path, [...ctx.expected.keys()])) : [];
  // A job that covers two feature-set variants of one row cannot say which it built (feature-set variants are told apart by their id only).
  const ambiguous = jobs.some((j) => [...ctx.expected.keys()].some((o) => o !== id && otherVariant(o, id) && covers(j, o, ctx.snap.path, [...ctx.expected.keys()])));
  return { progs, jobs, ambiguous, did: jobs.length > 0 || progs.length > 0 };
}
const classOf = (ctx, id) => ctx.expected.get(id)?.stage ?? (isBrokerRow(id) ? "B" : "A");
/** Every row that did work in this run: rows with a cache write, plus rows a broker job covers. */
const rowsThatWorked = (ctx) => new Set([...ctx.expected.keys()].filter((id) => workOf(ctx, id).did));
/** What the line cannot show without a validated baseline and an independent inventory (EGM design, rightkit#1514). */
const baselineGaps = (ctx) => [...(ctx.baselineGap ? [ctx.baselineGap] : []), ...(ctx.inventory && ctx.inventoryUncovered?.length ? [`the test's own enumeration covers the Rust formatting and compile rows only; ${ctx.inventoryUncovered.length} kinds of row are decided by rules that exist only in gate code (row-rules-only-in-gate-code.md), so the rows they decide are not measured`] : []), ...(ctx.inventory ? [] : ["no independent row inventory is available (a planner entry point that takes plain data is separate work), so which rows exist and what files each reads is not shown"])];

// ---- the twelve lines -------------------------------------------------------------------------------------------------------------------------------
const classAStarts = (ctx) => ctx.starts.filter((d) => Object.values(TOOLS).includes(programName(d.program)) && !isProbe(d.args));
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
  // Class A work is bound to the selected tree by the folder it ran in: a program that ran in another folder read another tree, and one whose folder
  // the record does not keep (Windows event 4688 has none) cannot be placed.
  const aStarts = classAStarts(ctx);
  if (aStarts.length && ctx.snap.path) {
    const unplaced = aStarts.filter((d) => !d.cwd);
    const elsewhere = aStarts.filter((d) => d.cwd && relToSnap(d.cwd, ctx.snap.path) === null);
    if (elsewhere.length) fails.push(`${elsewhere.length} Class A program(s) ran outside the selected snapshot (first: ${programName(elsewhere[0].program)} in ${elsewhere[0].cwd})`);
    if (unplaced.length) gaps.push(`${unplaced.length} Class A program(s) ran in a folder the process record does not keep, so that they read the selected tree is not shown`);
  } else if (aStarts.length) gaps.push("Class A work ran and no broker job names the snapshot folder, so that it read the selected tree is not shown");
  // A compile the gate ran itself, outside the broker, has no signed commit or tree.
  const own = ctx.starts.filter((d) => programName(d.program) === "cargo" && COMPILING.has(d.args.find((a) => !a.startsWith("-") && !a.startsWith("+"))) || programName(d.program) === "rustc");
  if (own.length) fails.push(`${own.length} compile(s) ran outside the broker (first: ${programName(own[0].program)} ${own[0].args.slice(0, 3).join(" ")}), so what they built is not shown`);
  return decide(fails, gaps, `selected commit ${short(ctx.commit)} is tree ${short(ctx.tree)}; ${ctx.jobs.length} broker job(s) signed for that tree on an unchanged snapshot; nothing compiled outside the broker`);
}

function nn02(ctx) {
  const fails = []; const gaps = [...ctx.processGaps];
  const mine = (d) => !d.cwd || !ctx.snap.path || relToSnap(d.cwd, ctx.snap.path) !== null;
  const a = classAStarts(ctx).filter(mine);
  // Class A failed: a Class A result the cache store took as a failure, or a Class A tool that exited non-zero.
  // A Class A tool that exited non-zero, or an unchanged Class A row whose earlier result was a failure and is reused (F-9): Class A failed.
  const failedRows = [...ctx.expected.values()].filter((e) => e.stage === "A" && e.expect === "reuse" && e.priorFailed).map((e) => e.id);
  const failedProgs = a.filter((d) => d.exit !== null && d.exit !== 0);
  const failed = failedRows.length > 0 || failedProgs.length > 0;
  const rust = ctx.starts.filter((d) => programName(d.program) === "rustright");
  const clients = rust.filter((d) => d.args[0] === "cargo");
  const probes = rust.filter((d) => !clients.includes(d));
  const unknownClient = probes.filter((d) => !(isProbe(d.args) || /^(quiet|verify-receipt|request-digest|status)$/.test(d.args[0] ?? "")));
  if (unknownClient.length) gaps.push(`${unknownClient.length} broker client start(s) that are not a send or a known probe (first: rustright ${unknownClient[0].args.slice(0, 2).join(" ")}), so whether they sent work is not shown`);
  gaps.push(...ctx.jobGaps);
  if (failed) {
    if (ctx.jobs.length || clients.length) fails.push(`Class A failed (${failedRows[0] ?? programName(failedProgs[0].program)}) and ${ctx.jobs.length} broker job(s) / ${clients.length} broker client start(s) still followed`);
    return decide(fails, gaps, `Class A failed (${failedRows.slice(0, 2).join(", ") || programName(failedProgs[0].program)}) and nothing went to the broker`);
  }
  if (!a.length && !ctx.expected.size) gaps.push("no Class A program started and no Class A row is known, so that Class A finished is not shown");
  for (const e of ctx.expected.values()) if (e.stage === "A" && e.expect === "run" && !workOf(ctx, e.id).did && toolOf(e.id)) gaps.push(`${e.id} is owed a Class A run and no program of it was seen, so when Class A finished is not shown`);
  const aEnd = Math.max(-Infinity, ...a.map((d) => d.end ?? Infinity));
  if (a.some((d) => d.end === null)) gaps.push("a Class A program has no recorded exit, so when Class A finished is not shown");
  for (const j of ctx.vjobs) {
    const named = typeof j.gate?.classA === "string" ? j.gate.classA : null;
    if (named === null) gaps.push(`broker job ${j.jobId}: its signed receipt names no Class A verdict, so what admitted it is not shown`);
    else { const t = ctx.verdictTimes?.get(named); if (!Number.isFinite(t)) gaps.push(`broker job ${j.jobId}: the Class A verdict it names (${path.basename(named)}) was not observed on disk`); else if (t < timeOf(ctx.launch?.startedAt)) fails.push(`broker job ${j.jobId} was admitted on a Class A verdict (${path.basename(named)}) written before this run began`); }
  }
  for (const j of ctx.jobs) {
    if (!Number.isFinite(j.submitted)) { gaps.push(`broker job ${j.jobId}: its receipt gives no time`); continue; }
    if (j.submitted < aEnd) fails.push(`broker job ${j.jobId} was submitted before Class A finished`);
    else if (j.submitted === aEnd) gaps.push(`broker job ${j.jobId} was submitted at the same moment Class A ended, so which came first is not shown`);
  }
  for (const c of clients) { if (c.at < aEnd) fails.push(`a broker client started (rustright ${c.args.slice(0, 2).join(" ")}) before Class A finished`); else if (c.at === aEnd) gaps.push("a broker client started at the same moment Class A ended, so which came first is not shown"); }
  if (clients.length > ctx.jobs.length) gaps.push(`${clients.length} broker client send(s) and ${ctx.jobs.length} signed job(s): a send with no receipt cannot be placed after Class A`);
  return decide(fails, gaps, `Class A passed (${a.length} tool program(s)), then ${ctx.jobs.length} broker job(s) were submitted after it ended`);
}

function nn03(ctx) {
  const gaps = [...ctx.processGaps, ...ctx.jobGaps]; const fails = [];
  const owedA = [...ctx.expected.values()].filter((e) => e.stage === "A" && e.expect === "run");
  const all = classAStarts(ctx);
  if (all.some((d) => d.end !== null && d.end < d.at)) return result(NOT_MEASURED, "the process record has a Class A program ending before it started");
  const a = all.filter((d) => Number.isFinite(d.at) && d.end !== null);
  gaps.push(...baselineGaps(ctx));
  if (!a.length) {
    // Positive proof of complete reuse is the one pass that needs no readiness: every Class A row is owed reuse, none ran, and the record is whole.
    const rows = [...ctx.expected.values()].filter((e) => e.stage === "A");
    if (!rows.length) gaps.push("no Class A row is known, so that none was owed work is not shown");
    if (owedA.length) fails.push(`${owedA.length} Class A row(s) were owed a run (first: ${owedA[0].id}) and no Class A program started`);
    return decide(fails, gaps, `Class A was wholly reused (${rows.length} row(s) owed reuse, no program started), so no row waited`);
  }
  const lanes = Number(ctx.launch?.lanes);
  if (!Number.isInteger(lanes) || lanes <= 0) return result(NOT_MEASURED, "the launcher's record gives no lane count for Class A on this host");
  // When each row became ready is an input from outside the gate (none exists on a real run yet); a row's start is held to it. Without it the first start
  // is the row's own evidence that it was ready, which can show a lane left idle but never a row that waited before any start.
  const ready = (d) => { const r = ctx.readiness?.get(programName(d.program)); return Number.isFinite(r) ? r : null; };
  for (const d of a) { const r = ready(d); if (r !== null && d.at - r > 0 && a.filter((o) => o !== d && o.at <= d.at && o.end > r).length < lanes) fails.push(`${programName(d.program)} was ready at ${new Date(r).toISOString()} and started ${Math.round(d.at - r)} ms later with a lane free of ${lanes}`); }
  const firstReady = Math.min(...a.map((d) => ready(d) ?? d.at));
  for (const d of a) {
    if (d.at === firstReady || ready(d) !== null) continue;
    const points = [...new Set([firstReady, ...a.flatMap((o) => [o.at, o.end]).filter((t) => t > firstReady && t < d.at)])].sort((x, y) => x - y);
    let idle = 0;
    points.forEach((t, i) => { const next = points[i + 1] ?? d.at; const busy = a.filter((o) => o !== d && o.at <= t && o.end > t).length; if (busy < lanes) idle += next - t; });
    if (idle > 0) fails.push(`${programName(d.program)} waited ${Math.round(d.at - firstReady)} ms, ${Math.round(idle)} ms of it with a lane free of ${lanes}`);
  }
  if (!ctx.readiness) gaps.push("when each Class A row became ready is not shown by any evidence outside the gate, so a row that waited before the first program started is not ruled out");
  gaps.push(...(ctx.launch?.lanesSource ? [] : ["the launcher's record does not say where its lane count came from"]));
  return decide(fails, gaps, `${a.length} Class A program(s); none waited while a lane of ${lanes} was free`);
}

function nn04() {
  // The contract sentence is about the order of two events inside the gate: its forecast written, then its first cache lookup. Evidence the gate does
  // not write shows neither. Chat gate, 2026-11-21: not measured until Adrian rewords the sentence; the contract is unchanged.
  return result(NOT_MEASURED, "the line is about the order of the gate's own forecast and first lookup, which evidence the gate does not write cannot show; it stays not measured until Adrian rewords it");
}

function nn05(ctx) {
  const fails = []; const gaps = [...ctx.processGaps, ...ctx.jobGaps, ...baselineGaps(ctx)];
  // The test cannot see a lookup or what the gate said about it; it can see the work. A mismatch between the work owed and the work done is the
  // disagreement the line is about, and that the gate stopped and named the row is not shown by evidence the gate does not write.
  const owed = new Set([...ctx.expected.values()].filter((e) => e.expect === "run").map((e) => e.id));
  for (const id of owed) { const w = workOf(ctx, id); if (!w.did) (ctx.processGaps.length || ctx.jobGaps.length || (!toolOf(id) && !isBrokerRow(id)) ? gaps : fails).push(`${id} was owed a rerun and ${toolOf(id) || isBrokerRow(id) ? "no program or signed job shows it" : "has no program or signed job that could show it (an in-process row)"}`); if (w.ambiguous) gaps.push(`${id}: a broker job covers it and another variant of it, so which it built is not shown`); }
  for (const id of rowsThatWorked(ctx)) {
    const e = ctx.expected.get(id);
    if (e?.expect === "reuse") (ctx.launch?.baseline?.stable === true ? fails : gaps).push(`${id} did work although no file it reads changed${ctx.launch?.baseline?.stable === true ? "" : " (and the launcher does not show the gate code, tools and environment are the baseline's)"}`);
  }
  const jobList = ctx.vjobs;
  const strays = jobList.filter((j) => j.sub && ![...ctx.expected.keys()].some((id) => covers(j, id, ctx.snap.path, [...ctx.expected.keys()])));
  const haveB = [...ctx.expected.values()].some((e) => e.stage === "B");
  if (strays.length) (haveB ? fails : gaps).push(`${strays.length} broker job(s) do no ${haveB ? "known" : "known (no Class B row is known at all)"} row's work (first: cargo ${strays[0].sub} ${strays[0].args.slice(0, 4).join(" ")})`);
  for (const e of [...ctx.expected.values()].filter((x) => x.expect === "run" && toolOf(x.id) === "rustfmt")) {
    const ed = e.id.split(":")[1]; const handed = rustfmtStarts(ctx).filter((d) => valuesOf(d.args, "--edition").includes(ed)).flatMap(rustfmtFiles);
    const extra = handed.filter((p) => !e.changed.includes(p));
    if (extra.length) fails.push(`${e.id}: rustfmt was handed ${extra.slice(0, 3).join(", ")}, which no owed change needs`);
  }
  for (const j of jobList.filter((x) => x.sub && !strays.includes(x))) {
    const extra = cratesOf(j).filter((c) => ![...owed].some((id) => covers({ ...j, args: ["-p", c, ...targetsOf(j).flatMap((t) => ["--target", t])] }, id, ctx.snap.path, [...ctx.expected.keys()])));
    if (extra.length) fails.push(`broker job ${j.jobId} also builds ${extra.join(", ")}, which no owed row needs`);
  }
  // A mismatch is a fail above; that the gate stopped and named the row and its cause is not shown by evidence the gate does not write, and matters only then.
  return decide(fails, gaps, `the work that ran is the ${owed.size} row(s) owed, no more, and nothing owed reuse did work`);
}

function nn06(ctx) {
  const fails = []; const gaps = [...ctx.processGaps, ...ctx.jobGaps, ...baselineGaps(ctx)];
  const reuse = [...ctx.expected.values()].filter((e) => e.expect === "reuse");
  // Git shows file changes only. A tool, environment or gate-code change also reruns a row; the launcher records these before the run and the run
  // counts as unchanged only when they equal the baseline's, so an unchanged-looking row that legitimately reruns is not a false pass.
  const stable = ctx.launch?.baseline?.stable === true;
  if (reuse.length && !stable) gaps.push("the launcher does not show that the gate code, tools and keyed environment are the baseline's, so an unchanged-looking row may still be owed a rerun");
  for (const e of reuse) {
    const w = workOf(ctx, e.id);
    if (w.jobs.length) (stable ? fails : gaps).push(`${e.id} is unchanged and broker job ${w.jobs[0].jobId} did its work`);
    if (w.progs.length) (stable ? fails : gaps).push(`${e.id} is unchanged and ${toolOf(e.id)} started${toolOf(e.id) === "rustfmt" ? " (rustfmt was handed files for an edition nothing changed in)" : ""}`);
  }
  // A reused failure stays a failure: the previous launched run's own evidence says the row failed, and this run cannot end green over it.
  if (reuse.length && !ctx.prior) gaps.push("the previous launched run's evidence is not available, so whether an unchanged row failed before is not shown");
  const failures = reuse.filter((e) => e.priorFailed);
  if (failures.length) { if (!Number.isInteger(ctx.exit)) gaps.push("the run's exit is not known"); else if (ctx.exit === 0) fails.push(`${failures[0].id} failed in the previous launched run and is unchanged, and the run exited 0`); }
  const inProcess = reuse.filter((e) => !toolOf(e.id) && !isBrokerRow(e.id));
  if (inProcess.length) gaps.push(`${inProcess.length} unchanged row(s) run in the gate's own process (first: ${inProcess[0].id}), so that none reran is not shown by any program or signed job`);
  return decide(fails, gaps, `${reuse.length} unchanged row(s) did no work${failures.length ? `, ${failures.length} of them an earlier failure that kept the run red` : ""}`);
}

const rustfmtStarts = (ctx) => ctx.starts.filter((d) => programName(d.program) === "rustfmt" && !isProbe(d.args));
/** The file operands of a rustfmt start, as given (rustfmt is run from the snapshot root). */
const rustfmtFiles = (d) => { const out = []; for (let i = 0; i < d.args.length; i += 1) { const a = String(d.args[i]); if (["--edition", "--config", "--config-path", "--emit", "--color"].includes(a)) { i += 1; continue; } if (!a.startsWith("-")) out.push(a.replaceAll("\\", "/")); } return out; };
function nn07(ctx) {
  const fails = []; const gaps = [...ctx.processGaps, ...ctx.jobGaps, ...baselineGaps(ctx)];
  const run = [...ctx.expected.values()].filter((e) => e.expect === "run");
  for (const e of run) {
    if (e.isNew && !toolOf(e.id) && !isBrokerRow(e.id)) { gaps.push(`${e.id} is a new row with no program or signed job that could show it ran`); continue; }
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
    if (isBrokerRow(e.id) ? !w.jobs.length : !w.did) (ctx.processGaps.length || ctx.jobGaps.length ? gaps : fails).push(`${e.id} changed (${e.changed.slice(0, 2).join(", ")}) and ${isBrokerRow(e.id) ? "no signed broker job built it" : "did no work"}`);
  }
  return decide(fails, gaps, `${run.length} changed row(s) ran again, each with only the work its change needs`);
}

function nn08(ctx) {
  const gaps = [...ctx.processGaps];
  // A shell wrapper whose program the record shows start as its child is that child's call: the child is compared, not the wrapper.
  const resolved = new Set(ctx.procs.filter((p) => /(^|[\\/])cmd\.exe$/i.test(p.exe) && ctx.procs.some((c) => c.ppid === p.pid && c.start >= p.start && (p.end === null || c.start <= p.end))));
  const calls = []; const wrapped = []; let unclear = 0;
  ctx.procs.forEach((p, i) => {
    if (resolved.has(p)) return;
    const d = ctx.starts[i];
    if (d.ambiguous) { if (d.intended) wrapped.push({ ...callOf({ ...d, ...d.intended }, { args: d.intended.args }), parent: ctx.parentKey(p), cwdKnown: p.cwd !== null }); else unclear += 1; return; }
    calls.push({ ...callOf(d, { args: p.args }), parent: ctx.parentKey(p), cwdKnown: p.cwd !== null });
  });
  const groups = new Map();
  for (const c of calls) { const k = JSON.stringify([c.name, c.path, c.args, c.cwdKnown ? c.cwd : null]); (groups.get(k) ?? groups.set(k, []).get(k)).push(c); }
  const repeats = [...groups.values()].filter((g) => g.length > 1);
  // A wrapper with no child shows only what it was written as: if that is the same call as another start, it is a repeat; otherwise it cannot be compared.
  for (const w of wrapped) { const same = calls.find((c) => sameProgram(c, w) && JSON.stringify(c.args) === JSON.stringify(w.args)); if (same) repeats.push([same, { ...w, name: `${w.name} (written again through cmd /c)` }]); else unclear += 1; }
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
  if (ctx.jobs.some((j) => !j.argv)) gaps.push("a broker job's signed receipt names no command, so whether it was a test build is not shown");
  const tests = ctx.jobs.filter((j) => j.sub === "test");
  // Every test request the process record saw go to the broker needs its own signed job.
  const requested = ctx.starts.filter((d) => programName(d.program) === "rustright" && d.args[0] === "cargo" && d.args[1] === "test");
  if (requested.length > tests.length) gaps.push(`${requested.length} test request(s) went to the broker and ${tests.length} signed test job(s) exist: a request with no receipt is not shown to be compile only`);
  if (!tests.length) return decide(fails, gaps, "the broker built no test and no cargo test ran outside it");
  let programs = 0; const claims = new Map();
  for (const j of tests) {
    const what = `broker job ${j.jobId ?? path.basename(j.file)}`;
    if (!j.verified) { (ctx.verifierPinned ? fails : gaps).push(`${what}: its receipt ${ctx.verifierPinned ? "does not verify" : "cannot be verified: no broker key is pinned"}`); continue; }
    if (!Array.isArray(j.testExecutables) && typeof j.testExecutablesReason !== "string") { fails.push(`${what}: the signed receipt does not show a compile only (no testExecutables), so the broker may have run the tests`); continue; }
    if (Array.isArray(j.testExecutables) && !j.testExecutables.length && typeof j.testExecutablesReason !== "string") gaps.push(`${what}: the receipt lists no test program and gives no reason, so that the gate had nothing to run is not shown`);
    for (const exe of Array.isArray(j.testExecutables) ? j.testExecutables : []) {
      programs += 1;
      const file = typeof exe === "string" ? exe : exe?.path; const digest = typeof exe === "object" ? exe?.sha256 : null;
      if (typeof file !== "string") { fails.push(`${what}: the receipt lists a test program with no path`); continue; }
      const ran = ctx.procs.filter((p) => dirKey(p.exe) === dirKey(file));
      if (!ran.length) { (ctx.processGaps.length ? gaps : fails).push(`${what}: the broker built ${path.basename(file)} and the gate never ran it`); continue; }
      if (ran.length > 1) fails.push(`${what}: the gate ran ${path.basename(file)} ${ran.length} times`);
      // One launch is credited to one receipt only: a program two receipts both list cannot have run for both.
      const claimed = claims.get(dirKey(file));
      if (claimed && claimed !== j.jobId) { fails.push(`${what}: ${path.basename(file)} is also listed by broker job ${claimed}, and its launch is credited to that receipt only`); continue; }
      claims.set(dirKey(file), j.jobId);
      // Execution after production: a launch before the broker finished the build ran something else.
      if (Number.isFinite(j.ended) && ran.some((p) => p.start < j.ended)) fails.push(`${what}: ${path.basename(file)} was started before the broker finished building it (job ended ${new Date(j.ended).toISOString()})`);
      // The image at execution: the file's digest now equals the signed digest, and the file was last written before the launch and not since, so what ran is what the broker built.
      const img = typeof ctx.digestOf === "function" ? ctx.digestOf(file) : null;
      const now = img && typeof img === "object" ? img.sha256 : img; const written = img && typeof img === "object" ? img.mtimeMs : null;
      if (typeof digest !== "string") gaps.push(`${what}: the receipt gives no digest for ${path.basename(file)}`);
      else if (now === null || now === undefined) gaps.push(`${what}: ${path.basename(file)} cannot be read now to compare with the digest the broker signed`);
      else if (now !== digest) fails.push(`${what}: ${path.basename(file)} is not the program the broker built (digest ${short(now)}, signed ${short(digest)})`);
      else if (!Number.isFinite(written)) gaps.push(`${what}: when ${path.basename(file)} was last written is not shown, so that the image that ran is the one the broker built is not shown`);
      else if (written > Math.min(...ran.map((p) => p.start))) fails.push(`${what}: ${path.basename(file)} was written after the gate started it, so the image that ran is not the image now`);
    }
  }
  return decide(fails, gaps, `${tests.length} test build(s) by the broker, compile only and signed; the gate ran the ${programs} program(s) they list, each the one the broker built`);
}

function nn11(ctx) {
  const { launch } = ctx;
  const startMono = /^\d+$/.test(String(launch?.startMono ?? "")) ? BigInt(launch.startMono) : null; const endMono = /^\d+$/.test(String(launch?.endMono ?? "")) ? BigInt(launch.endMono) : null;
  const limitMs = Number(launch?.limitMs);
  // The budget is selected by what was run, not by a label: the kind must be the stage the launched gate was asked for.
  const asked = Array.isArray(launch?.gate?.args) ? launch.gate.args[launch.gate.args.indexOf("--to") + 1] ?? null : null;
  if (launch?.kind !== undefined && asked !== null && launch.kind !== asked) return result(NOT_MEASURED, `the run kind ${JSON.stringify(launch.kind)} does not match the stage the gate was asked for (--to ${asked}), so the pinned limit is not this run's`);
  if (startMono === null) return result(NOT_MEASURED, "the launcher's record has no start on its monotonic clock");
  if (!Number.isFinite(limitMs) || limitMs <= 0) return result(NOT_MEASURED, `no time limit was pinned at launch for ${launch?.host ?? "this host"} and run kind ${launch?.kind ?? "(none)"}`);
  // Programs the run owns that outlive the gate count too (Council CV-1 F-29), on the wall clock of the process record.
  const lastProg = Math.max(-Infinity, ...ctx.procs.map((p) => p.end ?? -Infinity));
  const earlyBy = ctx.root && Number.isFinite(ctx.root.start) ? Math.max(0, timeOf(launch.startedAt) - ctx.root.start) : 0;
  // An owned program still open is counted up to the end of the process record: the run lasted at least that long.
  const openLower = Math.max(-Infinity, ...ctx.procs.filter((p) => p.end === null).map((p) => timeOf(ctx.witnessTo) - timeOf(launch.startedAt)));
  if (endMono === null) { if (Number.isFinite(openLower) && openLower > limitMs) return result(FAIL, `at least ${(openLower / 1000).toFixed(1)} s, limit ${(limitMs / 1000).toFixed(1)} s`); return result(NOT_MEASURED, `limit ${(limitMs / 1000).toFixed(1)} s, but the launcher never saw the run end`); }
  if (endMono < startMono) return result(NOT_MEASURED, "the launcher's monotonic clock runs backwards between its start and end");
  let took = Number(endMono - startMono) / 1e6 + earlyBy;
  const wallEnd = timeOf(launch.endedAt); if (Number.isFinite(lastProg) && Number.isFinite(wallEnd) && lastProg > wallEnd) took += lastProg - wallEnd;
  if (Number.isFinite(openLower)) took = Math.max(took, openLower);
  if (took > limitMs) return result(FAIL, `${(took / 1000).toFixed(1)}${Number.isFinite(openLower) ? " (at least)" : ""} s, limit ${(limitMs / 1000).toFixed(1)} s (pinned at launch${launch.limitsSha256 ? `, limits ${short(launch.limitsSha256)}` : ""})`);
  if (ctx.processGaps.length) return result(NOT_MEASURED, `${(took / 1000).toFixed(1)} s on the launcher's clock, inside the limit, but the run's own programs are not all accounted for: ${ctx.processGaps[0]}`);
  return result(PASS, `${(took / 1000).toFixed(1)} s on the launcher's clock, limit ${(limitMs / 1000).toFixed(1)} s pinned at launch`);
}

function nn12(ctx) {
  const { live } = ctx;
  if (!live) return result(NOT_MEASURED, "what was left behind is judged right after the run ends (--live)");
  // The snapshot lock is looked for in the snapshot root the launcher recorded, not inferred from broker work (a run with no broker job has one too).
  const locks = typeof live.locksIn === "function" && ctx.launch?.snapshotsRoot ? live.locksIn(ctx.launch.snapshotsRoot) : null;
  if (locks === null && !ctx.snap.path) return result(NOT_MEASURED, "neither the launcher's snapshot root nor a broker job names where the snapshot lock lives, so it cannot be looked for");
  if (locks && locks.length) return result(FAIL, `the lock ${locks[0]} is still there after the run`);
  if (ctx.snap.path && live.exists(`${ctx.snap.path}.lock`)) return result(FAIL, `the lock ${ctx.snap.path}.lock is still there after the run`);
  if (ctx.processGaps.length) return result(NOT_MEASURED, `the run's programs cannot all be shown gone: ${ctx.processGaps[0]}`);
  const rootProc = ctx.root ? [ctx.root] : [];
  const alive = [...rootProc, ...ctx.procs].filter((p) => live.alive(p.pid, p.start));
  if (alive.length) return result(FAIL, `${alive.length} program(s) the run started are still alive: ${alive.slice(0, 5).map((p) => `${programName(p.exe)} ${p.pid}`).join(", ")}`);
  if (!Number.isInteger(ctx.exit) || ctx.exit !== 0) {
    // A run that did not end normally passes only on the next ordinary run: its launcher took this run's lock over and named it, and it ended 0.
    const next = ctx.successor; const prev = next?.launch?.previous;
    const after = timeOf(next?.launch?.startedAt) >= timeOf(ctx.launch?.endedAt ?? ctx.launch?.startedAt);
    if (next && prev?.runId === ctx.launch?.runId && (prev.reclaimed === true || prev.fromLastRun === true) && after && next.launch?.exit === 0 && next.launch?.host === ctx.launch?.host) return result(PASS, `the run ended with ${ctx.exit ?? ctx.launch?.signal ?? "no exit"}; the next ordinary run took it over and ended 0, and nothing was left behind`);
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
  if (ctx.jobs.some((j) => !j.argv)) gaps.push("a broker job's signed receipt names no command, so what it built is not shown");
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
      // Output formatting changes how diagnostics are printed, not what is built: two compiles that differ only in it are compatible.
      if (a === "--message-format" || a === "--color") { i += 1; continue; }
      if (/^--(message-format|color)=/.test(a) || a === "-q" || a === "--quiet" || a === "-v" || a === "--verbose") continue;
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
  // The workspace of an invocation is read from the selected tree: the nearest ancestor folder (or its own) whose Cargo.toml declares [workspace];
  // a folder with a package and no workspace above it is its own. When a manifest on the way cannot be read the workspace is disputed.
  const wsOf = new Map();
  for (const r of new Set(rels.filter((x) => x !== null))) {
    let d = r; let found = null; let unread = null;
    for (;;) {
      try { if (workspaceMembers(ctx.readTree(`${d ? `${d}/` : ""}Cargo.toml`)) !== null) { found = d; break; } }
      catch (e) { if (e?.code !== "ENOENT") unread = e.message; }
      if (d === "") break; d = d.includes("/") ? d.slice(0, d.lastIndexOf("/")) : "";
    }
    if (found === null && unread !== null) gaps.push(`the workspace of a compile in ${r || "."} cannot be read from the selected tree (${unread}), so whether it shares a workspace with another is disputed`);
    wsOf.set(r, found ?? r);
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
  else for (const c of compiles) {
    const first = compiles.reduce((a, b) => (b.submitted < a.submitted ? b : a)); const late = c.submitted - first.submitted;
    if (c !== first && late > 250 && !compiles.some((o) => o !== c && o.ended <= c.submitted)) gaps.push(`broker job ${c.jobId} was submitted ${Math.round(late)} ms after the first while it ran; whether it waited for capacity or was held back is not shown`);
    const before = compiles.find((o) => o !== c && o.ended <= c.submitted); if (before) fails.push(`broker job ${c.jobId} (cargo ${c.sub} ${cratesOf(c).join(" ")}) was submitted only after job ${before.jobId} ended, so independent groups did not run together`); }
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
 * appeared during the run, each { file, sha256, doc, verified: { ok, problems } } (verified against the pinned broker key). `inventory`: Map row id ->
 * { stage, files } from a source that is not the gate (null until the planner takes plain data). `prior`: { failedRows } from the previous launched
 * run's own evidence. `git`: { tree, blobOf(path), priorBlobOf(path), readTree(path) } at the selected and the baseline commit. `live` / `successor`
 * (NN12),
 * `digestOf(file)` (NN10). Returns { lines: [{ id, text, verdict, why }], pass, failed, notMeasured }.
 */
export function judgeRun({ lines, launch = null, processes = null, witness = null, receipts = [], inventory = null, inventoryUncovered = [], prior = null, readiness = null, verdictTimes = null, git = null, live = null, successor = null, verifierPinned = false, digestOf = null } = {}) {
  const unlinked = Array.isArray(lines) ? linkProblems(lines) : ["no lines were given: read them from the contract with readAcceptanceLines"];
  if (unlinked.length) throw Object.assign(new Error(`the contract and the acceptance test do not match: ${unlinked.join("; ")}`), { problems: unlinked });
  const g = git ?? { tree: null, blobOf: () => null, readTree: () => { throw new Error("no git reader was given (--repo)"); } };
  const tree = runTree(launch, processes, witness);
  const start = timeOf(launch?.startedAt); const end = timeOf(launch?.endedAt);
  const jobsAll = (receipts ?? []).map(jobOf);
  const jobs = jobsAll.filter((j) => !Number.isFinite(j.submitted) || ((!Number.isFinite(start) || j.submitted >= start) && (!Number.isFinite(end) || j.submitted <= end)));
  const ctx = {
    launch, witnessTo: witness?.to ?? null, commit: launch?.commit ?? null, tree: g.tree,
    vjobs: jobs.filter((j) => j.verified), jobGaps: jobs.filter((j) => !j.verified).map((j) => `broker job ${j.jobId ?? path.basename(j.file)} is not credited: its receipt ${verifierPinned ? `does not verify (${j.problems.join("; ") || "no reason given"})` : "cannot be verified, no broker key is pinned"}`),
    inventory: inventory ? new Map(inventory) : null, inventoryUncovered, verdictTimes, readiness, prior,
    baselineGap: !launch?.baseline?.commit ? "the launcher has no earlier launched run to take a baseline commit from, so what changed since cannot be shown" : typeof g.priorBlobOf !== "function" ? "git cannot show the baseline commit's tree" : null, exit: Number.isFinite(end) && Number.isInteger(launch?.exit) ? launch.exit : null,
    root: tree.root, procs: tree.procs, processGaps: tree.gaps, starts: tree.procs.map((p) => ({ ...decodeStart({ cmd: p.exe, args: p.args, cwd: p.cwd ?? "" }), at: p.start, end: p.end, exit: p.exit })),
    parentKey: (p) => { const parent = [tree.root, ...tree.procs].find((q) => q && q.pid === p.ppid && q.start <= p.start); return parent ? `${programName(parent.exe)}#${parent.start}` : "?"; },
    jobs, verifierPinned, snap: { path: snapshotOf(jobs, g.readTree) }, readTree: g.readTree,
    expected: expectedRows(inventory ? new Map(inventory) : null, g, prior),
    live, successor, digestOf,
  };
  const judged = lines.map(({ id, text: sentence }) => { let r; try { r = JUDGES[id](ctx); } catch (e) { r = result(FAIL, `the check itself failed: ${e.message}`); } return { id, text: sentence, verdict: r.verdict, why: r.why }; });
  const failed = judged.filter((l) => l.verdict === FAIL).map((l) => l.id); const notMeasured = judged.filter((l) => l.verdict === NOT_MEASURED).map((l) => l.id);
  return { lines: judged, pass: failed.length === 0 && notMeasured.length === 0, failed, notMeasured };
}

/**
 * A run folder's evidence as judgeRun reads it, gathered from outside the gate: the launcher's record, the process record the launcher exported,
 * the broker receipts that appeared during the run (read once; the verifier is handed those bytes), git at the selected and the baseline commit, the
 * previous launched run's own evidence for what failed, and an independent row inventory when one is given. No gate file is read: no history, no
 * cache entry, no gate output.
 */
export function readRunDir(dir, { repo = null, receiptsDir = null, verify = null, inventoryFile = null } = {}) {
  const text = (p) => { try { return fs.readFileSync(p, "utf8"); } catch { return null; } };
  const json = (p) => { try { return JSON.parse(text(p) ?? "null"); } catch { return null; } };
  const launch = json(path.join(dir, "launch.json"));
  const procsOf = (d) => { const proc = json(path.join(d, "processes.json")); return proc?.kind === "windows-security" ? processesFromWindowsEvents(proc.events) : proc?.kind === "eslogger" ? processesFromEslogger(proc.events) : null; };
  const processes = procsOf(dir); const proc = json(path.join(dir, "processes.json"));
  const receiptsBefore = new Set((text(path.join(dir, "before", "receipts.txt")) ?? "").split(/\r?\n/).filter(Boolean));
  const receiptsOf = (rdir, skip) => {
    const out = []; if (!rdir) return out;
    for (const f of fs.readdirSync(rdir).filter((n) => n.endsWith(".json") && !skip.has(n))) {
      const file = path.join(rdir, f); const body = fs.readFileSync(file);
      let doc = null; try { doc = JSON.parse(body.toString("utf8")); } catch { doc = null; }
      // The verifier is handed these very bytes (a private copy), never the path again: nothing can change between what is verified and what is read.
      out.push({ file, sha256: createHash("sha256").update(body).digest("hex"), doc, verified: verify ? verify(body) : { ok: false, problems: ["no broker key is pinned"] } });
    }
    return out;
  };
  const receipts = receiptsOf(receiptsDir, receiptsBefore);
  const given = (() => { const j = inventoryFile ? json(inventoryFile) : null; return j ? new Map(Object.entries(j)) : null; })();
  let inventory = given; let inventoryUncovered = given ? [] : null;
  let git = null; let prior = null;
  if (repo && launch?.commit) {
    const run = (...a) => execFileSync("git", ["-C", repo, ...a], { encoding: "utf8", stdio: ["ignore", "pipe", "pipe"], windowsHide: true, maxBuffer: 256 * 1024 * 1024 });
    const blobsOf = (commit) => { try { const tree = run("rev-parse", `${commit}^{tree}`).trim(); const m = new Map(); for (const l of run("ls-tree", "-r", "-z", tree).split("\0")) { const x = /^\d+ blob ([0-9a-f]+)\t(.+)$/.exec(l); if (x) m.set(x[2], x[1]); } return { tree, m }; } catch { return null; } };
    const sel = blobsOf(launch.commit); const base = launch.baseline?.commit ? blobsOf(launch.baseline.commit) : null;
    // The test's own enumeration of the selected tree (acceptance-rows.mjs): the rows a tree states, and the kinds only gate code decides.
    if (sel && !given) { const e = enumerateRows({ files: sel.m, read: (p) => run("cat-file", "-p", `${sel.tree}:${p}`) }); inventory = e.rows; inventoryUncovered = e.uncovered; }
    if (sel) git = { tree: sel.tree, blobOf: (p) => sel.m.get(String(p).replaceAll("\\", "/")) ?? null, priorBlobOf: base ? (p) => base.m.get(String(p).replaceAll("\\", "/")) ?? null : undefined, readTree: (p) => { const t = run("cat-file", "-p", `${sel.tree}:${p}`); try { return JSON.parse(t); } catch { return t; } } };
  }
  // What failed in the previous launched run, from that run's own evidence: a program of a row's tool that exited non-zero, or a verified broker job
  // that did not succeed. Rows are the inventory's; a row with no such evidence is not marked failed.
  if (launch?.baseline?.out && inventory) {
    const pdir = launch.baseline.out; const prevLaunch = json(path.join(pdir, "launch.json")); const pp = procsOf(pdir);
    const failed = new Set();
    for (const p of pp ?? []) for (const [id] of inventory) { const tool = toolOf(id); if (tool && programName(p.exe) === tool && p.exit !== null && p.exit !== 0 && !isProbe(p.args)) failed.add(id); }
    prior = { failedRows: failed, runId: prevLaunch?.runId ?? null };
  }
  return { launch, processes, witness: proc ? { kind: proc.kind, from: proc.from, to: proc.to, newest: proc.newest, problems: proc.problems ?? [] } : null, receipts, inventory, inventoryUncovered, prior, git };
}

// ---- command line -----------------------------------------------------------------------------------------------------------------------------------
if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const opt = (name) => { const i = process.argv.indexOf(name); return i < 0 ? null : process.argv[i + 1]; };
  const usage = "usage: acceptance-lines.mjs --run <run dir> --repo <dir> --receipts <dir> [--inventory <file>] [--broker-key <hex> --broker-key-id <hex> [--rustright <client>]] [--successor <run dir>] [--live] [--contract <file>] [--json]";
  const runDir = opt("--run"); if (!runDir) { console.error(usage); process.exit(2); }
  const contractFile = opt("--contract") ?? CONTRACT_FILE; let lines;
  try { lines = readAcceptanceLines(fs.readFileSync(contractFile, "utf8")); } catch (e) { console.error(`NOT JUDGED: ${e.code === "ENOENT" ? `the contract ${contractFile} cannot be read` : e.message}`); process.exit(2); }
  const key = opt("--broker-key"); const keyId = opt("--broker-key-id"); const client = opt("--rustright") ?? "rustright";
  let host = null; try { host = JSON.parse(fs.readFileSync(path.join(runDir, "launch.json"), "utf8")).host; } catch { host = null; }
  // The broker's own verifier (rustright verify-receipt) with the key pinned by the caller.
  const verify = key && keyId && host ? (body) => { const dir = fs.mkdtempSync(path.join(os.tmpdir(), "rk-verify-")); const file = path.join(dir, "receipt.json"); fs.writeFileSync(file, body); try { execFileSync(client, ["verify-receipt", "--receipt", file, "--public-key", key, "--key-id", keyId, "--host", host], { stdio: ["ignore", "pipe", "pipe"], windowsHide: true }); return { ok: true, problems: [] }; } catch (e) { return { ok: false, problems: [String(e.stderr ?? e.message).trim().split(/\r?\n/).pop()] }; } } : null;
  const evidence = readRunDir(runDir, { repo: opt("--repo"), receiptsDir: opt("--receipts"), verify, inventoryFile: opt("--inventory") });
  const alive = (pid) => { try { process.kill(pid, 0); return true; } catch (e) { return e.code === "EPERM"; } };
  const locksIn = (d) => { try { return fs.readdirSync(d).filter((n) => n.endsWith(".lock")).map((n) => path.join(d, n)); } catch { return null; } };
  const live = process.argv.includes("--live") ? { exists: (p) => fs.existsSync(p), alive, locksIn } : null;
  const successor = opt("--successor") ? { launch: (() => { try { return JSON.parse(fs.readFileSync(path.join(opt("--successor"), "launch.json"), "utf8")); } catch { return null; } })() } : null;
  const digestOf = (file) => { try { const body = fs.readFileSync(file); return { sha256: createHash("sha256").update(body).digest("hex"), mtimeMs: fs.statSync(file).mtimeMs }; } catch { return null; } };
  const verdict = judgeRun({ lines, ...evidence, live, successor, verifierPinned: Boolean(verify), digestOf });
  if (process.argv.includes("--json")) console.log(JSON.stringify(verdict, null, 2));
  else { for (const l of verdict.lines) console.log(`${l.id} ${l.verdict.toUpperCase().padEnd(12)} ${l.text}\n      ${l.why}`); console.log(verdict.pass ? "PASS: all twelve lines" : `NOT PASSED: ${verdict.failed.length} failed, ${verdict.notMeasured.length} not measured`); }
  process.exit(verdict.pass ? 0 : 1);
}
