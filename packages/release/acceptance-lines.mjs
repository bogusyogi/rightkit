// The gate's acceptance test: twelve lines that must be true of every run (GATE-CONTRACT GC-A27; Adrian, 2026-11-20: "An acceptance test
// should be a single list of things that has to pass any run").
//
// It judges ONE run from the records that run leaves behind, and nothing else: its log, the list of every process it started (the spawn
// ledger), its forecast files (what it said it would reuse and run, and what happened), its row reports (coverage), its Class A verdict, its
// receipt and, where a split or a workspace must be checked, files of the run's own Git tree. It stages nothing and knows no scenario: a
// first build, a repeat, a run after an edit and a real build of an app are all judged by the same twelve lines. A line with no record to
// judge it from is "not measured", never a pass.
//
// Rules every check keeps (gatecodex reviews and Council CV-1, 2026-11-20):
//  * Missing proof is not the absence of work. "Nothing was sent", "nothing repeated" and "no other compile" are shown only from a process
//    list that is complete and bound to this run (parseLedger, ledgerGaps); otherwise the line is not measured.
//  * No record is trusted alone. The forecast, the lookup results, the coverage report, the dispatch list and the process list are joined
//    row by row, in both directions, and any disagreement among them is found here, not read from the gate's own summary of it.
//  * Every record must be this run's: the selected commit, the snapshot, its lock, the receipt and any record that names a run.
//  * What a record cannot tell apart (a wrapped command it cannot decode, a feature-set variant, a tie in time) is not measured, not guessed.
//
// The lines are not written here. They are read from the one table in docs/GATE-CONTRACT.md [GC-A27] between
// `<!-- acceptance-lines:begin -->` and `<!-- acceptance-lines:end -->`, and nothing is judged unless every line there has a check here and
// every check here has a line there.
//
//   node acceptance-lines.mjs --log <file> --ledger <file> --commit <sha> --pid <gate pid> [--repo <dir>] [--contract <file>] [--receipt <file>]
//                             [--limit-ms <n>] [--exit <code>] [--json]
import fs from "node:fs";
import path from "node:path";
import { execFileSync } from "node:child_process";
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
/** A log line's UTC stamp in ms, or null. */
const stampOf = (line) => { const m = /^(\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d(?:\.\d+)?Z) /.exec(line); return m ? Date.parse(m[1]) : null; };

/**
 * The process list (RK_GATE_SPAWN_LEDGER). Each line is one finished child `{pid, at, ms, sync, cmd, args, cwd, status}` (`at` is when it
 * started; the line is written when it closes), or a writer's end record `{end: true, pid, at, open, root?, lock?, parent?}` written when
 * that writer process finishes: `open` counts its children still running, the run's own gate process writes `root: true` with the run's
 * snapshot `lock`, and any other writer names its `parent` writer. Returns the entries as an array that also carries `problems` (every line
 * that is not a whole entry: nothing is dropped silently) and `ends` (end records by pid).
 */
export function parseLedger(text) {
  const entries = []; const problems = []; const ends = new Map();
  String(text ?? "").split(/\r?\n/).forEach((raw, i) => {
    if (!raw.trim()) return;
    let e; try { e = JSON.parse(raw); } catch { problems.push(`line ${i + 1} is not JSON`); return; }
    if (e && typeof e === "object" && e.end === true) {
      if (!Number.isInteger(e.pid) || !Number.isFinite(timeOf(e.at)) || !Number.isInteger(e.open)) problems.push(`line ${i + 1} is an end record without pid, at and open`);
      else if (ends.has(e.pid)) problems.push(`line ${i + 1} is a second end record for process ${e.pid}`);
      else ends.set(e.pid, e);
      return;
    }
    const missing = [];
    if (!Number.isInteger(e?.pid)) missing.push("pid");
    if (!Number.isFinite(timeOf(e?.at))) missing.push("at");
    if (typeof e?.ms !== "number" || !(e.ms >= 0)) missing.push("ms");
    if (typeof e?.cmd !== "string" || !e.cmd) missing.push("cmd");
    if (!Array.isArray(e?.args)) missing.push("args");
    if (typeof e?.cwd !== "string") missing.push("cwd");
    if (!e || typeof e !== "object" || !("status" in e)) missing.push("status");
    if (missing.length) { problems.push(`line ${i + 1} has no ${missing.join(", ")}`); return; }
    entries.push(e);
  });
  return Object.defineProperties(entries, { problems: { value: problems }, ends: { value: ends } });
}

/**
 * Why the process list cannot be taken as this run's complete list, or [] when it can (Council CV-1 F-3): every line is a whole entry; the
 * run's gate process closed it with one root end record bound to this run's snapshot lock; every process that wrote to it closed it too,
 * linked to the root through its parent, and none of them had a child still running. A list without that is a sample, and a sample cannot
 * show absence. An empty list closed this way is a valid record of a run that started nothing.
 */
export function ledgerGaps(ledger, { lock = null, runPid = null } = {}) {
  if (!Array.isArray(ledger)) return ["the run has no process list"];
  if (!Array.isArray(ledger.problems) || !(ledger.ends instanceof Map)) return ["the process list was not read with parseLedger, so whether it is complete is unknown"];
  const gaps = ledger.problems.map((p) => `the process list is incomplete: ${p}`);
  const roots = [...ledger.ends.values()].filter((e) => e.root === true);
  if (!roots.length) gaps.push("the process list has no end record of the run's own gate process (root), so a process started and never recorded cannot be ruled out");
  if (roots.length > 1) gaps.push(`the process list has ${roots.length} root end records, so which run it belongs to is contradictory`);
  const root = roots.length === 1 ? roots[0] : null;
  if (root && !lock) gaps.push("this run's snapshot lock is not known, so the process list cannot be bound to it");
  if (root && lock && root.lock !== lock) gaps.push(`the process list's root end record belongs to snapshot lock ${root.lock ?? "(none)"}, not this run's ${lock}`);
  // Frozen case GCX-02-2: the root end record is the gate's own word; the run's pid comes from its launcher, which the gate does not write.
  if (root && !Number.isInteger(runPid)) gaps.push("the run's own process id is not known from its launcher (--pid), so the root end record cannot be tied to the run");
  if (root && Number.isInteger(runPid) && root.pid !== runPid) gaps.push(`the process list's root end record is process ${root.pid}'s, not the run's own process ${runPid}`);
  for (const pid of new Set(ledger.map((e) => e.pid))) if (!ledger.ends.has(pid)) gaps.push(`process ${pid} wrote to the process list and no end record`);
  for (const [pid, end] of ledger.ends) {
    if (end.open !== 0) gaps.push(`process ${pid} ended with ${end.open} child(ren) still open, which the list does not show`);
    if (end.root === true) continue;
    // Each other writer must reach the root through its parents.
    let at = end; const seen = new Set();
    while (at && at.root !== true && !seen.has(at.pid)) { seen.add(at.pid); at = ledger.ends.get(at.parent); }
    if (!at || at.root !== true) gaps.push(`process ${pid}'s end record is not linked to the run's root through its parent`);
  }
  return gaps;
}

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
function toolStarts(ctx, id) {
  const tool = toolOf(id); if (!tool) return null;
  const ws = String(id).split(":").slice(1).find((s) => s && !/^\d+$/.test(s) && !/^20\d\d$/.test(s)) ?? null;
  return ctx.starts.filter((d) => programName(d.program) === tool && !isProbe(d.args)).map((d) => {
    if (tool === "rustfmt") return { ...d, mine: "maybe" };
    if (!ws) return { ...d, mine: true };
    const rel = relToSnap(d.cwd, ctx.snap.path); const manifest = valuesOf(d.args, "--manifest-path").map((m) => m.replaceAll("\\", "/"));
    const inWs = rel === dirKey(ws) || manifest.some((m) => m === `${ws}/Cargo.toml` || m.endsWith(`/${ws}/Cargo.toml`));
    const other = (rel !== null && rel !== "" && rel !== dirKey(ws)) || manifest.length > 0;
    return { ...d, mine: inWs ? true : other ? false : "maybe" };
  });
}

/** The forecast files a log names ("Class A cache decision: <file>", "Class B cache decision: <file>"), read; an unreadable one is reported. */
export function decisionsOf(log, { readJson = (f) => JSON.parse(fs.readFileSync(f, "utf8")) } = {}) {
  const out = [];
  for (const m of String(log).matchAll(/Class ([AB]) cache decision: (.+?)\s*$/gm)) {
    let doc = null; let problem = null;
    try { doc = readJson(m[2].trim()); } catch (e) { problem = `cannot read ${m[2].trim()} (${e.code ?? e.message})`; }
    out.push({ stage: m[1], file: m[2].trim(), doc, problem, at: stampOf(String(log).slice(0, m.index).split(/\r?\n/).pop() + m[0]) });
  }
  return out;
}
/** The coverage reports a log names, read. */
function coveragesOf(log, readJson) {
  const out = [];
  for (const m of String(log).matchAll(/coverage report (\S+\.json)/g)) {
    try { out.push({ file: m[1], doc: readJson(m[1]) }); } catch (e) { out.push({ file: m[1], doc: null, problem: `cannot read ${m[1]} (${e.code ?? e.message})` }); }
  }
  return out;
}
/** A record that names a run (its lock or run id) different from this run's is another run's record (Council CV-1 F-5). */
const foreignRun = (doc, lock) => {
  for (const k of ["snapshotLock", "lock", "runLock"]) if (doc && typeof doc[k] === "string" && lock && doc[k] !== lock) return `${k} ${doc[k]}`;
  return null;
};

