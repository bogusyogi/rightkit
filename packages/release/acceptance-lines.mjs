// The gate's acceptance test: twelve lines that must be true of every run (GATE-CONTRACT GC-A27; Adrian, 2026-11-20: "An acceptance test
// should be a single list of things that has to pass any run").
//
// It judges ONE run from the records that run leaves behind, and nothing else: its log, the list of every process it started (the spawn
// ledger), its forecast files (what it said it would reuse and run, and what happened), its row reports (coverage), its Class A verdict and
// its receipt. It stages nothing and knows no scenario: a first build, a repeat, a run after an edit and a real build of an app are all
// judged by the same twelve lines. A line with no record to judge it from is "not measured", never a pass.
//
// Rules every check keeps (gatecodex review, 2026-11-20):
//  * Missing proof is not the absence of work. "Nothing was sent", "nothing repeated" and "no other compile" are shown only from a process
//    list that is complete (parseLedger, ledgerGaps); an incomplete one leaves the line not measured.
//  * No record is trusted alone. The forecast, the lookup results, the coverage report, the dispatch list and the process list are joined
//    row by row, and any disagreement among them is found here, not read from the gate's own summary of it.
//  * Every record must be this run's: the selected commit, the snapshot, its lock and the receipt are bound to one another.
//
// The lines are not written here. They are read from the one table in docs/GATE-CONTRACT.md [GC-A27] between
// `<!-- acceptance-lines:begin -->` and `<!-- acceptance-lines:end -->`, and nothing is judged unless every line there has a check here and
// every check here has a line there.
//
//   node acceptance-lines.mjs --log <file> --ledger <file> --commit <sha> [--contract <file>] [--receipt <file>] [--limit-ms <n>] [--exit <code>] [--json]
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

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
  const show = (xs) => `${xs.slice(0, 4).join("; ")}${xs.length > 4 ? `; and ${xs.length - 4} more` : ""}`;
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
 * The process list (RK_GATE_SPAWN_LEDGER). Each line is one finished child `{pid, at, ms, sync, cmd, args, cwd, status}`, or a process's end
 * record `{end: true, pid, at, open}` written when that process finishes, `open` counting its children still running. Returns the entries
 * as an array that also carries `problems` (every line that is not a whole entry: nothing is dropped silently) and `ends` (end records by pid).
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
 * Why the process list cannot be taken as complete, or [] when it can: every line is a whole entry, and every process that wrote to it closed
 * it with an end record saying none of its children was still running. A list without that is a sample, and a sample cannot show absence.
 */
export function ledgerGaps(ledger) {
  if (!Array.isArray(ledger)) return ["the run has no process list"];
  if (!Array.isArray(ledger.problems) || !(ledger.ends instanceof Map)) return ["the process list was not read with parseLedger, so whether it is complete is unknown"];
  const gaps = ledger.problems.map((p) => `the process list is incomplete: ${p}`);
  if (!ledger.ends.size) gaps.push("the process list has no end record, so a process started and never recorded cannot be ruled out");
  for (const pid of new Set(ledger.map((e) => e.pid))) {
    const end = ledger.ends.get(pid);
    if (!end && ledger.ends.size) gaps.push(`process ${pid} wrote to the process list and no end record`);
    else if (end && end.open !== 0) gaps.push(`process ${pid} ended with ${end.open} child(ren) still open, which the list does not show`);
  }
  for (const [pid, end] of ledger.ends) if (end.open !== 0 && !ledger.some((e) => e.pid === pid)) gaps.push(`process ${pid} ended with ${end.open} child(ren) still open, which the list does not show`);
  return gaps;
}

const COMPILING = new Set(["build", "check", "clippy", "doc", "test", "rustc", "rustdoc"]);
/**
 * A compile sent to the broker, or null: `rustright cargo <sub> ...`, started directly or through `cmd /c "<...>rustright.cmd" cargo ...`.
 * Returns { sub, args (after the subcommand), cwd, at, ms }.
 */
export function brokerCompileOf(entry) {
  const whole = [entry.cmd, ...entry.args].join(" ").replaceAll('"', " ").replace(/\s+/g, " ");
  const m = /(?:^|[\\/ ])rustright(?:\.cmd|\.exe)? cargo (\S+)((?: .*)?)$/i.exec(whole);
  if (!m || !COMPILING.has(m[1])) return null;
  return { sub: m[1], args: m[2].trim().split(" ").filter(Boolean), cwd: String(entry.cwd ?? ""), at: typeof entry.at === "string" ? Date.parse(entry.at) : null, ms: typeof entry.ms === "number" ? entry.ms : null };
}
/**
 * Anything sent to the broker: a start of the rustright client itself, directly or through `cmd /c "<...>rustright.cmd" ...`, whatever it
 * asks (a compile, `cargo metadata`, a version). Looking the program up (`where rustright`) starts something else and is not a send.
 * Returns { args, cwd, at }, or null.
 */
export function brokerStartOf(entry) {
  let cmd = String(entry.cmd); let args = (entry.args ?? []).map(String);
  if (/(^|[\\/])cmd\.exe$/i.test(cmd)) {
    const c = args.findIndex((a) => /^\/c$/i.test(a));
    const line = c < 0 ? "" : args.slice(c + 1).join(" ").replaceAll('"', " ").trim();
    [cmd, ...args] = line.split(/\s+/);
  }
  if (!/(^|[\\/])rustright(\.cmd|\.exe)?$/i.test(String(cmd ?? ""))) return null;
  return { args, cwd: String(entry.cwd ?? ""), at: typeof entry.at === "string" ? Date.parse(entry.at) : null };
}
/** The values a cargo flag takes in an argument list: `--package x`, `--package=x`, `-p x`, `-px`. */
const valuesOf = (args, long, short = null) => {
  const out = [];
  for (let i = 0; i < args.length; i += 1) {
    const a = args[i];
    if (a === long || (short && a === short)) { if (i + 1 < args.length) out.push(args[(i += 1)]); }
    else if (a.startsWith(`${long}=`)) out.push(a.slice(long.length + 1));
    else if (short && a.startsWith(short) && a.length > short.length && !a.startsWith("--")) out.push(a.slice(short.length));
  }
  return out;
};
const cratesOf = (c) => valuesOf(c.args, "--package", "-p");
const targetsOf = (c) => valuesOf(c.args, "--target");
/** A Class B row id as its stage, the names it carries and its target: `clippy:rk:rk-leaf@x86_64-unknown-linux-gnu`, `test:rust:rk:rk-leaf`. */
export function rowOf(id) {
  const [head, target = null] = String(id).split("@");
  const [sub, ...rest] = head.split(":");
  const variant = rest.find((s) => /^\d+$/.test(s)) ?? null;
  return { sub, names: rest.filter((s) => s && s !== "rust" && !/^\d+$/.test(s)), target, variant };
}
/** A Class B row the broker compiles: a cargo stage, and for tests only the Rust ones (`test:npm:*` suites run in the gate, not the broker). */
const isBrokerRow = (id) => { const r = rowOf(id); return COMPILING.has(r.sub) && (r.sub !== "test" || String(id).startsWith("test:rust:")); };
/** Whether a broker compile does a Class B row's work: the same stage, a crate the row names, and the row's target when it names one. */
const covers = (c, id) => { const r = rowOf(id); return isBrokerRow(id) && c.sub === r.sub && cratesOf(c).some((n) => r.names.includes(n)) && (r.target === null || targetsOf(c).includes(r.target)); };
/** The same row in another feature set (`clippy:rk:rk-slow:0` and `clippy:rk:rk-slow:1`): one compile command cannot tell them apart. */
const otherVariant = (a, b) => { const x = rowOf(a); const y = rowOf(b); return x.variant !== y.variant && x.sub === y.sub && x.target === y.target && x.names.join(":") === y.names.join(":"); };

