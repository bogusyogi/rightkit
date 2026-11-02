#!/usr/bin/env node
// `right-release build-helpers check [--manifest-path <Cargo.toml>]` (Class A, rightkit#1450) and
// `right-release build-helpers trace [--manifest-path <Cargo.toml>]` (the one-time trace: eslogger on macOS (sudo + Full Disk Access), strace -ff on Linux, ETW on Windows (Administrator)).
//
// check: reads the workspace's resolved graph (`cargo metadata --format-version 1 --locked`, a non-compiling call run directly: GATE-CONTRACT
// GC-A11), its Cargo.lock, the host triple and toolchain (`rustc -vV`), and the host list, and refuses (exit 1) when any build-helper unit is
// untraced or traced as writing outside its build directories, naming each.
// trace: for each untraced unit, builds the workspace's dependency alone (`cargo build -p <crate>@<version> --locked`) under the host's tracer, into a
// scratch target, judges every write against OUT_DIR / target / CARGO_HOME / temp, and records the verdict in the host list.
// check --trace-missing (Linux only, where strace needs no admin; CI and Linux hosts): untraced units are traced first, then judged.
import { spawn, spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { buildHelperRefusal, buildHelperUnits, checkBuildHelpers, hostListPath, judgeTrace, mergeLists, parseEsloggerWrites, parseEtwWrites, parseStraceWrites, readStraceFiles, readHostList, recordTrace, repoListPath } from "./build-helper-trace.mjs";
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

export function inventory({ manifestPath, run = spawnSync, readFile = (f) => fs.readFileSync(f, "utf8") }) {
  const args = ["metadata", "--format-version", "1", "--locked", ...(manifestPath ? ["--manifest-path", manifestPath] : [])];
  const r = run("cargo", args, { encoding: "utf8", maxBuffer: 1 << 28 });
  if (r.error || r.status !== 0) throw new Error(`cargo ${args.join(" ")} failed (${r.error?.message ?? `exit ${r.status}`}): ${String(r.stderr ?? "").trim().split(/\r?\n/).slice(-2).join(" | ")}`);
  const metadata = JSON.parse(r.stdout);
  const lockText = readFile(path.join(metadata.workspace_root, "Cargo.lock"));
  return { metadata, units: buildHelperUnits(metadata, lockText, hostFacts(run)) };
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
  const gated = (dir) => { for (let d = dir; ; d = path.dirname(d)) { if (fsApi.existsSync(path.join(d, ".git"))) return fsApi.existsSync(path.join(d, ".rightkit")); if (path.dirname(d) === d) return false; } };
  return found.filter(gated).sort();
}

// `right-release build-helpers trace-all [--root <dir>]...`: one admin prompt, every workspace. On macOS it asks for the password once
// (`sudo -v`) and keeps that credential alive while it runs; each workspace's untraced units are traced and recorded in the host list and
// in the workspace's own committed list (--record-repo). Default root: /Volumes/D/claude (Mac), D:\claude (Windows), the current
// directory elsewhere.
function traceAll(argv, { env, platform, log, err }) {
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
      const code = main(["trace", "--manifest-path", path.join(ws, "Cargo.toml"), "--record-repo", ...(jobsAt >= 0 ? ["--jobs", argv[jobsAt + 1]] : [])], { env, platform, log, err });
      if (code !== 0) err(`right-release build-helpers trace-all: ${ws} ended with ${code}`);
      worst = Math.max(worst, code);
    }
  } finally { try { keepAlive?.kill(); } catch { /* already gone */ } }
  log(`right-release build-helpers trace-all: done; commit each workspace's .rightkit/build-helper-trace.json`);
  return worst;
}

export function main(argv = process.argv.slice(2), { env = process.env, platform = process.platform, run: runRaw = null, log = console.log, err = console.error } = {}) {
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
  try { return mainWith(argv, { env: directEnv, platform, run, log, err }); } finally { fs.rmSync(scratchTarget, { recursive: true, force: true }); }
}

