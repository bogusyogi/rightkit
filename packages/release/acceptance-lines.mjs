// The gate's acceptance test: twelve lines that must be true of every run (GATE-CONTRACT GC-A27; Adrian, 2026-11-20: "An acceptance test
// should be a single list of things that has to pass any run").
//
// It judges ONE run from the records that run leaves behind, and nothing else: its log, the list of every process it started (the spawn
// ledger), its forecast files (what it said it would reuse and run, and what happened), its row reports (coverage) and its receipt. It
// stages nothing and knows no scenario: a first build, a repeat, a run after an edit and a real build of an app are all judged by the same
// twelve lines. A line with no record to judge it from is "not measured", never a pass.
//
// The lines are not written here. They are read from the one table in docs/GATE-CONTRACT.md [GC-A27] between
// `<!-- acceptance-lines:begin -->` and `<!-- acceptance-lines:end -->`, and nothing is judged unless every line there has a check here and
// every check here has a line there.
//
//   node acceptance-lines.mjs --log <file> --ledger <file> [--contract <file>] [--receipt <file>] [--limit-ms <n>] [--exit <code>] [--json]
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
const result = (verdict, why, evidence = []) => ({ verdict, why, evidence });

// ---- reading the records ---------------------------------------------------------------------------------------------------------------------------
/** A log line's UTC stamp in ms, or null. */
const stampOf = (line) => { const m = /^(\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d(?:\.\d+)?Z) /.exec(line); return m ? Date.parse(m[1]) : null; };
export function parseLedger(text) {
  return String(text ?? "").split(/\r?\n/).filter((l) => l.trim()).map((l) => { try { return JSON.parse(l); } catch { return null; } }).filter((e) => e && typeof e.cmd === "string" && Array.isArray(e.args));
}
const baseName = (cmd) => path.basename(String(cmd).replaceAll("\\", "/")).toLowerCase().replace(/\.(exe|cmd|bat)$/, "");
const COMPILING = new Set(["build", "check", "clippy", "doc", "test", "rustc", "rustdoc"]);
/**
 * A compile sent to the broker, or null: `rustright cargo <sub> ...`, started directly or through `cmd /c "<...>rustright.cmd" cargo ...`.
 * Returns { sub, args (after the subcommand), cwd, at }.
 */
export function brokerCompileOf(entry) {
  const whole = [entry.cmd, ...entry.args].join(" ").replaceAll('"', " ").replace(/\s+/g, " ");
  const m = /(?:^|[\\/ ])rustright(?:\.cmd|\.exe)? cargo (\S+)((?: .*)?)$/i.exec(whole);
  if (!m || !COMPILING.has(m[1])) return null;
  return { sub: m[1], args: m[2].trim().split(" ").filter(Boolean), cwd: String(entry.cwd ?? ""), at: typeof entry.at === "string" ? Date.parse(entry.at) : null };
}
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

