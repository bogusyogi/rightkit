#!/usr/bin/env node
// Plans the jobs of .github/workflows/ci.yml (job `plan`). It compiles and tests nothing: it asks
// `gate.mjs --plan` what each runner OS has to verify, then cuts that into the shard jobs of
// scripts/ci/shards.mjs and writes them as GitHub Actions job outputs (JSON matrices).
//
//   node scripts/ci/ci-plan.mjs [--last-green <authenticated-results.json>] [--out <file>]
//
// A dispatch names its units (RK_UNITS or RK_UNITS_FILE) or an all-units exception (RK_ALL_UNITS_EXCEPTION): exactly one,
// else the plan fails. A push uses last-green.mjs's authenticated per-OS base (Actions API); with none recorded it selects
// the push's own diff (parent..sha). Nothing expands to every unit because evidence is missing. Pull requests use their
// merge base (gate.mjs resolves it); caches never prove green.
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { planHost, planHygiene, requiredShardCommands, resolveDispatchRequest, scanBase, selectionArgs, shardsConfig } from "./shards.mjs";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..", "..");
const RUNNERS = [
  { key: "windows", host: "windows", label: "windows-2025" },
  { key: "macos", host: "macos", label: "xcode-27" },
];
const argOf = (flag) => { const i = process.argv.indexOf(flag); return i > 0 ? process.argv[i + 1] : undefined; };
const isSha = (value) => /^[0-9a-f]{40}$/i.test(value ?? "");
const git = (...args) => spawnSync("git", args, { cwd: ROOT, encoding: "utf8" });

/** Consume only current-checkout results from required authenticated resolver step. */
function markerBase(file, runner) {
  try {
    const resolved = JSON.parse(fs.readFileSync(file, "utf8"));
    const head = git("rev-parse", "HEAD").stdout.trim();
    const result = resolved.results?.[runner.key];
    const proof = result?.provenance;
    if (!isSha(head) || resolved.currentHead !== head || !isSha(result?.base) || !proof) return null;
    if (proof.commit !== result.base || proof.repository !== process.env.GITHUB_REPOSITORY || proof.runner !== runner.label || proof.workflow !== ".github/workflows/ci.yml" || proof.markGreenJob !== `mark-green-${runner.key}` || proof.evidenceContract !== "rightkit-shared-ci-last-green/v1") return null;
    if (git("cat-file", "-e", `${result.base}^{commit}`).status !== 0) return null;
    return git("merge-base", "--is-ancestor", result.base, head).status === 0 ? result.base : null;
  } catch { return null; }
}

/** One `gate.mjs --plan` for a host. The gate must echo the host back: an old gate silently ignoring --host would plan the wrong OS. */
function gatePlan(host, extra) {
  const run = spawnSync(process.execPath, [path.join(ROOT, "scripts", "ci", "gate.mjs"), "--plan", "--host", host, ...extra], { cwd: ROOT, encoding: "utf8", maxBuffer: 256 * 1024 * 1024 });
  if (run.status !== 0) throw new Error(`gate --plan --host ${host} failed: ${run.stderr || run.stdout}`);
  const plan = JSON.parse(run.stdout);
  if (plan.host !== host) throw new Error(`gate --plan answered for host ${plan.host}, not ${host}: apply the --host change to scripts/ci/gate.mjs`);
  return plan;
}