function mainWith(argv, { env, platform, run, log, err }) {
  const [sub, ...rest] = argv;
  const at = rest.indexOf("--manifest-path");
  const manifestPath = at >= 0 ? rest[at + 1] : undefined;
  const listFile = hostListPath(resolveManagedRoots({ env, platform }).dataRoot, platform);
  let inv;
  try { inv = inventory({ manifestPath, run }); } catch (e) { err(`right-release build-helpers: ${e.message}`); return 2; }
  // The host's own list and the repository's committed list (.rightkit/build-helper-trace.json): CI and every host read the same reviewed
  // passes, and a host traces only what neither holds.
  const repoFile = repoListPath(inv.metadata.workspace_root);
  const list = mergeLists(readHostList(listFile), readHostList(repoFile));
  const verdict = checkBuildHelpers(inv.units, list);
  if (sub === "check" && !verdict.ok && rest.includes("--trace-missing") && platform === "linux" && verdict.untraced.length) {
    log(`right-release build-helpers: ${verdict.untraced.length} untraced unit(s) on this Linux host; tracing them now (strace -ff)`);
    const traced = mainWith(["trace", ...(manifestPath ? ["--manifest-path", manifestPath] : [])], { env, platform, run, log, err });
    if (traced !== 0) return traced;
    return mainWith(["check", ...(manifestPath ? ["--manifest-path", manifestPath] : [])], { env, platform, run, log, err });
  }
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
      const map = run("powershell", ["-NoProfile", "-Command", "$s='[DllImport(\\\"kernel32.dll\\\")] public static extern uint QueryDosDevice(string d, System.Text.StringBuilder b, uint m);'; $k=Add-Type -MemberDefinition $s -Name K -Namespace W -PassThru; foreach($d in [IO.DriveInfo]::GetDrives()){ $l=$d.Name.Substring(0,2); $b=New-Object Text.StringBuilder 512; [void]$k::QueryDosDevice($l,$b,512); \"$l=$b\" }"], { encoding: "utf8" });
      for (const line of String(map.stdout ?? "").split(/\r?\n/)) { const m = /^([A-Z]:)=(\\Device\\\S+)$/.exec(line.trim()); if (m) devices[m[2]] = m[1]; }
    }
    const recordRepo = rest.includes("--record-repo");
    const cargoHome = env.CARGO_HOME ?? path.join(os.homedir(), ".cargo");
    // Parallel: `--jobs N` builds at once (default 4). Each build is its own process tree, rooted at its own cargo, in its own scratch
    // target, and is judged only on the writes of that tree (eslogger and ETW events name their process; strace -ff writes one file per
    // process of its own run). The cores are shared out so the builds do not fight over them.
    const jobsAt = rest.indexOf("--jobs");
    const jobs = Math.max(1, Number.parseInt(jobsAt >= 0 ? rest[jobsAt + 1] : "4", 10) || 4);
    const perBuildJobs = String(Math.max(1, Math.floor(os.cpus().length / jobs)));
    const pending = [...verdict.untraced, ...verdict.failed.map((f) => f.unit)];
    log(`right-release build-helpers trace: ${pending.length} unit(s), ${jobs} at a time`);
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
      const buildArgs = ["build", "--profile", "release", "--manifest-path", path.join(probe, "Cargo.toml"), "--target-dir", target];
      const buildEnv = { ...env, CARGO_TARGET_DIR: target, CARGO_BUILD_JOBS: perBuildJobs };
      const straceOut = path.join(scratch, "strace");
      const job = platform === "linux"
        ? { cmd: "strace", args: ["-ff", "-qq", "-o", straceOut, "-e", "trace=open,openat,openat2,creat,rename,renameat,renameat2,unlink,unlinkat,mkdir,mkdirat,rmdir,link,linkat,symlink,symlinkat,truncate,chmod,fchmodat,chown,fchownat,lchown,utimensat", "cargo", ...buildArgs], env: buildEnv }
        : { cmd: "cargo", args: buildArgs, env: buildEnv };
      job.label = `${unit.crate} ${unit.version}`;
      return { unit, scratch, target, straceOut, job };
    });
    const sessionDir = fs.mkdtempSync(path.join(os.tmpdir(), "rk-bht-session-"));
    const traceFile = path.join(sessionDir, "trace.txt");
    // One tracer session for the whole run (macOS eslogger, Windows ETW); Linux traces per build through strace.
    let stopTracer = () => {};
    let etl = null;
    if (platform === "darwin") {
      const tracer = run("sh", ["-c", `sudo -n eslogger fork exec create rename unlink link truncate open close clone copyfile exchangedata > '${traceFile}' 2>'${traceFile}.err' & echo $!`], { encoding: "utf8" });
      const tracerPid = String(tracer.stdout ?? "").trim();
      run("sleep", ["2"]);
      stopTracer = () => { run("sleep", ["1"]); if (tracerPid) { run("sudo", ["-n", "pkill", "-INT", "-P", tracerPid]); run("kill", [tracerPid]); run("sleep", ["1"]); } };
    } else if (platform === "win32") {
      etl = { file: path.join(sessionDir, "file.etl"), proc: path.join(sessionDir, "proc.etl") };
      run("logman", ["stop", "rk-bht-file", "-ets"]); run("logman", ["stop", "rk-bht-proc", "-ets"]);
      const startProc = run("logman", ["start", "rk-bht-proc", "-p", "Microsoft-Windows-Kernel-Process", "0x10", "-o", etl.proc, "-ets"], { encoding: "utf8" });
      const startFile = run("logman", ["start", "rk-bht-file", "-p", "Microsoft-Windows-Kernel-File", "0x1E90", "-o", etl.file, "-ets"], { encoding: "utf8" });
      if (startProc.status !== 0 || startFile.status !== 0) { err(`right-release build-helpers trace: logman could not start the ETW sessions: ${String(startFile.stdout ?? startProc.stdout ?? "").trim()}`); return 2; }
      stopTracer = () => { run("logman", ["stop", "rk-bht-file", "-ets"]); run("logman", ["stop", "rk-bht-proc", "-ets"]); };
    }
    // The runner keeps `jobs` builds running and starts the next whenever one finishes; each build's pid is its tree's root.
    const jobsFile = path.join(sessionDir, "jobs.json"); const resultFile = path.join(sessionDir, "results.json");
    fs.writeFileSync(jobsFile, JSON.stringify(prepared.map((b) => b.job)));
    let runner;
    try { runner = run(process.execPath, ["-e", BATCH_RUNNER, jobsFile, resultFile, String(jobs)], { encoding: "utf8", env, stdio: ["ignore", "inherit", "inherit"] }); }
    finally { stopTracer(); }
    let results = [];
    try { results = JSON.parse(fs.readFileSync(resultFile, "utf8")); } catch { err(`right-release build-helpers trace: the build runner wrote no results (${runner?.error?.message ?? `exit ${runner?.status}`}); nothing recorded`); return 2; }
    let sessionText = "";
    if (platform === "darwin") { try { sessionText = fs.readFileSync(traceFile, "utf8"); } catch { /* no output */ } }
    if (platform === "win32") {
      const xml = path.join(sessionDir, "trace.xml");
      const exported = run("tracerpt", [etl.proc, etl.file, "-o", xml, "-of", "XML", "-y"], { encoding: "utf8" });
      if (exported.status !== 0) { err("right-release build-helpers trace: tracerpt could not export the ETW trace"); return 2; }
      sessionText = fs.readFileSync(xml, "utf8");
    }
    if (platform !== "linux" && !sessionText.trim() && results.some((r) => r.status === 0)) { err(`right-release build-helpers trace: the ${platform === "darwin" ? "eslogger" : "ETW"} session recorded nothing; nothing recorded`); return 2; }
    prepared.forEach((b, k) => {
      const r = results[k] ?? {};
      if (r.status !== 0 || !Number.isInteger(r.pid)) { err(`right-release build-helpers trace: ${b.unit.crate} ${b.unit.version} did not build (${r.error ?? `exit ${r.status}`}); not recorded`); return; }
      let writes;
      if (platform === "linux") { let text = ""; try { text = readStraceFiles(b.straceOut); } catch { /* none */ } writes = parseStraceWrites(text, { cwd: path.dirname(path.resolve(manifestPath ?? "Cargo.toml")) }); }
      else if (platform === "win32") writes = parseEtwWrites(sessionText, { rootPid: r.pid, devices });
      else writes = parseEsloggerWrites(sessionText, { rootPid: r.pid });
      const allowed = platform === "win32" ? [b.scratch, cargoHome, os.tmpdir(), env.LOCALAPPDATA ? path.join(env.LOCALAPPDATA, "Temp") : null]
        : [b.scratch, cargoHome, os.tmpdir(), "/private/var/folders", "/dev", "/proc/self"];
      // Allowed folders by their real path: a linked CARGO_HOME (~/.cargo -> /Volumes/D/...) or temp (/var -> /private/var) is reported by
      // the tracer under the real path, so comparing the link's path would fail every crate.
      const real = (d) => { try { return fs.realpathSync.native(d); } catch { return d; } };
      const roots = allowed.filter(Boolean).flatMap((d) => [d, real(d)]);
      const judged = judgeTrace(writes, { allowed: roots, platform, mustSee: real(b.target) });
      recordTrace(listFile, b.unit, judged);
      // --record-repo also writes the verdict into the repository's committed list, for review and commit by the repository's owner.
      if (recordRepo) recordTrace(repoFile, b.unit, judged);
      log(`${judged.ok ? "pass" : "FAIL"} ${b.unit.crate} ${b.unit.version}${judged.ok ? "" : `: wrote ${judged.outside.slice(0, 5).join(", ")}`}`);
    });
    for (const b of prepared) fs.rmSync(b.scratch, { recursive: true, force: true });
    fs.rmSync(sessionDir, { recursive: true, force: true });
    return 0;
  }
  err("usage: right-release build-helpers check|trace [--manifest-path <Cargo.toml>] [--record-repo] [--jobs N] | trace-all [--root <dir>]... [--jobs N]");
  return 2;
}

const invoked = (() => { try { return fs.realpathSync(process.argv[1] ?? "") === fs.realpathSync(fileURLToPath(import.meta.url)); } catch { return false; } })();
if (invoked) process.exit(main());
