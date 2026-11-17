// Runs the test programs a test-build call produced (design section 11). They run outside the broker.
//
// Input: the `testExecutables` list of the broker's signed receipt: [{ path, sha256, package_id, target, kind }].
// A program is run only when the file on disk has the sha256 the receipt lists. Results are per crate.
import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import { spawn as nodeSpawn, spawnSync } from "node:child_process";
import { packageNameOf } from "./crate-results.mjs";
import { testKeyEnv, testProgramEnv } from "../test-env.mjs";
import { recordLockChild } from "../snapshot.mjs";

/**
 * Reads a test program's output as it arrives and counts what the standard harness says, over the WHOLE stream: nothing is decided from a kept
 * tail, so an ignored or failed count, or a run that started and never reported, cannot disappear behind later output. One scanner per stream
 * (stdout and stderr interleave by chunk, not by line). `push(chunk)` takes text in any chunking; `stats()` closes the last line and returns
 * { started, results, unread, failedWord, passed, failed, ignored }.
 */
export function libtestScanner() {
  const st = { started: 0, results: 0, unread: 0, failedWord: false, passed: 0, failed: 0, ignored: 0 };
  let rest = "";
  const line = (raw) => {
    const text = raw.trim();
    if (/^running \d+ tests?$/.test(text)) { st.started += 1; return; }
    if (!text.startsWith("test result:")) return;
    st.results += 1;
    const m = /^test result: (ok|FAILED)\. (\d+) passed; (\d+) failed; (\d+) ignored/.exec(text);
    if (!m) { st.unread += 1; return; }
    if (m[1] === "FAILED") st.failedWord = true;
    st.passed += Number(m[2]); st.failed += Number(m[3]); st.ignored += Number(m[4]);
  };
  return {
    push(chunk) {
      rest += String(chunk);
      let at;
      while ((at = rest.indexOf("\n")) >= 0) { line(rest.slice(0, at)); rest = rest.slice(at + 1); }
      // A line with no end yet: the patterns are at the start of a line, so its first 4096 characters are all that is ever needed.
      if (rest.length > 8192) rest = rest.slice(0, 4096);
    },
    stats() { if (rest) { line(rest); rest = ""; } return { ...st }; },
  };
}
const scanText = (text) => { const sc = libtestScanner(); sc.push(String(text ?? "")); return sc.stats(); };
const addStats = (a, b) => ({ started: a.started + b.started, results: a.results + b.results, unread: a.unread + b.unread, failedWord: a.failedWord || b.failedWord, passed: a.passed + b.passed, failed: a.failed + b.failed, ignored: a.ignored + b.ignored });

/** Sum of the `test result:` lines a libtest program printed; null when it printed none. Takes the output text or a scanner's stats. */
export function libtestSummary(textOrStats) {
  const st = typeof textOrStats === "object" && textOrStats !== null ? textOrStats : scanText(textOrStats);
  const lines = st.results - st.unread;
  return lines ? { passed: st.passed, failed: st.failed, ignored: st.ignored, lines } : null;
}

/**
 * Why a program that exited 0 still cannot be counted as passed, from what it printed (the output text or a scanner's stats); null when
 * nothing contradicts the exit code.
 *   - a `test result:` line that cannot be read (its counts are unknown, so they are not taken as zero);
 *   - more `running N tests` headers than `test result:` lines (a run started and never reported);
 *   - a summary that says FAILED or reports failed tests.
 * A program that prints neither a header nor a result line (a custom harness, or one with nothing to say) is judged by its exit code alone, and a
 * libtest run of zero tests prints its own "0 passed" line: both still qualify.
 */
export function libtestProblem(textOrStats) {
  const st = typeof textOrStats === "object" && textOrStats !== null ? textOrStats : scanText(textOrStats);
  if (st.unread) return `${st.unread} "test result:" line(s) could not be read, so the counts are not known`;
  if (st.failedWord || st.failed) return `its summary reports ${st.failed} failed test(s)${st.failedWord ? " (FAILED)" : ""} although it exited 0`;
  if (st.started > st.results) return `${st.started} test run(s) started and ${st.results} reported a result: the output ended before the summary`;
  return null;
}

const sha256File = (file) => crypto.createHash("sha256").update(fs.readFileSync(file)).digest("hex");