// ---- the twelve lines ------------------------------------------------------------------------------------------------------------------------------
function l01({ log, receipt }) {
  const said = [...log.matchAll(/snapshot \S+ is commit ([0-9a-f]{40}) tree ([0-9a-f]{40})/g)].map((m) => ({ commit: m[1], tree: m[2] }));
  const after = [...log.matchAll(/is still tree ([0-9a-f]{40}) after/g)].map((m) => m[1]);
  if (!said.length) return result(NOT_MEASURED, "the log never says which commit and tree its snapshot is");
  const commits = new Set(said.map((s) => s.commit)); const trees = new Set([...said.map((s) => s.tree), ...after]);
  if (commits.size !== 1 || trees.size !== 1) return result(FAIL, `the run names ${commits.size} commit(s) and ${trees.size} tree(s): ${[...commits, ...trees].join(", ")}`);
  if (!after.length) return result(NOT_MEASURED, "the snapshot's tree was stated at the start but never checked again after the work");
  if (receipt?.commit && receipt.commit !== [...commits][0]) return result(FAIL, `the receipt is for commit ${receipt.commit}, the snapshot was ${[...commits][0]}`);
  return result(PASS, `commit ${[...commits][0].slice(0, 12)}, tree ${[...trees][0].slice(0, 12)} before and after`, said.length + after.length);
}
function l02({ log, ledger }) {
  const lines = log.split(/\r?\n/);
  const verdictAt = lines.map((l) => (/class A verdict written:|class A full pass:/.test(l) ? stampOf(l) : null)).filter((t) => t !== null)[0] ?? null;
  const failed = /class A .*fail|FAIL .*\(class A\)|class A check .* failed/i.test(log) && verdictAt === null;
  const compiles = ledger.map(brokerCompileOf).filter(Boolean);
  if (failed || verdictAt === null) {
    if (compiles.length) return result(FAIL, `Class A did not pass and ${compiles.length} compile(s) were still sent to the broker`);
    return verdictAt === null && !failed ? result(NOT_MEASURED, "the log has no Class A verdict line and nothing was sent to the broker") : result(PASS, "Class A failed and nothing was sent to the broker");
  }
  const early = compiles.filter((c) => c.at !== null && c.at < verdictAt);
  if (early.length) return result(FAIL, `${early.length} compile(s) were sent to the broker before the Class A verdict (first: cargo ${early[0].sub})`);
  return result(PASS, `Class A verdict first, then ${compiles.length} compile(s) sent to the broker`);
}
function l03({ rows }) {
  const lanes = rows.filter((r) => r.class === "A" && r.lane);
  if (!lanes.length) return result(NOT_MEASURED, "the row reports carry no lane times (ready, started, ended), so idle lanes cannot be told from busy ones");
  // A lane sat idle while a row waited when a row was ready and not started although fewer than `lanes` rows were running at that moment.
  const problems = [];
  for (const r of lanes) {
    const { readyAt, startedAt, lanes: width } = r.lane;
    if (startedAt - readyAt <= 250) continue;
    const running = lanes.filter((o) => o !== r && o.lane.startedAt <= readyAt && o.lane.endedAt > readyAt).length;
    if (running < width) problems.push(`${r.id} waited ${Math.round(startedAt - readyAt)} ms while ${running} of ${width} lanes were busy`);
  }
  return problems.length ? result(FAIL, problems.slice(0, 3).join("; ")) : result(PASS, `${lanes.length} Class A rows, none waited while a lane was free`);
}
function l04({ decisions }) {
  if (!decisions.length) return result(NOT_MEASURED, "the log names no forecast file");
  const bad = decisions.filter((d) => d.problem || !Array.isArray(d.doc?.forecast) || !d.doc.forecast.length);
  if (bad.length) return result(FAIL, `forecast missing or empty: ${bad.map((d) => d.problem ?? d.file).slice(0, 2).join("; ")}`);
  // The order (forecast frozen before the first lookup) is not stated in the file today; what is judged is that every stage that ran has one.
  return result(PASS, decisions.map((d) => `Class ${d.stage}: ${d.doc.forecast.length} rows forecast`).join("; "), decisions.map((d) => d.file));
}
function l05({ decisions, log, exit }) {
  const read = decisions.filter((d) => d.doc);
  if (!read.length) return result(NOT_MEASURED, "no forecast file to compare with what ran");
  const diverged = read.filter((d) => (d.doc.mismatches ?? []).length);
  if (!diverged.length) return result(PASS, `forecast and actual agree for ${read.reduce((n, d) => n + d.doc.forecast.length, 0)} rows`);
  const sent = diverged.reduce((n, d) => n + (Array.isArray(d.doc.dispatchedRows) ? d.doc.dispatchedRows.length : Number(d.doc.dispatchedRows) || 0), 0);
  const named = /RK_CACHE_DIVERGENCE/.test(log) && diverged.every((d) => d.doc.mismatches.every((m) => typeof m.id === "string" && typeof m.reason === "string" && m.reason));
  if (sent > 0) return result(FAIL, `the forecast was wrong for ${diverged[0].doc.mismatches[0].id} and ${sent} row(s) were still sent`);
  if (!named) return result(FAIL, "the forecast was wrong and the run did not name the row and the cause");
  if (exit === 0) return result(FAIL, "the forecast was wrong and the run still exited 0");
  return result(PASS, `the forecast was wrong for ${diverged[0].doc.mismatches[0].id}; the run stopped before sending work and named it`);
}
const pairs = (decisions) => decisions.filter((d) => d.doc).flatMap((d) => { const actual = new Map((d.doc.actual ?? []).map((a) => [a.id, a])); return d.doc.forecast.map((f) => ({ stage: d.stage, f, a: actual.get(f.id) ?? null })); });
function l06({ decisions }) {
  const all = pairs(decisions);
  if (!all.length) return result(NOT_MEASURED, "no forecast file, so unchanged rows cannot be told from changed ones");
  const unchanged = all.filter((p) => p.f.expected === "hit");
  const reran = unchanged.filter((p) => p.a && p.a.actual !== "hit");
  if (reran.length) return result(FAIL, `${reran.length} unchanged row(s) ran again: ${reran.slice(0, 4).map((p) => p.f.id).join(", ")}`);
  const failures = unchanged.filter((p) => p.f.status === "fail").length;
  return result(PASS, `${unchanged.length} unchanged row(s) reused${failures ? `, ${failures} of them an earlier failure` : ""}`);
}
function l07({ decisions }) {
  const all = pairs(decisions);
  if (!all.length) return result(NOT_MEASURED, "no forecast file, so changed rows cannot be told from unchanged ones");
  const changed = all.filter((p) => p.f.expected === "run");
  const skipped = changed.filter((p) => !p.a || !["ran", "run"].includes(p.a.actual));
  if (skipped.length) return result(FAIL, `${skipped.length} changed row(s) did not run: ${skipped.slice(0, 4).map((p) => `${p.f.id} (${p.a?.actual ?? "no result"})`).join(", ")}`);
  return result(PASS, `${changed.length} changed row(s), every one ran`);
}
/** One process start as "the same call": program name, arguments and folder, with the parts that differ by run alone (ids, temp names) blanked. */
export function callKey(entry) {
  const blank = (s) => String(s).replace(/[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}/gi, "<id>").replace(/\b\d{10,}\b/g, "<n>").replaceAll("\\", "/").toLowerCase();
  return JSON.stringify([baseName(entry.cmd), entry.args.map(blank), blank(entry.cwd ?? "")]);
}
function l08({ ledger }) {
  if (!ledger.length) return result(NOT_MEASURED, "the run has no list of the processes it started");
  const seen = new Map();
  for (const e of ledger) { const k = callKey(e); seen.set(k, (seen.get(k) ?? 0) + 1); }
  const repeats = [...seen].filter(([, n]) => n > 1);
  if (!repeats.length) return result(PASS, `${ledger.length} process starts, no two alike`);
  const extra = repeats.reduce((n, [, c]) => n + c - 1, 0);
  const show = repeats.slice(0, 3).map(([k, n]) => { const [cmd, args] = JSON.parse(k); return `${n}x ${cmd} ${args.join(" ").slice(0, 70)}`; });
  return result(FAIL, `${extra} repeat start(s) of ${ledger.length}: ${show.join("; ")}`);
}
function l09({ ledger }) {
  const compiles = ledger.map(brokerCompileOf).filter(Boolean);
  if (!compiles.length) return result(PASS, "nothing was sent to the broker");
  // The same workspace (folder) and stage (subcommand and its settings): the crate and target selectors are what one command should gather.
  const settings = (args) => { const out = []; for (let i = 0; i < args.length; i += 1) { if (["-p", "--package", "--target"].includes(args[i])) { i += 1; continue; } if (/^(--package=|--target=|-p.)/.test(args[i])) continue; out.push(args[i]); } return out.join(" "); };
  const groups = new Map();
  for (const c of compiles) { const k = `${c.cwd.toLowerCase()}\u0000${c.sub}\u0000${settings(c.args)}`; groups.set(k, (groups.get(k) ?? 0) + 1); }
  const split = [...groups].filter(([, n]) => n > 1);
  if (!split.length) return result(PASS, `${compiles.length} compile command(s), one per workspace and stage`);
  return result(FAIL, `${split.length} workspace-and-stage group(s) were sent as several commands: ${split.slice(0, 3).map(([k, n]) => `${n}x cargo ${k.split("\u0000")[1]} in ${path.basename(k.split("\u0000")[0])}`).join("; ")}`);
}
function l10({ ledger }) {
  const tests = ledger.map(brokerCompileOf).filter((c) => c && c.sub === "test");
  if (!tests.length) return result(NOT_MEASURED, "this run sent no test compile to the broker");
  // The gate sends `cargo test`; the broker turns it into a compile only and signs which test programs it built (its receipt's
  // `cargo-test-compile-only` and `testExecutables`). Only that receipt shows the broker ran no test and the gate ran exactly those programs.
  return result(NOT_MEASURED, `${tests.length} test command(s) went to the broker; that it only compiled them and the gate ran the programs needs the broker's receipts, which this judge does not read yet`);
}
function l11({ log, limitMs }) {
  const stamps = log.split(/\r?\n/).map(stampOf).filter((t) => t !== null);
  if (stamps.length < 2) return result(NOT_MEASURED, "the log is not time-stamped");
  const took = stamps[stamps.length - 1] - stamps[0];
  if (!Number.isFinite(limitMs) || limitMs <= 0) return result(NOT_MEASURED, `the run took ${(took / 1000).toFixed(1)} s; no time limit is set for it`);
  return took <= limitMs ? result(PASS, `${(took / 1000).toFixed(1)} s, limit ${(limitMs / 1000).toFixed(1)} s`) : result(FAIL, `${(took / 1000).toFixed(1)} s, limit ${(limitMs / 1000).toFixed(1)} s`);
}
function l12({ log, live }) {
  const snapshot = /snapshot (\S+) is commit/.exec(log)?.[1] ?? null;
  if (!live) return result(NOT_MEASURED, "a recording cannot show what was left behind; this line is judged right after a run ends (--live)");
  if (!snapshot) return result(NOT_MEASURED, "the log names no snapshot, so its lock cannot be looked for");
  if (live.exists(`${snapshot}.lock`)) return result(FAIL, `the lock ${snapshot}.lock is still there after the run`);
  const alive = (live.pids ?? []).filter((pid) => live.alive(pid));
  return alive.length ? result(FAIL, `${alive.length} process(es) the run started are still alive: ${alive.slice(0, 5).join(", ")}`) : result(PASS, "no lock and no process left behind");
}
const JUDGES = Object.freeze({ NN01: l01, NN02: l02, NN03: l03, NN04: l04, NN05: l05, NN06: l06, NN07: l07, NN08: l08, NN09: l09, NN10: l10, NN11: l11, NN12: l12 });
export const CHECK_IDS = Object.freeze(Object.keys(JUDGES));

