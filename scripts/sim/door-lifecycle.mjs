#!/usr/bin/env node
// Prolonged simulation 1 of 2: the door's whole lifecycle, end to end, through the trusted publisher.
//
//   node scripts/sim/door-lifecycle.mjs [--seed <n>] [--reps <n>] [--out <dir>]
//
// Each trial is one seeded release of a throwaway app repository through the real rightkit-gate (packages/release/bin/rightkit-gate.mjs):
// a host runs check -> compile -> package -> qualify, `stage` hands the sealed set and receipt to the publisher's draft release, and a
// fresh publisher runner (no receipt store, the staged bytes copied in) runs `--to release --receipt-in`. Only the outside world is
// simulated: right-release (qa / build / upload), gh, tar and the executor. Builds write real sealed manifests that the gate hashes.
//
// A trial is a positive control (no fault: exactly one upload, of the bytes that were built and qualified) or one fault drawn from
// MECHANISMS, each a distinct way the pipeline can be driven wrong. Every trial is judged by the same invariants, plus the fault's own
// expectation. The first failure of each mechanism is preserved (seed, trial, transcript) in <out>/first-failures.json, so it replays
// with `--seed <seed> --reps 1 --only <trial seed>`.
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import crypto from "node:crypto";
import { fileURLToPath } from "node:url";
import { spawnSync } from "../../packages/release/background-process.mjs";
import { main } from "../../packages/release/bin/rightkit-gate.mjs";

/** Each mechanism: how it corrupts the run, and what must then hold. `refuse`: no upload may happen at all; `expect`: the refusal
 * must be this one (a gate that refuses everything, or for another reason, fails the trial instead of passing it). */
