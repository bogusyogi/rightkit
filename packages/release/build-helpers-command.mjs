#!/usr/bin/env node
// `right-release build-helpers check [--manifest-path <Cargo.toml>]` (Class A, rightkit#1450) and
// `right-release build-helpers trace [--manifest-path <Cargo.toml>]` (the one-time trace: eslogger on macOS (sudo + Full Disk Access), ETW on Windows (Administrator)).
// Only broker hosts (Mac, Dell) vet build helpers (Adrian, 2026-11-03: "linux is the wrong target and it has to be only where we have brokers"):
// a Linux host or CI runner is a throwaway container, so check and trace do nothing there.
//
// check: reads the workspace's resolved graph (`cargo metadata --format-version 1 --locked`, a non-compiling call run directly: GATE-CONTRACT
// GC-A11), its Cargo.lock, the host triple and toolchain (`rustc -vV`), and the host list, and refuses (exit 1) when any build-helper unit is
// untraced or traced as writing outside its build directories, naming each.
// trace: for each untraced unit, builds the workspace's dependency alone (`cargo build -p <crate>@<version> --locked`) under the host's tracer, into a
// scratch target, judges every write against OUT_DIR / target / CARGO_HOME / temp, and records the verdict in the host list.
// Units are listed per target the host compiles: its own and the two cross-check targets (GC-A14), each from `cargo metadata --filter-platform`.
// A unit that does not build on this host for its target is recorded once as unbuildable and skipped (Adrian, 2026-11-03: "if it's not compiling
// on the OS, and doesn't generate output ... why not skip it?"): the gate's own scan cannot build it there either.
import { spawn, spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { buildHelperRefusal, buildHelperUnits, checkBuildHelpers, hostListPath, esloggerWritesByRoot, etwWritesByRoot, judgeTrace, mergeLists, readHostList, recordTrace, repoListPath } from "./build-helper-trace.mjs";
import { crossCompileEnv, crossTargetsFor } from "./cross-targets.mjs";
import { resolveManagedRoots } from "./managed-roots.mjs";
import { directCargoPath } from "./managed-cargo.mjs";

function hostFacts(run = spawnSync) {
  const r = run("rustc", ["-vV"], { encoding: "utf8" });
  if (r.error || r.status !== 0) throw new Error(`rustc -vV failed (${r.error?.message ?? `exit ${r.status}`})`);
  const host = /^host: (\S+)$/m.exec(r.stdout)?.[1];
  const release = /^release: (\S+)$/m.exec(r.stdout)?.[1];
  const commit = /^commit-hash: (\S+)$/m.exec(r.stdout)?.[1];
  if (!host || !release) throw new Error("rustc -vV printed no host or release");
  return { target: host, toolchain: `rustc ${release}${commit ? ` (${commit.slice(0, 9)})` : ""}` };
}

// Runs the trace builds, `argv[3]` at a time, starting the next whenever one finishes: argv[1] = jobs JSON [{cmd, args, env}], argv[2] = results
// file [{pid, status, error}] in job order.
const BATCH_RUNNER = `const fs = require("node:fs"); const { spawn } = require("node:child_process");
const jobs = JSON.parse(fs.readFileSync(process.argv[1], "utf8")); const limit = Math.max(1, Number(process.argv[3]) || 4);
const results = new Array(jobs.length); let next = 0; let done = 0;
const start = (i) => new Promise((resolve) => {
  const j = jobs[i]; let child;
  try { child = spawn(j.cmd, j.args, { env: j.env, stdio: ["ignore", "ignore", "pipe"] }); } catch (e) { results[i] = { status: -1, error: e.message }; resolve(); return; }
  let tail = ""; child.stderr.on("data", (d) => { tail = (tail + d).slice(-2000); });
  const finish = (r) => { if (results[i]) return; results[i] = r; done += 1; process.stdout.write("  [" + done + "/" + jobs.length + "] " + j.label + (r.status === 0 ? " built" : " failed") + "\\n"); resolve(); };
  child.on("error", (e) => finish({ pid: child.pid, status: -1, error: e.message }));
  child.on("close", (status) => finish({ pid: child.pid, status, ...(status ? { error: tail.trim().split("\\n").slice(-2).join(" | ") } : {}) }));
});
const worker = async () => { while (next < jobs.length) { const i = next++; await start(i); } };
Promise.all(Array.from({ length: Math.min(limit, jobs.length) }, worker)).then(() => fs.writeFileSync(process.argv[2], JSON.stringify(results)));`;

export function inventory({ manifestPath, run = spawnSync, readFile = (f) => fs.readFileSync(f, "utf8"), platform = process.platform }) {
  const host = hostFacts(run);
  // The host's own target plus the two targets every crate is cross-checked for: each target's resolved graph and features.
  const targets = [host.target, ...crossTargetsFor({}, platform)];
  let metadata = null; const units = [];
  for (const target of targets) {
    const args = ["metadata", "--format-version", "1", "--locked", "--filter-platform", target, ...(manifestPath ? ["--manifest-path", manifestPath] : [])];
    const r = run("cargo", args, { encoding: "utf8", maxBuffer: 1 << 28 });
    if (r.error || r.status !== 0) throw new Error(`cargo ${args.join(" ")} failed (${r.error?.message ?? `exit ${r.status}`}): ${String(r.stderr ?? "").trim().split(/\r?\n/).slice(-2).join(" | ")}`);
    const m = JSON.parse(r.stdout); metadata ??= m;
    units.push(...buildHelperUnits(m, readFile(path.join(m.workspace_root, "Cargo.lock")), { target, toolchain: host.toolchain }));
  }
  return { metadata, units, host: host.target };
}

// Every Cargo workspace (a directory holding Cargo.lock) under `root`, skipping build outputs, dependencies and vendored copies.
export function cargoWorkspaces(root, { depth = 4, fsApi = fs } = {}) {
  const SKIP = new Set(["target", "node_modules", ".build", ".git", "vendor", "t", "dist"]);
  const found = [];
  const walk = (dir, left) => {
    let entries; try { entries = fsApi.readdirSync(dir, { withFileTypes: true }); } catch { return; }
    if (entries.some((e) => e.isFile() && e.name === "Cargo.lock") && entries.some((e) => e.isFile() && e.name === "Cargo.toml")) found.push(dir);
    if (left > 0) for (const e of entries) if (e.isDirectory() && !SKIP.has(e.name) && !e.name.startsWith(".")) walk(path.join(dir, e.name), left - 1);
  };
  walk(root, depth);
  // Only repositories the gate builds: the workspace's repository root (nearest .git) carries a .rightkit directory.
  // A gate snapshot of a commit (<managed root>/snapshots/<key>) has .rightkit but no .git: the first ancestor with either decides.
  const gated = (dir) => { for (let d = dir; ; d = path.dirname(d)) { if (fsApi.existsSync(path.join(d, ".rightkit"))) return true; if (fsApi.existsSync(path.join(d, ".git"))) return false; if (path.dirname(d) === d) return false; } };
  return found.filter(gated).sort();
}

// `right-release build-helpers trace-all [--root <dir>]...`: one admin prompt, every workspace. On macOS it asks for the password once
// (`sudo -v`) and keeps that credential alive while it runs; each workspace's untraced units are traced and recorded in the host list and
// in the workspace's own committed list (--record-repo). Default root: /Volumes/D/claude (Mac), D:\claude (Windows), the current
// directory elsewhere.
function traceAll(argv, { env, platform, log, err }) {
  // A unit is judged once per run: a verdict recorded in one workspace (pass, fail or unbuildable) is never rebuilt in the next.
  const judged = new Set();
  const roots = argv.flatMap((a, i) => (a === "--root" && argv[i + 1] ? [path.resolve(argv[i + 1])] : []));
  if (!roots.length) roots.push(platform === "darwin" ? "/Volumes/D/claude" : platform === "win32" ? "D:\\claude" : process.cwd());
  let keepAlive = null;
  if (platform === "darwin") {
    log("right-release build-helpers trace-all: eslogger needs administrator rights (and Full Disk Access for this terminal); enter your password once.");
    const ok = spawnSync("sudo", ["-v"], { stdio: "inherit" });
    if (ok.status !== 0) { err("right-release build-helpers trace-all: sudo was not granted; nothing traced"); return 2; }
    keepAlive = spawn("sh", ["-c", `while kill -0 ${process.pid} 2>/dev/null; do sudo -n -v; sleep 50; done`], { stdio: "ignore", detached: true });
    keepAlive.unref();
  }
  const workspaces = roots.flatMap((r) => cargoWorkspaces(r));
  log(`right-release build-helpers trace-all: ${workspaces.length} Cargo workspace(s) under ${roots.join(", ")}`);
  let worst = 0;
  try {
    for (const ws of workspaces) {
      log(`--- ${ws}`);
      const jobsAt = argv.indexOf("--jobs");
      // The host list is what Class A reads on this host; a repository's committed list is written only on request (--record-repo), so
      // tracing never edits a checkout another owner holds.
      const code = main(["trace", "--manifest-path", path.join(ws, "Cargo.toml"), ...(argv.includes("--record-repo") ? ["--record-repo"] : []), ...(jobsAt >= 0 ? ["--jobs", argv[jobsAt + 1]] : [])], { env, platform, log, err, judged });
      if (code !== 0) err(`right-release build-helpers trace-all: ${ws} ended with ${code}`);
      worst = Math.max(worst, code);
    }
  } finally { try { keepAlive?.kill(); } catch { /* already gone */ } }
  log(`right-release build-helpers trace-all: done${argv.includes("--record-repo") ? "; commit each workspace's .rightkit/build-helper-trace.json" : ""}`);
  return worst;
}

export function main(argv = process.argv.slice(2), { env = process.env, platform = process.platform, run: runRaw = null, log = console.log, err = console.error, judged = new Set() } = {}) {
  if (argv[0] === "trace-all") return traceAll(argv.slice(1), { env, platform, log, err });
  // GATE-CONTRACT GC-A11: the check's cargo and rustc calls compile nothing and run directly with the real toolchain (RustRight shims off PATH),
  // writing to a scratch target, never through the broker.
  const pathKey = platform === "win32" ? (Object.keys(env).find((k) => k.toUpperCase() === "PATH") ?? "Path") : "PATH";
  const directPath = directCargoPath({ env: { ...env, PATH: env[pathKey] }, platform, gateWrappers: true }).path;
  const scratchTarget = fs.mkdtempSync(path.join(os.tmpdir(), "rk-bh-target-"));
  const run = runRaw ?? ((cmd, args, opts = {}) => spawnSync(cmd, args, { ...opts, env: { ...env, ...(opts.env ?? {}), [pathKey]: directPath, CARGO_TARGET_DIR: opts.env?.CARGO_TARGET_DIR ?? scratchTarget } }));
  // The parallel trace builds are spawned by the batch runner with the job's own env: that env carries the direct PATH too, so no build
  // ever goes through the RustRight shim (which refuses a build that redirects its output folder).
  const directEnv = { ...env, [pathKey]: directPath };
  try { return mainWith(argv, { env: directEnv, platform, run, log, err, judged }); } finally { fs.rmSync(scratchTarget, { recursive: true, force: true }); }
}

function mainWith(argv, { env, platform, run, log, err, judged = new Set(), confirm = null }) {
  const [sub, ...rest] = argv;
  const at = rest.indexOf("--manifest-path");
  const manifestPath = at >= 0 ? rest[at + 1] : undefined;
  if (platform !== "darwin" && platform !== "win32") { log(`right-release build-helpers ${sub}: nothing to do on ${platform}; only broker hosts (Mac, Dell) vet build helpers`); return 0; }
  const listFile = hostListPath(resolveManagedRoots({ env, platform }).dataRoot, platform);
  let inv;
  try { inv = inventory({ manifestPath, run, platform }); } catch (e) { err(`right-release build-helpers: ${e.message}`); return 2; }
  // The host's own list and the repository's committed list (.rightkit/build-helper-trace.json): CI and every host read the same reviewed
  // passes, and a host traces only what neither holds.
  const repoFile = repoListPath(inv.metadata.workspace_root);
  const list = mergeLists(readHostList(listFile), readHostList(repoFile));
  const verdict = checkBuildHelpers(inv.units, list);
  if (sub === "check") {
    if (verdict.ok) { log(`right-release build-helpers: ${inv.units.length} build-helper unit(s), every one traced on this host (${listFile})`); return 0; }
    for (const line of buildHelperRefusal(verdict)) err(line);
    return 1;
  }
  if (sub === "trace") {
    let devices = {};
    if (platform === "win32") {
      // ETW kernel sessions need an elevated shell; kernel paths are mapped back to drive letters once (QueryDosDevice).
      const admin = run("net", ["session"], { encoding: "utf8" });
      if (admin.status !== 0) { err("right-release build-helpers trace: run from an elevated (Administrator) shell; ETW kernel tracing needs it"); return 2; }
      const map = run("powershell", ["-NoProfile", "-Command", "$s='[DllImport(\"kernel32.dll\")] public static extern uint QueryDosDevice(string d, System.Text.StringBuilder b, uint m);'; $k=Add-Type -MemberDefinition $s -Name K -Namespace W -PassThru; foreach($d in [IO.DriveInfo]::GetDrives()){ $l=$d.Name.Substring(0,2); $b=New-Object Text.StringBuilder 512; [void]$k::QueryDosDevice($l,$b,512); \"$l=$b\" }"], { encoding: "utf8" });
      for (const line of String(map.stdout ?? "").split(/\r?\n/)) { const m = /^([A-Z]:)=(\\Device\\\S+)$/.exec(line.trim()); if (m) devices[m[2]] = m[1]; }
      // Without the map every kernel path would be judged outside the allowed folders: stop instead of recording false verdicts.
      if (!Object.keys(devices).length) { err(`right-release build-helpers trace: could not map kernel device paths to drive letters (QueryDosDevice): ${String(map.stderr ?? "").trim().split(/\r?\n/)[0] ?? ""}`); return 2; }
    }
    const recordRepo = rest.includes("--record-repo");
    const cargoHome = env.CARGO_HOME ?? path.join(os.homedir(), ".cargo");
    // Parallel: `--jobs N` builds at once (default 4). Each build is its own process tree, rooted at its own cargo, in its own scratch
    // target, and is judged only on the writes of that tree (eslogger and ETW events name their process). The cores are shared out so the
    // builds do not fight over them.
    const jobsAt = rest.indexOf("--jobs");
    // A confirming pass builds one unit at a time, so nothing else of this run shares the machine with it.
    const jobs = confirm ? 1 : Math.max(1, Number.parseInt(jobsAt >= 0 ? rest[jobsAt + 1] : "4", 10) || 4);
    const perBuildJobs = String(Math.max(1, Math.floor(os.cpus().length / jobs)));
    // A cross target builds with the provisioned zig C toolchain; without it those units wait (never recorded as unbuildable).
    // The toolchain is proved first: a one-line C file compiled through the target's CC wrapper. A wrapper that cannot run (on the Dell a
    // Node junction an elevated shell refused to traverse) failed every C crate, and 0.6.29 recorded those as unbuildable.
    const workingCrossEnv = (target) => {
      const cc = crossCompileEnv(target, undefined, platform);
      if (!cc) return null;
      const probeDir = fs.mkdtempSync(path.join(os.tmpdir(), "rk-bht-cc-"));
      try {
        fs.writeFileSync(path.join(probeDir, "probe.c"), "int rk_probe(void) { return 0; }\n");
        const zigCache = { ZIG_GLOBAL_CACHE_DIR: path.join(probeDir, "zig-global"), ZIG_LOCAL_CACHE_DIR: path.join(probeDir, "zig-local") };
        // A .cmd wrapper runs through the shell on Windows, which joins arguments unquoted: every path is quoted there.
        const q = (s) => (platform === "win32" ? `"${s}"` : s);
        const r = run(q(cc[`CC_${target.replaceAll("-", "_")}`]), ["-c", q(path.join(probeDir, "probe.c")), "-o", q(path.join(probeDir, "probe.o"))], { encoding: "utf8", env: { ...env, ...zigCache }, shell: platform === "win32" });
        if (r.status === 0 && fs.existsSync(path.join(probeDir, "probe.o"))) return cc;
        err(`right-release build-helpers trace: the ${target} C compiler wrapper does not work (${String(r.error?.message ?? r.stderr ?? r.stdout ?? `exit ${r.status}`).trim().split(/\r?\n/)[0]}); its units are not traced (rerun scripts/ci/provision-zig.mjs)`);
        return null;
      } finally { fs.rmSync(probeDir, { recursive: true, force: true }); }
    };
    const crossEnv = new Map(); const waiting = [];
    const pending = [...verdict.untraced, ...verdict.failed.map((f) => f.unit)].filter((unit) => (confirm ? confirm.has(unit.key) : !judged.has(unit.key))).filter((unit) => {
      if (unit.target === inv.host) return true;
      if (!crossEnv.has(unit.target)) crossEnv.set(unit.target, workingCrossEnv(unit.target));
      if (crossEnv.get(unit.target)) return true;
      waiting.push(unit); return false;
    });
    if (waiting.length) err(`right-release build-helpers trace: ${waiting.length} cross-target unit(s) not traced: the zig C toolchain is not provisioned or does not work (scripts/ci/provision-zig.mjs)`);
    log(`right-release build-helpers trace: ${pending.length} unit(s)${confirm ? " to confirm" : ""}, ${jobs} at a time`);
    const firstFails = new Map();
    if (!pending.length) return 0;
    const prepared = pending.map((unit) => {
      const scratch = fs.mkdtempSync(path.join(os.tmpdir(), "rk-bht-"));
      const target = path.join(scratch, "target");
      // The unit is built on its own: a throwaway probe package in the scratch folder depends on exactly this version with exactly its
      // resolved features (`cargo build -p <crate>@<ver>` panics for a crate the workspace uses only as a build dependency, which most
      // build helpers are). The workspace's Cargo.lock seeds the probe so its other crates resolve to the same versions.
      const probe = path.join(scratch, "probe");
      fs.mkdirSync(path.join(probe, "src"), { recursive: true });
      const features = unit.features.filter((f) => f !== "default");
      fs.writeFileSync(path.join(probe, "Cargo.toml"), `[package]\nname = "rk-build-helper-probe"\nversion = "0.0.0"\nedition = "2021"\npublish = false\n\n[dependencies]\n${JSON.stringify(unit.crate)} = { version = "=${unit.version}", default-features = ${unit.features.includes("default")}, features = ${JSON.stringify(features)} }\n\n[workspace]\n`);
      fs.writeFileSync(path.join(probe, "src", "lib.rs"), "");
      try { fs.copyFileSync(path.join(inv.metadata.workspace_root, "Cargo.lock"), path.join(probe, "Cargo.lock")); } catch { /* the probe resolves on its own */ }
      // Release profile, like every other build (GATE-CONTRACT GC-A15): no dev or debug build anywhere, a throwaway probe included.
      // Built for the unit's own target, as the gate builds it: the host's, or a cross-check target with --target and zig.
      const cross = unit.target !== inv.host;
      const buildArgs = ["build", "--profile", "release", "--manifest-path", path.join(probe, "Cargo.toml"), "--target-dir", target, ...(cross ? ["--target", unit.target] : [])];
      // zig (the cross C compiler) keeps its caches in this build's scratch folder: its default global cache is a host folder outside the
      // allowed roots, and every crate with C code was failed for zig's own cache writes (ring, rustls, blake3, libsqlite3-sys on 0.6.29).
      const zigCache = cross ? { ZIG_GLOBAL_CACHE_DIR: path.join(scratch, "zig-global"), ZIG_LOCAL_CACHE_DIR: path.join(scratch, "zig-local") } : {};
      const buildEnv = { ...env, ...(cross ? crossEnv.get(unit.target) : {}), ...zigCache, CARGO_TARGET_DIR: target, CARGO_BUILD_JOBS: perBuildJobs };
      const job = { cmd: "cargo", args: buildArgs, env: buildEnv };
      job.label = `${unit.crate} ${unit.version}${cross ? ` (${unit.target})` : ""}`;
      return { unit, scratch, target, job };
    });
    const sessionDir = fs.mkdtempSync(path.join(os.tmpdir(), "rk-bht-session-"));
    const traceFile = path.join(sessionDir, "trace.txt");
    // One tracer session for the whole run (macOS eslogger, Windows ETW).
    let stopTracer = () => {};
    let etl = null;
    if (platform === "darwin") {
      const tracer = run("sh", ["-c", `sudo -n eslogger fork exec create rename unlink link truncate open close clone copyfile exchangedata > '${traceFile}' 2>'${traceFile}.err' & echo $!`], { encoding: "utf8" });
      const tracerPid = String(tracer.stdout ?? "").trim();
      run("sleep", ["2"]);
      // Drain before stopping: eslogger writes a block-buffered file and Endpoint Security delivers late, so the last builds' events were
      // still in flight when a fixed one-second wait ended (every "build was not observed" fail on the Mac was a build that finished last
      // in its session). Marker files are created in the session folder, one a second, until the first appears in the recording: every
      // event before it is then on disk. Two minutes without it leaves the verdicts to judgeTrace, which fails a build it did not see.
      const drain = () => {
        const tag = `rk-bht-drain-${process.pid}-`;
        let from = 0; try { from = fs.statSync(traceFile).size; } catch { /* nothing yet */ }
        for (let i = 0; i < 120; i += 1) {
          fs.writeFileSync(path.join(sessionDir, `${tag}${i}`), "");
          run("sleep", ["1"]);
          let size = 0; try { size = fs.statSync(traceFile).size; } catch { continue; }
          if (size <= from) continue;
          const fd = fs.openSync(traceFile, "r");
          try {
            const chunk = Buffer.alloc(8 << 20);
            for (let at = from; at < size; at += chunk.length - tag.length) {
              const n = fs.readSync(fd, chunk, 0, chunk.length, at);
              if (chunk.subarray(0, n).includes(tag)) return true;
            }
          } finally { fs.closeSync(fd); }
          from = Math.max(from, size - tag.length);
        }
        err("right-release build-helpers trace: eslogger did not record the drain marker within 2 minutes; builds it did not see are judged unobserved");
        return false;
      };
      stopTracer = () => { drain(); if (tracerPid) { run("sudo", ["-n", "pkill", "-INT", "-P", tracerPid]); run("kill", [tracerPid]); run("sleep", ["1"]); } };
    } else if (platform === "win32") {
      // One ETW session with both kernel providers, each with an explicit level (0x5): a separate process-only session without a level
      // recorded nothing on the Dell (proc.etl 0 MB), so no write could be tied to its build. Tested on the Dell: process starts (with
      // ParentProcessID) and file writes both recorded.
      etl = { file: path.join(sessionDir, "trace.etl") };
      const providers = path.join(sessionDir, "providers.txt");
      fs.writeFileSync(providers, '"Microsoft-Windows-Kernel-Process" 0x10 0x5\r\n"Microsoft-Windows-Kernel-File" 0x1E90 0x5\r\n');
      run("logman", ["stop", "rk-bht", "-ets"]);
      const started = run("logman", ["start", "rk-bht", "-pf", providers, "-o", etl.file, "-ets"], { encoding: "utf8" });
      if (started.status !== 0) { err(`right-release build-helpers trace: logman could not start the ETW session: ${String(started.stdout ?? "").trim()}`); return 2; }
      stopTracer = () => { run("logman", ["stop", "rk-bht", "-ets"]); };
    }
    // The runner keeps `jobs` builds running and starts the next whenever one finishes; each build's pid is its tree's root.
    const jobsFile = path.join(sessionDir, "jobs.json"); const resultFile = path.join(sessionDir, "results.json");
    fs.writeFileSync(jobsFile, JSON.stringify(prepared.map((b) => b.job)));
    let runner;
    try { runner = run(process.execPath, ["-e", BATCH_RUNNER, jobsFile, resultFile, String(jobs)], { encoding: "utf8", env, stdio: ["ignore", "inherit", "inherit"] }); }
    finally { stopTracer(); }
    let results = [];
    try { results = JSON.parse(fs.readFileSync(resultFile, "utf8")); } catch { err(`right-release build-helpers trace: the build runner wrote no results (${runner?.error?.message ?? `exit ${runner?.status}`}); nothing recorded`); return 2; }
    // The whole-run recording is streamed, never read into one string, and attributed to every build's tree in one pass.
    const roots = new Set(results.filter((r) => r?.status === 0 && Number.isInteger(r.pid)).map((r) => r.pid));
    let byRoot = new Map(); let recorded = false;
    if (platform === "darwin") { try { recorded = fs.statSync(traceFile).size > 0; if (recorded) byRoot = esloggerWritesByRoot(traceFile, roots); } catch { /* no output */ } }
    if (platform === "win32") {
      const xml = path.join(sessionDir, "trace.xml");
      const exported = run("tracerpt", [etl.file, "-o", xml, "-of", "XML", "-y"], { encoding: "utf8" });
      if (exported.status !== 0) { err("right-release build-helpers trace: tracerpt could not export the ETW trace"); return 2; }
      try { recorded = fs.statSync(xml).size > 0; if (recorded) byRoot = etwWritesByRoot(xml, roots, devices, { toolchainDirs: [env.ProgramFiles, env["ProgramFiles(x86)"]].filter(Boolean).map((d) => path.win32.join(d, "Microsoft Visual Studio")) }); } catch { /* no output */ }
    }
    if (!recorded && roots.size) { err(`right-release build-helpers trace: the ${platform === "darwin" ? "eslogger" : "ETW"} session recorded nothing; nothing recorded`); return 2; }
    prepared.forEach((b, k) => {
      const r = results[k] ?? {};
      if (!Number.isInteger(r.status)) { err(`right-release build-helpers trace: ${b.unit.crate} ${b.unit.version} did not finish (${r.error ?? "no result"}); not recorded`); return; }
      if (r.status !== 0 && confirm) {
        // It built in the first pass, so a failed rebuild is a passing problem (memory, a lock), not "unbuildable": the first FAIL stands.
        const before = [...confirm.get(b.unit.key)];
        judged.add(b.unit.key);
        recordTrace(listFile, b.unit, { ok: false, outside: before });
        if (recordRepo) recordTrace(repoFile, b.unit, { ok: false, outside: before });
        log(`FAIL ${b.unit.crate} ${b.unit.version} (${b.unit.target}): the rebuild did not build; the first trace stands: wrote ${before.slice(0, 5).join(", ")}`);
        return;
      }
      if (r.status !== 0) {
        // It does not build here for this target, so nothing this host's gate runs builds it either: recorded once, skipped from then on.
        const reason = String(r.error ?? `exit ${r.status}`);
        judged.add(b.unit.key);
        recordTrace(listFile, b.unit, { ok: false, unbuildable: true, outside: [reason] });
        if (recordRepo) recordTrace(repoFile, b.unit, { ok: false, unbuildable: true, outside: [reason] });
        log(`skip ${b.unit.crate} ${b.unit.version} (${b.unit.target}): does not build on this host (${reason.slice(0, 160)})`);
        return;
      }
      const writes = byRoot.get(r.pid) ?? [];
      const allowed = platform === "win32" ? [b.scratch, cargoHome, os.tmpdir(), env.LOCALAPPDATA ? path.join(env.LOCALAPPDATA, "Temp") : null]
        : [b.scratch, cargoHome, os.tmpdir(), "/private/var/folders", "/dev"];
      // Allowed folders by their real path: a linked CARGO_HOME (~/.cargo -> /Volumes/D/...) or temp (/var -> /private/var) is reported by
      // the tracer under the real path, so comparing the link's path would fail every crate.
      const real = (d) => { try { return fs.realpathSync.native(d); } catch { return d; } };
      const roots = allowed.filter(Boolean).flatMap((d) => [d, real(d)]);
      let verdictOf = judgeTrace(writes, { allowed: roots, platform, mustSee: real(b.target) });
      // A FAIL must repeat (Dell, 2026-11-03: Windows' recorder pinned other programs' writes on a build now and then). The first FAIL is
      // held and the unit is rebuilt alone; it fails only on the outside paths both builds wrote, or when either build went unobserved.
      // Only Windows' recorder sees the whole machine, so only there can another program's write land on a build: on the Mac (eslogger
      // follows the build's own processes) an outside write fails at once, and the rebuild only retries a build that went unobserved.
      const unseen = (o) => o.startsWith("(the tracer recorded no write");
      if (!verdictOf.ok && !confirm && (platform === "win32" || verdictOf.outside.every(unseen))) { firstFails.set(b.unit.key, new Set(verdictOf.outside)); log(`recheck ${b.unit.crate} ${b.unit.version} (${b.unit.target}): rebuilding it alone`); return; }
      if (!verdictOf.ok && confirm) {
        const before = confirm.get(b.unit.key);
        // An unobserved first build proves nothing either way: then every outside write of the rebuild counts.
        const firstUnseen = [...before].some(unseen);
        const repeated = verdictOf.outside.filter((o) => !unseen(o) && (firstUnseen || before.has(o)));
        const unobserved = verdictOf.outside.some(unseen);
        verdictOf = repeated.length || unobserved ? { ok: false, outside: repeated.length ? repeated : verdictOf.outside } : { ok: true, outside: [] };
      }
      judged.add(b.unit.key);
      recordTrace(listFile, b.unit, verdictOf);
      // --record-repo also writes the verdict into the repository's committed list, for review and commit by the repository's owner.
      if (recordRepo) recordTrace(repoFile, b.unit, verdictOf);
      log(`${verdictOf.ok ? "pass" : "FAIL"} ${b.unit.crate} ${b.unit.version} (${b.unit.target})${verdictOf.ok ? "" : `: wrote ${verdictOf.outside.slice(0, 5).join(", ")}`}`);
    });
    for (const b of prepared) fs.rmSync(b.scratch, { recursive: true, force: true });
    fs.rmSync(sessionDir, { recursive: true, force: true });
    if (firstFails.size) return mainWith(argv, { env, platform, run, log, err, judged, confirm: firstFails });
    return 0;
  }
  err("usage: right-release build-helpers check|trace [--manifest-path <Cargo.toml>] [--record-repo] [--jobs N] | trace-all [--root <dir>]... [--jobs N]");
  return 2;
}

const invoked = (() => { try { return fs.realpathSync(process.argv[1] ?? "") === fs.realpathSync(fileURLToPath(import.meta.url)); } catch { return false; } })();
if (invoked) process.exit(main());