/** Allowed values, by record (Council CV-1 F-13): anything else is invalid, and an invalid value is never agreement. */
const FORECAST = new Set(["hit", "run", "uncacheable"]); const LOOKUP = new Set(["hit", "run", "ran", "uncacheable"]); const COVERED = new Set(["hit", "ran"]);
/**
 * A forecast, lookup or coverage value as reuse ("hit"), run ("run") or "uncacheable" (a row the gate keeps no result for, so it runs every
 * time: forecast and looked up as such, covered as ran), or null for a value outside those.
 */
const normal = (v) => (v === "hit" ? "hit" : v === "run" || v === "ran" ? "run" : v === "uncacheable" ? "uncacheable" : null);
const runs = (v) => v === "run" || v === "uncacheable";
const sameOutcome = (forecast, other) => normal(forecast) !== null && normal(other) !== null && (normal(forecast) === normal(other) || (normal(forecast) === "uncacheable" && normal(other) === "run"));
/**
 * Whether a row's inputs changed, from its own keys and change list, never from its label (Council CV-1 F-16): "changed", "unchanged", or
 * null when the record carries neither.
 */
const changeOf = (f) => {
  if (typeof f.key === "string" && typeof f.priorKey === "string") return f.key === f.priorKey ? "unchanged" : "changed";
  if (Number.isInteger(f.changes?.inputsChanged)) return f.changes.inputsChanged === 0 ? "unchanged" : "changed";
  return null;
};

/**
 * One stage (A or B) with every record of it joined by row: the forecast (F), the lookup results (A), the coverage rows of this pass (C, null
 * when no coverage report covers the stage) and the dispatch list (null when the decision has none). `problem` says why the stage's decision
 * cannot be used: none named, unreadable, more than one, or a document that declares another stage.
 */
function stageOf(ctx, stage) {
  const ds = ctx.decisions.filter((d) => d.stage === stage);
  let problem = null; let contradictory = false;
  if (!ds.length) problem = `no Class ${stage} forecast is named in the log`;
  else if (ds.length > 1) { problem = `the log names ${ds.length} Class ${stage} forecasts`; contradictory = true; }
  else if (!ds[0].doc) problem = `the Class ${stage} forecast cannot be read (${ds[0].problem})`;
  else if (ds[0].doc.stage !== `class-${stage.toLowerCase()}`) { problem = `the Class ${stage} forecast ${path.basename(ds[0].file)} declares itself ${JSON.stringify(ds[0].doc.stage)}`; contradictory = true; }
  const d = problem ? null : ds[0];
  const doc = d?.doc ?? null;
  const reports = ctx.coverage.filter((c) => c.doc && Array.isArray(c.doc.scans) && c.doc.scans.some((s) => s.class === stage && s.coverage !== "skipped"));
  const covRows = reports.flatMap((c) => c.doc.scans).filter((s) => s.class === stage && s.coverage !== "skipped");
  const C = reports.length ? new Map(covRows.map((s) => [s.id, s])) : null;
  const F = new Map((Array.isArray(doc?.forecast) ? doc.forecast : []).map((f) => [f.id, f]));
  const A = new Map((Array.isArray(doc?.actual) ? doc.actual : []).map((a) => [a.id, a]));
  const dispatched = Array.isArray(doc?.dispatchedRows) ? new Set(doc.dispatchedRows) : null;
  // A row recorded twice in one record cannot be read by id: the later entry would silently hide the earlier one.
  const twice = (ids) => [...new Set(ids.filter((x, i) => ids.indexOf(x) !== i))];
  const dups = {
    forecast: twice((Array.isArray(doc?.forecast) ? doc.forecast : []).map((f) => f.id)),
    lookup: twice((Array.isArray(doc?.actual) ? doc.actual : []).map((a) => a.id)),
    coverage: twice(covRows.map((s) => s.id)),
    dispatch: twice(Array.isArray(doc?.dispatchedRows) ? doc.dispatchedRows : []),
  };
  const dupIds = new Set(Object.values(dups).flat());
  return { stage, decisions: ds, d, doc, F, A, C, dispatched, dups, dupIds, problem, contradictory };
}

/**
 * What the records say about the work a stage did in this run: `did` names the evidence of work (a row run, dispatched or covered as ran, a
 * broker compile for Class B, a verdict written for Class A), `unknown` why none cannot be shown. Both empty: proven none.
 */
function stageWork(ctx, stage) {
  const s = stageOf(ctx, stage); const did = []; const unknown = [];
  if (s.dispatched?.size) did.push(`${s.dispatched.size} row(s) dispatched`);
  const ran = [...(s.C?.values() ?? [])].filter((c) => c.coverage === "ran").length; if (ran) did.push(`${ran} row(s) covered as ran`);
  const looked = [...s.A.values()].filter((a) => runs(normal(a.actual))).length; if (looked) did.push(`${looked} row(s) looked up to run`);
  if (stage === "B" && ctx.compiles.length) did.push(`${ctx.compiles.length} broker compile(s)`);
  if (stage === "A" && ctx.verdict.file) did.push("a Class A verdict was written in this run");
  if (s.problem) unknown.push(s.problem); else if (!s.dispatched) unknown.push(`no Class ${stage} dispatch list`);
  if (!s.C) unknown.push(`no coverage report for Class ${stage}`);
  if (stage === "B") unknown.push(...ctx.ledgerGaps);
  return { did, unknown };
}
/** Whether a stage has any record of being part of this run (Council CV-1 F-11): a forecast named, coverage rows of this pass, or work. */
const stageEvidenced = (ctx, stage) => { const s = stageOf(ctx, stage); return s.decisions.length > 0 || s.C !== null || (stage === "B" && ctx.compiles.length > 0); };

// ---- the twelve lines ------------------------------------------------------------------------------------------------------------------------------
function snapshotOf(log) {
  const said = [...log.matchAll(/snapshot (\S+) is commit ([0-9a-f]{40}) tree ([0-9a-f]{40})/g)].map((m) => ({ path: m[1], commit: m[2], tree: m[3] }));
  const afterLines = [...log.matchAll(/is still tree ([0-9a-f]{40}) after the (\w+)/g)];
  const after = afterLines.map((m) => m[1]); const afterStages = new Set(afterLines.map((m) => m[2]));
  const lock = /snapshot lock held (\S+)/.exec(log)?.[1] ?? null;
  return { said, after, afterStages, lock, commit: said[0]?.commit ?? null, tree: said[0]?.tree ?? null, path: said[0]?.path ?? null };
}