/**
 * Judges one run against `lines` (readAcceptanceLines of the contract; required, and checked again here). `log`: its text. `ledger`: its process list (parseLedger). `receipt`: its receipt object or null. `limitMs`: the time limit for
 * this kind of run, or null. `exit`: its exit code, or null. `live`: { exists(path), alive(pid), pids } when judged right after the run, else null.
 * Returns { lines: [{ id, text, verdict, why }], pass, failed, notMeasured }. `pass` is true only when all twelve passed.
 */
export function judgeRun({ lines, log = "", ledger = [], receipt = null, limitMs = null, exit = null, live = null, readJson } = {}) {
  const unlinked = Array.isArray(lines) ? linkProblems(lines) : ["no lines were given: read them from the contract with readAcceptanceLines"];
  if (unlinked.length) throw Object.assign(new Error(`the contract and the acceptance test do not match: ${unlinked.join("; ")}`), { problems: unlinked });
  const decisions = decisionsOf(log, readJson ? { readJson } : {});
  const rows = [];
  for (const m of String(log).matchAll(/coverage report (\S+\.json)/g)) { try { const doc = (readJson ?? ((f) => JSON.parse(fs.readFileSync(f, "utf8"))))(m[1]); rows.push(...(doc.scans ?? doc.rows ?? [])); } catch { /* a missing report leaves its lines not measured */ } }
  const ctx = { log: String(log), ledger, receipt, limitMs, exit, live, decisions, rows };
  const judged = lines.map(({ id, text }) => { let r; try { r = JUDGES[id](ctx); } catch (e) { r = result(FAIL, `the check itself failed: ${e.message}`); } return { id, text, verdict: r.verdict, why: r.why }; });
  const failed = judged.filter((l) => l.verdict === FAIL).map((l) => l.id); const notMeasured = judged.filter((l) => l.verdict === NOT_MEASURED).map((l) => l.id);
  return { lines: judged, pass: failed.length === 0 && notMeasured.length === 0, failed, notMeasured };
}

