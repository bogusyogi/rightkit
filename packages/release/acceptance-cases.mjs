// The twelve-line acceptance test decided by cases (GATE-CONTRACT GC-A27; rightkit#1514).
// Adrian, 2026-11-24: "Unlock the judge files; adopt the 12-line evidence table and its 4 named limits, the 10.1 reading of D20, and the 7 fixture cases + 1 real build
// as the acceptance run."
//
// A line is decided from records made outside the gate: the launcher's own record of each run (exit, times, listings of folders before and after), the operating
// system's record of the programs the gate started, and the broker's signed receipts. Where the gate's own statement is read (its output, its Class A decision record),
// that is said in the reason. Each line has one function. Every function answers pass or fail, never "not measured": evidence that is missing is a fail with its reason.
//
// The four named limits (they are the only ones):
//   1. NN03: rows that run inside the gate take milliseconds and are not judged for lanes.
//   2. NN04: the order of the forecast and the first lookup inside the gate is not witnessed; that the forecast does not come from the cache is.
//   3. NN06: a check inside the gate that was silently repeated without writing anything shows only as lost time against the case's limit.
//   4. NN10: the test program file at the instant it was started is not witnessed.
//
// Pure: `facts` in, verdicts out. Gathering the facts of a run folder is `factsOfRun` at the end; running the cases is acceptance-run.mjs.
import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import { cargoVerb, dirKey, isProbe, programName, readRunDir, runFacts, valuesOf } from "./acceptance-lines.mjs";

export const PASS = "pass"; export const FAIL = "fail";
const verdict = (fails, passWhy) => (fails.length ? { verdict: FAIL, why: fails.slice(0, 6).join("; ") + (fails.length > 6 ? `; and ${fails.length - 6} more` : "") } : { verdict: PASS, why: passWhy });
const plain = (p) => dirKey(String(p ?? "").replace(/^\\\\\?\\/, ""));
// The lines the evidence table holds every case to, whatever the case lists: every receipt names the commit (NN01), a run that reached the broker did so after
// Class A (NN02), nothing repeats (NN08), the run is inside its limit (NN11) and ends clean (NN12).
const ALWAYS = Object.freeze(["NN01", "NN02", "NN08", "NN11", "NN12"]);
export const LINE_IDS = Object.freeze(["NN01", "NN02", "NN03", "NN04", "NN05", "NN06", "NN07", "NN08", "NN09", "NN10", "NN11", "NN12", "NN13"]);
// NN13 is decided once per run by acceptance-callers.mjs (every caller of `rightkit-gate check`), not by a fixture case (rightkit#1695).
export const RUN_LINES = Object.freeze(["NN13"]);