function nn01(ctx) {
  const { log, receipt, commit, snap } = ctx; const fails = []; const gaps = [];
  if (!snap.said.length) return result(NOT_MEASURED, "the log never says which commit and tree its snapshot is");
  if (!commit) gaps.push("the judge was not told which commit was selected (--commit), so the run cannot be held to it");
  const commits = new Set(snap.said.map((s) => s.commit)); const trees = new Set([...snap.said.map((s) => s.tree), ...snap.after]);
  if (commit && (commits.size !== 1 || !commits.has(commit))) fails.push(`the selected commit is ${short(commit)}, the snapshot was built from ${[...commits].map(short).join(", ")}`);
  if (commits.size !== 1) fails.push(`the snapshot names ${commits.size} commits`);
  if (trees.size !== 1) fails.push(`the snapshot names ${trees.size} trees: ${[...trees].map(short).join(", ")}`);
  for (const m of log.matchAll(/--to \S+ for ([0-9a-f]{7,40})\b/g)) if (commit && !commit.startsWith(m[1])) fails.push(`the gate was asked for ${m[1]}, not the selected ${short(commit)}`);
  if (!snap.after.length) gaps.push("the snapshot's tree was stated at the start but never checked again after the work");
  // Frozen case gate 13: each stage this run did work in has the log's own check of the tree after it; another stage's check does not cover it.
  else for (const [name, cls] of [["check", "A"], ["compile", "B"]]) {
    const w = stageWork(ctx, cls);
    if (w.did.length && !snap.afterStages.has(name)) gaps.push(`this run did ${name} work (${w.did.join(", ")}) and the log never checks the snapshot's tree after the ${name}`);
  }
  const runCommit = commit ?? snap.commit;
  for (const d of ctx.decisions) if (d.doc && d.doc.commit !== runCommit) fails.push(`the Class ${d.stage} forecast is for commit ${short(d.doc.commit)}`);
  for (const d of ctx.decisions) if (d.doc && d.stage === "B" && d.doc.tree && d.doc.tree !== snap.tree) fails.push(`the Class B forecast is for tree ${short(d.doc.tree)}`);
  for (const c of ctx.coverage) if (c.doc && c.doc.commit && c.doc.commit !== runCommit) fails.push(`the coverage report ${path.basename(c.file)} is for commit ${short(c.doc.commit)}`);
  if (ctx.verdict.doc && (ctx.verdict.doc.commit !== runCommit || ctx.verdict.doc.tree !== snap.tree)) fails.push(`the Class A verdict is for commit ${short(ctx.verdict.doc.commit)} tree ${short(ctx.verdict.doc.tree)}`);
  // Council CV-1 F-1: every broker compile builds from this run's snapshot: its folder, and any manifest it names, are inside it.
  if (!snap.path) gaps.push("the log names no snapshot folder, so where the compiles built cannot be checked");
  else for (const c of ctx.compiles) {
    if (relToSnap(c.cwd, snap.path) === null) fails.push(`a broker compile (cargo ${c.sub}) ran in ${c.cwd}, outside the snapshot ${snap.path}`);
    for (const mp of valuesOf(c.args, "--manifest-path")) {
      const abs = /^([a-z]:[\\/]|[\\/])/i.test(mp) ? mp : `${c.cwd}${isWin(c.cwd) ? "\\" : "/"}${mp}`;
      const norm = dirKey(abs).split("/").reduce((acc, p) => (p === ".." ? acc.slice(0, -1) : p === "." ? acc : [...acc, p]), []).join("/");
      if (relToSnap(norm, snap.path) === null) fails.push(`a broker compile (cargo ${c.sub}) builds the manifest ${mp}, outside the snapshot`);
    }
  }
  if (!receipt) gaps.push("the run has no receipt to bind the build to the commit and snapshot");
  else {
    if (receipt.commit !== runCommit) fails.push(`the receipt is for commit ${short(receipt.commit)}, the run was for ${short(runCommit)}`);
    if (receipt.binding?.tree !== snap.tree) fails.push(`the receipt is bound to tree ${short(receipt.binding?.tree)}, the snapshot is ${short(snap.tree)}`);
    if (!snap.lock) gaps.push("the log names no snapshot lock, so the receipt cannot be tied to this run");
    // Every stage the receipt records is of this commit's tree, and holds this run's snapshot lock. The one exception is a stage carried from an
    // earlier run of the same commit and tree (recorded before this run began, under that run's lock), and only when every record shows this
    // run did none of that stage's work (gatecodex ruling 4): an earlier time and a matching tree do not bind fresh work to an old receipt.
    const runStart = stampOf(log.split(/\r?\n/).find((l) => stampOf(l) !== null) ?? "");
    const STAGE_CLASS = { check: "A", compile: "B" };
    const stages = Object.entries(receipt.stages ?? {}).filter(([, st]) => st);
    if (!stages.length) fails.push("the receipt records no stage");
    // Council CV-1 F-2: a stage this run did work in must be in the receipt, with its own proof.
    for (const [name, cls] of Object.entries(STAGE_CLASS)) {
      if (receipt.stages?.[name]) continue;
      const w = stageWork(ctx, cls);
      if (w.did.length) gaps.push(`this run did ${name} work (${w.did.join(", ")}) and the receipt records no ${name} stage to bind it to the tree and lock`);
    }
    for (const [name, st] of stages) {
      if (st.snapshot?.tree !== snap.tree) fails.push(`the receipt's ${name} stage read tree ${short(st.snapshot?.tree)}`);
      if (st.snapshot?.intactAfter !== true) fails.push(`the receipt's ${name} stage does not record the snapshot intact after it`);
      const when = timeOf(st.at);
      if (!Number.isFinite(when) || runStart === null) { gaps.push(`the receipt's ${name} stage has no time to place it in this run or an earlier one`); continue; }
      if (!snap.lock || st.snapshotLock === snap.lock) continue;
      if (when >= runStart) { fails.push(`the receipt's ${name} stage, recorded during this run, holds snapshot lock ${st.snapshotLock}; this run held ${snap.lock}`); continue; }
      const cls = STAGE_CLASS[name];
      if (!cls) { gaps.push(`the receipt's ${name} stage was carried from an earlier run, and no record shows whether this run did its work`); continue; }
      const w = stageWork(ctx, cls);
      if (w.did.length) fails.push(`the receipt's ${name} stage is an earlier run's (lock ${st.snapshotLock}), yet this run did work in it: ${w.did.join(", ")}`);
      else if (w.unknown.length) gaps.push(`the receipt's ${name} stage was carried from an earlier run; that this run did none of its work cannot be shown: ${w.unknown.slice(0, 2).join("; ")}`);
    }
  }
  return decide(fails, gaps, `selected commit ${short(runCommit)}, tree ${short(snap.tree)} before and after, every compile in the snapshot, receipt and records bound to lock ${snap.lock}`);
}

/** The Class A tool starts of this run (Council CV-1 F-6, F-10): starts of a program a Class A row runs, other than probes. */
const classAStarts = (ctx) => ctx.starts.filter((d) => Object.values(TOOLS).includes(programName(d.program)) && !isProbe(d.args));

function nn02(ctx) {
  const { log, compiles, snap } = ctx; const fails = []; const gaps = [];
  const runCommit = ctx.commit ?? snap.commit; const v = ctx.verdict;
  const foreign = foreignRun(v.doc, snap.lock);
  const verdictOk = v.doc && !foreign && v.doc.status === "pass" && v.doc.commit === runCommit && v.doc.tree === snap.tree && Number.isFinite(v.at);
  const covA = stageOf(ctx, "A").C;
  // Frozen case gate 9: whether Class A failed is read from this run's verdict file and Class A coverage only, never from log wording, which
  // can be about a nested unit; with no verdict file the line is not measured below.
  const aFailed = v.doc?.status === "fail" || [...(covA?.values() ?? [])].some((s) => s.status === "fail");
  const sentB = stageOf(ctx, "B").dispatched;
  if (foreign) fails.push(`the Class A verdict ${path.basename(v.file)} belongs to another run (${foreign})`);
  else if (v.doc && !verdictOk && !aFailed) fails.push(`the Class A verdict ${path.basename(v.file)} is not a pass of commit ${short(runCommit)} tree ${short(snap.tree)} (it says ${v.doc.status} for ${short(v.doc.commit)} ${short(v.doc.tree)})`);
  if (v.file && !v.doc) gaps.push(`the Class A verdict ${v.file} cannot be read`);
  const lgaps = ctx.ledgerGaps;
  const sends = ctx.brokerStarts.filter((b) => !b.ambiguous);
  const unclear = ctx.brokerStarts.filter((b) => b.ambiguous);
  if (unclear.length) gaps.push(`${unclear.length} start(s) name the broker client in a command line this judge cannot decode, so whether and when they sent work is unknown`);
  if (aFailed || (v.doc && !verdictOk)) {
    if (sends.length) fails.push(`Class A did not pass and ${sends.length} start(s) still went to the broker (${compiles.length} of them compiles)`);
    if (sentB?.size) fails.push(`Class A did not pass and ${sentB.size} Class B row(s) were still dispatched`);
    gaps.push(...lgaps.map((g) => `that nothing was sent cannot be shown: ${g}`));
    return decide(fails, gaps, "Class A failed and nothing was sent to the broker (complete process list)");
  }
  if (!v.doc) {
    if (sends.length) fails.push(`${sends.length} start(s) went to the broker with no Class A verdict for this run`);
    return decide(fails, ["the log names no Class A verdict for this run"], "");
  }
  // Council CV-1 F-6: a send must come strictly after Class A finished: after the verdict, and after every Class A tool process ended. A tie
  // with no record of which came first proves neither order.
  const aEnd = Math.max(v.at, ...classAStarts(ctx).map((d) => d.at + (d.ms ?? 0)));
  for (const b of sends) {
    if (!Number.isFinite(b.at)) gaps.push("a broker start has no time");
    else if (b.at < v.at) fails.push(`a start went to the broker before the Class A verdict (rustright ${b.args.slice(0, 2).join(" ")})`);
    else if (b.at < aEnd) fails.push(`a start went to the broker while a Class A tool was still running (rustright ${b.args.slice(0, 2).join(" ")})`);
    else if (b.at === v.at || b.at === aEnd) gaps.push(`a broker start and the end of Class A carry the same time, and no record says which came first (rustright ${b.args.slice(0, 2).join(" ")})`);
  }
  gaps.push(...lgaps.map((g) => `that nothing went to the broker before the verdict cannot be shown: ${g}`));
  return decide(fails, gaps, `Class A passed ${short(runCommit)} first, then ${compiles.length} compile(s) went to the broker (complete process list)`);
}