/**
 * Asks the system to stop `child` and everything it started: taskkill /T /F on Windows (bounded to `killMs`), the child's own process
 * group elsewhere (it is spawned detached there). Returns { ok, detail }: ok is true only when the request was accepted or the target was already gone.
 */
export function stopTree(child, { platform = process.platform, run = spawnSync, kill = (pid, sig) => process.kill(pid, sig), killMs = 10000 } = {}) {
  if (!child?.pid) return { ok: false, detail: "the program has no process id" };
  if (platform === "win32") {
    let r;
    try { r = run("taskkill", ["/T", "/F", "/PID", String(child.pid)], { windowsHide: true, stdio: "ignore", timeout: killMs }); } catch (error) { return { ok: false, detail: `taskkill could not be started (${error.message})` }; }
    if (r?.error) return { ok: false, detail: `taskkill did not finish (${r.error.code ?? r.error.message})` };
    return r?.status === 0 || r?.status === 128 ? { ok: true, detail: "" } : { ok: false, detail: `taskkill exited ${r?.status}` };
  }
  try { kill(-child.pid, "SIGKILL"); return { ok: true, detail: "" }; }
  catch (error) { return error?.code === "ESRCH" ? { ok: true, detail: "" } : { ok: false, detail: `the process group could not be signalled (${error?.code ?? error?.message})` }; }
}

/**
 * Whether the process group `child` leads is gone. POSIX: true only when signalling the group answers "no such process".
 * That is not proof for the whole tree: a process it started can leave the group (setsid, setpgid) and nothing here follows it.
 * So no answer of this function lets a timed-out program count as stopped; it only says how far the stop is known to have got.
 * Windows: null, always. The end of the program and a clean taskkill do not prove that a process it started (one that left the tree) is gone,
 * and nothing here can ask. The gate's lock-holder job (snapshot.mjs) is what proves it later, when the lock is reclaimed.
 */
export function treeGone(child, { platform = process.platform, kill = (pid, sig) => process.kill(pid, sig) } = {}) {
  if (platform === "win32") return null;
  try { kill(-child.pid, 0); return false; } catch (error) { return error?.code === "ESRCH"; }
}

/**
 * Runs one program. On a timeout its process tree is asked to stop and the result waits (up to `reapMs`) for the group to go.
 * A timed-out program is always `unreaped: true`: on no host can this code prove that nothing it started is left (a process can
 * leave the group on POSIX and the tree on Windows). The same when its record beside the snapshot lock cannot be written.
 */
function runOne({ file, args, cwd, env, timeoutMs, spawn, platform = process.platform, stop = stopTree, gone = treeGone, record = recordLockChild, reapMs = 15000, pollMs = 100 }) {
  return new Promise((resolve) => {
    let out = ""; let done = false; let timedOut = false; let closed = false; let poll = null;
    // `output` is the kept tail, for a person to read. `summary` is counted over everything the program wrote.
    const scanOut = libtestScanner(); const scanErr = libtestScanner();
    const finish = (r) => { if (!done) { done = true; clearInterval(poll); resolve({ ...r, output: out.slice(-20000), summary: addStats(scanOut.stats(), scanErr.stats()) }); } };
    const late = `timed out after ${Math.round(timeoutMs / 1000)} s`;
    let child;
    try { child = spawn(file, args, { cwd, env, stdio: ["ignore", "pipe", "pipe"], windowsHide: true, ...(platform === "win32" ? {} : { detached: true }) }); }
    catch (error) { finish({ exit: null, error: String(error.message ?? error) }); return; }
    // A detached group no longer dies with the gate: it is recorded beside the held snapshot lock, so a crashed gate's leftover keeps the lock (snapshot.mjs).
    // If that record cannot be written the program is running unrecorded: it is asked to stop, and it is unreaped whatever the stop answers.
    try { record(child, { platform }); }
    catch (error) { const asked = stop(child, { platform }); finish({ exit: null, unreaped: true, error: `${String(error.message ?? error)}; it is not proven stopped${asked.ok ? "" : ` (${asked.detail})`}` }); return; }
    const timer = setTimeout(() => {
      timedOut = true;
      const deadline = Date.now() + reapMs; // the clock starts before the stop request, so a slow taskkill is inside it
      const asked = stop(child, { platform });
      const unproven = (why) => finish({ exit: null, unreaped: true, error: `${late}; what it started is not proven stopped (${why})` });
      const look = () => {
        if (!asked.ok) { unproven(asked.detail); return; }
        const empty = gone(child, { platform });
        if (empty === true) { unproven("its process group is gone, but a process that left the group cannot be ruled out"); return; }
        if (empty === null && closed) { unproven("the program ended, and Windows gives no proof for the processes it started"); return; }
        if (Date.now() >= deadline) unproven(empty === null ? `the program had not ended ${Math.round(reapMs / 1000)} s after the stop` : `its process group still answered ${Math.round(reapMs / 1000)} s after the stop`);
      };
      poll = setInterval(look, pollMs); look();
    }, timeoutMs);
    child.stdout.on("data", (d) => { scanOut.push(d); out += d; if (out.length > 400000) out = out.slice(-200000); });
    child.stderr.on("data", (d) => { scanErr.push(d); out += d; if (out.length > 400000) out = out.slice(-200000); });
    child.on("error", (error) => { clearTimeout(timer); closed = true; if (!timedOut) finish({ exit: null, error: String(error.message ?? error) }); });
    child.on("close", (code, signal) => { clearTimeout(timer); closed = true; if (!timedOut) finish({ exit: code, signal }); });
  });
}

