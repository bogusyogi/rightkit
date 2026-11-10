#!/usr/bin/env node
// `right-release publish-local cargo --crate <name> --commit <sha> --compile-report <file>`: the local crates route (Adrian, 2026-11-03,
// rightkit#1471: "the npm and crates are both local ... feel free to push but no ci"). The crates.io token is cargo's own,
// stored by cargo's macOS Keychain provider; on the Mac this command reads it through /usr/bin/security and hands it to its one `cargo publish`
// call (cargo's own provider is admitted per program path, which the broker's pinned toolchain changes).
//
// It publishes exactly one commit's crate, and only when:
//   1. the commit is on origin/main;
//   2. a unit-gate report (`rightkit-gate --to compile --unit cargo:<name> --commit <sha> --receipt-out <file>`, schema
//      rightkit.unit-gate-report/1) from either broker host names this crate and version, this exact commit and its tree, with the
//      check and compile stages ok: the gate's Class A and its broker compile of that commit are the verification;
//      or a unit-list receipt (`rightkit-gate --to compile --units-file <file> --receipt-dir <dir>`, schema
//      rightkit.unit-list-compile/1, one per unit of one shared run) names this crate with status "passed", this exact commit, and
//      binds every recorded command to this commit and tree with a passing broker seam receipt (rightkit#1560);
//   3. the version is not on crates.io yet.
// The commit's `crates/` folder is exported (git archive) to a temporary folder; `cargo package` and `cargo publish` run there with
// `--no-verify --locked` and compile nothing, so they run with the broker's own cargo directly, as the gate's Class A cargo does
// (GATE-CONTRACT GC-A11; RustRight serves only a repository's primary checkout). The crates.io index checksum must then equal the
// sha256 of the packaged .crate.
import crypto from "node:crypto";
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { directCargoPath } from "./managed-cargo.mjs";

export const UNIT_REPORT_SCHEMA = "rightkit.unit-gate-report/1";
/** The per-unit receipt a unit-list run writes (`rightkit-gate --to compile --units-file <file> --receipt-dir <dir>`): one shared run, one receipt per unit (rightkit#1560). */
export const UNIT_LIST_COMPILE_SCHEMA = "rightkit.unit-list-compile/1";

/**
 * Why a unit-list receipt does not prove this crate at this commit; empty when it does. The gate writes status "passed" only when its
 * shared run exited 0, its union was verified and every requirement of the unit passed (rightkit-gate.mjs, the unit-list report). This
 * re-checks what the file itself carries: the unit, the exact commit, every recorded command bound to that commit and tree with a
 * passing broker seam receipt as its last attempt, every requirement and every scan passed (a scan with evidence of its own must show a
 * broker seam receipt; one answered from the gate's cache has none). The receipt names no version: the caller
 * reads the version from the commit's own Cargo.toml, and the commit and tree are bound here.
 */
export function unitListCompileProblems(report, { name, commit, tree }) {
  const problems = [];
  const unit = `cargo:${name}`;
  const passed = (status) => status === "pass" || status === "passed";
  const seamReceipt = (e) => e?.seam === true && typeof e.requestId === "string" && e.requestId !== "" && /^[0-9a-f]{64}$/.test(e.receiptSha256 ?? "");
  if (report?.unit !== unit) problems.push(`unit is ${JSON.stringify(report?.unit)}, not ${unit}`);
  if (report?.status !== "passed") problems.push(`unit status is ${JSON.stringify(report?.status)}, not "passed"`);
  if (report?.commit !== commit) problems.push(`commit ${report?.commit} != ${commit}`);
  const commands = Array.isArray(report?.commands) ? report.commands : [];
  if (!commands.length) problems.push("no broker command is recorded for the unit");
  for (const c of commands) {
    if (c?.selector !== unit) problems.push(`command ${c?.id} belongs to ${JSON.stringify(c?.selector)}`);
    if (c?.commit !== commit) problems.push(`command ${c?.id} ran on commit ${c?.commit}`);
    if (c?.tree !== tree) problems.push(`command ${c?.id} ran on tree ${c?.tree}, not ${tree}`);
    const last = Array.isArray(c?.attempts) ? c.attempts.at(-1) : undefined;
    if (!last || !passed(last.status) || !seamReceipt(last.snapshotEvidence)) problems.push(`command ${c?.id} has no passing broker seam receipt as its last attempt`);
  }
  const requirements = Array.isArray(report?.requirements) ? report.requirements : [];
  if (!requirements.length) problems.push("no requirement is recorded for the unit");
  for (const r of requirements) if (r?.selector !== unit || !passed(r?.status)) problems.push(`requirement ${r?.id} (${JSON.stringify(r?.selector)}) is ${JSON.stringify(r?.status)}`);
  const scans = Array.isArray(report?.scans) ? report.scans : [];
  if (!scans.length) problems.push("no scan is recorded for the unit");
  // A scan the run answered from the gate's cache carries no evidence of its own (the pass was stored by the run that executed it); a scan
  // that carries evidence must carry a broker seam receipt. The unit's own commands above are never answered without one.
  for (const sc of scans) {
    if (!passed(sc?.status)) problems.push(`scan ${sc?.id} is ${JSON.stringify(sc?.status)}`);
    else if (sc.snapshotEvidence !== undefined && !seamReceipt(sc.snapshotEvidence)) problems.push(`scan ${sc?.id} carries evidence that is not a broker seam receipt`);
  }
  return problems;
}

