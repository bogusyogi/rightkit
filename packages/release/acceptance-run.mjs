#!/usr/bin/env node
// Runs the acceptance cases of acceptance-cases.json against an installed gate and the live broker, and prints the twelve-line verdict (GC-A27; rightkit#1514;
// Adrian, 2026-11-24: "the 7 fixture cases + 1 real build as the acceptance run").
//
//   node acceptance-run.mjs --fixture <witness fixture checkout> --out <folder> --gate <installed rightkit-gate.mjs> --managed <managed data root>
//        [--cases 1,2,4,3,5,6,7] [--real <repository>] [--rustright <client>] [--keep-going]
//
// What it does to set a case up, and nothing else: removes the fixture's scan store and its history for the case from nothing (both are kept under --out);
// plants an error in the fixture's working checkout and restores it; makes the case commits as unreferenced commits under refs/witness/acceptance-* and deletes
// those refs at the end; moves the `license` store files away and puts them back; kills the gate's process tree for the kill case. It hands the gate no variable
// and no flag beyond the stage. Verdicts come from acceptance-cases.mjs; this file decides nothing.
import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { execFileSync, spawn } from "node:child_process";
import { fileURLToPath } from "node:url";
import { cacheStoreDir, launch } from "./acceptance-launch.mjs";
import { verifierArgv } from "./acceptance-lines.mjs";
import { decideAll, decideCase, decideRealBuild, factsOfRun } from "./acceptance-cases.mjs";

const here = path.dirname(fileURLToPath(import.meta.url));
/** The error case 1 plants in the working checkout (never in the commit). */
const PLANTED = "pub fn leaf() -> u32 { this does not compile\n";
const sha256 = (b) => crypto.createHash("sha256").update(b).digest("hex");
const git = (repo, args, opts = {}) => String(execFileSync("git", ["-C", repo, ...args], { encoding: "utf8", windowsHide: true, stdio: ["pipe", "pipe", "pipe"], maxBuffer: 64 * 1024 * 1024, ...opts })).trim();
const IDENT = { GIT_AUTHOR_NAME: "acceptance", GIT_AUTHOR_EMAIL: "acceptance@rightkit.invalid", GIT_COMMITTER_NAME: "acceptance", GIT_COMMITTER_EMAIL: "acceptance@rightkit.invalid", GIT_AUTHOR_DATE: "2026-11-24T00:00:00Z", GIT_COMMITTER_DATE: "2026-11-24T00:00:00Z" };

/** An unreferenced commit on `base` with the case's edits (append to a tracked file, or a new file's content), kept alive under refs/witness/acceptance-<name>. No checkout is written. */
export function caseCommit(repo, base, edits, name, scratch) {
  const idx = path.join(scratch, `index-${name}`); fs.rmSync(idx, { force: true }); const env = { ...process.env, ...IDENT, GIT_INDEX_FILE: idx };
  git(repo, ["read-tree", base], { env });
  for (const e of edits) {
    const body = typeof e.append === "string" ? execFileSync("git", ["-C", repo, "show", `${base}:${e.path}`], { windowsHide: true }).toString("utf8") + e.append : e.content;
    const blob = git(repo, ["hash-object", "-w", "--stdin"], { input: body });
    git(repo, ["update-index", "--add", "--cacheinfo", `100644,${blob},${e.path}`], { env });
  }
  const tree = git(repo, ["write-tree"], { env });
  const commit = git(repo, ["commit-tree", tree, "-p", base, "-m", `acceptance case ${name}`], { env });
  git(repo, ["update-ref", `refs/witness/acceptance-${name}`, commit]); fs.rmSync(idx, { force: true });
  return commit;
}
/** The history file the gate keeps for a store (class-a-cache-history.mjs: named after the store's path). */
export const historyFileOf = (managed, storeDir) => path.join(managed, "runs", "rightkit-class-a-history", `${sha256(path.resolve(storeDir).toLowerCase())}.json`);
const moveAside = (from, to) => { if (!fs.existsSync(from)) return false; fs.mkdirSync(path.dirname(to), { recursive: true }); fs.renameSync(from, to); return true; };
/** Ends the gate and every program it started (Windows: the whole tree by pid; elsewhere the process group is the launcher's to signal). */
const killTree = (pid) => { try { if (process.platform === "win32") execFileSync("taskkill", ["/PID", String(pid), "/T", "/F"], { windowsHide: true, stdio: "ignore" }); else process.kill(pid, "SIGKILL"); } catch { /* already gone */ } };