/** The forecast files a log names ("Class A cache decision: <file>", "Class B cache decision: <file>"), read; an unreadable one is reported. */
export function decisionsOf(log, { readJson = (f) => JSON.parse(fs.readFileSync(f, "utf8")) } = {}) {
  const out = [];
  for (const m of String(log).matchAll(/Class ([AB]) cache decision: (.+?)\s*$/gm)) {
    let doc = null; let problem = null;
    try { doc = readJson(m[2].trim()); } catch (e) { problem = `cannot read ${m[2].trim()} (${e.code ?? e.message})`; }
    out.push({ stage: m[1], file: m[2].trim(), doc, problem });
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
/**
 * A forecast, lookup or coverage value as reuse ("hit"), run ("run") or "uncacheable" (a row the gate keeps no result for, so it runs every
 * time: forecast and looked up as such, covered as ran).
 */
const normal = (v) => (v === "hit" ? "hit" : v === "run" || v === "ran" ? "run" : v === "uncacheable" ? "uncacheable" : null);
const runs = (v) => v === "run" || v === "uncacheable";
const sameOutcome = (forecast, other) => normal(forecast) === normal(other) || (normal(forecast) === "uncacheable" && normal(other) === "run");

/**
 * One stage (A or B) with every record of it joined by row: the forecast (F), the lookup results (A), the coverage rows of this pass (C, null
 * when no coverage report covers the stage) and the dispatch list (null when the decision has none).
 */
function stageOf(ctx, stage) {
  const ds = ctx.decisions.filter((d) => d.stage === stage);
  const d = ds.length === 1 ? ds[0] : null;
  const doc = d?.doc ?? null;
  const reports = ctx.coverage.filter((c) => c.doc && Array.isArray(c.doc.scans) && c.doc.scans.some((s) => s.class === stage && s.coverage !== "skipped"));
  const C = reports.length ? new Map(reports.flatMap((c) => c.doc.scans).filter((s) => s.class === stage && s.coverage !== "skipped").map((s) => [s.id, s])) : null;
  const F = new Map((Array.isArray(doc?.forecast) ? doc.forecast : []).map((f) => [f.id, f]));
  const A = new Map((Array.isArray(doc?.actual) ? doc.actual : []).map((a) => [a.id, a]));
  const dispatched = Array.isArray(doc?.dispatchedRows) ? new Set(doc.dispatchedRows) : null;
  // A row recorded twice in one record cannot be read by id: the later entry would silently hide the earlier one.
  const twice = (ids) => [...new Set(ids.filter((x, i) => ids.indexOf(x) !== i))];
  const dups = {
    forecast: twice((Array.isArray(doc?.forecast) ? doc.forecast : []).map((f) => f.id)),
    lookup: twice((Array.isArray(doc?.actual) ? doc.actual : []).map((a) => a.id)),
    coverage: twice(reports.flatMap((c) => c.doc.scans).filter((s) => s.class === stage && s.coverage !== "skipped").map((s) => s.id)),
    dispatch: twice(Array.isArray(doc?.dispatchedRows) ? doc.dispatchedRows : []),
  };
  const dupIds = new Set(Object.values(dups).flat());
  return { stage, decisions: ds, d, doc, F, A, C, dispatched, dups, dupIds };
}

/**
 * What the records say about the work a stage did in this run: `did` names the evidence of work (a row run, dispatched or covered as ran, a
 * broker compile for Class B), `unknown` why none cannot be shown (a missing record, an incomplete process list). Both empty: proven none.
 */
function stageWork(ctx, stage) {
  const s = stageOf(ctx, stage); const did = []; const unknown = [];
  if (s.dispatched?.size) did.push(`${s.dispatched.size} row(s) dispatched`);
  const ran = [...(s.C?.values() ?? [])].filter((c) => c.coverage === "ran").length; if (ran) did.push(`${ran} row(s) covered as ran`);
  const looked = [...s.A.values()].filter((a) => runs(normal(a.actual))).length; if (looked) did.push(`${looked} row(s) looked up to run`);
  if (stage === "B" && ctx.compiles.length) did.push(`${ctx.compiles.length} broker compile(s)`);
  if (!s.doc) unknown.push(`no Class ${stage} forecast`); else if (!s.dispatched) unknown.push(`no Class ${stage} dispatch list`);
  if (!s.C) unknown.push(`no coverage report for Class ${stage}`);
  if (stage === "B") unknown.push(...ctx.ledgerGaps);
  return { did, unknown };
}

// ---- the twelve lines ------------------------------------------------------------------------------------------------------------------------------
function snapshotOf(log) {
  const said = [...log.matchAll(/snapshot (\S+) is commit ([0-9a-f]{40}) tree ([0-9a-f]{40})/g)].map((m) => ({ path: m[1], commit: m[2], tree: m[3] }));
  const after = [...log.matchAll(/is still tree ([0-9a-f]{40}) after/g)].map((m) => m[1]);
  const lock = /snapshot lock held (\S+)/.exec(log)?.[1] ?? null;
  return { said, after, lock, commit: said[0]?.commit ?? null, tree: said[0]?.tree ?? null, path: said[0]?.path ?? null };
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
  const runCommit = commit ?? snap.commit;
  for (const d of ctx.decisions) if (d.doc && d.doc.commit !== runCommit) fails.push(`the Class ${d.stage} forecast is for commit ${short(d.doc.commit)}`);
  for (const d of ctx.decisions) if (d.doc && d.stage === "B" && d.doc.tree && d.doc.tree !== snap.tree) fails.push(`the Class B forecast is for tree ${short(d.doc.tree)}`);
  for (const c of ctx.coverage) if (c.doc && c.doc.commit && c.doc.commit !== runCommit) fails.push(`the coverage report ${path.basename(c.file)} is for commit ${short(c.doc.commit)}`);
  if (ctx.verdict.doc && (ctx.verdict.doc.commit !== runCommit || ctx.verdict.doc.tree !== snap.tree)) fails.push(`the Class A verdict is for commit ${short(ctx.verdict.doc.commit)} tree ${short(ctx.verdict.doc.tree)}`);
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
  return decide(fails, gaps, `selected commit ${short(runCommit)}, tree ${short(snap.tree)} before and after, receipt and records bound to lock ${snap.lock}`);
}

function nn02(ctx) {
  const { log, compiles, snap } = ctx; const fails = []; const gaps = [];
  const runCommit = ctx.commit ?? snap.commit; const v = ctx.verdict;
  const verdictOk = v.doc && v.doc.status === "pass" && v.doc.commit === runCommit && v.doc.tree === snap.tree && Number.isFinite(v.at);
  const covA = stageOf(ctx, "A").C;
  const aFailed = v.doc?.status === "fail" || /class A .*fail|FAIL .*\(class A\)|class A check .* failed/i.test(log) || [...(covA?.values() ?? [])].some((s) => s.status === "fail");
  const sentB = stageOf(ctx, "B").dispatched;
  if (v.doc && !verdictOk && !aFailed) fails.push(`the Class A verdict ${path.basename(v.file)} is not a pass of commit ${short(runCommit)} tree ${short(snap.tree)} (it says ${v.doc.status} for ${short(v.doc.commit)} ${short(v.doc.tree)})`);
  if (v.file && !v.doc) gaps.push(`the Class A verdict ${v.file} cannot be read`);
  const lgaps = ctx.ledgerGaps;
  const sends = ctx.brokerStarts;
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
  const early = sends.filter((c) => !(c.at >= v.at));
  if (early.length) fails.push(`${early.length} start(s) went to the broker before the Class A verdict (first: rustright ${early[0].args.slice(0, 2).join(" ")})`);
  gaps.push(...lgaps.map((g) => `that nothing went to the broker before the verdict cannot be shown: ${g}`));
  return decide(fails, gaps, `Class A passed ${short(runCommit)} first, then ${compiles.length} compile(s) went to the broker (complete process list)`);
}

const LANE_TOLERANCE_MS = 250;
function nn03(ctx) {
  const s = stageOf(ctx, "A"); const covA = s.C;
  if (!covA) return result(NOT_MEASURED, "no coverage report covers Class A");
  // Every row any record says ran: covered as ran, dispatched, or looked up to run. Each needs its lane times in the coverage report.
  const ranIds = new Set([...[...covA.values()].filter((c) => c.coverage === "ran").map((c) => c.id), ...(s.dispatched ?? []), ...[...s.A.values()].filter((a) => runs(normal(a.actual))).map((a) => a.id)]);
  if (!ranIds.size) {
    // Gatecodex ruling 1: "no row waited" passes only for a proven, complete, wholly reused Class A: nothing forecast to run, nothing
    // dispatched or run, and a coverage report naming exactly the forecast's rows.
    const why = [];
    if (!s.doc || !s.F.size) why.push("no Class A forecast"); else if ([...s.F.values()].some((f) => f.expected !== "hit")) why.push("the forecast owes Class A work that no record shows ran");
    if (!s.dispatched) why.push("no Class A dispatch list");
    if (s.F.size && ([...covA.keys()].some((id) => !s.F.has(id)) || [...s.F.keys()].some((id) => !covA.has(id)))) why.push("the coverage report and forecast name different rows");
    if (s.dupIds.size) why.push(`row(s) recorded twice: ${[...s.dupIds].slice(0, 3).join(", ")}`);
    return why.length ? result(NOT_MEASURED, `no Class A row ran, and that none was owed cannot be shown: ${why.join("; ")}`) : result(PASS, `Class A was wholly reused (${s.F.size} rows forecast, looked up and covered as reused, none dispatched), so no row waited`);
  }
  const lane = (c) => c?.lane && Number.isInteger(c.lane.lanes) && c.lane.lanes > 0 && [c.lane.readyAt, c.lane.startedAt, c.lane.endedAt].every(Number.isFinite) && c.lane.readyAt <= c.lane.startedAt && c.lane.startedAt <= c.lane.endedAt;
  const bare = [...ranIds].filter((id) => !lane(covA.get(id)));
  if (bare.length) return result(NOT_MEASURED, `no valid lane record (ready <= started <= ended, lanes >= 1) in the coverage report for row(s) that ran: ${bare.slice(0, 4).join(", ")}${bare.length > 4 ? ` and ${bare.length - 4} more` : ""}`);
  const ran = [...ranIds].map((id) => covA.get(id));
  const fails = [];
  // A row waited beside a free lane when, at any moment from its ready time to its start, fewer rows ran than there were lanes (checked over the
  // whole wait, segment by segment between the other rows' starts and ends, not only at the ready time).
  for (const r of ran) {
    const { readyAt, startedAt, lanes } = r.lane;
    if (startedAt - readyAt <= LANE_TOLERANCE_MS) continue;
    const others = ran.filter((o) => o !== r);
    const points = [...new Set([readyAt, ...others.flatMap((o) => [o.lane.startedAt, o.lane.endedAt]).filter((t) => t > readyAt && t < startedAt)])].sort((a, b) => a - b);
    let idle = 0; let idleFrom = null;
    points.forEach((t, i) => {
      const next = points[i + 1] ?? startedAt; const busy = others.filter((o) => o.lane.startedAt <= t && o.lane.endedAt > t).length;
      if (busy < lanes) { idle += next - t; idleFrom ??= t; }
    });
    if (idle > LANE_TOLERANCE_MS) fails.push(`${r.id} waited ${Math.round(startedAt - readyAt)} ms, ${Math.round(idle)} ms of it with a lane free (from ${idleFrom})`);
  }
  return decide(fails, [], `${ran.length} Class A row(s) ran; none waited while a lane was free`);
}

function nn04(ctx) {
  const fails = []; const gaps = [];
  const runCommit = ctx.commit ?? ctx.snap.commit;
  const worked = { A: !!stageOf(ctx, "A").C || !!ctx.verdict.file, B: !!stageOf(ctx, "B").C || ctx.compiles.length > 0 };
  for (const stage of ["A", "B"]) {
    const s = stageOf(ctx, stage);
    if (!s.decisions.length) { if (worked[stage]) fails.push(`Class ${stage} worked (coverage or compiles) with no forecast named in the log`); continue; }
    if (s.decisions.length > 1) { fails.push(`the log names ${s.decisions.length} Class ${stage} forecasts`); continue; }
    const { d, doc } = s;
    if (d.problem || !doc) { fails.push(`Class ${stage} forecast: ${d.problem ?? "unreadable"}`); continue; }
    if (!Array.isArray(doc.forecast) || !doc.forecast.length) { fails.push(`the Class ${stage} forecast ${path.basename(d.file)} is missing or empty`); continue; }
    if (doc.commit !== runCommit) fails.push(`the Class ${stage} forecast is for commit ${short(doc.commit)}, not ${short(runCommit)}`);
    const ids = doc.forecast.map((f) => f.id);
    for (const id of new Set(ids.filter((x, i) => ids.indexOf(x) !== i))) fails.push(`Class ${stage} row ${id} appears more than once in the forecast`);
    for (const f of doc.forecast) if (normal(f.expected) === null || f.expected === "ran") fails.push(`Class ${stage} row ${f.id} is forecast as ${JSON.stringify(f.expected)}, not reuse, run or uncacheable`);
    const frozen = timeOf(doc.forecastFrozenAt); const lookup = timeOf(doc.firstLookupAt);
    if (!Number.isFinite(frozen) || !Number.isFinite(lookup)) gaps.push(`the Class ${stage} forecast does not record when it was frozen and when the first lookup began (forecastFrozenAt, firstLookupAt)`);
    else if (frozen > lookup) fails.push(`the Class ${stage} forecast was frozen at ${doc.forecastFrozenAt}, after the first lookup at ${doc.firstLookupAt}`);
    // Class B's work is what goes to the broker: none of it may start before its forecast is frozen, whatever the lookup times say.
    if (stage === "B" && Number.isFinite(frozen)) { const early = ctx.brokerStarts.filter((b) => !(b.at >= frozen)); if (early.length) fails.push(`${early.length} start(s) went to the broker before the Class B forecast was frozen at ${doc.forecastFrozenAt}`); }
    if (s.C) {
      const missing = [...s.C.keys()].filter((id) => !s.F.has(id)); const extra = [...s.F.keys()].filter((id) => !s.C.has(id));
      if (missing.length) fails.push(`the Class ${stage} forecast leaves out row(s) the run covered: ${missing.slice(0, 4).join(", ")}`);
      if (extra.length) fails.push(`the Class ${stage} forecast names row(s) the run never covered: ${extra.slice(0, 4).join(", ")}`);
    } else gaps.push(`no coverage report covers Class ${stage}, so its forecast cannot be shown complete`);
  }
  if (!ctx.decisions.length && !fails.length) return result(NOT_MEASURED, "the log names no forecast file");
  return decide(fails, gaps, ctx.decisions.map((d) => `Class ${d.stage}: ${d.doc.forecast.length} rows forecast before the first lookup`).join("; "));
}

function nn05(ctx) {
  const fails = []; const gaps = []; const notes = [];
  for (const stage of ["A", "B"]) {
    const s = stageOf(ctx, stage);
    if (!s.doc || !s.F.size) continue;
    for (const [record, ids] of Object.entries(s.dups)) for (const id of ids) fails.push(`Class ${stage}: ${id} appears more than once in the ${record} record, so which entry holds cannot be known`);
    const lookup = [];
    for (const [id, f] of s.F) { const a = s.A.get(id); if (!a) lookup.push(`${id} has no lookup result`); else if (!sameOutcome(f.expected, a.actual)) lookup.push(`${id} was forecast ${f.expected}, looked up ${a.actual}`); }
    for (const id of s.A.keys()) if (!s.F.has(id)) lookup.push(`${id} was looked up and not forecast`);
    if (!s.dispatched) gaps.push(`the Class ${stage} forecast records no dispatch list, so what was sent cannot be shown`);
    if (lookup.length) {
      const named = Array.isArray(s.doc.mismatches) ? s.doc.mismatches : [];
      const divergence = [...ctx.log.matchAll(/RK_CACHE_DIVERGENCE (.*)$/gm)].map((m) => m[1]).join("\n");
      const ids = lookup.map((l) => l.split(" ")[0]);
      for (const id of ids) {
        const m = named.find((x) => x?.id === id);
        if (!m || typeof m.reason !== "string" || !m.reason.trim()) fails.push(`Class ${stage}: the lookup disagreed for ${id} and the run did not record its cause`);
        if (!divergence.includes(id)) fails.push(`Class ${stage}: the lookup disagreed for ${id} and the run did not name it (RK_CACHE_DIVERGENCE)`);
      }
      if (ctx.exit === null || ctx.exit === undefined) gaps.push(`Class ${stage}: the lookup disagreed (${lookup[0]}) and the run's exit code is not known`);
      else if (ctx.exit === 0) fails.push(`Class ${stage}: the lookup disagreed (${lookup[0]}) and the run still exited 0`);
      if (s.dispatched?.size) fails.push(`Class ${stage}: the lookup disagreed (${lookup[0]}) and ${s.dispatched.size} row(s) were still sent`);
      const ranRows = [...(s.C?.values() ?? [])].filter((c) => c.coverage === "ran").map((c) => c.id);
      if (ranRows.length) fails.push(`Class ${stage}: the lookup disagreed and ${ranRows.slice(0, 3).join(", ")} still ran`);
      if (ctx.compiles.length) fails.push(`Class ${stage}: the lookup disagreed and ${ctx.compiles.length} compile(s) were still sent to the broker`);
      gaps.push(...ctx.ledgerGaps.map((g) => `that nothing was sent after the disagreement cannot be shown: ${g}`));
      notes.push(`Class ${stage}: the lookup disagreed for ${ids.slice(0, 3).join(", ")}; the run stopped before sending work, named it and failed`);
      continue;
    }
    if (Array.isArray(s.doc.mismatches) && s.doc.mismatches.length) fails.push(`Class ${stage}: the run recorded mismatches (${s.doc.mismatches.map((m) => m?.id).join(", ")}) where the forecast and lookup agree`);
    const run = new Set([...s.F].filter(([, f]) => runs(normal(f.expected))).map(([id]) => id));
    if (!s.C) gaps.push(`no coverage report covers Class ${stage}, so what ran cannot be compared with the forecast`);
    else {
      for (const [id, c] of s.C) if (!s.F.has(id)) fails.push(`Class ${stage}: ${id} was covered and not forecast`);
      for (const [id, f] of s.F) { const c = s.C.get(id); if (!c) fails.push(`Class ${stage}: ${id} was forecast and not covered`); else if (!sameOutcome(f.expected, c.coverage)) fails.push(`Class ${stage}: ${id} was forecast ${f.expected} and ${c.coverage}`); }
    }
    if (s.dispatched) {
      for (const id of s.dispatched) if (!run.has(id)) fails.push(`Class ${stage}: ${id} was sent and the forecast said reuse`);
      for (const id of run) if (!s.dispatched.has(id)) fails.push(`Class ${stage}: ${id} was forecast to run and not sent`);
    }
    if (stage === "B") {
      for (const c of ctx.compiles) if (!(s.dispatched ? [...s.dispatched] : [...run]).some((id) => covers(c, id))) fails.push(`a broker compile (cargo ${c.sub} ${cratesOf(c).map((n) => `-p ${n}`).join(" ")}) does no row the forecast said to run`);
      gaps.push(...ctx.ledgerGaps.map((g) => `that no other compile was sent cannot be shown: ${g}`));
    }
    notes.push(`Class ${stage}: forecast, lookup, coverage and dispatch agree on ${s.F.size} rows (${run.size} run)`);
  }
  if (!notes.length && !fails.length && !gaps.length) return result(NOT_MEASURED, "no forecast file to compare with what ran");
  return decide(fails, gaps, notes.join("; "));
}

function nn06(ctx) {
  const fails = []; const gaps = []; let reused = 0; let failures = 0;
  for (const stage of ["A", "B"]) {
    const s = stageOf(ctx, stage);
    const hits = [...s.F.values()].filter((f) => f.expected === "hit");
    // Absence is shown only from complete records (gatecodex review 2): with no coverage report, or a process list that may be missing
    // starts, a reused row that ran cannot be ruled out.
    if (hits.length && !s.C) gaps.push(`no coverage report covers Class ${stage}, so that its ${hits.length} reused row(s) did not run cannot be shown`);
    if (hits.length && ctx.ledgerGaps.length && (stage === "B" || hits.some((f) => /^rustfmt:/.test(f.id)))) gaps.push(`that no process did a reused Class ${stage} row's work cannot be shown: ${ctx.ledgerGaps[0]}`);
    for (const [id, f] of s.F) {
      if (f.expected === "uncacheable") {
        // Gatecodex ruling 2: not measured while whether its inputs changed is unknown; a failure when its own record shows them unchanged and it ran.
        const unchanged = (typeof f.key === "string" && f.key === f.priorKey) || f.changes?.inputsChanged === 0;
        const ran = s.dispatched?.has(id) || runs(normal(s.A.get(id)?.actual)) || s.C?.get(id)?.coverage === "ran";
        if (unchanged && ran) fails.push(`${id} is uncacheable, its inputs did not change, and it ran again`);
        else gaps.push(`${id} is uncacheable: it runs whether or not its inputs changed, so that it is never run again unchanged cannot be shown`);
        continue;
      }
      if (f.expected !== "hit") continue;
      reused += 1;
      if (s.dupIds.has(id)) { gaps.push(`${id} is recorded more than once, so its reuse cannot be read`); continue; }
      const a = s.A.get(id); const c = s.C?.get(id) ?? null;
      if (!a) gaps.push(`${id} was forecast unchanged and has no lookup result`);
      else {
        if (normal(a.actual) !== "hit") fails.push(`${id} is unchanged and ran again (${a.actual})`);
        if (typeof f.key === "string" && typeof a.key === "string" && f.key !== a.key) fails.push(`${id} was reused under key ${short(a.key)}, not its forecast key ${short(f.key)}`);
      }
      if (s.C && !c) gaps.push(`${id} was forecast unchanged and is not in the coverage report`);
      if (c && normal(c.coverage) !== "hit") fails.push(`${id} is unchanged and the coverage report says it ${c.coverage}`);
      if (f.status === "fail") {
        failures += 1;
        const recorded = [a?.status, c?.status].filter((x) => x !== undefined && x !== null);
        if (!recorded.length) gaps.push(`${id} was an earlier failure and no record says what the reuse returned`);
        if (recorded.some((x) => x !== "fail")) fails.push(`${id} was an earlier failure and came back ${recorded.find((x) => x !== "fail")}: a reused failure stays a failure`);
      }
      if (stage === "B") {
        const c2 = ctx.compiles.find((x) => covers(x, id));
        // A compile naming this crate for a changed feature set of the same row cannot be told from one doing this row's work: say so, not guess.
        const sibling = c2 ? [...s.F].find(([o, of]) => of.expected !== "hit" && otherVariant(id, o) && covers(c2, o)) : null;
        if (sibling) gaps.push(`${id} is unchanged and a compile for ${sibling[0]} names the same crate; which feature set it built is not recorded`);
        else if (c2) fails.push(`${id} is unchanged and a broker compile did its work (cargo ${c2.sub} ${cratesOf(c2).map((n) => `-p ${n}`).join(" ")})`);
      }
      if (stage === "A" && /^rustfmt:/.test(id)) {
        // A rustfmt start for this edition does this row's work if it checks one of this row's files; with no changed row of the edition, any
        // start for it does. Another changed row of the same edition (`rustfmt:2021` and `rustfmt:2021:2`) owns the starts that name its files.
        const ed = id.split(":")[1]; const starts = ctx.ledger.filter((e) => isRustfmt(e) && valuesOf(e.args.map(String), "--edition").includes(ed));
        const sharing = [...s.F.values()].some((o) => o.id !== id && /^rustfmt:/.test(o.id) && o.id.split(":")[1] === ed && runs(normal(o.expected)));
        if (starts.length && !sharing) fails.push(`${id} is unchanged and rustfmt ran for edition ${ed}`);
        else if (starts.length) {
          const own = changedFilesOf(s.doc, { ...f, priorKey: null });
          if (own.gap) gaps.push(own.gap);
          else { const touched = starts.flatMap((e) => rustfmtFiles(e.args)).filter((x) => own.files.has(x)); if (touched.length) fails.push(`${id} is unchanged and rustfmt checked its file(s): ${touched.slice(0, 3).join(", ")}`); }
        }
      }
    }
  }
  if (!ctx.decisions.some((d) => d.doc)) return result(NOT_MEASURED, "no forecast file, so unchanged rows cannot be told from changed ones");
  return decide(fails, gaps, `${reused} unchanged row(s) reused${failures ? `, ${failures} of them an earlier failure that stayed one` : ""}`);
}

const isRustfmt = (e) => /(^|[\\/])rustfmt(\.exe)?$/i.test(String(e.cmd));
/** The file operands of a rustfmt start: what is not a flag or a flag's value. */
const rustfmtFiles = (args) => { const out = []; for (let i = 0; i < args.length; i += 1) { const a = String(args[i]); if (["--edition", "--config", "--config-path", "--emit", "--color"].includes(a)) { i += 1; continue; } if (!a.startsWith("-")) out.push(a.replaceAll("\\", "/")); } return out; };
/** The files whose change made a Class A row run, from its fingerprint and the forecast's change list, or { gap } when they cannot be known. */
function changedFilesOf(doc, f) {
  const fp = (doc.fingerprints ?? []).find((x) => x.id === f.id);
  // The inputs are the key's JSON with its schema written after it (`{...}:rightkit.class-a-key/3`).
  let files; try { files = JSON.parse(String(fp?.inputs ?? "null").replace(/:rightkit\.[\w.-]+\/\d+$/, ""))?.files; } catch { files = null; }
  if (!Array.isArray(files)) return { gap: `${f.id} has no recorded input files` };
  if (!f.priorKey) return { files: new Set(files.map((x) => String(x[0]))) };
  const listed = f.changes?.inputs; const count = f.changes?.inputsChanged;
  if (!Array.isArray(listed) || !Number.isInteger(count)) return { gap: `${f.id} has no list of its changed inputs` };
  if (listed.length < count) return { gap: `${f.id}'s forecast lists ${listed.length} of its ${count} changed inputs, so the files the change touched cannot be named` };
  // Two spellings: by index into the input files (`files.10.1`) or by path (`files/rk/crates/rk-core/src/lib.rs`).
  const unread = listed.filter((p) => !/^files[./]/.test(p));
  if (unread.length) return { gap: `${f.id}'s changed inputs include ${unread.slice(0, 2).join(", ")}, which name no file` };
  const byIndex = listed.map((p) => /^files\.(\d+)\./.exec(p)?.[1]).filter(Boolean).map((i) => String(files[Number(i)]?.[0]));
  const byPath = listed.map((p) => /^files\/(.+)$/.exec(p)?.[1]).filter(Boolean);
  return { files: new Set([...byIndex, ...byPath]) };
}

function nn07(ctx) {
  const fails = []; const gaps = []; let changed = 0;
  for (const stage of ["A", "B"]) {
    const s = stageOf(ctx, stage);
    for (const [id, f] of s.F) {
      if (f.expected !== "run") continue;
      changed += 1;
      if (s.dupIds.has(id)) { gaps.push(`${id} is recorded more than once, so its run cannot be read`); continue; }
      const a = s.A.get(id); const c = s.C?.get(id) ?? null;
      if (!a) gaps.push(`${id} was forecast changed and has no lookup result`);
      else if (normal(a.actual) !== "run") fails.push(`${id} changed and did not run (${a.actual})`);
      if (s.C && !c) gaps.push(`${id} was forecast changed and is not in the coverage report`);
      if (c && normal(c.coverage) !== "run") fails.push(`${id} changed and the coverage report says it ${c.coverage}`);
      if (s.dispatched && !s.dispatched.has(id)) fails.push(`${id} changed and was not sent`);
      if (stage === "B" && isBrokerRow(id) && !ctx.compiles.some((x) => covers(x, id))) (ctx.ledgerGaps.length ? gaps : fails).push(`${id} changed and no broker compile in the process list does its work`);
      if (stage === "A" && /^rustfmt:/.test(id)) {
        const ed = id.split(":")[1]; const starts = ctx.ledger.filter((e) => isRustfmt(e) && valuesOf(e.args.map(String), "--edition").includes(ed));
        if (!starts.length) (ctx.ledgerGaps.length ? gaps : fails).push(`${id} changed and no rustfmt start for edition ${ed} is in the process list`);
        const scope = changedFilesOf(s.doc, f);
        if (scope.gap) gaps.push(scope.gap);
        else for (const e of starts) { const extra = rustfmtFiles(e.args).filter((x) => !scope.files.has(x)); if (extra.length) fails.push(`${id} ran rustfmt on file(s) the change did not touch: ${extra.slice(0, 4).join(", ")}`); }
      }
    }
  }
  // A compile does only the change's work when every crate it names belongs to a changed row of its stage.
  const runB = [...stageOf(ctx, "B").F].filter(([id, f]) => runs(normal(f.expected)) && isBrokerRow(id)).map(([id]) => id);
  for (const c of ctx.compiles) {
    const needed = new Set(runB.filter((id) => rowOf(id).sub === c.sub).flatMap((id) => rowOf(id).names));
    const extra = cratesOf(c).filter((n) => !needed.has(n));
    if (extra.length) fails.push(`a broker compile (cargo ${c.sub}) also names ${extra.join(", ")}, which no changed row needs`);
  }
  if (!ctx.decisions.some((d) => d.doc)) return result(NOT_MEASURED, "no forecast file, so changed rows cannot be told from unchanged ones");
  return decide(fails, gaps, `${changed} changed row(s), every one ran, and only on what changed`);
}

/**
 * NN08's per-run parts, the only ones blanked before two starts are compared (carried from the old test's NN07, gate-acceptance.mjs
 * normalizeRunPart): the temp folders of probes and shims, the macOS temp root, and pid values. Nothing else is rewritten: arguments keep their
 * case and their ids, because two starts that differ there are two different calls.
 */
const RUN_PARTS = [[/(rk-probe-|rk-gate-shim-|rk-cargo-shim-)[A-Za-z0-9_-]+/g, "$1*"], [/\/var\/folders\/[^/]+\/[^/]+\/T\//g, "$TMP/"], [/\b(pid|PID)([= :]?)\d+/g, "$1$2*"]];
const runPart = (s) => RUN_PARTS.reduce((v, [re, to]) => v.replace(re, to), String(s));
const pathKey = (s) => runPart(String(s).replaceAll("\\", "/")).toLowerCase();
/** A start as program, arguments and folder. A shell-string start (execSync: the whole line in cmd, no args) is split on whitespace. */
function callOf(e) {
  let cmd = String(e.cmd); let args = e.args.map(String);
  if (!args.length && /\s/.test(cmd.trim())) [cmd, ...args] = cmd.trim().split(/\s+/);
  const p = cmd.replaceAll("\\", "/"); const bare = !p.includes("/");
  return { name: p.split("/").pop().replace(/\.(exe|cmd|bat)$/i, "").toLowerCase(), path: bare ? null : pathKey(p.replace(/\.(exe|cmd|bat)$/i, "")), args: args.map(runPart), cwd: pathKey(e.cwd ?? ""), truncated: args.some((a) => /^\.\.\. \d+ args$/.test(a)) };
}
/** The same program: one path, or a bare name (found on PATH) and a path ending in it. Two different paths are two programs. */
const sameProgram = (a, b) => a.name === b.name && (a.path === null || b.path === null || a.path === b.path);

function nn08(ctx) {
  const { ledger } = ctx;
  if (!Array.isArray(ledger)) return result(NOT_MEASURED, "the run has no list of the processes it started");
  const buckets = new Map();
  for (const e of ledger) { const c = callOf(e); const k = JSON.stringify([c.args, c.cwd]); (buckets.get(k) ?? buckets.set(k, []).get(k)).push(c); }
  const repeats = [];
  for (const calls of buckets.values()) {
    const groups = [];
    for (const c of calls) { const g = groups.find((x) => sameProgram(x[0], c)); if (g) g.push(c); else groups.push([c]); }
    for (const g of groups) if (g.length > 1) repeats.push(g);
  }
  const fails = repeats.sort((a, b) => b.length - a.length).map((g) => `${g.length}x ${g[0].name} ${g[0].args.join(" ").slice(0, 70)}`);
  const gaps = [...ctx.ledgerGaps];
  const cut = ledger.filter((e) => callOf(e).truncated).length;
  if (cut) gaps.push(`${cut} start(s) had their arguments cut short in the process list, so they cannot be compared`);
  return decide(fails.length ? [`${repeats.reduce((n, g) => n + g.length - 1, 0)} repeat start(s) of ${ledger.length}: ${fails.join("; ")}`] : [], gaps, `${ledger.length} process starts, no two alike (complete process list)`);
}

function nn09(ctx) {
  const { compiles } = ctx; const fails = []; const gaps = [...ctx.ledgerGaps];
  if (!compiles.length) return decide([], gaps, "nothing was sent to the broker (complete process list)");
  // One group is one workspace (folder) and stage (subcommand and its settings): crate and target selectors are what one command gathers.
  // Settings compare as a set, so the same flags in another order are the same settings (gatecodex review 2); a feature list compares as a
  // set too. What follows `--` goes to the compiler in order and is kept in order.
  const VALUE_FLAGS = new Set(["--profile", "--features", "-F", "--message-format", "--manifest-path", "--config", "-Z", "--color", "-j", "--jobs", "--target-dir", "--bin", "--test", "--example", "--bench"]);
  const featureSet = (v) => v.split(/[\s,]+/).filter(Boolean).sort().join(",");
  const settings = (args) => {
    const cut = args.indexOf("--"); const head = cut < 0 ? args : args.slice(0, cut); const tail = cut < 0 ? [] : args.slice(cut);
    const units = [];
    for (let i = 0; i < head.length; i += 1) {
      const a = head[i];
      if (["-p", "--package", "--target"].includes(a)) { i += 1; continue; }
      if (/^(--package=|--target=)/.test(a) || /^-p./.test(a)) continue;
      if (/^--features=/.test(a)) { units.push(`--features ${featureSet(a.slice(11))}`); continue; }
      if (VALUE_FLAGS.has(a) && i + 1 < head.length) { const v = head[(i += 1)]; units.push(`${a === "-F" ? "--features" : a} ${a === "--features" || a === "-F" ? featureSet(v) : v}`); continue; }
      units.push(a);
    }
    return [...units.sort(), ...tail].join(" ");
  };
  const groups = new Map();
  for (const c of compiles) { const k = `${c.cwd.replaceAll("\\", "/").toLowerCase()}\u0000${c.sub}\u0000${settings(c.args)}`; (groups.get(k) ?? groups.set(k, []).get(k)).push(c); }
  // A split is justified, by the check itself, when every command beyond one names exactly one crate that the commit's own ci/shards.json
  // lists as `solo` (a crate that always gets a job of its own). The file is read from the run's snapshot; no claim of the gate's is taken.
  const sep = String(ctx.snap.path ?? "").includes("\\") ? "\\" : "/";
  const shardsFile = ctx.snap.path ? `${ctx.snap.path}${sep}ci${sep}shards.json` : null;
  let solo = null; let shardsProblem = null;
  const soloSet = () => { if (solo || shardsProblem) return solo; if (!shardsFile) { shardsProblem = "the log names no snapshot to read ci/shards.json from"; return null; } try { const d = ctx.read(shardsFile); solo = new Set(Array.isArray(d?.solo) ? d.solo.map(String) : []); } catch (e) { shardsProblem = `the commit's ci/shards.json cannot be read (${e.code ?? e.message})`; } return solo; };
  const justified = [];
  for (const [k, cs] of groups) {
    if (cs.length < 2) continue;
    const where = `${cs.length}x cargo ${k.split("\u0000")[1]} in ${path.basename(k.split("\u0000")[0])}`;
    const set = soloSet();
    if (!set) { gaps.push(`${where} were sent as separate commands; ${shardsProblem}, so the split cannot be checked`); continue; }
    const soloOnly = cs.filter((c) => { const n = cratesOf(c); return n.length === 1 && set.has(n[0]); });
    if (cs.length - soloOnly.length <= 1) justified.push(`${soloOnly.map((c) => cratesOf(c)[0]).join(", ")} solo per ci/shards.json`);
    else fails.push(`${where} were sent as separate commands, and only ${soloOnly.length} of the extra command(s) are for a crate ci/shards.json lists solo`);
  }
  // Groups that ran one after the other: whether a lane was free beside them needs the broker's capacity and each group's ready time, which no
  // record of the run carries yet; until it does, such a run is not a pass.
  const spans = [...groups.values()].map((cs) => ({ from: Math.min(...cs.map((c) => c.at)), to: Math.max(...cs.map((c) => c.at + (c.ms ?? NaN))) }));
  if (spans.some((s) => !Number.isFinite(s.from) || !Number.isFinite(s.to))) gaps.push("a compile in the process list has no start time or duration");
  else { let serial = 0; for (let i = 0; i < spans.length; i += 1) for (let j = i + 1; j < spans.length; j += 1) if (spans[i].to <= spans[j].from || spans[j].to <= spans[i].from) serial += 1; if (serial) gaps.push(`${serial} pair(s) of compile groups ran one after the other; that no lane was free beside them needs a record of the broker's capacity and each group's ready time`); }
  return decide(fails, gaps, `${compiles.length} compile command(s) in ${groups.size} group(s), one per workspace and stage${justified.length ? ` (split: ${justified.join("; ")})` : ""}, running together`);
}

function nn10({ compiles }) {
  const tests = compiles.filter((c) => c.sub === "test");
  if (!tests.length) return result(NOT_MEASURED, "this run sent no test compile to the broker");
  // The gate sends `cargo test`; the broker turns it into a compile only and signs which test programs it built (its receipt's
  // `cargo-test-compile-only` and `testExecutables`). Only that receipt shows the broker ran no test and the gate ran exactly those programs.
  return result(NOT_MEASURED, `${tests.length} test command(s) went to the broker; that it only compiled them and the gate ran the programs needs the broker's receipts, which this judge does not read yet`);
}

function nn11({ log, limitMs }) {
  const stamps = log.split(/\r?\n/).map(stampOf).filter((t) => t !== null);
  if (stamps.length < 2) return result(NOT_MEASURED, "the log is not time-stamped");
  const back = stamps.findIndex((t, i) => i > 0 && t < stamps[i - 1]);
  if (back > 0) return result(NOT_MEASURED, `the log's times go backwards (line ${back + 1} of its stamped lines), so it gives no interval to judge`);
  const took = stamps[stamps.length - 1] - stamps[0];
  if (!Number.isFinite(limitMs) || limitMs <= 0) return result(NOT_MEASURED, `the run took ${(took / 1000).toFixed(1)} s; no time limit is set for it`);
  return took <= limitMs ? result(PASS, `${(took / 1000).toFixed(1)} s, limit ${(limitMs / 1000).toFixed(1)} s`) : result(FAIL, `${(took / 1000).toFixed(1)} s, limit ${(limitMs / 1000).toFixed(1)} s`);
}

function nn12({ log, live }) {
  const snapshot = /snapshot (\S+) is commit/.exec(log)?.[1] ?? null;
  if (!live) return result(NOT_MEASURED, "a recording cannot show what was left behind; this line is judged right after a run ends (--live)");
  if (!snapshot) return result(NOT_MEASURED, "the log names no snapshot, so its lock cannot be looked for");
  if (!Array.isArray(live.pids)) return result(NOT_MEASURED, "the processes the run started are not known, so none can be shown gone");
  if (live.exists(`${snapshot}.lock`)) return result(FAIL, `the lock ${snapshot}.lock is still there after the run`);
  const alive = live.pids.filter((pid) => live.alive(pid));
  return alive.length ? result(FAIL, `${alive.length} process(es) the run started are still alive: ${alive.slice(0, 5).join(", ")}`) : result(PASS, "no lock and no process left behind");
}
const JUDGES = Object.freeze({ NN01: nn01, NN02: nn02, NN03: nn03, NN04: nn04, NN05: nn05, NN06: nn06, NN07: nn07, NN08: nn08, NN09: nn09, NN10: nn10, NN11: nn11, NN12: nn12 });
export const CHECK_IDS = Object.freeze(Object.keys(JUDGES));

/**
 * Judges one run against `lines` (readAcceptanceLines of the contract; required, and checked again here). `log`: its text. `ledger`: its
 * process list (parseLedger). `commit`: the commit selected for the run, from whoever started it. `receipt`: its receipt object or null.
 * `limitMs`: the time limit for this kind of run, or null. `exit`: its exit code, or null. `live`: { exists(path), alive(pid), pids } when
 * judged right after the run, else null. Returns { lines: [{ id, text, verdict, why }], pass, failed, notMeasured }. `pass` is true only
 * when all twelve passed.
 */
export function judgeRun({ lines, log = "", ledger = null, commit = null, receipt = null, limitMs = null, exit = null, live = null, readJson } = {}) {
  const unlinked = Array.isArray(lines) ? linkProblems(lines) : ["no lines were given: read them from the contract with readAcceptanceLines"];
  if (unlinked.length) throw Object.assign(new Error(`the contract and the acceptance test do not match: ${unlinked.join("; ")}`), { problems: unlinked });
  const read = readJson ?? ((f) => JSON.parse(fs.readFileSync(f, "utf8")));
  const text = String(log);
  const entries = Array.isArray(ledger) ? ledger : [];
  const vm = /class A verdict written: (.+?)\s*$/m.exec(text); const vLine = vm ? text.slice(0, vm.index).split(/\r?\n/).length - 1 : -1;
  let verdictDoc = null; if (vm) { try { verdictDoc = read(vm[1].trim()); } catch { verdictDoc = null; } }
  const ctx = {
    log: text, ledger: entries, ledgerGaps: ledgerGaps(ledger), commit, receipt, limitMs, exit, live,
    snap: snapshotOf(text), decisions: decisionsOf(text, { readJson: read }), coverage: coveragesOf(text, read),
    compiles: entries.map(brokerCompileOf).filter(Boolean), brokerStarts: entries.map(brokerStartOf).filter(Boolean), read,
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
  if (!logFile || !ledgerFile) { console.error("usage: acceptance-lines.mjs --log <file> --ledger <file> --commit <sha> [--contract <file>] [--receipt <file>] [--limit-ms <n>] [--exit <code>] [--json]"); process.exit(2); }
  const contractFile = opt("--contract") ?? CONTRACT_FILE; let lines;
  try { lines = readAcceptanceLines(fs.readFileSync(contractFile, "utf8")); } catch (e) { console.error(`NOT JUDGED: ${e.code === "ENOENT" ? `the contract ${contractFile} cannot be read` : e.message}`); process.exit(2); }
  const receiptFile = opt("--receipt"); let receipt = null; if (receiptFile) { try { receipt = JSON.parse(fs.readFileSync(receiptFile, "utf8")); } catch { receipt = null; } }
  const verdict = judgeRun({ lines, log: fs.readFileSync(logFile, "utf8"), ledger: parseLedger(fs.readFileSync(ledgerFile, "utf8")), commit: opt("--commit"), receipt, limitMs: opt("--limit-ms") === null ? null : Number(opt("--limit-ms")), exit: opt("--exit") === null ? null : Number(opt("--exit")) });
  if (process.argv.includes("--json")) console.log(JSON.stringify(verdict, null, 2));
  else { for (const l of verdict.lines) console.log(`${l.id} ${l.verdict.toUpperCase().padEnd(12)} ${l.text}\n      ${l.why}`); console.log(verdict.pass ? "PASS: all twelve lines" : `NOT PASSED: ${verdict.failed.length} failed, ${verdict.notMeasured.length} not measured`); }
  process.exit(verdict.pass ? 0 : 1);
}