/** The Keychain item cargo's macOS provider stores the crates.io token under. */
export const CARGO_KEYCHAIN_SERVICE = "cargo-registry:https://github.com/rust-lang/crates.io-index";
const KEYCHAIN_READ_MS = 60_000;
const PUBLISH_CALL_MS = 10 * 60_000;

function fail(message) { throw new Error(message); }

export function parseArgs(argv) {
  const out = { crate: null, commit: null, report: null, repo: process.cwd() };
  for (let i = 0; i < argv.length; i += 1) {
    if (argv[i] === "--crate") out.crate = argv[++i];
    else if (argv[i] === "--commit") out.commit = argv[++i];
    else if (argv[i] === "--compile-report") out.report = path.resolve(argv[++i]);
    else if (argv[i] === "--repo") out.repo = path.resolve(argv[++i]);
    else fail(`unknown option ${argv[i]}`);
  }
  if (!/^rightkit-[a-z0-9-]+$/.test(out.crate ?? "")) fail("--crate rightkit-<name> is required");
  if (!/^[0-9a-f]{7,40}$/.test(out.commit ?? "")) fail("--commit <sha> is required");
  if (!out.report) fail("--compile-report <file> is required: the unit-gate report of `rightkit-gate --to compile --unit cargo:<name> --commit <sha> --receipt-out <file>`");
  return out;
}

/** The per-unit result of a together compile (gate/compile.mjs unitResult): each crate carries its own result and the broker job that proves each call. */
export const UNIT_RESULT_SCHEMA = "rightkit.unit-result/1";

/**
 * Why a unit result does not prove this crate at this commit; empty when it does. It must name the unit, the exact commit and tree, say
 * "passed", show lint, test build and tests passed (docs too when the crate has a library call), and every call that named the crate must have
 * passed with a broker job as evidence (a request id and the sha256 of its signed receipt). A call skipped because it had already passed for this
 * commit carries the job that proved it then.
 */
export function unitResultProblems(report, { name, commit, tree }) {
  const problems = [];
  const unit = `cargo:${name}`;
  const job = (e) => typeof e?.requestId === "string" && e.requestId !== "" && /^[0-9a-f]{64}$/.test(e.receiptSha256 ?? "");
  if (report?.unit !== unit) problems.push(`unit is ${JSON.stringify(report?.unit)}, not ${unit}`);
  if (report?.result !== "passed") problems.push(`unit result is ${JSON.stringify(report?.result)}, not "passed"`);
  if (report?.commit !== commit) problems.push(`commit ${report?.commit} != ${commit}`);
  if (report?.tree !== tree) problems.push(`tree ${report?.tree} != ${tree}`);
  if (report?.stage !== "compile") problems.push(`stage is ${JSON.stringify(report?.stage)}, not "compile"`);
  for (const stage of ["lint", "test-build", "tests"]) if (report?.stages?.[stage]?.result !== "passed") problems.push(`stage ${stage} is ${JSON.stringify(report?.stages?.[stage]?.result)}, not "passed"`);
  if (report?.stages?.docs !== undefined && report.stages.docs.result !== "passed") problems.push(`stage docs is ${JSON.stringify(report.stages.docs.result)}, not "passed"`);
  const calls = Array.isArray(report?.calls) ? report.calls : [];
  if (!calls.length) problems.push("no call is recorded for the unit");
  for (const c of calls) {
    if (c?.result !== "passed") problems.push(`call ${c?.id} is ${JSON.stringify(c?.result)}`);
    else if (!job(c.evidence)) problems.push(`call ${c?.id} carries no broker job as evidence`);
  }
  for (const stage of ["lint", "test-build"]) if (!calls.some((c) => c?.stage === stage)) problems.push(`no ${stage} call is recorded for the unit`);
  return problems;
}

