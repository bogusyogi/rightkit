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
      const code = main(["trace", "--manifest-path", path.join(ws, "Cargo.toml"), "--record-repo"], { env, platform, log, err });
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
  try { return mainWith(argv, { env, platform, run, log, err }); } finally { fs.rmSync(scratchTarget, { recursive: true, force: true }); }
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
    for (const unit of [...verdict.untraced, ...verdict.failed.map((f) => f.unit)]) {
      const scratch = fs.mkdtempSync(path.join(os.tmpdir(), "rk-bht-"));
      const traceFile = path.join(scratch, "trace.txt");
      const buildArgs = ["build", "-p", `${unit.crate}@${unit.version}`, "--locked", "--target-dir", path.join(scratch, "target"), ...(manifestPath ? ["--manifest-path", manifestPath] : [])];
      const buildEnv = { ...env, CARGO_TARGET_DIR: path.join(scratch, "target") };
      let build;
      if (platform === "win32") {
        const etlFile = path.join(scratch, "file.etl"); const etlProc = path.join(scratch, "proc.etl");
        run("logman", ["stop", "rk-bht-file", "-ets"]); run("logman", ["stop", "rk-bht-proc", "-ets"]);
        const startProc = run("logman", ["start", "rk-bht-proc", "-p", "Microsoft-Windows-Kernel-Process", "0x10", "-o", etlProc, "-ets"], { encoding: "utf8" });
        const startFile = run("logman", ["start", "rk-bht-file", "-p", "Microsoft-Windows-Kernel-File", "0x1E90", "-o", etlFile, "-ets"], { encoding: "utf8" });
        if (startProc.status !== 0 || startFile.status !== 0) { err(`right-release build-helpers trace: logman could not start the ETW sessions: ${String(startFile.stdout ?? startProc.stdout ?? "").trim()}`); return 2; }
        try { build = run("cargo", buildArgs, { encoding: "utf8", env: buildEnv }); }
        finally { run("logman", ["stop", "rk-bht-file", "-ets"]); run("logman", ["stop", "rk-bht-proc", "-ets"]); }
        const xml = path.join(scratch, "trace.xml");
        const exported = run("tracerpt", [etlProc, etlFile, "-o", xml, "-of", "XML", "-y"], { encoding: "utf8" });
        if (exported.status !== 0) { err("right-release build-helpers trace: tracerpt could not export the ETW trace"); return 2; }
        fs.writeFileSync(traceFile, fs.readFileSync(xml, "utf8"));
      } else if (platform === "linux") {
        // strace follows every child (-f) and records the file calls; it needs no root for the agent's own processes.
        // -ff: one output file per process, so no system call is split across <unfinished>/<resumed> lines.
        build = run("strace", ["-ff", "-qq", "-o", traceFile, "-e", "trace=open,openat,openat2,creat,rename,renameat,renameat2,unlink,unlinkat,mkdir,mkdirat,rmdir,link,linkat,symlink,symlinkat,truncate,chmod,fchmodat,chown,fchownat,lchown,utimensat", "cargo", ...buildArgs], { encoding: "utf8", env: buildEnv });
        if (build.error?.code === "ENOENT") { err("right-release build-helpers trace: strace is not installed on this host"); return 2; }
      } else {
        // Endpoint Security (eslogger) names every event's process and its parent, so the whole tree under this process is traced: build
        // scripts and everything they start (sh, cc, cmake, perl), whatever the name. Only the logger is root (`sudo -n`: the credential
        // `trace-all` cached with one `sudo -v`); the build runs as the user. eslogger needs Full Disk Access for the terminal running it.
        const tracer = run("sh", ["-c", `sudo -n eslogger fork exec create rename unlink link truncate open close clone copyfile exchangedata > '${traceFile}' 2>'${traceFile}.err' & echo $!`], { encoding: "utf8" });
        const tracerPid = String(tracer.stdout ?? "").trim();
        run("sleep", ["2"]);
        build = run("cargo", buildArgs, { encoding: "utf8", env: buildEnv });
        run("sleep", ["1"]);
        if (tracerPid) { run("sudo", ["-n", "pkill", "-INT", "-P", tracerPid]); run("kill", [tracerPid]); run("sleep", ["1"]); }
      }
      if (build.error || build.status !== 0) { err(`right-release build-helpers trace: ${unit.crate} ${unit.version} did not build (${build.error?.message ?? `exit ${build.status}`}); not recorded`); continue; }
      let text = ""; try { text = platform === "linux" ? readStraceFiles(traceFile) : fs.readFileSync(traceFile, "utf8"); } catch { /* no trace output */ }
      if (!text.trim()) { err(`right-release build-helpers trace: ${platform === "linux" ? "strace" : platform === "darwin" ? "eslogger (it needs sudo and Full Disk Access for this terminal)" : "ETW"} recorded nothing; nothing recorded`); return 2; }
      const writes = platform === "win32" ? parseEtwWrites(text, { rootPid: process.pid, devices })
        : platform === "linux" ? parseStraceWrites(text, { cwd: path.dirname(path.resolve(manifestPath ?? "Cargo.toml")) }) : parseEsloggerWrites(text, { rootPid: process.pid });
      const allowed = platform === "win32" ? [path.join(scratch, "target"), cargoHome, os.tmpdir(), env.LOCALAPPDATA ? path.join(env.LOCALAPPDATA, "Temp") : null]
        : [path.join(scratch, "target"), cargoHome, os.tmpdir(), "/private/var/folders", "/dev", "/proc/self"];
      const judged = judgeTrace(writes, { allowed, platform, mustSee: path.join(scratch, "target") });
      recordTrace(listFile, unit, judged);
      // --record-repo also writes the verdict into the repository's committed list, for review and commit by the repository's owner.
      if (recordRepo) recordTrace(repoFile, unit, judged);
      log(`${judged.ok ? "pass" : "FAIL"} ${unit.crate} ${unit.version}${judged.ok ? "" : `: wrote ${judged.outside.slice(0, 5).join(", ")}`}`);
      fs.rmSync(scratch, { recursive: true, force: true });
    }
    return 0;
  }
  err("usage: right-release build-helpers check|trace [--manifest-path <Cargo.toml>] [--record-repo] | trace-all [--root <dir>]...");
  return 2;
}

const invoked = (() => { try { return fs.realpathSync(process.argv[1] ?? "") === fs.realpathSync(fileURLToPath(import.meta.url)); } catch { return false; } })();
if (invoked) process.exit(main());