// ---- reading a run's facts ------------------------------------------------------------------------------------------------------------------------------
/** A Class A row's own program: a tool the gate starts for a row, not a version probe. Rows that run inside the gate start none (limit 1). */
export function isClassAProgram(start) {
  const name = programName(start.program); const args = (start.args ?? []).map(String);
  if (isProbe(args)) return false;
  if (["rustfmt", "actionlint", "shellcheck", "cargo-deny", "cargo-audit", "oxlint", "tsc"].includes(name)) return true;
  const absolute = (a) => /^([a-z]:[\\/]|[\\/])/i.test(a);
  if (name === "node" && args.includes("--check")) return args.filter((a) => !a.startsWith("-")).every((a) => !absolute(a)); // a tracked file, named relative to the snapshot; never a build output
  if (name === "node") return args.some((a) => /[\\/]pnpm\.(c|m)?js$/i.test(a) && args.some((b) => ["audit", "exec"].includes(b)));
  if (name === "pnpm") return args.some((a) => ["audit", "exec"].includes(a));
  if (name === "cargo") return ["deny", "audit"].includes(cargoVerb(args));
  return false;
}
/** The workspace, verb, feature flags and targets of a broker job: what NN09 holds against the cases file's exact list. A command that names no --target builds for the host. */
export function groupOf(job, hostTarget) {
  const args = job.args ?? []; const snap = plain(job.gate?.snapshot); const cwd = plain(job.cwd);
  const workspace = snap && cwd.startsWith(snap) ? cwd.slice(snap.length).replace(/^\//, "") || "." : cwd;
  const named = valuesOf(args, "--target"); const feats = valuesOf(args, "--features", "-F");
  const features = args.includes("--all-features") ? "--all-features" : feats.length ? `--features ${feats.join(",")}` : "";
  return { workspace, verb: job.sub, targets: (named.length ? [...new Set(named)] : [hostTarget]).sort(), features, crates: valuesOf(args, "--package", "-p") };
}
const groupKey = (g) => `${g.workspace} | ${g.verb} | ${g.features || "default features"} | ${g.targets.join(" + ")}`;
/** The cases file's exact command list as keys; `prerequisite` commands (a sidecar build other commands need first) are returned apart. */
export function expectedCommands(bc) {
  const out = new Map();
  for (const c of bc.commands ?? []) { const targets = (c.targets === "each target" ? bc.targets : c.targets === "host" ? [bc.hostTarget] : [c.targets]).slice().sort(); out.set(groupKey({ workspace: c.workspace, verb: c.verb, features: c.features, targets }), c); }
  return out;
}
const jobName = (j) => `broker job ${j.jobId ?? "(no id)"}`;
const firstSubmission = (run) => Math.min(...run.jobs.map((j) => j.submitted).filter(Number.isFinite));

// ---- the checks one run can be given, whatever the case ---------------------------------------------------------------------------------------------------
/** NN01, every run: each broker receipt verifies, names the selected commit and its tree, and says the snapshot was unchanged. */
/** A job the broker refused before it started (no signed start, not a success) built nothing: a run killed under it leaves one (case 7 on 0.6.67). Skipped in the kill case only: anywhere else an unstarted refusal may be a job sent before Class A, which NN02 must see. */
export const neverRan = (j) => !Number.isFinite(j.started) && j.status !== "SUCCEEDED";
export function receiptsNameTheCommit(run, { skipNeverRan = false } = {}) {
  const fails = (run.foreignJobs ?? []).filter((x) => !(skipNeverRan && neverRan(x))).map((j) => `${jobName(j)} ran for this repository during the run but not from its snapshot of the selected commit (cwd ${j.cwd})`);
  for (const j of run.jobs.filter((x) => !(skipNeverRan && neverRan(x)))) {
    if (!j.verified) { fails.push(`${jobName(j)}: its receipt does not verify (${(j.problems ?? []).join("; ") || "no reason given"})`); continue; }
    if (j.gate?.commit !== run.commit) fails.push(`${jobName(j)} is signed for commit ${String(j.gate?.commit).slice(0, 12)}, not the selected ${String(run.commit).slice(0, 12)}`);
    if (!run.tree || j.gate?.tree !== run.tree) fails.push(`${jobName(j)} is signed for tree ${String(j.gate?.tree).slice(0, 12)}, not the selected commit's ${String(run.tree).slice(0, 12)}`);
    // In the kill case a job the kill cancelled (owner lost) never reached its post-checks, so the broker cannot sign the snapshot unchanged; its commit and
    // tree are still held above, and its outputs are never admitted. Seen when the kill landed after submission (final run, 2026-11-25).
    const killedUnder = skipNeverRan && j.status === "CANCELLED" && j.postChecksRan === false && j.reaped === true;
    if (j.gate?.snapshotUnchanged !== true && !killedUnder) fails.push(`${jobName(j)}: the broker did not sign that the snapshot was unchanged`);
  }
  return fails;
}
/** NN02, a run that reached the broker: the Class A verdict file the receipts name is older than the first signed submission, and every Class A program ended before it. */
export function classABeforeBroker(run, { skipNeverRan = false } = {}) {
  const fails = []; if (skipNeverRan) { run = { ...run, jobs: run.jobs.filter((x) => !neverRan(x)) }; if (!run.jobs.length) return []; } else if (!run.jobs.length) return ["no broker receipt exists, so the order of Class A and the first submission cannot be shown"];
  const first = firstSubmission(run); if (!Number.isFinite(first)) return ["no broker receipt signs a usable start time and waits, so the first submission time is not known"];
  // Each job against the verdict file its own receipt names: that file, as the launcher lists it after the run, must be older than the job's signed submission.
  // A file written later was not this run's verdict when the job was sent (an older run's file of the same name was what the broker read).
  for (const j of run.jobs) {
    if (!j.gate?.classA) continue; const name = path.basename(plain(j.gate.classA)); const at = run.verdicts?.[name.toLowerCase()];
    if (!Number.isFinite(at)) fails.push(`the Class A verdict file ${name} that ${jobName(j)} names is not in the launcher's listing`);
    else if (!Number.isFinite(j.submitted) || at >= j.submitted) fails.push(`${jobName(j)} (cargo ${j.sub}) was submitted at ${new Date(j.submitted).toISOString()}, before the Class A verdict file it names was written (${name}, ${new Date(at).toISOString()})`);
  }
  if (run.jobs.some((j) => !j.gate?.classA)) fails.push("a broker receipt names no Class A verdict file");
  const late = run.starts.filter(isClassAProgram).filter((s) => s.end === null || s.end > first);
  if (late.length) fails.push(`${late.length} Class A program(s) had not ended at the first submission (first: ${programName(late[0].program)})`);
  if (run.processGaps.length) fails.push(`the process record is incomplete (${run.processGaps[0]})`);
  return fails;
}
/**
 * The programs the gate itself started: children of the gate process, and the program a shell wrapper of the gate's ran (the wrapper is not counted beside it).
 * What a tool starts for itself (cargo's rustc probes, pnpm's node) is that tool's work, not a call the gate made. Approved for the earlier judge on 21 November
 * ("cargo's own rustc target-info probe [is] not [a program] the run chose to start"); the same rule, stated once for every tool.
 */
export function startedByGate(run) {
  if (!Number.isInteger(run.rootPid)) return run.starts;
  const isShell = (s) => /(^|[\\/])cmd\.exe$/i.test(String(s.exe));
  const mine = run.starts.filter((s) => s.ppid === run.rootPid);
  const shells = mine.filter(isShell);
  const wrapped = run.starts.filter((s) => shells.some((w) => w.pid === s.ppid && s.at >= w.at && (w.end === null || s.at <= w.end)));
  const withChild = new Set(wrapped.map((s) => s.ppid));
  return [...mine.filter((s) => !(isShell(s) && withChild.has(s.pid))), ...wrapped];
}
/** NN08, every run: no program starts twice with the same arguments and no broker command is sent twice, beyond the pinned legitimate repeats. */
export function repeats(run, legitimate = []) {
  const fails = []; if (run.processGaps.length) fails.push(`the process record is incomplete (${run.processGaps[0]})`);
  const groups = new Map();
  for (const s of startedByGate(run)) { if (!s.argsKnown || s.ambiguous) continue; const k = JSON.stringify([programName(s.program), s.args]); groups.set(k, (groups.get(k) ?? 0) + 1); }
  for (const [k, n] of groups) {
    if (n < 2) continue; const [name, args] = JSON.parse(k);
    const line = args.join(" ");
    const allowed = legitimate.find((l) => (l.programPattern ? new RegExp(l.programPattern).test(name) : l.program === name) && (l.argsPattern ? new RegExp(l.argsPattern).test(line) : JSON.stringify(l.args) === JSON.stringify(args)))?.count ?? 1;
    if (n > allowed) fails.push(`${name} ${args.join(" ").slice(0, 80)} started ${n} times (${allowed} allowed)`);
  }
  const sent = new Map();
  for (const j of run.jobs) { const k = `${plain(j.cwd)}\u0000${(j.argv ?? []).join("\u0000")}`; sent.set(k, (sent.get(k) ?? 0) + 1); }
  for (const [k, n] of sent) if (n > 1) fails.push(`the broker command ${k.split("\u0000").slice(1, 6).join(" ")} was sent ${n} times`);
  return fails;
}
/** NN10, a run that built tests: the broker compiled them without running them and signed each program's hash; the gate started exactly those programs, after they were built. */
export function testsBuiltByBrokerRunByGate(run) {
  const fails = []; const tests = run.jobs.filter((j) => j.sub === "test");
  if (!tests.length) return ["no broker receipt is a test build, so that the broker compiled the tests is not shown"];
  const signed = new Map();
  for (const j of tests) {
    if (!j.verified) { fails.push(`${jobName(j)}: its receipt does not verify`); continue; }
    if (!(j.effectiveArgv ?? j.argv ?? []).includes("--no-run")) fails.push(`${jobName(j)} is a cargo test the broker ran without --no-run: the broker ran the tests`);
    if (!Array.isArray(j.testExecutables)) { fails.push(`${jobName(j)} signs no list of test programs`); continue; }
    for (const exe of j.testExecutables) {
      const file = typeof exe === "string" ? exe : exe?.path; const digest = typeof exe === "object" ? exe?.sha256 : null;
      if (typeof file !== "string" || typeof digest !== "string") { fails.push(`${jobName(j)} lists a test program with no path or no hash`); continue; }
      signed.set(plain(file), { file, digest, ended: j.ended });
    }
  }
  if (!signed.size && !fails.length) fails.push("the test builds sign no test program at all, so there was nothing for the gate to run");
  for (const [key, s] of signed) {
    const ran = run.starts.filter((p) => plain(p.exe) === key);
    if (ran.length !== 1) fails.push(`${path.basename(s.file)} was started ${ran.length} times by the gate, not once`);
    if (ran.some((p) => Number.isFinite(s.ended) && p.at < s.ended)) fails.push(`${path.basename(s.file)} was started before the broker finished building it`);
    const now = run.imageNow?.[key];
    if (now !== s.digest) fails.push(`${path.basename(s.file)} does not have the signed hash after the run (${String(now).slice(0, 12)} against ${s.digest.slice(0, 12)})`);
  }
  // A test program the gate started that no receipt signs was not built by the broker (cargo puts test programs under deps/).
  const unsigned = run.starts.filter((p) => /[\\/]deps[\\/][^\\/]+-[0-9a-f]{16}(\.exe)?$/i.test(String(p.exe)) && !signed.has(plain(p.exe)));
  if (unsigned.length) fails.push(`the gate started ${unsigned.length} test program(s) no receipt signs (first: ${path.basename(unsigned[0].exe)})`);
  if (run.processGaps.length) fails.push(`the process record is incomplete (${run.processGaps[0]})`);
  return fails;
}
/** NN11, every run: it finished inside the limit pinned for it. */
/**
 * The time the broker held this run: the union of its jobs' [submitted, ended] spans inside the run. The broker is shared with every other build on the host,
 * so how long a job waits or runs there is not the gate's time (Adrian, 2026-11-25, NN11 option b: the same work took 10.6 s and 81.4 s of compile).
 */
export function brokerHeldMs(run) {
  const from = Number.isFinite(run.startMs) ? run.startMs : -Infinity; const to = Number.isFinite(run.startMs) && Number.isFinite(run.durationMs) ? run.startMs + run.durationMs : Infinity;
  const spans = (run.jobs ?? []).filter((j) => Number.isFinite(j.submitted) && Number.isFinite(j.ended)).map((j) => [Math.max(j.submitted, from), Math.min(j.ended, to)]).filter(([a, b]) => b > a).sort((x, y) => x[0] - y[0]);
  let total = 0; let cur = null;
  for (const [a, b] of spans) { if (!cur || a > cur[1]) { if (cur) total += cur[1] - cur[0]; cur = [a, b]; } else cur[1] = Math.max(cur[1], b); }
  return cur ? total + cur[1] - cur[0] : 0;
}
/** NN11 limits the gate's own time: the run's wall time less the time the broker held it. Both are printed. */
export function insideLimit(run, limitMs, label) {
  if (!Number.isFinite(limitMs)) return [`no time limit is pinned for ${label} (the gate's own time x 1.25, at least + 15 s, goes into the cases file)`];
  if (!Number.isFinite(run.durationMs)) return [`${label}: the launcher did not record when the run ended`];
  const broker = brokerHeldMs(run); const own = run.durationMs - broker;
  return own <= limitMs ? [] : [`${label} spent ${Math.round(own / 1000)} s of its own (wall ${Math.round(run.durationMs / 1000)} s, broker ${Math.round(broker / 1000)} s); its limit is ${Math.round(limitMs / 1000)} s`];
}
/** NN12, a run that was not killed: no lock of its snapshot and no program of it is left. */
export function endsClean(run, label) {
  const fails = [];
  if (run.rootAlive) fails.push(`${label}: the gate process was still alive when the launcher looked`);
  if (run.locksKnown !== true) fails.push(`${label}: the launcher could not read the snapshot folder for locks`);
  if (!run.snapshot) fails.push(`${label}: which snapshot the run used is not shown, so its lock cannot be looked for`);
  else if ((run.locksAfter ?? []).some((l) => plain(l) === `${plain(run.snapshot)}.lock`)) fails.push(`${label}: the snapshot lock was left behind`);
  const open = run.starts.filter((s) => s.end === null);
  if (open.length) fails.push(`${label}: ${open.length} program(s) of the run have no recorded exit (first: ${programName(open[0].program)})`);
  if (run.processGaps.length) fails.push(`${label}: the process record is incomplete (${run.processGaps[0]})`);
  return fails;
}
const exited = (run) => Number.isInteger(run.exit) && !run.killed;
const failedRun = (run, label) => (exited(run) && run.exit !== 0 ? [] : [`${label} did not end with a failing exit (exit ${run.exit}${run.killed ? ", killed" : ""})`]);
// A failing run counts only when the gate failed it for the case's own reason: any non-zero exit (a crash at start, a usage error) passed before
// (review 2026-11-25 #3). The row must be in the gate's own verdict line, `verdict deterministic failed=<ids>`.
const failedOn = (run, row, label) => {
  const f = failedRun(run, label); if (f.length || !row) return f;
  const m = /verdict deterministic failed=(\S+)/.exec(String(run.log ?? ""));
  return m && m[1].split(",").includes(row) ? [] : [`${label} failed, but not on ${row}: the gate's verdict line ${m ? `names ${m[1]}` : "is missing"}`];
};
const passedRun = (run, label) => (exited(run) && run.exit === 0 ? [] : [`${label} did not pass (exit ${run.exit}${run.killed ? ", killed" : ""})`]);
const noReceipt = (run, label) => (run.jobs.length ? [`${label} left ${run.jobs.length} broker receipt(s)`] : []);
const noClassAProgram = (run, label) => { const a = run.starts.filter(isClassAProgram); return [...(a.length ? [`${label} started ${a.length} Class A program(s) (first: ${programName(a[0].program)})`] : []), ...(run.processGaps.length ? [`${label}: the process record is incomplete (${run.processGaps[0]})`] : [])]; };
const noCacheWrite = (run, label) => (!Array.isArray(run.cacheWritten) ? [`${label}: the launcher could not list the scan store`] : run.cacheWritten.length ? [`${label} wrote ${run.cacheWritten.length} scan store file(s)`] : []);

// ---- the twelve lines, each by the cases that decide it ---------------------------------------------------------------------------------------------------
const LINES = {
  // Only the selected commit is built.
  NN01(c, cases) {
    const fails = c.runs.flatMap((r) => receiptsNameTheCommit(r, { skipNeverRan: c.id === 7 }));
    if (c.id === 1) fails.push(...passedRun(c.runs[0], "the run with an error planted in the checkout that the commit does not hold"));
    if (c.id === 5) c.runs.forEach((r, i) => fails.push(...failedOn(r, c.expect?.outputNames?.[0], `run ${i + 1} of the commit that holds the error, with a clean checkout`)));
    return verdict(fails, c.id === 1 ? "an error in the checkout that is not in the commit did not fail the run, and every receipt names the selected commit and tree with the snapshot unchanged" : c.id === 5 ? "an error in the commit failed the run with a clean checkout" : `${c.runs.reduce((n, r) => n + r.jobs.length, 0)} receipt(s), each naming the selected commit and tree with the snapshot unchanged`);
  },
  // Class A finishes before anything goes to the broker. If it fails, nothing goes.
  NN02(c) {
    if (c.id === 5) return verdict(c.runs.flatMap((r, i) => [...failedOn(r, c.expect?.outputNames?.[0], `run ${i + 1}`), ...noReceipt(r, `run ${i + 1}`)]), "a failing Class A check ended the run with a failing exit and no broker receipt, twice");
    return verdict(c.runs.filter((r) => r.jobs.length).flatMap((r) => classABeforeBroker(r, { skipNeverRan: c.id === 7 })), "the Class A verdict file is older than the first signed submission, and every Class A program had ended by then");
  },
  // Class A starts ready, independent work whenever an eligible lane is free.
  NN03(c, cases) {
    const run = c.runs[0]; const fails = []; const lanes = run.lanes;
    if (!Number.isInteger(lanes) || lanes < 1) return verdict(["the launcher recorded no lane count"], "");
    if (run.processGaps.length) fails.push(`the process record is incomplete (${run.processGaps[0]})`);
    const progs = run.starts.filter(isClassAProgram).sort((a, b) => a.at - b.at);
    const rows = Object.values(cases.classA?.programRows ?? {}).flat().length;
    if (progs.length < 2) return verdict([...fails, `${progs.length} Class A program(s) started in a run from nothing; the cases file lists ${rows} rows that start one`], "");
    const runningAt = (t) => progs.filter((p) => p.at <= t && (p.end === null || p.end > t)).length;
    const peak = Math.max(...progs.map((p) => runningAt(p.at)));
    const need = Math.min(lanes, progs.length);
    // A later program is a row that was still waiting at the earlier start. If a lane was free after that earlier start and the waiting row started more than
    // `slackMs` later, a row waited beside an idle lane.
    const slackMs = cases.nn03SlackMs ?? 2000; let idle = 0; let worst = null;
    for (let i = 1; i < progs.length; i += 1) {
      const gap = progs[i].at - progs[i - 1].at; const free = runningAt(progs[i - 1].at) < lanes;
      if (free && gap > slackMs) { idle += 1; if (!worst || gap > worst.gap) worst = { gap, name: programName(progs[i].program) }; }
    }
    if (idle) fails.push(`${idle} Class A program(s) started more than ${slackMs} ms after the one before while a lane was free (longest: ${programName(worst.name)} after ${worst.gap} ms)`);
    if (peak < Math.min(need, 2)) fails.push(`at most ${peak} Class A program(s) ran at a time on ${lanes} lanes`);
    return verdict(fails, `${progs.length} Class A programs on ${lanes} lanes, up to ${peak} at a time; none waited more than ${slackMs} ms beside a free lane`);
  },
  // Before the first cache lookup, the gate records the expected rows and which will be reused and which will run.
  NN04(c, cases) {
    const run = c.runs[0]; const row = c.expect?.outputNames?.[0] ?? "license"; const fails = [...failedRun(run, "the run with one cache file deleted")];
    const recs = run.decisions ?? [];
    if (!recs.length) fails.push("the gate wrote no Class A decision record for the run, so no forecast is on record");
    const f = recs.flatMap((d) => (Array.isArray(d.forecast) ? d.forecast : [])).filter((x) => x.id === row);
    if (recs.length && !f.length) fails.push(`the decision record's forecast does not hold the row ${row}`);
    else if (f.some((x) => x.expected !== "hit")) fails.push(`with its cache file deleted, the forecast expected ${row} to ${f.find((x) => x.expected !== "hit").expected}: the forecast came from what is in the cache`);
    return verdict(fails, `the gate's own decision record expected ${row} to be reused although its cache file was gone, and the run stopped: the forecast does not come from the cache`);
  },
  // If lookup disagrees with that record, the gate stops before sending work and names the row and the cause. Otherwise the work that ran matches it.
  NN05(c, cases) {
    const run = c.runs[0];
    if (c.id === 4) {
      const row = c.expect?.outputNames?.[0] ?? "license";
      const fails = [...failedRun(run, "the run with one cache file deleted"), ...noReceipt(run, "it"), ...noClassAProgram(run, "it")];
      // The gate's own stop record for the row, not any line that mentions it beside a likely word (review 2026-11-25 #3).
      const stops = String(run.log ?? "").split(/\r?\n/).map((l) => /RK_CACHE_DIVERGENCE (\{.*\})/.exec(l)).filter(Boolean).map((m) => { try { return JSON.parse(m[1]); } catch { return null; } });
      const stop = stops.find((d) => d?.id === row);
      if (!stop) fails.push(`the gate did not stop with RK_CACHE_DIVERGENCE naming the row ${row}`);
      else if (stop.expected !== "hit" || !stop.reason) fails.push(`the gate's stop for ${row} expected ${stop.expected} with reason ${stop.reason ?? "none"}`);
      return verdict(fails, `the run stopped with a failing exit, no receipt and no Class A program, and its output names ${row} and the cause`);
    }
    return verdict(workMatches(c, cases), "the broker commands and the scan store files written are exactly the rows the cases file expects");
  },
  // A row whose inputs did not change is never run again, including one that failed before.
  NN06(c) {
    if (c.id === 2) { const r = c.runs[0]; return verdict([...passedRun(r, "the second run of the same commit"), ...noReceipt(r, "it"), ...noClassAProgram(r, "it"), ...noCacheWrite(r, "it")], "the same commit again: no broker receipt, no Class A program, no scan store file written"); }
    const r = c.runs[1]; if (!r) return verdict(["the failing commit was not run a second time"], "");
    return verdict([...failedRun(r, "the second run of the failing commit"), ...noReceipt(r, "it"), ...noClassAProgram(r, "it"), ...noCacheWrite(r, "it")], "the failing commit again: it still fails, with no Class A program started and nothing written");
  },
  // A row whose inputs did change is always run again, and does only the work the change needs.
  NN07(c, cases) {
    const run = c.runs[0]; const fails = [...passedRun(run, "the run of the one-file edit"), ...workMatches(c, cases)];
    const want = new Set(c.expect?.classAPrograms ?? []); const got = new Set(run.starts.filter(isClassAProgram).map((s) => programName(s.program)));
    for (const p of want) if (!got.has(p)) fails.push(`the Class A program ${p} did not run although its input changed`);
    for (const p of got) if (!want.has(p)) fails.push(`the Class A program ${p} ran although no input of it changed`);
    return verdict(fails, `the edit ran exactly ${[...want].join(", ")} in Class A and sent the broker only ${(c.expect?.brokerCrates ?? []).join(", ")}`);
  },
  // No repeat calls: nothing is started twice in one run.
  NN08(c, cases) { return verdict(c.runs.flatMap((r) => repeats(r, cases.legitimateRepeats ?? [])), `${c.runs.reduce((n, r) => n + r.starts.length, 0)} program start(s) and ${c.runs.reduce((n, r) => n + r.jobs.length, 0)} broker command(s), none repeated beyond the pinned list`); },
  // Compatible compiles are combined per workspace and stage. Every split has a reason the check can verify. Independent groups run at the same time when there is capacity.
  NN09(c, cases) {
    const run = c.runs[0]; const fails = [...passedRun(run, "the run from nothing")]; const bc = cases.brokerCommands ?? {};
    const expected = expectedCommands(bc); const seen = new Map(); const prerequisite = new Set();
    for (const j of run.jobs) {
      if (!j.verified) { fails.push(`${jobName(j)}: its receipt does not verify`); continue; }
      const k = groupKey(groupOf(j, bc.hostTarget)); seen.set(k, (seen.get(k) ?? 0) + 1);
      if (expected.get(k)?.why === "the sidecar binary") prerequisite.add(j);
    }
    for (const k of expected.keys()) if (!seen.has(k)) fails.push(`no broker command for ${k}`);
    for (const [k, n] of seen) {
      if (!expected.has(k)) fails.push(`a broker command for ${k}, which the cases file does not list`);
      else if (n > 1) fails.push(`${n} broker commands for ${k}: the cases file lists it once`);
    }
    // Early submission. The broker runs one build of a repository at a time and owns the order; what the gate owes is not to hold a command back. A command
    // submitted when every earlier command of the run had already ended was held until then: the gate waited for a job before sending the next. The sidecar
    // build is the one command others need first, so it is left out on both sides.
    const rest = run.jobs.filter((j) => !prerequisite.has(j));
    const byTime = rest.filter((j) => Number.isFinite(j.submitted) && Number.isFinite(j.ended)).sort((a, b) => a.submitted - b.submitted);
    if (byTime.length !== rest.length) fails.push("a broker receipt signs no usable submission or end time");
    let lastEnd = -Infinity; const held = [];
    byTime.forEach((j, i) => { if (i > 0 && j.submitted >= lastEnd) held.push(j); lastEnd = Math.max(lastEnd, j.ended); });
    if (held.length) fails.push(`${held.length} of ${byTime.length - 1} later command(s) were submitted only after every earlier one had ended (first: cargo ${held[0].sub} in ${groupOf(held[0], bc.hostTarget).workspace} at ${new Date(held[0].submitted).toISOString()}): the gate waited for a job before sending the next`);
    return verdict(fails, `${run.jobs.length} broker commands, exactly the cases file's list, each sent once, none held back until earlier jobs had ended`);
  },
  // Tests are compiled by the broker and run by the gate.
  NN10(c) {
    if (c.id === 6) { const run = c.runs[0]; const name = c.expect?.outputNames?.[0]; return verdict([...failedRun(run, "the commit with a failing unit test"), ...(name && !String(run.log ?? "").includes(name) ? [`the gate's output does not name the failing test ${name}`] : [])], "a failing unit test failed the run, and the output names it"); }
    return verdict([...passedRun(c.runs[0], "the run"), ...testsBuiltByBrokerRunByGate(c.runs[0])], "the broker compiled the tests without running them and signed each program; the gate started exactly those programs, once each, and each still has its signed hash");
  },
  // The run finishes inside its time limit.
  NN11(c, cases) { const limit = cases.limitsMs?.[c.limitKey ?? `case${c.id}`]; return verdict(c.runs.filter((r) => !r.killed).flatMap((r, i) => insideLimit(r, limit, `${c.label ?? `case ${c.id}`} run ${i + 1}`)), `every run finished inside ${Math.round((limit ?? 0) / 1000)} s`); },
  // The run ends clean, even if killed: the next ordinary run starts safely by itself.
  NN12(c) {
    const fails = [];
    if (c.id === 7) {
      const [first, second] = c.runs;
      if (!first?.killed) fails.push("the first run was not killed midway (it had ended before the kill, or no kill was sent)");
      else if (first.exit === 0) fails.push("the first run is marked killed but ended with exit 0: it finished before the kill landed");
      if (!second) fails.push("no ordinary run followed the killed one");
      else fails.push(...passedRun(second, "the ordinary run after the kill"), ...endsClean(second, "the ordinary run after the kill"));
      return verdict(fails, "the run was killed midway; the next ordinary run started by itself, passed, and left no lock and no program");
    }
    c.runs.forEach((r, i) => { if (!r.killed) fails.push(...endsClean(r, `run ${i + 1}`)); });
    return verdict(fails, "no lock and no program was left behind");
  },
};
/** Case 3: the broker commands and the scan store files written equal what the cases file expects for the edit. */
function workMatches(c, cases) {
  const run = c.runs[0]; const e = c.expect ?? {}; const fails = []; const bc = cases.brokerCommands ?? {};
  const wantCrates = new Set(e.brokerCrates ?? []); const want = e.brokerCommands ?? {};
  const counts = { clippy: 0, test: 0, doc: 0, check: 0 };
  for (const j of run.jobs) {
    if (!j.verified) { fails.push(`${jobName(j)}: its receipt does not verify`); continue; }
    const g = groupOf(j, bc.hostTarget);
    if (g.workspace !== want.workspace) fails.push(`${jobName(j)} ran in workspace ${g.workspace}; only ${want.workspace} holds the edit`);
    for (const crate of g.crates) if (!wantCrates.has(crate)) fails.push(`${jobName(j)} names ${crate}, which the edit does not need`);
    if (!g.crates.length) fails.push(`${jobName(j)} names no crate, so it built the whole workspace`);
    if (g.verb in counts) counts[g.verb] += 1; else fails.push(`${jobName(j)} is a cargo ${g.verb}, which the cases file does not expect`);
  }
  delete counts.check;
  for (const v of Object.keys(counts)) if (counts[v] !== (want[v] ?? 0)) fails.push(`${counts[v]} ${v} command(s) for the edit; the cases file expects ${want[v] ?? 0}`);
  if (!Array.isArray(run.cacheWritten)) fails.push("the launcher could not list the scan store");
  else {
    const expected = new Set([...(e.classARows ?? []), ...(e.compileRows ?? [])]);
    const index = new Map([...expected].map((id) => [crypto.createHash("sha256").update(`rk-row-index/1\0${id}`).digest("hex"), id]));
    const written = new Set();
    // A combined command stores one result under its own id (clippy:batch:<hash>) and the planner stores which targets it combined (feature-partition-targets);
    // each names its crates, and stands for the expected rows of that verb and those crates. A rule verdict of the new commit names no row and is expected.
    const verbOf = (id) => (/^test:rust/.test(id) ? "test" : String(id).split(":")[0]); const crateOf = (id) => String(id).split(":").pop().split("@")[0];
    for (const f of run.cacheWritten) {
      const id = f.row ?? index.get(path.basename(f.name, ".json")) ?? null;
      if (/^rule-verdicts\//.test(String(f.name).replaceAll("\\", "/"))) continue;
      if (id === null) { fails.push(`the scan store file ${f.name} was written and names no row`); continue; }
      const combined = /^([a-z]+):batch:/.exec(id)?.[1] ?? (/^feature-partition/.test(id) ? "plan" : null);
      if (combined) {
        const extra = (f.crates ?? []).filter((x) => !wantCrates.has(x));
        if (!Array.isArray(f.crates) || !f.crates.length) fails.push(`the combined result ${id} was written and the launcher could not read which crates it names`);
        else if (extra.length) fails.push(`the combined result ${id} names ${extra.join(", ")}, which the edit does not touch`);
        else if (combined !== "plan") for (const e of expected) if (verbOf(e) === combined && f.crates.includes(crateOf(e))) written.add(e);
        continue;
      }
      if (!expected.has(id)) fails.push(`a result for ${id} was written, a row the edit does not touch`); else written.add(id);
    }
    for (const id of expected) if (!written.has(id)) fails.push(`no result was written for ${id}, a row the edit changes`);
  }
  return fails;
}

/** The lines one case is judged on: the ones it lists, plus the ones the evidence table says every case is held to. */
export const linesOf = (caseDef) => LINE_IDS.filter((id) => (caseDef.decides ?? []).includes(id) || ALWAYS.includes(id));
/** One case: { id, decides, expect, runs: [facts] } -> { NNxx: { verdict, why } } for its lines. A check that throws is a fail. */
export function decideCase(c, cases) {
  const out = {};
  for (const id of linesOf(c)) { try { out[id] = LINES[id](c, cases); } catch (e) { out[id] = { verdict: FAIL, why: `the check itself failed: ${e.message}` }; } }
  return out;
}
/** The real build: judged on the lines that need no expected rows. */
export function decideRealBuild(run, cases) {
  const c = { id: "real", label: "the real build", limitKey: "realBuild", runs: [run] }; const out = {};
  const checks = {
    NN01: () => verdict([...passedRun(run, "the real build"), ...(run.jobs.length ? [] : ["the real build left no broker receipt"]), ...receiptsNameTheCommit(run)], "every receipt names the selected commit and tree with the snapshot unchanged"),
    NN02: () => verdict(classABeforeBroker(run), "the Class A verdict file is older than the first signed submission, and every Class A program had ended by then"),
    NN08: () => LINES.NN08(c, cases), NN10: () => verdict(testsBuiltByBrokerRunByGate(run), "tests compiled by the broker, run by the gate"), NN11: () => LINES.NN11(c, cases), NN12: () => LINES.NN12(c, cases),
  };
  for (const id of cases.realBuild?.lines ?? Object.keys(checks)) { try { out[id] = checks[id](); } catch (e) { out[id] = { verdict: FAIL, why: `the check itself failed: ${e.message}` }; } }
  return out;
}
/**
 * The counting rule: a line passes when every case that is judged on it passes it, and every line must be judged by at least one case. Twelve of twelve is all twelve
 * passing by that rule and the real build passing its lines. A case that was not run fails every line it lists.
 */
export function decideAll({ cases, results, realBuild = null, callers = null }) {
  const lines = LINE_IDS.map((id) => {
    if (RUN_LINES.includes(id)) return { id, verdict: callers?.verdict === PASS ? PASS : FAIL, why: callers?.why ?? "the callers of rightkit-gate check were not run", cases: [] };
    const from = [];
    for (const def of cases.cases) { if (!linesOf(def).includes(id)) continue; const r = results[def.id]?.[id]; from.push({ case: def.id, verdict: r?.verdict ?? FAIL, why: r?.why ?? "the case was not run" }); }
    const failed = from.filter((f) => f.verdict !== PASS);
    return { id, verdict: from.length && !failed.length ? PASS : FAIL, why: !from.length ? "no case decides this line" : failed.length ? failed.map((f) => `case ${f.case}: ${f.why}`).join(" | ") : from.map((f) => `case ${f.case}: ${f.why}`).join(" | "), cases: from };
  });
  const real = (cases.realBuild?.lines ?? []).map((id) => ({ id, verdict: realBuild?.[id]?.verdict ?? FAIL, why: realBuild?.[id]?.why ?? "the real build was not run" }));
  const passed = lines.filter((l) => l.verdict === PASS).length;
  return { lines, realBuild: real, passed, of: LINE_IDS.length, pass: passed === LINE_IDS.length && real.every((l) => l.verdict === PASS) };
}

// ---- gathering the facts of one run folder ------------------------------------------------------------------------------------------------------------------
/** The facts of a launched run, from its folder (acceptance-launch.mjs) and the broker's receipts. `killed` is the caller's: the launcher knows a kill only as a signal. */
export function factsOfRun(dir, { repo, receiptsDir, verify = null, killed = false, imageNow = null } = {}) {
  const read = readRunDir(dir, { repo, receiptsDir, verify }); const f = runFacts(read); const launch = read.launch ?? {};
  const json = (p) => { try { return JSON.parse(fs.readFileSync(p, "utf8")); } catch { return null; } };
  const sideBefore = json(path.join(dir, "before", "side-folders.json")); const sideAfter = json(path.join(dir, "side-folders-after.json"));
  const verdicts = {}; for (const [name, v] of Object.entries(sideAfter?.verdicts ?? {})) verdicts[String(name).toLowerCase()] = v[1];
  const newDecisions = Object.keys(sideAfter?.decisions ?? {}).filter((n) => !sideBefore?.decisions?.[n]);
  const decisions = newDecisions.map((n) => json(path.join(sideAfter.decisionDir, n))).filter((d) => d && (!d.commit || d.commit === launch.commit));
  let log = ""; try { log = fs.readFileSync(path.join(dir, "gate.log"), "utf8"); } catch { log = ""; }
  const start = Date.parse(launch.startedAt); const end = Date.parse(launch.endedAt);
  const inRun = f.jobs.filter((j) => !Number.isFinite(j.submitted) || ((!Number.isFinite(start) || j.submitted >= start - 1000) && (!Number.isFinite(end) || j.submitted <= end + 1000)));
  // This run's jobs: the ones signed for its commit, or run in its snapshot (another repository's builds share the broker and the receipts folder).
  const logSnap = snapshotIn(log) ? plain(snapshotIn(log)) : null;
  const mine = inRun.filter((j) => j.gate?.commit === launch.commit || (logSnap !== null && (plain(j.gate?.snapshot) === logSnap || plain(j.cwd).startsWith(logSnap))));
  const snapshot = mine.map((j) => j.gate?.snapshot).find(Boolean) ?? snapshotIn(log);
  // Jobs of THIS repository in the run's window that are not this run's: built from the working checkout, or in the snapshot under another commit. They
  // were dropped before NN01 looked (review 2026-11-25 #7). Other repositories' jobs share the broker under normal load and are not counted (Gemini, 2026-11-25).
  const repoKey = repo ? plain(repo) : null;
  const foreignJobs = inRun.filter((j) => !mine.includes(j) && ((repoKey && plain(j.cwd).startsWith(repoKey)) || (logSnap !== null && (plain(j.gate?.snapshot) === logSnap || plain(j.cwd).startsWith(logSnap)))));
  return {
    foreignJobs,
    dir, startMs: Number.isFinite(start) ? start : NaN, commit: launch.commit ?? null, tree: read.git?.tree ?? null, exit: launch.exit ?? null, signal: launch.signal ?? null, killed: killed || !!launch.signal,
    durationMs: Number.isFinite(start) && Number.isFinite(end) ? end - start : NaN, lanes: launch.lanes ?? null, log,
    jobs: mine, starts: f.starts, rootPid: f.root?.pid ?? null, processGaps: f.gaps, cacheWritten: json(path.join(dir, "cache-written.json")), verdicts, decisions,
    locksAfter: launch.afterRun?.locks ?? [], locksKnown: launch.afterRun?.locksKnown === true, rootAlive: launch.afterRun?.rootAlive === true, snapshot, imageNow,
  };
}
const snapshotIn = (log) => /snapshot (\S+?) is commit [0-9a-f]{40}/.exec(String(log))?.[1] ?? null;