/** The reasons a unit-gate report does not prove this crate, version and commit compiled; empty when it does. */
export function compileReportProblems(report, { name, version, commit, tree }) {
  if (report?.schema === UNIT_RESULT_SCHEMA) return unitResultProblems(report, { name, commit, tree });
  if (report?.schema === UNIT_LIST_COMPILE_SCHEMA) return unitListCompileProblems(report, { name, commit, tree });
  const problems = [];
  if (report?.schema !== UNIT_REPORT_SCHEMA) problems.push(`schema is ${JSON.stringify(report?.schema)}, not ${UNIT_REPORT_SCHEMA}`);
  if (report?.unit?.kind !== "cargo" || report?.unit?.name !== name) problems.push(`unit is ${JSON.stringify(report?.unit)}, not cargo:${name}`);
  if (report?.unit?.version !== version) problems.push(`unit version ${report?.unit?.version} != ${version}`);
  if (report?.executor !== "broker") problems.push(`executor is ${JSON.stringify(report?.executor)}, not a broker host's compile`);
  if (report?.commit !== commit) problems.push(`commit ${report?.commit} != ${commit}`);
  if (report?.tree !== tree) problems.push(`tree ${report?.tree} != ${tree}`);
  for (const stage of ["check", "compile"]) if (report?.stages?.[stage]?.ok !== true) problems.push(`stage ${stage} is not ok`);
  return problems;
}

/** crates.io sparse index path for a crate name (https://doc.rust-lang.org/cargo/reference/registry-index.html#index-files). */
export function indexPath(name) {
  const n = name.toLowerCase();
  if (n.length <= 2) return `${n.length}/${n}`;
  if (n.length === 3) return `3/${n[0]}/${n}`;
  return `${n.slice(0, 2)}/${n.slice(2, 4)}/${n}`;
}

async function indexEntry(name, version, fetchImpl = fetch) {
  const res = await fetchImpl(`https://index.crates.io/${indexPath(name)}`, { headers: { "cache-control": "no-cache" } });
  if (res.status === 404) return null;
  if (!res.ok) fail(`crates.io index answered ${res.status} for ${name}`);
  for (const line of (await res.text()).split("\n")) {
    if (!line.trim()) continue;
    const entry = JSON.parse(line);
    if (entry.vers === version) return entry;
  }
  return null;
}

