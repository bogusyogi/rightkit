#!/usr/bin/env node
// `right-release build-helpers check [--manifest-path <Cargo.toml>]` (Class A, rightkit#1450) and
// `right-release build-helpers trace [--manifest-path <Cargo.toml>]` (the one-time trace; needs admin rights for fs_usage / ETW).
//
// check: reads the workspace's resolved graph (`cargo metadata --format-version 1 --locked`, a non-compiling call run directly: GATE-CONTRACT
// GC-A11), its Cargo.lock, the host triple and toolchain (`rustc -vV`), and the host list, and refuses (exit 1) when any build-helper unit is
// untraced or traced as writing outside its build directories, naming each.
// trace: for each untraced unit, builds the workspace's dependency alone (`cargo build -p <crate>@<version> --locked`) under fs_usage into a
// scratch target, judges every write against OUT_DIR / target / CARGO_HOME / temp, and records the verdict in the host list. macOS only for
// now; the Windows ETW tracer is the host step Adrian enables.
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { buildHelperRefusal, buildHelperUnits, checkBuildHelpers, hostListPath, judgeTrace, parseFsUsageWrites, readHostList, recordTrace } from "./build-helper-trace.mjs";
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

export function main(argv = process.argv.slice(2), { env = process.env, platform = process.platform, run: runRaw = null, log = console.log, err = console.error } = {}) {
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
  const list = readHostList(listFile);
  const verdict = checkBuildHelpers(inv.units, list);
  if (sub === "check") {
    if (verdict.ok) { log(`right-release build-helpers: ${inv.units.length} build-helper unit(s), every one traced on this host (${listFile})`); return 0; }
    for (const line of buildHelperRefusal(verdict)) err(line);
    return 1;
  }
  if (sub === "trace") {
    if (platform !== "darwin") { err("right-release build-helpers trace: only macOS (fs_usage) is implemented; the Windows ETW tracer needs Adrian's host step"); return 2; }
    const cargoHome = env.CARGO_HOME ?? path.join(os.homedir(), ".cargo");
    for (const unit of [...verdict.untraced, ...verdict.failed.map((f) => f.unit)]) {
      const scratch = fs.mkdtempSync(path.join(os.tmpdir(), "rk-bht-"));
      const traceFile = path.join(scratch, "fs_usage.txt");
      const tracer = run("sh", ["-c", `fs_usage -w -f filesys cargo rustc build-script-build > '${traceFile}' 2>&1 & echo $!`], { encoding: "utf8" });
      const tracerPid = String(tracer.stdout ?? "").trim();
      const build = run("cargo", ["build", "-p", `${unit.crate}@${unit.version}`, "--locked", "--target-dir", path.join(scratch, "target"), ...(manifestPath ? ["--manifest-path", manifestPath] : [])], { encoding: "utf8", env: { ...env, CARGO_TARGET_DIR: path.join(scratch, "target") } });
      if (tracerPid) run("kill", [tracerPid]);
      if (build.error || build.status !== 0) { err(`right-release build-helpers trace: ${unit.crate} ${unit.version} did not build (${build.error?.message ?? `exit ${build.status}`}); not recorded`); continue; }
      let text = ""; try { text = fs.readFileSync(traceFile, "utf8"); } catch { /* no trace output: fs_usage needs admin rights */ }
      if (!text.trim()) { err("right-release build-helpers trace: fs_usage recorded nothing (it needs admin rights); nothing recorded"); return 2; }
      const judged = judgeTrace(parseFsUsageWrites(text), { allowed: [path.join(scratch, "target"), cargoHome, os.tmpdir(), "/private/var/folders", "/dev"], platform });
      recordTrace(listFile, unit, judged);
      log(`${judged.ok ? "pass" : "FAIL"} ${unit.crate} ${unit.version}${judged.ok ? "" : `: wrote ${judged.outside.slice(0, 5).join(", ")}`}`);
      fs.rmSync(scratch, { recursive: true, force: true });
    }
    return 0;
  }
  err("usage: right-release build-helpers check|trace [--manifest-path <Cargo.toml>]");
  return 2;
}

const invoked = (() => { try { return fs.realpathSync(process.argv[1] ?? "") === fs.realpathSync(fileURLToPath(import.meta.url)); } catch { return false; } })();
if (invoked) process.exit(main());