function nn03(ctx) {
  const s = stageOf(ctx, "A"); const covA = s.C;
  if (!covA) return result(NOT_MEASURED, "no coverage report covers Class A");
  if (s.problem) return result(s.contradictory ? FAIL : NOT_MEASURED, s.problem);
  // Council CV-1 F-7: every row owed or recorded as run, in any record, is accounted for first.
  const owed = new Set([...[...s.F.values()].filter((f) => runs(normal(f.expected))).map((f) => f.id), ...[...covA.values()].filter((c) => c.coverage === "ran").map((c) => c.id), ...(s.dispatched ?? []), ...[...s.A.values()].filter((a) => runs(normal(a.actual))).map((a) => a.id)]);
  if (!owed.size) {
    // Gatecodex ruling 1 and F-7: "no row waited" passes only for a proven, complete, wholly reused Class A: every row forecast, looked up and
    // covered as reused, the three records naming the same rows, and nothing dispatched.
    const why = [];
    if (!s.F.size) why.push("no Class A forecast rows");
    if (!s.dispatched) why.push("no Class A dispatch list");
    const ids = [...s.F.keys()];
    if (ids.some((id) => s.A.get(id)?.actual !== "hit")) why.push("a row has no lookup result of reuse");
    if (ids.some((id) => covA.get(id)?.coverage !== "hit")) why.push("a row is not covered as reused");
    if ([...covA.keys()].some((id) => !s.F.has(id)) || [...s.A.keys()].some((id) => !s.F.has(id))) why.push("the coverage report, lookup and forecast name different rows");
    if (s.dupIds.size) why.push(`row(s) recorded twice: ${[...s.dupIds].slice(0, 3).join(", ")}`);
    return why.length ? result(NOT_MEASURED, `no Class A row ran, and that none was owed cannot be shown: ${why.join("; ")}`) : result(PASS, `Class A was wholly reused (${s.F.size} rows forecast, looked up and covered as reused, none dispatched), so no row waited`);
  }
  const lane = (c) => c?.lane && Number.isInteger(c.lane.lanes) && c.lane.lanes > 0 && [c.lane.readyAt, c.lane.startedAt, c.lane.endedAt].every(Number.isFinite) && c.lane.readyAt <= c.lane.startedAt && c.lane.startedAt <= c.lane.endedAt;
  const bare = [...owed].filter((id) => !lane(covA.get(id)));
  if (bare.length) return result(NOT_MEASURED, `no valid lane record (ready <= started <= ended, lanes >= 1) in the coverage report for row(s) owed or run: ${bare.slice(0, 4).join(", ")}${bare.length > 4 ? ` and ${bare.length - 4} more` : ""}`);
  const ran = [...owed].map((id) => covA.get(id));
  // Council CV-1 F-8: the number of lanes is the run's own announcement ("class A scans N at a time"), not the row report's word; a wait
  // beside a free lane fails however short, except within a timing resolution a lane record states.
  const announced = Number(/class A scans (\d+) at a time/.exec(ctx.log)?.[1] ?? NaN);
  const fails = []; const gaps = [];
  for (const r of ran) {
    const { readyAt, startedAt } = r.lane; const tolerance = Number.isFinite(r.lane.resolutionMs) && r.lane.resolutionMs >= 0 ? r.lane.resolutionMs : 0;
    if (startedAt - readyAt <= tolerance) continue;
    if (!Number.isInteger(announced) || announced <= 0) { gaps.push(`${r.id} waited ${Math.round(startedAt - readyAt)} ms; how many lanes Class A had is not announced in the log, so whether one was free cannot be shown`); continue; }
    const others = ran.filter((o) => o !== r);
    const points = [...new Set([readyAt, ...others.flatMap((o) => [o.lane.startedAt, o.lane.endedAt]).filter((t) => t > readyAt && t < startedAt)])].sort((a, b) => a - b);
    let idle = 0; let idleFrom = null;
    points.forEach((t, i) => {
      const next = points[i + 1] ?? startedAt; const busy = others.filter((o) => o.lane.startedAt <= t && o.lane.endedAt > t).length;
      if (busy < announced) { idle += next - t; idleFrom ??= t; }
    });
    if (idle > tolerance) fails.push(`${r.id} waited ${Math.round(startedAt - readyAt)} ms, ${Math.round(idle)} ms of it with a lane free of ${announced} (from ${idleFrom})`);
  }
  return decide(fails, gaps, `${ran.length} Class A row(s) ran; none waited while a lane was free`);
}

function nn04(ctx) {
  const fails = []; const gaps = [];
  const runCommit = ctx.commit ?? ctx.snap.commit;
  const worked = { A: !!stageOf(ctx, "A").C || !!ctx.verdict.file, B: !!stageOf(ctx, "B").C || ctx.compiles.length > 0 };
  const freezes = []; const lookups = [];
  for (const stage of ["A", "B"]) {
    const s = stageOf(ctx, stage);
    if (!s.decisions.length) { if (worked[stage]) fails.push(`Class ${stage} worked (coverage or compiles) with no forecast named in the log`); continue; }
    if (s.problem) { (s.contradictory ? fails : gaps).push(s.problem); if (s.decisions.length === 1 && s.decisions[0].doc && s.contradictory) continue; if (!s.decisions[0]?.doc) continue; }
    const doc = s.doc ?? s.decisions[0].doc; const d = s.decisions[0];
    if (!Array.isArray(doc.forecast) || !doc.forecast.length) { fails.push(`the Class ${stage} forecast ${path.basename(d.file)} is missing or empty`); continue; }
    const foreign = foreignRun(doc, ctx.snap.lock); if (foreign) fails.push(`the Class ${stage} forecast belongs to another run (${foreign})`);
    if (doc.commit !== runCommit) fails.push(`the Class ${stage} forecast is for commit ${short(doc.commit)}, not ${short(runCommit)}`);
    const ids = doc.forecast.map((f) => f.id);
    for (const id of new Set(ids.filter((x, i) => ids.indexOf(x) !== i))) fails.push(`Class ${stage} row ${id} appears more than once in the forecast`);
    for (const f of doc.forecast) if (!FORECAST.has(f.expected)) fails.push(`Class ${stage} row ${f.id} is forecast as ${JSON.stringify(f.expected)}, not reuse, run or uncacheable`);
    // Council CV-1 F-9: every row looked up was in the frozen forecast.
    const fset = new Set(ids);
    for (const a of Array.isArray(doc.actual) ? doc.actual : []) if (!fset.has(a.id)) fails.push(`Class ${stage} row ${a.id} was looked up and is not in the forecast`);
    const frozen = timeOf(doc.forecastFrozenAt); const lookup = timeOf(doc.firstLookupAt);
    if (!Number.isFinite(frozen) || !Number.isFinite(lookup)) gaps.push(`the Class ${stage} forecast does not record when it was frozen and when the first lookup began (forecastFrozenAt, firstLookupAt)`);
    else { freezes.push({ stage, t: frozen, seq: doc.forecastFrozenSeq }); lookups.push({ stage, t: lookup, seq: doc.firstLookupSeq }); }
    if (s.C) {
      const missing = [...s.C.keys()].filter((id) => !s.F.has(id)); const extra = [...s.F.keys()].filter((id) => !s.C.has(id));
      if (missing.length) fails.push(`the Class ${stage} forecast leaves out row(s) the run covered: ${missing.slice(0, 4).join(", ")}`);
      if (extra.length) fails.push(`the Class ${stage} forecast names row(s) the run never covered: ${extra.slice(0, 4).join(", ")}`);
    } else gaps.push(`no coverage report covers Class ${stage}, so its forecast cannot be shown complete`);
  }
  // Council CV-1 F-10: the line is about the run's FIRST cache lookup. Every required forecast is frozen before it, no stage's work starts
  // before its forecast is frozen, and a tie with no sequence proves no order.
  if (freezes.length && lookups.length) {
    const last = freezes.reduce((a, b) => (b.t > a.t ? b : a)); const first = lookups.reduce((a, b) => (b.t < a.t ? b : a));
    if (last.t > first.t) fails.push(`the Class ${last.stage} forecast was frozen at ${new Date(last.t).toISOString()}, after the run's first lookup (Class ${first.stage}) at ${new Date(first.t).toISOString()}`);
    else if (last.t === first.t && !(Number.isFinite(last.seq) && Number.isFinite(first.seq) && last.seq < first.seq)) gaps.push(`the last forecast was frozen at the same time as the run's first lookup, and no sequence says which came first`);
  }
  const freezeOf = (stage) => freezes.find((f) => f.stage === stage)?.t;
  if (Number.isFinite(freezeOf("B"))) { const early = ctx.brokerStarts.filter((b) => !(b.at > freezeOf("B"))); if (early.length) fails.push(`${early.length} start(s) went to the broker before the Class B forecast was frozen`); }
  if (Number.isFinite(freezeOf("A"))) { const early = classAStarts(ctx).filter((d) => !(d.at > freezeOf("A"))); if (early.length) fails.push(`${early.length} Class A tool start(s) (first: ${programName(early[0].program)}) came before the Class A forecast was frozen`); }
  if (!ctx.decisions.length && !fails.length) return result(NOT_MEASURED, "the log names no forecast file");
  return decide(fails, gaps, ctx.decisions.map((d) => `Class ${d.stage}: ${d.doc?.forecast?.length ?? 0} rows forecast before the run's first lookup`).join("; "));
}

/** The RK_CACHE_DIVERGENCE notices of the log: each the mismatch row the gate wrote as JSON, with the time of its line (Council CV-1 F-14). */
function divergencesOf(log) {
  const out = [];
  for (const line of String(log).split(/\r?\n/)) {
    const m = /RK_CACHE_DIVERGENCE (\{.*\})\s*$/.exec(line); if (!m) continue;
    let row = null; try { row = JSON.parse(m[1]); } catch { row = null; }
    out.push({ row, at: stampOf(line) });
  }
  return out;
}