export async function publishLocalCargo(argv, { run = spawnSync, log = console.log, fetchImpl = fetch, sleep = (ms) => new Promise((r) => setTimeout(r, ms)), env = process.env, platform = process.platform } = {}) {
  const o = parseArgs(argv);
  const git = (args) => {
    const r = run("git", args, { cwd: o.repo, encoding: "utf8", maxBuffer: 1 << 26 });
    if (r.status !== 0) fail(`git ${args.join(" ")} failed: ${String(r.stderr ?? "").trim()}`);
    return String(r.stdout ?? "").trim();
  };
  const commit = git(["rev-parse", "--verify", `${o.commit}^{commit}`]);
  git(["fetch", "-q", "origin", "main"]);
  if (run("git", ["merge-base", "--is-ancestor", commit, "origin/main"], { cwd: o.repo }).status !== 0) fail(`${commit} is not on origin/main: push it first ([skip ci])`);
  const dir = `crates/${o.crate}`;
  const manifest = git(["show", `${commit}:${dir}/Cargo.toml`]);
  const name = /^name\s*=\s*"([^"]+)"/m.exec(manifest)?.[1];
  const version = /^version\s*=\s*"([^"]+)"/m.exec(manifest)?.[1];
  if (name !== o.crate || !version) fail(`${dir}/Cargo.toml at ${commit} does not declare package ${o.crate} with a version`);
  if (await indexEntry(name, version, fetchImpl)) fail(`${name} ${version} is already on crates.io; bump the version`);

  const tree = git(["rev-parse", `${commit}^{tree}`]);
  let report;
  try { report = JSON.parse(fs.readFileSync(o.report, "utf8")); } catch (e) { fail(`cannot read the compile report ${o.report}: ${e.message}`); }
  const problems = compileReportProblems(report, { name, version, commit, tree });
  if (problems.length) fail(`the compile report does not prove cargo:${name} ${version} at ${commit}: ${problems.join("; ")}`);
  log(`publish-local: compile report ok for cargo:${name} ${version} at ${commit.slice(0, 12)} (executor ${report.executor ?? (report.schema === UNIT_LIST_COMPILE_SCHEMA ? `broker seam, unit-list run ${report.runId}` : report.schema === UNIT_RESULT_SCHEMA ? "broker seam, together compile" : "unknown")})`);
  // The broker's cargo, run directly: package and publish --no-verify compile nothing (GC-A11).
  const pathKey = platform === "win32" ? (Object.keys(env).find((k) => k.toUpperCase() === "PATH") ?? "Path") : "PATH";
  const direct = directCargoPath({ env: { ...env, PATH: env[pathKey] }, platform, gateWrappers: true });
  const cargoEnv = { ...env, [pathKey]: direct.path };

  const work = fs.mkdtempSync(path.join(os.tmpdir(), "rk-publish-local-cargo-"));
  try {
    // The whole crates/ folder: cargo reads each path dependency's manifest while it packages.
    const archive = run("git", ["archive", "--format=tar", commit, "crates"], { cwd: o.repo, maxBuffer: 1 << 30 });
    if (archive.status !== 0) fail(`git archive ${commit} crates failed`);
    if (run("tar", ["-x", "-C", work], { input: archive.stdout }).status !== 0) fail("unpacking the commit's crate folder failed");
    const manifestPath = path.join(work, dir, "Cargo.toml");
    const targetDir = path.join(work, "package-target");
    const pkg = run("cargo", ["package", "--no-verify", "--locked", "--manifest-path", manifestPath, "--target-dir", targetDir], { encoding: "utf8", env: cargoEnv });
    if (pkg.status !== 0) fail(`cargo package failed: ${String(pkg.stderr ?? "").trim().split(/\r?\n/).slice(-3).join(" | ")}`);
    const crateFile = path.join(targetDir, "package", `${name}-${version}.crate`);
    const sha256 = crypto.createHash("sha256").update(fs.readFileSync(crateFile)).digest("hex");
    log(`publish-local: packaged ${name}-${version}.crate sha256 ${sha256}`);

    // Mac: cargo's own Keychain provider is admitted per program path, and the broker's pinned toolchain moves that path with every
    // toolchain, so a background publish waited on an approval prompt nobody saw. The token is read here through /usr/bin/security (one
    // fixed path, as the npm route reads its token), bounded so a refusal fails instead of hanging, and handed to this one cargo call only.
    const publishEnv = { ...cargoEnv };
    if (platform === "darwin") {
      const read = run("/usr/bin/security", ["find-generic-password", "-s", CARGO_KEYCHAIN_SERVICE, "-w"], { encoding: "utf8", stdio: ["ignore", "pipe", "ignore"], timeout: KEYCHAIN_READ_MS });
      const token = read.status === 0 ? String(read.stdout ?? "").trim() : "";
      if (!token) fail(`the crates.io token could not be read from the Keychain item ${CARGO_KEYCHAIN_SERVICE} within ${KEYCHAIN_READ_MS / 1000} s (${read.error?.code === "ETIMEDOUT" ? "waiting for an access approval: allow /usr/bin/security on that item once in Keychain Access" : "missing or refused"})`);
      Object.assign(publishEnv, { CARGO_REGISTRY_TOKEN: token, CARGO_REGISTRY_GLOBAL_CREDENTIAL_PROVIDERS: "cargo:token" });
    }
    const pub = run("cargo", ["publish", "--no-verify", "--locked", "--manifest-path", manifestPath, "--target-dir", targetDir], { encoding: "utf8", env: publishEnv, timeout: PUBLISH_CALL_MS });
    if (pub.status !== 0) fail(`cargo publish failed${pub.error?.code === "ETIMEDOUT" ? ` (no answer within ${PUBLISH_CALL_MS / 60000} min)` : ""}: ${String(pub.stderr ?? "").trim().split(/\r?\n/).slice(-4).join(" | ")}`);

    let entry = null;
    for (let i = 0; i < 20 && !entry; i += 1) { entry = await indexEntry(name, version, fetchImpl); if (!entry) await sleep(15000); }
    if (!entry) fail(`${name} ${version} is not in the crates.io index after 5 minutes`);
    if (entry.cksum !== sha256) fail(`crates.io index checksum ${entry.cksum} != packaged ${sha256} for ${name} ${version}`);
    log(`publish-local: ${name} ${version} published from ${commit.slice(0, 12)}; crates.io checksum ${entry.cksum} matches`);
    return { name, version, commit, sha256 };
  } finally {
    fs.rmSync(work, { recursive: true, force: true });
  }
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  publishLocalCargo(process.argv.slice(2)).catch((e) => { console.error(`right-release publish-local cargo: ${e.message}`); process.exit(1); });
}