/**
 * The machine's total CPU load, sampled every two seconds for the whole acceptance run (Adrian, 2026-11-25: "run the 7 cases on a quiet machine with CPU load
 * recorded"). A run's time limit (NN11) is only measured on a quiet machine; the record shows whether it was. Windows: typeperf; elsewhere nothing is sampled.
 */
export function startCpuSampler(file) {
  if (process.platform !== "win32") return { stop: () => {}, file: null };
  const child = spawn("typeperf", ["\\Processor(_Total)\\% Processor Time", "-si", "2", "-f", "CSV", "-o", file, "-y"], { windowsHide: true, stdio: "ignore" });
  return { file, stop: () => { try { child.kill(); } catch { /* gone */ } } };
}
/** Average and peak CPU % between two times, from typeperf's CSV ("MM/DD/YYYY HH:MM:SS.mmm","value"). Null when nothing was sampled in the window. */
export function cpuBetween(file, fromMs, toMs) {
  let text = ""; try { text = fs.readFileSync(file, "utf8"); } catch { return null; }
  const values = [];
  for (const line of text.split(/\r?\n/).slice(1)) {
    const m = /^"(\d\d)\/(\d\d)\/(\d{4}) (\d\d):(\d\d):(\d\d)\.(\d+)","([\d.]+)"/.exec(line); if (!m) continue;
    const at = new Date(Number(m[3]), Number(m[1]) - 1, Number(m[2]), Number(m[4]), Number(m[5]), Number(m[6]), Number(m[7].slice(0, 3))).getTime();
    if (at >= fromMs && at <= toMs) values.push(Number(m[8]));
  }
  if (!values.length) return null;
  return { samples: values.length, average: Math.round(values.reduce((a, b) => a + b, 0) / values.length), peak: Math.round(Math.max(...values)) };
}