function nn05(ctx) {
  const fails = []; const gaps = []; const notes = [];
  const notices = divergencesOf(ctx.log);
  const allBroker = [...stageOf(ctx, "B").F.keys()];
  for (const stage of ["A", "B"]) {
    const s = stageOf(ctx, stage);
    // Council CV-1 F-11: a stage with any record in this run has one readable, correctly identified decision, or the line cannot pass.
    if (s.problem) { if (stageEvidenced(ctx, stage)) (s.contradictory ? fails : gaps).push(s.problem); continue; }
    if (!s.F.size) { gaps.push(`the Class ${stage} forecast has no rows`); continue; }
    for (const [record, ids] of Object.entries(s.dups)) for (const id of ids) fails.push(`Class ${stage}: ${id} appears more than once in the ${record} record, so which entry holds cannot be known`);
    const foreign = s.C && ctx.coverage.map((c) => foreignRun(c.doc, ctx.snap.lock)).find(Boolean); if (foreign) fails.push(`a coverage report belongs to another run (${foreign})`);
    // Council CV-1 F-13: values are checked against what each record may hold before anything is compared.
    for (const f of s.F.values()) if (f.expected === undefined || f.expected === null) gaps.push(`Class ${stage}: ${f.id} has no forecast value`); else if (!FORECAST.has(f.expected)) fails.push(`Class ${stage}: ${f.id} is forecast ${JSON.stringify(f.expected)}, which is not a forecast value`);
    for (const a of s.A.values()) if (a.actual === undefined || a.actual === null) gaps.push(`Class ${stage}: ${a.id} has no lookup value`); else if (!LOOKUP.has(a.actual)) fails.push(`Class ${stage}: ${a.id} was looked up ${JSON.stringify(a.actual)}, which is not a lookup value`);
    for (const c of s.C?.values() ?? []) if (!COVERED.has(c.coverage)) fails.push(`Class ${stage}: ${c.id} is covered ${JSON.stringify(c.coverage)}, which is not a coverage value`);
    const lookup = [];
    for (const [id, f] of s.F) { const a = s.A.get(id); if (!a) lookup.push({ id, why: `${id} has no lookup result` }); else if (FORECAST.has(f.expected) && LOOKUP.has(a.actual) && !sameOutcome(f.expected, a.actual)) lookup.push({ id, why: `${id} was forecast ${f.expected}, looked up ${a.actual}` }); }
    for (const id of s.A.keys()) if (!s.F.has(id)) lookup.push({ id, why: `${id} was looked up and not forecast` });
    if (!s.dispatched) gaps.push(`the Class ${stage} forecast records no dispatch list, so what was sent cannot be shown`);
    if (lookup.length) {
      const named = Array.isArray(s.doc.mismatches) ? s.doc.mismatches : [];
      for (const { id } of lookup) {
        const m = named.find((x) => x?.id === id);
        if (!m || typeof m.reason !== "string" || !m.reason.trim()) fails.push(`Class ${stage}: the lookup disagreed for ${id} and the run did not record its cause`);
        const n = notices.find((x) => x.row?.id === id);
        if (!n) fails.push(`Class ${stage}: the lookup disagreed for ${id} and the run did not name it (RK_CACHE_DIVERGENCE)`);
        else if (m?.reason && n.row.reason !== undefined && n.row.reason !== m.reason) fails.push(`Class ${stage}: the stop notice for ${id} gives another cause than the recorded mismatch`);
        else if (m?.reason && n.row.reason === undefined) gaps.push(`Class ${stage}: the stop notice for ${id} carries no cause to match the recorded one`);
      }
      for (const n of notices) if (n.row?.id && !lookup.some((l) => l.id === n.row.id) && !allBroker.includes(n.row.id) && !s.F.has(n.row.id)) fails.push(`a stop notice names ${n.row.id}, which is no row whose lookup disagreed`);
      for (const n of notices) if (!n.row || typeof n.row.id !== "string") fails.push("a stop notice (RK_CACHE_DIVERGENCE) carries no row id");
      if (ctx.exit === null || ctx.exit === undefined || !Number.isInteger(ctx.exit)) gaps.push(`Class ${stage}: the lookup disagreed (${lookup[0].why}) and the run's exit code is not known`);
      else if (ctx.exit === 0) fails.push(`Class ${stage}: the lookup disagreed (${lookup[0].why}) and the run still exited 0`);
      if (s.dispatched?.size) fails.push(`Class ${stage}: the lookup disagreed (${lookup[0].why}) and ${s.dispatched.size} row(s) were still sent`);
      const ranRows = [...(s.C?.values() ?? [])].filter((c) => c.coverage === "ran").map((c) => c.id);
      if (ranRows.length) fails.push(`Class ${stage}: the lookup disagreed and ${ranRows.slice(0, 3).join(", ")} still ran`);
      if (ctx.compiles.length) fails.push(`Class ${stage}: the lookup disagreed and ${ctx.compiles.length} compile(s) were still sent to the broker`);
      // Council CV-1 F-15: no work of any kind starts after the stop notice.
      const stopAt = Math.min(...notices.filter((n) => lookup.some((l) => l.id === n.row?.id)).map((n) => n.at).filter(Number.isFinite));
      if (Number.isFinite(stopAt)) { const after = ctx.starts.filter((d) => d.at >= stopAt && (programName(d.program) === "rustright" || Object.values(TOOLS).includes(programName(d.program)) || (ctx.snap.path && relToSnap(d.program, ctx.snap.path) !== null))); if (after.length) fails.push(`Class ${stage}: work started after the stop notice (${programName(after[0].program)})`); }
      gaps.push(...ctx.ledgerGaps.map((g) => `that nothing was sent after the disagreement cannot be shown: ${g}`));
      notes.push(`Class ${stage}: the lookup disagreed for ${lookup.slice(0, 3).map((l) => l.id).join(", ")}; the run stopped before sending work, named it and failed`);
      continue;
    }
    if (Array.isArray(s.doc.mismatches) && s.doc.mismatches.length) fails.push(`Class ${stage}: the run recorded mismatches (${s.doc.mismatches.map((m) => m?.id).join(", ")}) where the forecast and lookup agree`);
    const run = new Set([...s.F].filter(([, f]) => runs(normal(f.expected))).map(([id]) => id));
    if (!s.C) gaps.push(`no coverage report covers Class ${stage}, so what ran cannot be compared with the forecast`);
    else {
      for (const [id] of s.C) if (!s.F.has(id)) fails.push(`Class ${stage}: ${id} was covered and not forecast`);
      for (const [id, f] of s.F) { const c = s.C.get(id); if (!c) fails.push(`Class ${stage}: ${id} was forecast and not covered`); else if (FORECAST.has(f.expected) && COVERED.has(c.coverage) && !sameOutcome(f.expected, c.coverage)) fails.push(`Class ${stage}: ${id} was forecast ${f.expected} and ${c.coverage}`); }
    }
    if (s.dispatched) {
      for (const id of s.dispatched) if (!run.has(id)) fails.push(`Class ${stage}: ${id} was sent and the forecast said reuse`);
      for (const id of run) if (!s.dispatched.has(id)) fails.push(`Class ${stage}: ${id} was forecast to run and not sent`);
    }
    // Council CV-1 F-12: owed work and observed work are compared in both directions, in the process list as well as the gate's summaries.
    if (stage === "B") {
      const owedB = [...run].filter(isBrokerRow);
      for (const id of owedB) if (!ctx.compiles.some((c) => covers(c, id, ctx.snap.path, allBroker))) (ctx.ledgerGaps.length ? gaps : fails).push(`Class B: ${id} was owed and no broker compile in the process list does its work`);
      for (const c of ctx.compiles) {
        const ts = targetsOf(c).length ? targetsOf(c) : [null];
        for (const crate of cratesOf(c)) for (const t of ts) {
          const unit = (id) => { const r = rowOf(id); return r.sub === c.sub && r.crate === crate && inWorkspace(c, r, ctx.snap.path) && (r.target === t || (r.target === null && (t === null || !allBroker.some((o) => { const q = rowOf(o); return q.sub === c.sub && q.crate === crate && q.ws === r.ws && q.target === t; })))); };
          if (!owedB.some(unit)) fails.push(`a broker compile (cargo ${c.sub}) builds ${crate}${t ? ` for ${t}` : ""}${relToSnap(c.cwd, ctx.snap.path) === null ? " outside the snapshot" : ""}, which no row the forecast said to run asks for`);
        }
      }
      gaps.push(...ctx.ledgerGaps.map((g) => `that no other compile was sent cannot be shown: ${g}`));
    } else {
      for (const id of run) { const st = toolStarts(ctx, id); if (st && !st.some((d) => d.mine)) (ctx.ledgerGaps.length ? gaps : fails).push(`Class A: ${id} was owed and no start of ${toolOf(id)} in the process list does its work`); }
    }
    notes.push(`Class ${stage}: forecast, lookup, coverage, dispatch and process list agree on ${s.F.size} rows (${run.size} run)`);
  }
  if (!notes.length && !fails.length && !gaps.length) return result(NOT_MEASURED, "no forecast file to compare with what ran");
  return decide(fails, gaps, notes.join("; "));
}