function main() {
  const event = process.env.GITHUB_EVENT_NAME ?? "push";
  const request = event === "workflow_dispatch" ? resolveDispatchRequest({
    units: process.env.RK_UNITS, unitsFile: process.env.RK_UNITS_FILE, exceptionId: process.env.RK_ALL_UNITS_EXCEPTION,
    read: (file) => fs.readFileSync(path.join(ROOT, file), "utf8"),
    isTracked: (file) => git("ls-files", "--error-unmatch", "--", file).status === 0,
  }) : null;
  const parent = event === "push" ? git("rev-parse", "--verify", "HEAD^").stdout.trim() : null;
  const markers = argOf("--last-green");
  const config = JSON.parse(fs.readFileSync(path.join(ROOT, "ci/gate.config.json"), "utf8"));
  const shards = shardsConfig(JSON.parse(fs.readFileSync(path.join(ROOT, "ci/shards.json"), "utf8")));

  const everything = gatePlan("linux", ["--universe"]); // the catalogue of every unit; lists, never runs
  const universe = { rust: everything.rust.test, npm: everything.npm.test };
  const packages = universe.npm.map((dir) => ({ ...JSON.parse(fs.readFileSync(path.join(ROOT, dir, "package.json"), "utf8")), dir }));
  const swiftTests = ["swift/RightKitSwift"].filter((dir) => fs.existsSync(path.join(ROOT, dir, "Tests")));

  const output = {};
  const selections = [];
  const evidence = [];
  const summary = ["## RightKit CI plan", "", "| Runner | Base | Rust crates | Dependents | npm | Root | Swift | Shard jobs | Closure jobs |", "|---|---|---|---|---|---|---|---|---|"];
  for (const runner of RUNNERS) {
    // A dispatch never reads the baseline: it names its units. A pull request leaves --base off (the gate compares it with its merge base).
    const base = event === "push" ? markerBase(markers, runner) : null;
    const { args: extra, how } = selectionArgs({ event, request, base, parent });
    if (event === "push" && !base) console.log(`ci-plan: ${runner.label} has no last-green baseline; selecting the push diff ${parent.slice(0, 10)}..HEAD, not every unit`);
    const selection = gatePlan(runner.host, extra);
    if (selection.all && request?.kind !== "all") throw new Error(`${runner.label}: the gate selected every unit (${how}); only an all-units exception may`);
    selections.push(selection);
    const plan = planHost({ host: runner.host, label: runner.label, universe, selection, config, shards });
    for (const [step, jobs] of [["test", plan.shards], ["closure", plan.closure]]) {
      for (const job of jobs) evidence.push({ artifact: `qa-evidence-${runner.label}-${job.id}`, host: runner.host, tokens: job.tokens, steps: step, commands: requiredShardCommands(job.tokens, { config, packages, swiftTests }) });
    }
    output[`${runner.key}_shards`] = JSON.stringify({ include: plan.shards });
    output[`${runner.key}_shard_count`] = String(plan.shards.length);
    output[`${runner.key}_closure`] = JSON.stringify({ include: plan.closure });
    output[`${runner.key}_closure_count`] = String(plan.closure.length);
    output[`${runner.key}_base`] = selection.base ?? "";
    summary.push(`| ${runner.label} | ${how}${selection.base ? ` ${selection.base.slice(0, 10)}` : ""} | ${selection.rust.test.length} | ${selection.rust.check.length} | ${selection.npm.test.length} | ${selection.root.length} | ${selection.swift.length} | ${plan.shards.length} | ${plan.closure.length} |`);
  }
  const hygiene = planHygiene(selections);
  output.evidence = JSON.stringify(evidence);
  // A run proves a baseline for later diffs only if it verified every unit; a named subset proves just itself.
  output.proves_baseline = String(request?.kind === "all");
  output.hygiene_tokens = hygiene.tokens;
  output.scan_base = scanBase(selections, (...args) => git(...args));
  output.scan_crates = hygiene.crates.join(" ");
  output.scan_packages = hygiene.packages.join(" ");

  summary.push("", `Hygiene: ${hygiene.crates.length} crate(s) and ${hygiene.packages.length} package(s) for lock, fmt and the secret scan.`);

  const text = `${summary.join("\n")}\n`;
  console.log(text);
  console.log(JSON.stringify(output, null, 1));
  const out = argOf("--out");
  if (out) fs.writeFileSync(out, JSON.stringify(output, null, 2));
  // The evidence plan grows with the units a run touches (132 KB for a run over every unit) and a job output reaches the
  // summary step as one environment string, which Linux caps at 128 KiB ("Argument list too long", run 40034020934). It
  // travels as a file (the ci-plan artifact) instead; every other output stays a job output.
  const evidenceOut = argOf("--evidence-out");
  if (evidenceOut) {
    fs.mkdirSync(path.dirname(evidenceOut), { recursive: true });
    fs.writeFileSync(evidenceOut, output.evidence);
  }
  if (process.env.GITHUB_STEP_SUMMARY) fs.appendFileSync(process.env.GITHUB_STEP_SUMMARY, text);
  if (process.env.GITHUB_OUTPUT) {
    // One line per output; none of these values contains a newline.
    fs.appendFileSync(process.env.GITHUB_OUTPUT, `${Object.entries(output).filter(([k]) => k !== "evidence").map(([k, v]) => `${k}=${v}`).join("\n")}\n`);
  }
}

try { main(); } catch (error) { console.error(`ci-plan: ${error.message}`); process.exit(1); }
