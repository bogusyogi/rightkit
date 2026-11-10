// Runs the test programs a test-build call produced (design section 11). They run outside the broker.
//
// Input: the `testExecutables` list of the broker's signed receipt: [{ path, sha256, package_id, target, kind }].
// A program is run only when the file on disk has the sha256 the receipt lists. Results are per crate.
import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import { spawn as nodeSpawn } from "node:child_process";
import { packageNameOf } from "./crate-results.mjs";

/** Sum of the `test result:` lines a libtest program printed; null when it printed none. */
export function libtestSummary(text) {
  const sum = { passed: 0, failed: 0, ignored: 0, lines: 0 };
  for (const line of String(text ?? "").split(/\r?\n/)) {
    const m = /^test result: (?:ok|FAILED)\. (\d+) passed; (\d+) failed; (\d+) ignored/.exec(line.trim());
    if (m) { sum.passed += Number(m[1]); sum.failed += Number(m[2]); sum.ignored += Number(m[3]); sum.lines += 1; }
  }
  return sum.lines ? sum : null;
}

const sha256File = (file) => crypto.createHash("sha256").update(fs.readFileSync(file)).digest("hex");

function runOne({ file, args, cwd, env, timeoutMs, spawn }) {
  return new Promise((resolve) => {
    let out = ""; let done = false;
    const finish = (r) => { if (!done) { done = true; resolve({ ...r, output: out.slice(-20000) }); } };
    let child;
    try { child = spawn(file, args, { cwd, env, stdio: ["ignore", "pipe", "pipe"], windowsHide: true }); }
    catch (error) { finish({ exit: null, error: String(error.message ?? error) }); return; }
    const timer = setTimeout(() => { try { child.kill(); } catch { /* gone */ } finish({ exit: null, error: `timed out after ${Math.round(timeoutMs / 1000)} s` }); }, timeoutMs);
    child.stdout.on("data", (d) => { out += d; if (out.length > 400000) out = out.slice(-200000); });
    child.stderr.on("data", (d) => { out += d; if (out.length > 400000) out = out.slice(-200000); });
    child.on("error", (error) => { clearTimeout(timer); finish({ exit: null, error: String(error.message ?? error) }); });
    child.on("close", (code, signal) => { clearTimeout(timer); finish({ exit: code, signal }); });
  });
}

/**
 * Runs every listed program and returns { byCrate: { crate: { result: "passed"|"failed"|"unknown", detail, programs, passed, failed, ignored } }, counts: { programs, ran, cached } }.
 * `executables`: the receipt list (null means the broker listed none: every crate in `crates` is unknown).
 * `crates`: [{ name, dir, quiet?: boolean, serialToo?: boolean, allowIgnored?: boolean }] the crates whose tests are owed; `dir` is the crate's directory in the snapshot.
 * `store`: run-calls.mjs fileStore or null; a passed program is remembered under its sha256 and arguments.
 * Quiet crates run last, one program at a time. `serialToo` crates run a second time with one test thread.
 */
export async function runTests({ executables, crates, env = process.env, jobs = Math.max(1, os.availableParallelism?.() ?? os.cpus().length), timeoutMs = 30 * 60 * 1000, store = null, spawn = nodeSpawn, hashOf = sha256File }) {
  const byCrate = {}; const counts = { programs: 0, ran: 0, cached: 0 };
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
    const key = `test-${exe.sha256}-${crypto.createHash("sha256").update(args.join("\0")).digest("hex").slice(0, 12)}`;
    const remembered = store?.get(key);
    if (remembered) { counts.cached += 1; runs.get(crate.name).push({ ...remembered.run, cached: true }); return; }
    let run;
    let actual = null;
    try { actual = hashOf(exe.path); } catch (error) { actual = null; run = { ok: false, program: exe.target, detail: `the listed program cannot be read (${error.message})` }; }
    if (!run && actual !== exe.sha256) run = { ok: false, program: exe.target, detail: "the program on disk is not the one the receipt lists (sha256 differs)" };
    if (!run) {
      counts.ran += 1;
      const r = await runOne({ file: exe.path, args, cwd: crate.dir, env: { ...env, CARGO_MANIFEST_DIR: crate.dir }, timeoutMs, spawn });
      const sum = libtestSummary(r.output);
      run = { ok: r.exit === 0, program: exe.target, args, passed: sum?.passed ?? 0, failed: sum?.failed ?? 0, ignored: sum?.ignored ?? 0, detail: r.exit === 0 ? "" : (r.error ?? `exit ${r.exit}${r.signal ? ` (${r.signal})` : ""}: ${r.output.slice(-1500)}`) };
      if (run.ok) store?.put(key, { run: { ok: true, program: run.program, args, passed: run.passed, failed: 0, ignored: run.ignored, detail: "" } });
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
    if (bad.length) { result = "failed"; detail = bad.map((r) => `${r.program}: ${r.detail}`).join(" | ").slice(0, 4000); }
    else if (list.length && passed === 0 && ignored > 0 && !c.allowIgnored) { result = "failed"; detail = `no test passed: ${ignored} ignored of ${ignored} (summed over ${list.length} program run(s))`; }
    byCrate[c.name] = { result, detail, programs: list.length, passed, failed, ignored };
  }
  return { byCrate, counts };
}