function nn06(ctx) {
  const fails = []; const gaps = []; let reused = 0; let failures = 0;
  for (const stage of ["A", "B"]) {
    const s = stageOf(ctx, stage);
    if (s.problem) { if (stageEvidenced(ctx, stage)) (s.contradictory ? fails : gaps).push(s.problem); continue; }
    const allIds = [...s.F.keys()];
    // A row is unchanged by its own keys and change list (Council CV-1 F-16); only a row that carries neither is taken at its label.
    const unchanged = [...s.F.values()].filter((f) => f.expected !== "uncacheable" && (changeOf(f) === "unchanged" || (changeOf(f) === null && f.expected === "hit")));
    // Absence is shown only from complete records (gatecodex review 2): with no coverage report, or a process list that may be missing
    // starts, a reused row that ran cannot be ruled out.
    if (unchanged.length && !s.C) gaps.push(`no coverage report covers Class ${stage}, so that its ${unchanged.length} unchanged row(s) did not run cannot be shown`);
    if (unchanged.length && ctx.ledgerGaps.length) gaps.push(`that no process did an unchanged Class ${stage} row's work cannot be shown: ${ctx.ledgerGaps[0]}`);
    for (const f of s.F.values()) {
      if (f.expected !== "uncacheable") continue;
      // Gatecodex ruling 2 and F-33: proven changed passes; proven unchanged and run fails; unknown is not measured.
      const ch = changeOf(f); const ran = s.dispatched?.has(f.id) || runs(normal(s.A.get(f.id)?.actual)) || s.C?.get(f.id)?.coverage === "ran";
      if (ch === "unchanged" && ran) fails.push(`${f.id} is uncacheable, its inputs did not change, and it ran again`);
      else if (ch === null) gaps.push(`${f.id} is uncacheable: it runs whether or not its inputs changed, and its record does not say whether they did`);
    }
    for (const f of unchanged) {
      const id = f.id; reused += 1;
      if (s.dupIds.has(id)) { gaps.push(`${id} is recorded more than once, so its reuse cannot be read`); continue; }
      const a = s.A.get(id); const c = s.C?.get(id) ?? null;
      if (!a) gaps.push(`${id} was forecast unchanged and has no lookup result`);
      else {
        if (normal(a.actual) !== "hit") fails.push(`${id} is unchanged and ran again (${a.actual})`);
        if (typeof f.key === "string" && typeof a.key === "string" && f.key !== a.key) fails.push(`${id} was reused under key ${short(a.key)}, not its forecast key ${short(f.key)}`);
        // Frozen cases GCX-06-2 and gate 15: with no key on either side, "reused under its forecast key" cannot be shown.
        else if (typeof f.key !== "string" || typeof a.key !== "string") gaps.push(`${id} was reused, and its ${typeof f.key !== "string" ? "forecast" : "lookup"} records no key, so that it was reused under its forecast key cannot be shown`);
      }
      if (s.C && !c) gaps.push(`${id} was forecast unchanged and is not in the coverage report`);
      if (c && normal(c.coverage) !== "hit") fails.push(`${id} is unchanged and the coverage report says it ${c.coverage}`);
      // Council CV-1 F-17: an unchanged row is checked against dispatch and against the starts that would do its work.
      if (s.dispatched?.has(id)) fails.push(`${id} is unchanged and was dispatched`);
      if (f.status === "fail") {
        failures += 1;
        const recorded = [a?.status, c?.status].filter((x) => x !== undefined && x !== null);
        if (!recorded.length) gaps.push(`${id} was an earlier failure and no record says what the reuse returned`);
        if (recorded.some((x) => x !== "fail")) fails.push(`${id} was an earlier failure and came back ${recorded.find((x) => x !== "fail")}: a reused failure stays a failure`);
      }
      if (stage === "B") {
        const c2 = ctx.compiles.find((x) => covers(x, id, ctx.snap.path, allIds));
        const sibling = c2 ? allIds.find((o) => o !== id && s.F.get(o)?.expected !== "hit" && otherVariant(id, o) && covers(c2, o, ctx.snap.path, allIds)) : null;
        if (sibling) gaps.push(`${id} is unchanged and a compile for ${sibling} names the same crate; which feature set it built is not recorded`);
        else if (c2) fails.push(`${id} is unchanged and a broker compile did its work (cargo ${c2.sub} ${cratesOf(c2).map((n) => `-p ${n}`).join(" ")})`);
      } else if (/^rustfmt:/.test(id)) {
        // A rustfmt start does this row's work if it checks one of this row's files; with no changed row of the edition, any start for it does.
        const ed = id.split(":")[1]; const starts = rustfmtStarts(ctx, ed);
        const sharing = [...s.F.values()].some((o) => o.id !== id && /^rustfmt:/.test(o.id) && o.id.split(":")[1] === ed && runs(normal(o.expected)));
        if (starts.length && !sharing) fails.push(`${id} is unchanged and rustfmt ran for edition ${ed}`);
        else if (starts.length) {
          const own = changedFilesOf(s.doc, { ...f, priorKey: null });
          if (own.gap) gaps.push(own.gap);
          else { const touched = starts.flatMap((d) => rustfmtFiles(ctx, d)).filter((x) => own.files.has(x)); if (touched.length) fails.push(`${id} is unchanged and rustfmt checked its file(s): ${touched.slice(0, 3).join(", ")}`); }
        }
      } else {
        const st = toolStarts(ctx, id);
        if (st?.some((d) => d.mine === true)) fails.push(`${id} is unchanged and ${toolOf(id)} ran for it`);
        else if (st?.some((d) => d.mine === "maybe")) gaps.push(`${id} is unchanged and a start of ${toolOf(id)} cannot be told apart from its work`);
      }
    }
  }
  if (!ctx.decisions.some((d) => d.doc)) return result(NOT_MEASURED, "no forecast file, so unchanged rows cannot be told from changed ones");
  return decide(fails, gaps, `${reused} unchanged row(s) reused${failures ? `, ${failures} of them an earlier failure that stayed one` : ""}`);
}

/** The rustfmt starts of this run for one edition, decoded. */
const rustfmtStarts = (ctx, ed) => ctx.starts.filter((d) => programName(d.program) === "rustfmt" && !isProbe(d.args) && valuesOf(d.args, "--edition").includes(ed));
/** The file operands of a rustfmt start, as paths from the snapshot root (resolved against the folder it ran in). */
const rustfmtFiles = (ctx, d) => {
  const out = []; const rel = relToSnap(d.cwd, ctx.snap.path);
  for (let i = 0; i < d.args.length; i += 1) {
    const a = String(d.args[i]);
    if (["--edition", "--config", "--config-path", "--emit", "--color", "--files-with-diff"].includes(a)) { if (a !== "--files-with-diff") i += 1; continue; }
    if (a.startsWith("-")) continue;
    const f = a.replaceAll("\\", "/");
    out.push(rel ? `${rel}/${f}` : f);
  }
  return out;
};
/** The files whose change made a Class A row run, from its fingerprint and the forecast's change list, or { gap } when they cannot be known. */
function changedFilesOf(doc, f) {
  const fp = (doc.fingerprints ?? []).find((x) => x.id === f.id);
  // The inputs are the key's JSON with its schema written after it (`{...}:rightkit.class-a-key/3`).
  let files; try { files = JSON.parse(String(fp?.inputs ?? "null").replace(/:rightkit\.[\w.-]+\/\d+$/, ""))?.files; } catch { files = null; }
  if (!Array.isArray(files)) return { gap: `${f.id} has no recorded input files` };
  if (!f.priorKey) return { files: new Set(files.map((x) => String(x[0]))), all: new Set(files.map((x) => String(x[0]))) };
  const listed = f.changes?.inputs; const count = f.changes?.inputsChanged;
  if (!Array.isArray(listed) || !Number.isInteger(count)) return { gap: `${f.id} has no list of its changed inputs` };
  if (listed.length < count) return { gap: `${f.id}'s forecast lists ${listed.length} of its ${count} changed inputs, so the files the change touched cannot be named` };
  // Two spellings: by index into the input files (`files.10.1`) or by path (`files/rk/crates/rk-core/src/lib.rs`).
  const unread = listed.filter((p) => !/^files[./]/.test(p));
  if (unread.length) return { gap: `${f.id}'s changed inputs include ${unread.slice(0, 2).join(", ")}, which name no file` };
  const byIndex = listed.map((p) => /^files\.(\d+)\./.exec(p)?.[1]).filter(Boolean).map((i) => String(files[Number(i)]?.[0]));
  const byPath = listed.map((p) => /^files\/(.+)$/.exec(p)?.[1]).filter(Boolean);
  return { files: new Set([...byIndex, ...byPath]), all: new Set(files.map((x) => String(x[0]))) };
}