export const MECHANISMS = {
  "class-a-red": { refuse: true, expect: /stopped at check/, what: "class A fails on the host" },
  "compile-red": { refuse: true, expect: /stopped at compile/, what: "the compile fails" },
  "source-qa-red": { refuse: true, expect: /stopped at package/, what: "source QA fails while packaging" },
  "build-red": { refuse: true, expect: /stopped at package/, what: "the signed build fails" },
  "build-seals-nothing": { refuse: true, expect: /sealed nothing/, what: "the build exits 0 but writes no sealed set" },
  "installed-qa-red": { refuse: true, expect: /stopped at qualify/, what: "installed QA fails on the qualifying host" },
  "dirty-checkout": { refuse: true, expect: /checkout differs from HEAD/, what: "a tracked file is edited before the host run" },
  "bytes-swapped-after-qualify": { refuse: true, expect: /stage needs this host's receipt/, what: "the sealed bytes change between qualify and stage" },
  "staged-bytes-swapped": { refuse: true, expect: /--to release only uploads/, what: "the archive the publisher unpacks is not the qualified one" },
  "receipt-other-commit": { refuse: true, expect: /is not a rightkit\.gate-receipt\/1 receipt/, what: "the publisher checks out a later commit than the receipt's" },
  "receipt-gate-forged": { refuse: true, expect: /is not a rightkit\.gate-receipt\/1 receipt/, what: "the receipt names another gate digest" },
  "receipt-lock-forged": { refuse: true, expect: /is not a rightkit\.gate-receipt\/1 receipt/, what: "the receipt names other lockfiles" },
  "receipt-corrupt": { refuse: true, expect: /is not a rightkit\.gate-receipt\/1 receipt/, what: "the receipt is truncated JSON" },
  "receipt-missing-qualify": { refuse: true, expect: /--to release only uploads|was not qualified/, what: "the receipt has no qualify stage" },
  "publisher-env-missing": { refuse: true, expect: /runs only in the trusted publisher/, what: "release runs outside the protected publication job" },
  "tier-missing": { refuse: true, expect: /needs --tier/, what: "release runs with no explicit tier" },
  "stage-before-qualify": { refuse: true, expect: /stage needs this host's receipt/, what: "the host stages after package only" },
  "executor-refused": { refuse: true, expect: /BLOCKED: this host is managed/, what: "a managed host without a reachable broker" },
  "partial-check-admitted": { refuse: true, expect: /PARTIAL/, what: "a caller takes `check --only` as its admission" },
  "upload-red": { refuse: false, expect: null, what: "the first upload fails; a retry must upload the same qualified bytes" },
};

const here = path.dirname(fileURLToPath(import.meta.url));
const GATE_BIN = path.resolve(here, "..", "..", "packages", "release", "bin", "rightkit-gate.mjs");

/** mulberry32: a small, seedable generator, so every trial replays from its seed. */
export function rng(seed) {
  let a = seed >>> 0;
  return () => { a = (a + 0x6d2b79f5) >>> 0; let t = a; t = Math.imul(t ^ (t >>> 15), t | 1); t ^= t + Math.imul(t ^ (t >>> 7), t | 61); return ((t ^ (t >>> 14)) >>> 0) / 4294967296; };
}
const pick = (r, xs) => xs[Math.floor(r() * xs.length)];

function capture() {
  const out = []; const err = [];
  return { out, err, io: { out: (s) => out.push(String(s)), err: (s) => err.push(String(s)) } };
}

/** One trial. Returns { ok, problems, transcript }. */
export async function trial({ seed, mechanism, mutant = null }) {
  const r = rng(seed);
  const platform = pick(r, ["mac", "win"]);
  const tier = pick(r, ["patch", "update"]);
  const executor = pick(r, ["direct", "broker"]);
  const extraCommits = Math.floor(r() * 3);
  // host: one host packages and qualifies; split: a packaging runner hands its receipt to a fresh qualifying runner.
  const flow = pick(r, ["host", "split"]);
  const rerun = r() < 0.5; // the host runs its stage twice before staging (receipt reuse on the same inputs)
  const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "rk-sim-door-")));
  const transcript = [`seed=${seed} mechanism=${mechanism ?? "positive"} platform=${platform} tier=${tier} executor=${executor} extraCommits=${extraCommits} flow=${flow} rerun=${rerun}${mutant ? ` MUTANT=${mutant}` : ""}`];
  const problems = [];
  const uploads = []; const builds = []; const qualified = [];
  // A fault counts only once it was actually injected: a trial whose fault never applied is reported apart, never as a negative.
  let applied = !mechanism; const hit = (m) => { if (mechanism === m) applied = true; };
  try {
    const git = (...a) => spawnSync("git", a, { cwd: root, encoding: "utf8" });
    git("init", "-q"); git("config", "user.email", "sim@example.test"); git("config", "user.name", "sim");
    fs.writeFileSync(path.join(root, ".gitignore"), ".right-release/\n");
    fs.writeFileSync(path.join(root, "Cargo.lock"), `version = 4\n# ${seed}\n`);
    git("add", "-A"); git("commit", "-q", "-m", "app");
    for (let i = 0; i < extraCommits; i++) { fs.writeFileSync(path.join(root, `f${i}.txt`), `${seed}-${i}\n`); git("add", "-A"); git("commit", "-q", "-m", `c${i}`); }
    const head = () => git("rev-parse", "HEAD").stdout.trim();
    const sealedDir = (commit) => path.join(root, ".right-release", "sealed", `app-1.0.0-${commit.slice(0, 8)}`, platform);
    const manifest = (commit) => path.join(sealedDir(commit), "release-manifest.json");
    const digestOnDisk = (commit) => { try { return crypto.createHash("sha256").update(fs.readFileSync(manifest(commit))).digest("hex"); } catch { return null; } };
    let buildNo = 0; let uploadFailuresLeft = mechanism === "upload-red" ? 1 : 0;
    // right-release, simulated: it sees only its arguments and the door run's environment, like the real CLI.
    const rightRelease = (runner) => (cmd, a, opts) => {
      const args = a.slice(1); const verb = args[0];
      transcript.push(`[${runner}] right-release ${args.join(" ")}${opts?.env?.RK_GATE_RUN ? " (door run)" : " (NO door run)"}`);
      if (!opts?.env?.RK_GATE_RUN) problems.push(`right-release ${verb} ran outside a door run`);
      const commit = head();
      if (verb === "qa" && args.includes("source")) { hit("source-qa-red"); return { status: mechanism === "source-qa-red" ? 1 : 0 }; }
      if (verb === "build") {
        if (runner === "publisher") problems.push("the publisher built: it may only upload qualified bytes");
        hit("build-red"); hit("build-seals-nothing");
        if (mechanism === "build-red") return { status: 1 };
        if (mechanism !== "build-seals-nothing") {
          fs.mkdirSync(sealedDir(commit), { recursive: true });
          fs.writeFileSync(manifest(commit), JSON.stringify({ commit, platform, build: ++buildNo, seed }));
          builds.push(digestOnDisk(commit));
        }
        return { status: 0 };
      }
      if (verb === "qa" && args.includes("installed")) {
        if (runner === "publisher") problems.push("the publisher ran installed QA: it cannot, and must take the host's result");
        hit("installed-qa-red");
        if (mechanism === "installed-qa-red") return { status: 1 };
        qualified.push(digestOnDisk(commit));
        return { status: 0 };
      }
      if (verb === "upload") {
        if (runner !== "publisher") problems.push(`upload ran on ${runner}`);
        if (uploadFailuresLeft > 0) { hit("upload-red"); uploadFailuresLeft--; return { status: 1 }; }
        uploads.push({ runner, digest: digestOnDisk(commit), tier: args[args.indexOf("--tier") + 1] });
        return { status: 0 };
      }
      return { status: 0 };
    };
    const executorOf = () => (hit("executor-refused"), mechanism === "executor-refused" ? { kind: "refuse", problem: "this host is managed by RustRight but its broker is not reachable" } : { kind: executor, cargo: executor === "broker" ? ["rustright", "cargo"] : ["cargo"] });
    const staged = {};
    const outside = (cmd, a, opts) => {
      if (cmd === "tar") { staged.archive = digestOnDisk(head()); transcript.push(`[host] tar ${a[0]}`); return { status: 0, stdout: "", stderr: "" }; }
      if (cmd === "gh") {
        transcript.push(`[host] gh ${a.slice(0, 2).join(" ")}`);
        if (a[0] === "release" && a[1] === "upload") staged.receipt = fs.readFileSync(a[4], "utf8");
        if (a[0] === "release" && a[1] === "view") return { status: 1, stdout: "", stderr: "" };
        if (a[0] === "repo") return { status: 0, stdout: "bogusyogi/app\n", stderr: "" };
        if (a[0] === "workflow") staged.dispatched = true;
        return { status: 0, stdout: "", stderr: "" };
      }
      return spawnSync(cmd, a, { ...opts, encoding: "utf8" });
    };
    const run = async (runner, args, extra = {}) => {
      const c = capture();
      // Oracle self-test: a gate that refuses everything must turn the suite red (positives and wrong-reason refusals).
      if (mutant === "refuse-all") { transcript.push(`[${runner}] rightkit-gate ${args.join(" ")} -> 1 (mutant: refused)`); return 1; }
      const code = await main(args, {
        ...c.io, cwd: root, spawn: outside, resolveExecutor: executorOf,
        runCheck: async () => (hit("class-a-red"), mechanism === "class-a-red" ? 1 : 0), runCompile: async () => (hit("compile-red"), mechanism === "compile-red" ? 1 : 0),
        spawnInherit: rightRelease(runner), ...extra,
      });
      transcript.push(`[${runner}] rightkit-gate ${args.join(" ")} -> ${code}`, ...c.err.map((l) => `  ${l}`));
      return code;
    };

    // ---- the host -------------------------------------------------------------------------------------------------------------
    if (mechanism === "dirty-checkout") { fs.writeFileSync(path.join(root, "Cargo.lock"), "edited\n"); hit("dirty-checkout"); }
    hit("stage-before-qualify");
    if (mechanism === "partial-check-admitted") {
      // A consumer that admits on exit 0 (the broker, `--to check`, a CI `run:` step) is handed the partial view.
      hit("partial-check-admitted");
      const code = await run("host", ["check", "--only", "release-config"], { appCheck: async () => 0 });
      if (code === 0) problems.push("a green `check --only` exited 0: an exit-0 consumer would admit a partial view");
      if (!/PARTIAL/.test(transcript.join("\n"))) problems.push("partial-check-admitted: the partial view did not say it was partial");
      return { ok: problems.length === 0, applied, problems, transcript, config: { platform, tier, executor, extraCommits, flow, rerun } };
    }
    const hostTo = mechanism === "stage-before-qualify" ? "package" : "qualify";
    let hostCode;
    if (flow === "split" && hostTo === "qualify") {
      const handoff = path.join(root, "..", `${path.basename(root)}-package.json`);
      try {
        hostCode = await run("package-runner", ["--to", "package", "--platform", platform, "--receipt-out", handoff]);
        if (hostCode === 0) {
          fs.rmSync(path.join(root, ".git", "rightkit-gate"), { recursive: true, force: true });
          hostCode = await run("host", ["--to", "qualify", "--platform", platform, "--receipt-in", handoff]);
        }
      } finally { fs.rmSync(handoff, { force: true }); }
    } else hostCode = await run("host", ["--to", hostTo, "--platform", platform]);
    if (rerun && hostCode === 0) hostCode = await run("host", ["--to", hostTo, "--platform", platform]);
    if (mechanism === "bytes-swapped-after-qualify" && digestOnDisk(head())) { fs.appendFileSync(manifest(head()), " "); hit(mechanism); }
    const stageCode = await run("host", ["stage", "--platform", platform, "--tier", tier]);
    if (stageCode !== 0 || !staged.receipt) {
      transcript.push("nothing staged: the publisher is never dispatched");
    } else {
      // ---- the publisher: a fresh runner, the staged commit checked out, the staged bytes unpacked ----------------------------
      const commit = head();
      for (const m of ["staged-bytes-swapped", "receipt-other-commit", "receipt-gate-forged", "receipt-lock-forged", "receipt-corrupt", "receipt-missing-qualify", "publisher-env-missing", "tier-missing"]) hit(m);
      fs.rmSync(path.join(root, ".git", "rightkit-gate"), { recursive: true, force: true });
      let receiptText = staged.receipt;
      const receipt = JSON.parse(receiptText);
      if (mechanism === "receipt-gate-forged") receiptText = JSON.stringify({ ...receipt, binding: { ...receipt.binding, gate: "0".repeat(64) } });
      if (mechanism === "receipt-lock-forged") receiptText = JSON.stringify({ ...receipt, binding: { ...receipt.binding, locks: { "Cargo.lock": "0".repeat(64) } } });
      if (mechanism === "receipt-corrupt") receiptText = receiptText.slice(0, Math.floor(receiptText.length / 2));
      if (mechanism === "receipt-missing-qualify") { const s = { ...receipt.stages }; delete s.qualify; receiptText = JSON.stringify({ ...receipt, stages: s }); }
      if (mechanism === "staged-bytes-swapped") fs.writeFileSync(manifest(commit), JSON.stringify({ commit, platform, build: "attacker" }));
      if (mechanism === "receipt-other-commit") { fs.writeFileSync(path.join(root, "later.txt"), "later\n"); git("add", "-A"); git("commit", "-q", "-m", "later"); }
      const receiptFile = path.join(root, "..", `${path.basename(root)}-receipt.json`);
      fs.writeFileSync(receiptFile, receiptText);
      const env = mechanism === "publisher-env-missing" ? { ...process.env, GITHUB_ACTIONS: undefined, RIGHTKIT_PUBLISHER: undefined } : { ...process.env, GITHUB_ACTIONS: "true", RIGHTKIT_PUBLISHER: "github-environment" };
      const releaseArgs = ["--to", "release", "--platform", platform, ...(mechanism === "tier-missing" ? [] : ["--tier", tier]), "--receipt-in", receiptFile];
      try {
        let code = await run("publisher", releaseArgs, { env });
        if (mechanism === "upload-red") {
          if (code === 0) problems.push("a failed upload reported success");
          code = await run("publisher", releaseArgs, { env });
          if (code !== 0) problems.push("the retry after a failed upload did not publish");
        }
      } finally { fs.rmSync(receiptFile, { force: true }); }
    }
    transcript.push(`host=${hostCode} stage=${stageCode} builds=${builds.length} qualified=${qualified.length} uploads=${uploads.length}`);

    // ---- invariants, every trial ------------------------------------------------------------------------------------------------
    for (const u of uploads) {
      if (!qualified.includes(u.digest)) problems.push(`uploaded bytes ${String(u.digest).slice(0, 12)} were never qualified`);
      if (!builds.includes(u.digest)) problems.push(`uploaded bytes ${String(u.digest).slice(0, 12)} were never built by the host`);
      if (u.tier !== tier) problems.push(`uploaded with tier ${u.tier}, not the chosen ${tier}`);
    }
    if (uploads.length > 1) problems.push(`${uploads.length} uploads for one release`);
    if (!mechanism && uploads.length !== 1) problems.push(`positive control: expected exactly one upload, saw ${uploads.length}`);
    if (mechanism && MECHANISMS[mechanism].refuse && uploads.length) problems.push(`${mechanism}: expected no upload, saw ${uploads.length}`);
    if (mechanism === "upload-red" && uploads.length !== 1) problems.push(`upload-red: expected exactly one upload after the retry, saw ${uploads.length}`);
    // The refusal must be the fault's own, not any refusal: a gate that refused everything would otherwise pass every negative.
    const expect = mechanism ? MECHANISMS[mechanism].expect : null;
    if (applied && expect && !expect.test(transcript.join("\n"))) problems.push(`${mechanism}: refused, but not with ${expect}`);
  } catch (e) {
    problems.push(`trial crashed: ${e?.stack ?? e}`);
  } finally {
    fs.rmSync(root, { recursive: true, force: true });
  }
  return { ok: problems.length === 0, applied, problems, transcript, config: { platform, tier, executor, extraCommits, flow, rerun } };
}