async function main() {
  const argv = process.argv.slice(2); const opt = (n) => { const i = argv.indexOf(n); return i < 0 ? null : argv[i + 1]; };
  const fixture = opt("--fixture"); const out = opt("--out"); const gate = opt("--gate"); const managed = opt("--managed"); const real = opt("--real");
  if (!out || !gate || !managed || (!fixture && !real)) { console.error("usage: acceptance-run.mjs --fixture <dir> --out <dir> --gate <rightkit-gate.mjs> --managed <data root> [--cases 1,2,...] [--real <repo>]"); process.exit(2); }
  const casesFile = path.join(here, "acceptance-cases.json"); const casesBody = fs.readFileSync(casesFile); const cases = JSON.parse(casesBody.toString("utf8"));
  const receiptsDir = path.join(managed, "rightkit-build-control", "receipts");
  const pin = JSON.parse(fs.readFileSync(path.join(managed, "pins", `broker-${process.platform}.json`), "utf8")); const client = opt("--rustright") ?? "rustright";
  const verify = (body) => {
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), "rk-verify-")); const file = path.join(dir, "receipt.json"); fs.writeFileSync(file, body);
    const a = verifierArgv({ file, body, key: pin.publicKey, keyId: pin.keyId, host: pin.host, brokerDigest: pin.brokerDigest });
    try { if (!a) return { ok: false, problems: ["the receipt names no job id, nonce or request digest"] }; execFileSync(client, a, { stdio: ["ignore", "pipe", "pipe"], windowsHide: true, shell: process.platform === "win32" }); return { ok: true, problems: [] }; }
    catch (e) { return { ok: false, problems: [String(e.stderr ?? e.message).trim().split(/\r?\n/).pop()] }; }
    finally { fs.rmSync(dir, { recursive: true, force: true }); }
  };
  fs.mkdirSync(out, { recursive: true });
  const cpu = startCpuSampler(path.join(out, "cpu.csv"));
  process.on("exit", () => cpu.stop());
  const record = { schema: "rightkit.acceptance-run/1", casesSha256: sha256(casesBody), gate: path.resolve(gate), startedAt: new Date().toISOString(), cases: {}, realBuild: null };
  const results = {}; const refs = [];
  const one = async (repo, commit, name, { args, kill = false } = {}) => {
    const dir = path.join(out, name); fs.rmSync(dir, { recursive: true, force: true }); fs.mkdirSync(dir, { recursive: true });
    let killed = false; let timer = null;
    const onStart = kill ? ({ child, out: runDir }) => {
      // Killed midway: once the gate says Class A's verdict is written the compile is under way; a little later the whole tree is ended.
      timer = setInterval(() => { let log = ""; try { log = fs.readFileSync(path.join(runDir, "gate.log"), "utf8"); } catch { log = ""; } if (/class A verdict written/i.test(log)) { clearInterval(timer); setTimeout(() => { killed = true; killTree(child.pid); }, 8000); } }, 500);
    } : null;
    const code = await launch({ out: dir, commit, receiptsDir, repo, gate, gateArgs: args, onStart, lock: path.join(out, "acceptance-launch.lock") });
    if (timer) clearInterval(timer);
    let facts = factsOfRun(dir, { repo, receiptsDir, verify, killed });
    // The test programs the receipts sign, hashed now: straight after the run, before anything else can build.
    const imageNow = {};
    for (const j of facts.jobs) for (const exe of Array.isArray(j.testExecutables) ? j.testExecutables : []) { const f = typeof exe === "string" ? exe : exe?.path; if (typeof f !== "string") continue; try { imageNow[f.replace(/^\\\\\?\\/, "").replaceAll("\\", "/").toLowerCase()] = sha256(fs.readFileSync(f.replace(/^\\\\\?\\/, ""))); } catch { /* unreadable: no hash, which fails the line */ } }
    facts = { ...facts, imageNow };
    facts = { ...facts, cpu: cpu.file ? cpuBetween(cpu.file, Date.parse(JSON.parse(fs.readFileSync(path.join(dir, "launch.json"), "utf8")).startedAt), Date.parse(JSON.parse(fs.readFileSync(path.join(dir, "launch.json"), "utf8")).endedAt)) : null };
    console.log(`  ${name}: exit ${code}${killed ? " (killed)" : ""}, ${Math.round(facts.durationMs / 1000)} s, cpu ${facts.cpu ? `avg ${facts.cpu.average}% peak ${facts.cpu.peak}%` : "not sampled"}, ${facts.jobs.length} broker job(s), ${facts.starts.length} program(s), ${Array.isArray(facts.cacheWritten) ? facts.cacheWritten.length : "?"} store file(s) written`);
    return facts;
  };
  const lite = (f) => ({ dir: f.dir, commit: f.commit, exit: f.exit, killed: f.killed, durationMs: f.durationMs, cpu: f.cpu ?? null, jobs: f.jobs.length, programs: f.starts.length, cacheWritten: Array.isArray(f.cacheWritten) ? f.cacheWritten.length : null, processGaps: f.processGaps });

  if (fixture) {
    const pinned = cases.fixture.commit; if (git(fixture, ["cat-file", "-t", pinned]) !== "commit") throw new Error(`the fixture has no commit ${pinned}`);
    // A run killed during case 1 cannot put back the error it planted: the next run does, and only when that is the one change (anything else refuses).
    { const planted = path.join(fixture, "rk", "crates", "rk-leaf", "src", "lib.rs"); let body = ""; try { body = fs.readFileSync(planted, "utf8"); } catch { body = ""; }
      if (body === PLANTED && git(fixture, ["status", "--porcelain", "--untracked-files=no"]).trim() === "M rk/crates/rk-leaf/src/lib.rs") { git(fixture, ["checkout", "--", "rk/crates/rk-leaf/src/lib.rs"]); console.log("note: the error a killed case 1 planted was still in the checkout; it was put back"); } }
    if (git(fixture, ["status", "--porcelain", "--untracked-files=no"])) throw new Error("the fixture's working checkout has changes to tracked files; the cases need it clean to start");
    const storeDir = cacheStoreDir(fixture); const args = ["--to", cases.fixture.stage];
    const wanted = (opt("--cases") ?? cases.order.join(",")).split(",").map(Number);
    try {
      for (const id of cases.order.filter((n) => wanted.includes(n))) {
        const def = cases.cases.find((c) => c.id === id); console.log(`case ${id}: ${def.name}`);
        const commit = def.commit === "fixture" ? pinned : caseCommit(fixture, pinned, def.commit.edits, String(id), out);
        if (def.commit !== "fixture") refs.push(`refs/witness/acceptance-${id}`);
        const runs = [];
        if (id === 1) {
          const aside = path.join(out, "set-aside", new Date().toISOString().replace(/[:.]/g, "-"));
          moveAside(storeDir, path.join(aside, "rightkit-gate-cache")); moveAside(historyFileOf(managed, storeDir), path.join(aside, "history.json"));
          const dirty = path.join(fixture, "rk", "crates", "rk-leaf", "src", "lib.rs");
          fs.writeFileSync(dirty, "pub fn leaf() -> u32 { this does not compile\n");
          try { runs.push(await one(fixture, commit, "case1", { args })); } finally { git(fixture, ["checkout", "--", "rk/crates/rk-leaf/src/lib.rs"]); }
        } else if (id === 4) {
          const held = path.join(out, "case4-held"); fs.mkdirSync(held, { recursive: true }); const moved = [];
          for (const n of fs.readdirSync(storeDir).filter((x) => x.endsWith(".json"))) { let d = null; try { d = JSON.parse(fs.readFileSync(path.join(storeDir, n), "utf8")); } catch { d = null; } if (d?.row?.id === "license") { fs.renameSync(path.join(storeDir, n), path.join(held, n)); moved.push(n); } }
          if (!moved.length) throw new Error("case 4: the store holds no result for the row license to delete");
          try { runs.push(await one(fixture, commit, "case4", { args })); } finally { for (const n of moved) if (!fs.existsSync(path.join(storeDir, n))) fs.renameSync(path.join(held, n), path.join(storeDir, n)); }
        } else if (id === 7) {
          runs.push(await one(fixture, commit, "case7-killed", { args, kill: true }));
          runs.push(await one(fixture, commit, "case7-after", { args }));
        } else for (let i = 0; i < (def.runs ?? 1); i += 1) runs.push(await one(fixture, commit, `case${id}${(def.runs ?? 1) > 1 ? `-run${i + 1}` : ""}`, { args }));
        results[id] = decideCase({ ...def, runs }, cases); record.cases[id] = { commit, runs: runs.map(lite), lines: results[id] };
        for (const [line, r] of Object.entries(results[id])) console.log(`    ${line} ${r.verdict}: ${r.why}`);
        if (!argv.includes("--keep-going") && runs.some((r) => r.processGaps.length)) console.log("    note: the process record of a run is incomplete; lines that read it fail for that reason");
      }
    } finally { for (const r of refs) { try { git(fixture, ["update-ref", "-d", r]); } catch { /* reported by its absence */ } } }
  }
  let realLines = null;
  if (real) {
    console.log("the real build");
    const commit = git(real, ["rev-parse", "HEAD"]); const facts = await one(real, commit, "real-build", { args: cases.realBuild.args });
    realLines = decideRealBuild(facts, cases); record.realBuild = { repository: real, commit, run: lite(facts), lines: realLines };
    for (const [line, r] of Object.entries(realLines)) console.log(`    ${line} ${r.verdict}: ${r.why}`);
  }
  const all = decideAll({ cases, results, realBuild: realLines }); record.endedAt = new Date().toISOString(); record.verdict = all;
  fs.writeFileSync(path.join(out, "verdict.json"), `${JSON.stringify(record, null, 2)}\n`);
  console.log(`\n${all.passed} of ${all.of} lines pass${all.pass ? "; the real build passes its lines: ACCEPTED" : ""}`);
  for (const l of all.lines) console.log(`${l.id} ${l.verdict}${l.verdict === "pass" ? "" : `: ${l.why}`}`.slice(0, 1200));
  for (const l of all.realBuild) console.log(`real build ${l.id} ${l.verdict}${l.verdict === "pass" ? "" : `: ${l.why}`}`.slice(0, 600));
  process.exit(all.pass ? 0 : 1);
}
if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) main().catch((e) => { console.error(`acceptance-run: ${e.stack ?? e.message}`); process.exit(2); });