function nn07(ctx) {
  const fails = []; const gaps = []; let changed = 0;
  const allBroker = [...stageOf(ctx, "B").F.keys()];
  for (const stage of ["A", "B"]) {
    const s = stageOf(ctx, stage);
    if (s.problem) { if (stageEvidenced(ctx, stage)) (s.contradictory ? fails : gaps).push(s.problem); continue; }
    // A row is changed by its own keys and change list (Council CV-1 F-16); only a row that carries neither is taken at its label.
    const changedRows = [...s.F.values()].filter((f) => f.expected !== "uncacheable" && (changeOf(f) === "changed" || (changeOf(f) === null && f.expected === "run")));
    // Council CV-1 F-19: a changed row needs its coverage report and dispatch list; missing records are not measured, not passed.
    if (changedRows.length && !s.C) gaps.push(`no coverage report covers Class ${stage}, so that its ${changedRows.length} changed row(s) ran cannot be shown`);
    if (changedRows.length && !s.dispatched) gaps.push(`the Class ${stage} forecast records no dispatch list, so that its changed rows were sent cannot be shown`);
    for (const f of changedRows) {
      const id = f.id; changed += 1;
      if (s.dupIds.has(id)) { gaps.push(`${id} is recorded more than once, so its run cannot be read`); continue; }
      if (f.expected === "hit") fails.push(`${id} changed (key ${short(f.key)} against ${short(f.priorKey)}) and was forecast for reuse`);
      const a = s.A.get(id); const c = s.C?.get(id) ?? null;
      if (!a) gaps.push(`${id} was forecast changed and has no lookup result`);
      else if (normal(a.actual) !== "run") fails.push(`${id} changed and did not run (${a.actual})`);
      if (s.C && !c) gaps.push(`${id} was forecast changed and is not in the coverage report`);
      if (c && normal(c.coverage) !== "run") fails.push(`${id} changed and the coverage report says it ${c.coverage}`);
      if (s.dispatched && !s.dispatched.has(id)) fails.push(`${id} changed and was not sent`);
      if (stage === "B" && isBrokerRow(id)) {
        const doers = ctx.compiles.filter((x) => covers(x, id, ctx.snap.path, allBroker));
        if (!doers.length) (ctx.ledgerGaps.length ? gaps : fails).push(`${id} changed and no broker compile in the process list does its work`);
        else if (rowOf(id).variant !== null && allBroker.some((o) => otherVariant(id, o))) gaps.push(`${id} is one feature set of a crate built in several; which feature set the compile built is not recorded`);
      }
      if (stage === "A" && /^rustfmt:/.test(id)) {
        // Council CV-1 F-21: the starts of this row together are handed exactly the files the change touched: none missing, none extra.
        const ed = id.split(":")[1]; const scope = changedFilesOf(s.doc, f);
        const starts = rustfmtStarts(ctx, ed);
        if (!starts.length) (ctx.ledgerGaps.length ? gaps : fails).push(`${id} changed and no rustfmt start for edition ${ed} is in the process list`);
        for (const d of starts) if (!rustfmtFiles(ctx, d).length) fails.push(`${id}: a rustfmt start for edition ${ed} was handed no file`);
        if (scope.gap) gaps.push(scope.gap);
        else {
          const mine = starts.filter((d) => rustfmtFiles(ctx, d).some((x) => scope.all.has(x)));
          const handed = new Set(mine.flatMap((d) => rustfmtFiles(ctx, d)));
          const sibling = (x) => [...s.F.values()].some((o) => o.id !== id && /^rustfmt:/.test(o.id) && changedFilesOf(s.doc, { ...o, priorKey: null }).all?.has(x));
          const extra = [...handed].filter((x) => !scope.files.has(x) && !sibling(x));
          const missing = [...scope.files].filter((x) => /\.rs$/.test(x) && !handed.has(x));
          if (extra.length) fails.push(`${id} ran rustfmt on file(s) the change did not touch: ${extra.slice(0, 4).join(", ")}`);
          if (missing.length) fails.push(`${id} changed file(s) that rustfmt was never handed: ${missing.slice(0, 4).join(", ")}`);
        }
      } else if (stage === "A") {
        const st = toolStarts(ctx, id);
        if (st && !st.some((d) => d.mine)) (ctx.ledgerGaps.length ? gaps : fails).push(`${id} changed and no start of ${toolOf(id)} in the process list does its work`);
      }
    }
  }
  // A compile does only the change's work when every crate and target it names belongs to a changed row of its stage and workspace.
  const runB = [...stageOf(ctx, "B").F].filter(([id, f]) => runs(normal(f.expected)) && isBrokerRow(id)).map(([id]) => id);
  for (const c of ctx.compiles) {
    const needed = new Set(runB.filter((id) => rowOf(id).sub === c.sub && inWorkspace(c, rowOf(id), ctx.snap.path)).map((id) => rowOf(id).crate));
    const extra = cratesOf(c).filter((n) => !needed.has(n));
    if (extra.length) fails.push(`a broker compile (cargo ${c.sub}) also names ${extra.join(", ")}, which no changed row of its workspace needs`);
  }
  if (!ctx.decisions.some((d) => d.doc)) return result(NOT_MEASURED, "no forecast file, so changed rows cannot be told from unchanged ones");
  return decide(fails, gaps, `${changed} changed row(s), every one ran, and only on what changed`);
}

/**
 * NN08's per-run parts, the only ones blanked before two starts are compared (carried from the old test's NN07, gate-acceptance.mjs
 * normalizeRunPart): the temp folders of probes and shims, the macOS temp root, and pid values. Nothing else is rewritten: arguments keep their
 * case, their ids and their `-c` options, because two starts that differ there are two different calls.
 */
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

function nn08(ctx) {
  const { ledger } = ctx;
  if (!Array.isArray(ledger)) return result(NOT_MEASURED, "the run has no list of the processes it started");
  const calls = ledger.map((e, i) => callOf(ctx.starts[i], e));
  const buckets = new Map();
  const written = calls.map((c, i) => (c.ambiguous && ctx.starts[i].intended ? callOf({ ...ctx.starts[i], ...ctx.starts[i].intended }, ledger[i]) : null));
  for (const c of calls) { const k = JSON.stringify([c.args, c.cwd]); (buckets.get(k) ?? buckets.set(k, []).get(k)).push(c); }
  const repeats = [];
  for (const group of buckets.values()) {
    const groups = [];
    for (const c of group) { const g = groups.find((x) => sameProgram(x[0], c)); if (g) g.push(c); else groups.push([c]); }
    for (const g of groups) if (g.length > 1) repeats.push(g);
  }
  written.forEach((w, i) => {
    if (!w) return;
    const same = calls.filter((c, j) => j !== i && !c.ambiguous && sameProgram(c, w) && c.cwd === w.cwd && JSON.stringify(c.args) === JSON.stringify(w.args));
    if (same.length) repeats.push([same[0], { ...w, name: `${w.name} (written again through cmd /c)` }]);
  });
  const fails = repeats.sort((a, b) => b.length - a.length).map((g) => `${g.length}x ${g[0].name} ${g[0].args.join(" ").slice(0, 70)}`);
  const gaps = [...ctx.ledgerGaps];
  const cut = calls.filter((c) => c.truncated).length;
  if (cut) gaps.push(`${cut} start(s) had their arguments cut short in the process list, so they cannot be compared`);
  const unclear = calls.filter((c) => c.ambiguous).length;
  if (unclear) gaps.push(`${unclear} start(s) are command lines this judge cannot decode to one program and its arguments, so they cannot be compared`);
  return decide(fails.length ? [`${repeats.reduce((n, g) => n + g.length - 1, 0)} repeat start(s) of ${ledger.length}: ${fails.join("; ")}`] : [], gaps, `${ledger.length} process starts, no two alike (complete process list)`);
}