/** sha256 of each file the verdicts depend on: the gate under test and this simulation. */
export function codeIdentity(files) {
  return Object.fromEntries(files.map((f) => [path.relative(path.resolve(here, "..", ".."), f), crypto.createHash("sha256").update(fs.readFileSync(f)).digest("hex")]));
}

async function cli(argv) {
  const opt = (n, d) => { const i = argv.indexOf(n); return i < 0 ? d : argv[i + 1]; };
  const seed = Number(opt("--seed", "1")); const per = Number(opt("--per", "100"));
  const only = opt("--only", null); const mutant = opt("--mutant", null);
  const rel = path.resolve(here, "..", "..", "packages", "release");
  const identity = codeIdentity([GATE_BIN, path.join(rel, "gate-stages.mjs"), fileURLToPath(import.meta.url)]);
  const codeId = crypto.createHash("sha256").update(JSON.stringify(identity)).digest("hex").slice(0, 12);
  // Every run gets its own directory, named by seed, repetitions and code identity, and never overwrites an earlier one: a first red
  // run stays next to the repaired green one.
  const out = path.resolve(opt("--out", path.join(os.tmpdir(), "rk-sim")), `door-lifecycle-seed${seed}-per${per}-${codeId}${only ? `-only${only}` : ""}${mutant ? `-mutant-${mutant}` : ""}`);
  if (fs.existsSync(out)) { process.stderr.write(`door-lifecycle: ${out} exists; a run is never overwritten\n`); return 2; }
  fs.mkdirSync(out, { recursive: true });
  // `per` seeded repetitions of every mechanism and `per` positive controls.
  const names = Object.keys(MECHANISMS);
  const r = rng(seed);
  const plan = [];
  for (const mechanism of [null, ...names]) for (let i = 0; i < per; i++) plan.push({ seed: Math.floor(r() * 2 ** 31), mechanism });
  const todo = only ? plan.filter((p) => String(p.seed) === only) : plan;
  const results = fs.openSync(path.join(out, "results.jsonl"), "w");
  const firstFailures = {}; const tally = {}; const configs = {};
  for (const p of todo) {
    const key = p.mechanism ?? "positive";
    const res = await trial({ ...p, mutant });
    fs.writeSync(results, `${JSON.stringify({ key, seed: p.seed, ok: res.ok, applied: res.applied, config: res.config, problems: res.problems, ...(res.ok ? {} : { transcript: res.transcript }) })}\n`);
    const t = (tally[key] ??= { runs: 0, applied: 0, failed: 0, distinctConfigs: 0 }); t.runs++;
    if (res.applied) t.applied++;
    const ck = JSON.stringify(res.config); (configs[key] ??= new Set()).add(ck); t.distinctConfigs = configs[key].size;
    if (!res.ok) { t.failed++; firstFailures[key] ??= { seed: p.seed, problems: res.problems, transcript: res.transcript }; }
  }
  fs.closeSync(results);
  const neg = Object.entries(tally).filter(([k]) => k !== "positive");
  const summary = {
    suite: "door-lifecycle", seed, per, mutant, identity, codeId,
    positives: tally.positive ?? null,
    distinctNegativeMechanisms: { defined: names.length, executed: neg.filter(([, t]) => t.applied > 0).length },
    negativeTrials: { planned: neg.reduce((n, [, t]) => n + t.runs, 0), executed: neg.reduce((n, [, t]) => n + t.applied, 0), notApplied: neg.reduce((n, [, t]) => n + t.runs - t.applied, 0), distinctConfigs: neg.reduce((n, [, t]) => n + t.distinctConfigs, 0) },
    tally, failed: Object.keys(firstFailures),
  };
  fs.writeFileSync(path.join(out, "summary.json"), `${JSON.stringify(summary, null, 2)}\n`);
  fs.writeFileSync(path.join(out, "first-failures.json"), `${JSON.stringify(firstFailures, null, 2)}\n`);
  for (const [k, v] of Object.entries(tally)) process.stdout.write(`${v.failed ? "FAIL" : "pass"} ${k}: ${v.runs - v.failed}/${v.runs} (applied ${v.applied}, distinct configs ${v.distinctConfigs})\n`);
  const n = summary.negativeTrials;
  process.stdout.write(`door-lifecycle ${codeId}: seed ${seed}, ${per} per scenario; mechanisms ${summary.distinctNegativeMechanisms.executed}/${names.length} executed; negative trials ${n.executed} executed of ${n.planned} (${n.notApplied} not applied, ${n.distinctConfigs} distinct configs); positives ${summary.positives?.runs ?? 0}; ${summary.failed.length ? `RED: ${summary.failed.join(", ")}` : "green"}; ${out}\n`);
  return summary.failed.length ? 1 : 0;
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) process.exitCode = await cli(process.argv.slice(2));