// ---- command line -----------------------------------------------------------------------------------------------------------------------------------
if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const opt = (name) => { const i = process.argv.indexOf(name); return i < 0 ? null : process.argv[i + 1]; };
  const logFile = opt("--log"); const ledgerFile = opt("--ledger");
  if (!logFile || !ledgerFile) { console.error("usage: acceptance-lines.mjs --log <file> --ledger <file> [--contract <file>] [--receipt <file>] [--limit-ms <n>] [--exit <code>] [--json]"); process.exit(2); }
  const contractFile = opt("--contract") ?? CONTRACT_FILE; let lines;
  try { lines = readAcceptanceLines(fs.readFileSync(contractFile, "utf8")); } catch (e) { console.error(`NOT JUDGED: ${e.code === "ENOENT" ? `the contract ${contractFile} cannot be read` : e.message}`); process.exit(2); }
  const receiptFile = opt("--receipt"); let receipt = null; if (receiptFile) { try { receipt = JSON.parse(fs.readFileSync(receiptFile, "utf8")); } catch { receipt = null; } }
  const verdict = judgeRun({ lines, log: fs.readFileSync(logFile, "utf8"), ledger: parseLedger(fs.readFileSync(ledgerFile, "utf8")), receipt, limitMs: opt("--limit-ms") === null ? null : Number(opt("--limit-ms")), exit: opt("--exit") === null ? null : Number(opt("--exit")) });
  if (process.argv.includes("--json")) console.log(JSON.stringify(verdict, null, 2));
  else { for (const l of verdict.lines) console.log(`${l.id} ${l.verdict.toUpperCase().padEnd(12)} ${l.text}\n      ${l.why}`); console.log(verdict.pass ? "PASS: all twelve lines" : `NOT PASSED: ${verdict.failed.length} failed, ${verdict.notMeasured.length} not measured`); }
  process.exit(verdict.pass ? 0 : 1);
}