/**
 * How many test programs run at once: the host's cores minus 2, at least 1. The same rule as the per-crate rows (deterministic-scans.mjs
 * testRunConcurrency; ADR-001 C2, kept by Adrian 2026-11-02; a test keeps the two equal). Before this the build-together path started one
 * program per core, and each program runs its own tests on as many threads as there are cores.
 */
export function testProgramsAtOnce({ cpus = os.cpus().length } = {}) { return Math.max(1, cpus - 2); }

/**
 * Runs every listed program and returns { byCrate: { crate: { result: "passed"|"failed"|"unknown", detail, programs, passed, failed, ignored } }, counts: { programs, ran, cached, unreaped } }.
 * `executables`: the receipt list (null means the broker listed none: every crate in `crates` is unknown).
 * `crates`: [{ name, dir, quiet?: boolean, serialToo?: boolean, allowIgnored?: boolean, assetGated?: boolean }] the crates whose tests are owed; `dir` is the crate's directory in the snapshot.
 * `store`: run-calls.mjs fileStore or null; a passed program is remembered under `scope`, its crate, its sha256, its arguments and the
 *   whole environment it is given (test-env.mjs testKeyEnv, which carries the asset rule). `scope` is required with a store: the caller's
 *   hash of commit, toolchain, host, broker and gate code. A pass is therefore never reused on another commit: the files a test reads at
 *   run time, the binaries it starts and the crates it depends on are all fixed by the commit, and nothing narrower is known here.
 * Each program runs in testProgramEnv (test-env.mjs): no credential-named variable, assets required unless the crate is `assetGated`.
 * A program past `timeoutMs` is asked to stop as a tree and is always counted in `unreaped`; after that no further program starts.
 * Quiet crates run last, one program at a time. `serialToo` crates run a second time with one test thread.
 */