/** The values a minimal TOML `[workspace] members = [...]` holds, or null when the manifest has no workspace table. */
const workspaceMembers = (toml) => {
  const t = String(toml ?? ""); const ws = /^\[workspace\]\s*$([\s\S]*?)(?=^\[|(?![\s\S]))/m.exec(t); if (!ws) return null;
  const m = /members\s*=\s*\[([\s\S]*?)\]/.exec(ws[1]); if (!m) return [];
  return [...m[1].matchAll(/"([^"]+)"/g)].map((x) => x[1].replace(/\/+$/, ""));
};

function nn09(ctx) {
  const { compiles } = ctx; const fails = []; const gaps = [...ctx.ledgerGaps];
  if (ctx.brokerStarts.some((b) => b.ambiguous)) gaps.push("a start names the broker client in a command line this judge cannot decode");
  if (!compiles.length) return decide([], gaps, "nothing was sent to the broker (complete process list)");
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
  const starts = compiles.map((c) => c.at);
  if (starts.some((t) => !Number.isFinite(t))) gaps.push("a compile in the process list has no start time");
  else if (compiles.length > 1 && new Set(starts).size > 1) gaps.push(`${compiles.length} compile commands started at ${new Set(starts).size} different times; that none waited beside free capacity needs a record of the broker's capacity and each command's ready time`);
  return decide(fails, gaps, `${compiles.length} compile command(s) in ${groups.size} group(s), one per workspace and stage${justified.length ? ` (split: ${justified.join("; ")})` : ""}, all started together`);
}

function nn10({ compiles }) {
  const tests = compiles.filter((c) => c.sub === "test");
  if (!tests.length) return result(NOT_MEASURED, "this run sent no test compile to the broker");
  // The gate sends `cargo test`; the broker turns it into a compile only and signs which test programs it built (its receipt's
  // `cargo-test-compile-only` and `testExecutables`). Only that receipt shows the broker ran no test and the gate ran exactly those programs.
  return result(NOT_MEASURED, `${tests.length} test command(s) went to the broker; that it only compiled them and the gate ran the programs needs the broker's receipts, which this judge does not read yet`);
}

function nn11(ctx) {
  const { log, limitMs } = ctx;
  const stamps = log.split(/\r?\n/).map(stampOf).filter((t) => t !== null);
  if (stamps.length < 2) return result(NOT_MEASURED, "the log is not time-stamped");
  const back = stamps.findIndex((t, i) => i > 0 && t < stamps[i - 1]);
  if (back > 0) return result(NOT_MEASURED, `the log's times go backwards (line ${back + 1} of its stamped lines), so it gives no interval to judge`);
  // Council CV-1 F-29: the run's span is every event it owns: its log, the processes it started and the end records of its writers. A pass
  // needs a known finish (an exit code); an overrun already proven by owned events fails even when the finish is unknown.
  const owned = [...stamps, ...ctx.ledger.flatMap((e) => [timeOf(e.at), timeOf(e.at) + (Number.isFinite(e.ms) ? e.ms : 0)]), ...[...(ctx.ledger.ends?.values() ?? [])].map((e) => timeOf(e.at))].filter(Number.isFinite);
  const start = Math.min(...owned); const last = Math.max(...owned); const took = last - start;
  const finished = Number.isInteger(ctx.exit);
  if (!Number.isFinite(limitMs) || limitMs <= 0) return result(NOT_MEASURED, `the run took ${(took / 1000).toFixed(1)} s${finished ? "" : " so far"}; no time limit is set for it`);
  if (took > limitMs) return result(FAIL, `${(took / 1000).toFixed(1)} s${finished ? "" : " at least"}, limit ${(limitMs / 1000).toFixed(1)} s`);
  if (!finished) return result(NOT_MEASURED, `${(took / 1000).toFixed(1)} s recorded, limit ${(limitMs / 1000).toFixed(1)} s, but the run's exit is not known, so its end is not shown`);
  return result(PASS, `${(took / 1000).toFixed(1)} s from its first to its last recorded event, limit ${(limitMs / 1000).toFixed(1)} s`);
}

function nn12(ctx) {
  const { log, live } = ctx;
  const snapshot = /snapshot (\S+) is commit/.exec(log)?.[1] ?? null;
  if (!live) return result(NOT_MEASURED, "a recording cannot show what was left behind; this line is judged right after a run ends (--live)");
  if (!snapshot) return result(NOT_MEASURED, "the log names no snapshot, so its lock cannot be looked for");
  if (!Array.isArray(live.pids)) return result(NOT_MEASURED, "the processes the run started are not known, so none can be shown gone");
  if (live.exists(`${snapshot}.lock`)) return result(FAIL, `the lock ${snapshot}.lock is still there after the run`);
  const alive = live.pids.filter((pid) => live.alive(pid));
  if (alive.length) return result(FAIL, `${alive.length} process(es) the run started are still alive: ${alive.slice(0, 5).join(", ")}`);
  // Council CV-1 F-30: a clean pass needs the whole list of the run's own processes, and a run that did not end normally passes only on a
  // record that the next ordinary run started safely after it.
  const known = new Set([...ctx.ledger.map((e) => e.pid), ...(ctx.ledger.ends?.keys() ?? [])]);
  const missing = [...known].filter((p) => !live.pids.includes(p));
  if (!live.pids.length || missing.length) return result(NOT_MEASURED, `the list of the run's processes checked after it ${live.pids.length ? `leaves out ${missing.slice(0, 5).join(", ")}, which the process list names` : "is empty"}, so none can be shown gone`);
  if (!Number.isInteger(ctx.exit) || ctx.exit !== 0) {
    if (live.successor?.ok === true && live.successor?.reclaimed === true) return result(PASS, `the run ended with ${ctx.exit ?? "no exit code"}; the next ordinary run reclaimed it and started safely, and no lock or process was left behind`);
    return result(NOT_MEASURED, `the run ended with ${ctx.exit ?? "no exit code"}, not normally; that the next ordinary run starts safely by itself needs that run's record`);
  }
  return result(PASS, "no lock and no process left behind");
}
const JUDGES = Object.freeze({ NN01: nn01, NN02: nn02, NN03: nn03, NN04: nn04, NN05: nn05, NN06: nn06, NN07: nn07, NN08: nn08, NN09: nn09, NN10: nn10, NN11: nn11, NN12: nn12 });
export const CHECK_IDS = Object.freeze(Object.keys(JUDGES));

/**
 * Judges one run against `lines` (readAcceptanceLines of the contract; required, and checked again here). `log`: its text. `ledger`: its
 * process list (parseLedger). `commit`: the commit selected for the run, from whoever started it. `receipt`: its receipt object or null.
 * `readTree(path)`: a file of the run's own Git tree (the tree its snapshot line names), parsed JSON or text; throws when unavailable.
 * `limitMs`: the time limit for this kind of run on this machine, or null. `exit`: its exit code, or null. `live`: { exists(path),
 * alive(pid), pids, successor? } when judged right after the run, else null. Returns { lines: [{ id, text, verdict, why }], pass, failed,
 * notMeasured }. `pass` is true only when all twelve passed.
 */
export function judgeRun({ lines, log = "", ledger = null, commit = null, runPid = null, receipt = null, readTree = null, limitMs = null, exit = null, live = null, readJson } = {}) {
  const unlinked = Array.isArray(lines) ? linkProblems(lines) : ["no lines were given: read them from the contract with readAcceptanceLines"];
  if (unlinked.length) throw Object.assign(new Error(`the contract and the acceptance test do not match: ${unlinked.join("; ")}`), { problems: unlinked });
  const read = readJson ?? ((f) => JSON.parse(fs.readFileSync(f, "utf8")));
  const text = String(log);
  const entries = Array.isArray(ledger) ? ledger : [];
  const vm = /class A verdict written: (.+?)\s*$/m.exec(text); const vLine = vm ? text.slice(0, vm.index).split(/\r?\n/).length - 1 : -1;
  let verdictDoc = null; if (vm) { try { verdictDoc = read(vm[1].trim()); } catch { verdictDoc = null; } }
  const snap = snapshotOf(text);
  const ctx = {
    log: text, ledger: Object.assign(entries, {}), ledgerGaps: ledgerGaps(ledger, { lock: snap.lock, runPid }), commit, receipt, limitMs, exit, live, snap,
    decisions: decisionsOf(text, { readJson: read }), coverage: coveragesOf(text, read),
    starts: entries.map((e) => ({ ...decodeStart(e), at: timeOf(e.at), ms: typeof e.ms === "number" ? e.ms : null })),
    compiles: entries.map(brokerCompileOf).filter(Boolean), brokerStarts: entries.map(brokerStartOf).filter(Boolean),
    readTree: readTree ?? (() => { throw new Error("no reader for the run's Git tree was given (--repo)"); }),
    verdict: { file: vm ? vm[1].trim() : null, doc: verdictDoc, at: vLine >= 0 ? stampOf(text.split(/\r?\n/)[vLine]) ?? NaN : NaN },
  };
  const judged = lines.map(({ id, text: sentence }) => { let r; try { r = JUDGES[id](ctx); } catch (e) { r = result(FAIL, `the check itself failed: ${e.message}`); } return { id, text: sentence, verdict: r.verdict, why: r.why }; });
  const failed = judged.filter((l) => l.verdict === FAIL).map((l) => l.id); const notMeasured = judged.filter((l) => l.verdict === NOT_MEASURED).map((l) => l.id);
  return { lines: judged, pass: failed.length === 0 && notMeasured.length === 0, failed, notMeasured };
}

// ---- command line -----------------------------------------------------------------------------------------------------------------------------------
if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const opt = (name) => { const i = process.argv.indexOf(name); return i < 0 ? null : process.argv[i + 1]; };
  const logFile = opt("--log"); const ledgerFile = opt("--ledger");
  if (!logFile || !ledgerFile) { console.error("usage: acceptance-lines.mjs --log <file> --ledger <file> --commit <sha> --pid <gate pid> [--repo <dir>] [--contract <file>] [--receipt <file>] [--limit-ms <n>] [--exit <code>] [--json]"); process.exit(2); }
  const contractFile = opt("--contract") ?? CONTRACT_FILE; let lines;
  try { lines = readAcceptanceLines(fs.readFileSync(contractFile, "utf8")); } catch (e) { console.error(`NOT JUDGED: ${e.code === "ENOENT" ? `the contract ${contractFile} cannot be read` : e.message}`); process.exit(2); }
  const receiptFile = opt("--receipt"); let receipt = null; if (receiptFile) { try { receipt = JSON.parse(fs.readFileSync(receiptFile, "utf8")); } catch { receipt = null; } }
  const log = fs.readFileSync(logFile, "utf8"); const tree = snapshotOf(log).tree; const repo = opt("--repo");
  // The run's own Git tree, read by its id from a repository that holds it: never the snapshot folder, which a later run can change.
  const readTree = repo && tree ? (p) => { const t = execFileSync("git", ["-C", repo, "cat-file", "-p", `${tree}:${p}`], { encoding: "utf8", stdio: ["ignore", "pipe", "pipe"] }); try { return JSON.parse(t); } catch { return t; } } : null;
  const exitArg = opt("--exit"); const exit = exitArg === null ? null : /^-?\d+$/.test(exitArg) ? Number(exitArg) : null;
  const pidArg = opt("--pid"); const runPid = pidArg !== null && /^\d+$/.test(pidArg) ? Number(pidArg) : null;
  const verdict = judgeRun({ lines, log, ledger: parseLedger(fs.readFileSync(ledgerFile, "utf8")), commit: opt("--commit"), runPid, receipt, readTree, limitMs: opt("--limit-ms") === null ? null : Number(opt("--limit-ms")), exit });
  if (process.argv.includes("--json")) console.log(JSON.stringify(verdict, null, 2));
  else { for (const l of verdict.lines) console.log(`${l.id} ${l.verdict.toUpperCase().padEnd(12)} ${l.text}\n      ${l.why}`); console.log(verdict.pass ? "PASS: all twelve lines" : `NOT PASSED: ${verdict.failed.length} failed, ${verdict.notMeasured.length} not measured`); }
  process.exit(verdict.pass ? 0 : 1);
}