export async function runTests({ executables, crates, env = process.env, jobs = testProgramsAtOnce(), timeoutMs = 30 * 60 * 1000, store = null, scope = null, spawn = nodeSpawn, hashOf = sha256File, platform = process.platform, stop = stopTree, gone = treeGone, record = recordLockChild, reapMs = 15000, pollMs = 100 }) {
  if (store && (typeof scope !== "string" || !scope)) throw new TypeError("runTests: a store needs a scope; a test pass cannot be remembered without the commit and gate it belongs to");
  const byCrate = {}; const counts = { programs: 0, ran: 0, cached: 0, unreaped: 0 };
  if (!Array.isArray(executables)) {
    for (const c of crates) byCrate[c.name] = { result: "unknown", detail: "the test build's receipt listed no test programs", programs: 0, passed: 0, failed: 0, ignored: 0 };
    return { byCrate, counts };
  }
  const owed = new Map(crates.map((c) => [c.name, c]));
  const work = [];
  for (const exe of executables) {
    const crate = owed.get(packageNameOf(exe.package_id));
    if (!crate) continue;
    work.push({ exe, crate, args: [] });
    if (crate.serialToo) work.push({ exe, crate, args: ["--test-threads=1"] });
  }
  counts.programs = work.length;
  const runs = new Map(crates.map((c) => [c.name, []]));
  const one = async ({ exe, crate, args }) => {
    const programEnv = { ...testProgramEnv(env, crate.assetGated === true), CARGO_MANIFEST_DIR: crate.dir };
    const key = `test-${crypto.createHash("sha256").update(JSON.stringify({ v: 2, scope, crate: crate.name, program: exe.sha256, args, env: testKeyEnv(programEnv) })).digest("hex")}`;
    // After a program that is not proven stopped, no further program is started: it may still be using the shared build folder.
    if (counts.unreaped) { runs.get(crate.name).push({ ok: false, skipped: true, program: exe.target, detail: "not run: an earlier test program is not proven stopped" }); return; }
    const remembered = store?.get(key);
    if (remembered) { counts.cached += 1; runs.get(crate.name).push({ ...remembered.run, cached: true }); return; }
    let run;
    let actual = null;
    try { actual = hashOf(exe.path); } catch (error) { actual = null; run = { ok: false, program: exe.target, detail: `the listed program cannot be read (${error.message})` }; }
    if (!run && actual !== exe.sha256) run = { ok: false, program: exe.target, detail: "the program on disk is not the one the receipt lists (sha256 differs)" };
    if (!run) {
      counts.ran += 1;
      const r = await runOne({ file: exe.path, args, cwd: crate.dir, env: programEnv, timeoutMs, spawn, platform, stop, gone, record, reapMs, pollMs });
      if (r.unreaped) counts.unreaped += 1;
      const sum = libtestSummary(r.summary);
      // Exit 0 is not enough: what the program printed must not contradict it (libtestProblem). Only a run with no such contradiction is remembered.
      const contradiction = r.exit === 0 ? libtestProblem(r.summary) : null;
      run = { ok: r.exit === 0 && !contradiction, program: exe.target, args, passed: sum?.passed ?? 0, failed: sum?.failed ?? 0, ignored: sum?.ignored ?? 0, detail: r.exit !== 0 ? (r.error ?? `exit ${r.exit}${r.signal ? ` (${r.signal})` : ""}: ${r.output.slice(-1500)}`) : contradiction ? `exited 0 but ${contradiction}` : "" };
      if (run.ok) store?.put(key, { run: { ok: true, program: run.program, args, passed: run.passed, failed: run.failed, ignored: run.ignored, detail: "" } });
    }
    runs.get(crate.name).push(run);
  };
  // Loud crates in parallel, then quiet crates one program at a time.
  const loud = work.filter((w) => !w.crate.quiet); const quiet = work.filter((w) => w.crate.quiet);
  let next = 0;
  await Promise.all(Array.from({ length: Math.min(jobs, loud.length) }, async () => { while (next < loud.length) { const w = loud[next]; next += 1; await one(w); } }));
  for (const w of quiet) await one(w);
  for (const c of crates) {
    const list = runs.get(c.name);
    const passed = list.reduce((n, r) => n + (r.passed ?? 0), 0); const failed = list.reduce((n, r) => n + (r.failed ?? 0), 0); const ignored = list.reduce((n, r) => n + (r.ignored ?? 0), 0);
    const bad = list.filter((r) => !r.ok);
    let result = "passed"; let detail = "";
    if (bad.length && bad.every((r) => r.skipped)) { result = "unknown"; detail = bad[0].detail; }
    else if (bad.length) { result = "failed"; detail = bad.map((r) => `${r.program}: ${r.detail}`).join(" | ").slice(0, 4000); }
    // GATE-CONTRACT GC-A08: an ignored test blocks its unit unless the unit is named (ci/gate.config.json rust.<crate>.allowIgnored, a written reason),
    // whatever else passed. A unit with no test at all, and none ignored, qualifies. The per-crate rows judge the same way (local-publish.mjs judgeUnit).
    else if (ignored > 0 && !c.allowIgnored) { result = "failed"; detail = `${ignored} test(s) ignored and this crate is not named as allowed to ignore tests (${passed} passed; summed over ${list.length} program run(s))`; }
    byCrate[c.name] = { result, detail, programs: list.length, passed, failed, ignored };
  }
  return { byCrate, counts };
}
