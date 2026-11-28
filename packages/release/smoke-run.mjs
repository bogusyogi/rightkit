#!/usr/bin/env node
// The host smoke for a peer promote (rustright-install `trusted-gate promote --peer-result --smoke`; rightkit#1848; Adrian, 27 Nov 2026: "yes promote mac").
// A host that cannot run the GC-A27 acceptance (the Mac: no process record, a Windows cases file) trusts a candidate gate on a pinned peer's passing result for
// the same archive bytes, plus this smoke, written here and verified by the installer:
//   - the candidate's gate prints the archive's version;
//   - the runtime manifest read through the agent shim names the broker running here;
//   - one real compile through this host's broker with the candidate gate, every broker job tagged with the contract-run nonce
//     (RK_GATE_CONTRACT_*: each job's signed nonce is contractJobNonce(runId, sealed, "smoke", call), the definition RustRight's job_nonce shares), so the
//     installer can bind each signed claim to this candidate and a smoke for another candidate (or an earlier one) does not verify.
// It writes rightkit.host-smoke/1 and its job-claims/ beside it. Exit 0 when the smoke is written, whether or not it passed: the verdict is in the record.
//
//   node smoke-run.mjs --sealed <candidate sha256> --managed <managed root> --repo <primary RightKit checkout> --out <dir> [--host mac] [--unit cargo:rightkit-time]
import fs from "node:fs";
import path from "node:path";
import crypto from "node:crypto";
import { spawnSync } from "node:child_process";
import { pathToFileURL } from "node:url";
import { CONTRACT_JOBS_ENV, CONTRACT_RUN_ID_ENV, CONTRACT_SEALED_ENV, CONTRACT_STEP_ENV } from "./gate-seam.mjs";
import { claimOf } from "./acceptance-result.mjs";

export const SMOKE_SCHEMA = "rightkit.host-smoke/1";
export const SMOKE_STEP = "smoke";
const HEX64 = /^[0-9a-f]{64}$/;

/** The `rustright` client on PATH (the agent shim on the Mac), or null. */
export function rustrightOnPath({ env = process.env, platform = process.platform, exists = fs.existsSync } = {}) {
  const names = platform === "win32" ? ["rustright.exe", "rustright.cmd"] : ["rustright"];
  for (const dir of String(env.PATH ?? env.Path ?? "").split(path.delimiter).filter(Boolean)) for (const name of names) { const file = path.join(dir, name); if (exists(file)) return file; }
  return null;
}

/** The claims of the tagged jobs a gate launch filed in `jobsFile`: { id, bytes, sha256, call } each, read from the broker receipt each job names. */
export function claimsOfJobs(jobsFile, { read = (f) => fs.readFileSync(f, "utf8") } = {}) {
  let text = ""; try { text = read(jobsFile); } catch { return { claims: [], problems: [`no tagged job was filed (${jobsFile} is missing)`] }; }
  const claims = []; const problems = [];
  for (const line of text.split(/\r?\n/).filter(Boolean)) {
    let job; try { job = JSON.parse(line); } catch { problems.push("a filed job is not JSON"); continue; }
    let receipt = job.receipt;
    if (typeof receipt === "string") { try { receipt = JSON.parse(read(receipt)); } catch { problems.push(`job ${job.requestId}: its receipt ${job.receipt} cannot be read`); continue; } }
    const claim = receipt && typeof receipt === "object" ? claimOf(receipt) : null;
    if (!claim) { problems.push(`job ${job.requestId ?? "(no id)"}: no broker claim in its receipt`); continue; }
    if (job.requestId && claim.id !== job.requestId) { problems.push(`job ${job.requestId}: its receipt is for job ${claim.id}`); continue; }
    claims.push({ ...claim, call: String(job.call ?? "") });
  }
  return { claims, problems };
}

/** Builds the smoke record from what was observed. `pass` holds only when every observation is present and the compile passed with at least one signed job. */
export function buildSmoke({ runId, host, sealed, archive, versionPrinted, runtimeBrokerDigest, commit, tree, compileExit, claims, problems }) {
  const all = [...problems];
  if (versionPrinted !== archive.version) all.push(`the candidate's gate printed ${JSON.stringify(versionPrinted)}, the archive is ${archive.version}`);
  if (!HEX64.test(String(runtimeBrokerDigest ?? ""))) all.push("the runtime manifest names no broker digest");
  if (compileExit !== 0) all.push(`the compile through the broker exited ${compileExit}`);
  if (!claims.length) all.push("no signed broker job: nothing in the smoke is signed");
  return {
    schema: SMOKE_SCHEMA, runId, host, pass: all.length === 0, problems: all,
    gate: { sealedSha256: sealed, archiveIntegrity: archive.integrity, archiveVersion: archive.version },
    versionPrinted, runtimeBrokerDigest, commit, tree,
    jobs: claims.map((c) => ({ requestId: c.id, claimSha256: c.sha256, call: c.call })), jobClaims: "job-claims",
  };
}

export async function main(argv = process.argv.slice(2), { out = console.log } = {}) {
  const opt = (name) => { const i = argv.indexOf(name); return i < 0 ? null : argv[i + 1]; };
  const sealed = opt("--sealed"); const managed = opt("--managed"); const repo = opt("--repo"); const outDir = opt("--out");
  const host = opt("--host") ?? (process.platform === "darwin" ? "mac" : process.platform === "win32" ? "dell" : "linux"); const unit = opt("--unit") ?? "cargo:rightkit-time";
  if (!HEX64.test(String(sealed)) || !managed || !repo || !outDir) { out("usage: smoke-run.mjs --sealed <candidate sha256> --managed <root> --repo <primary RightKit checkout> --out <dir> [--host mac] [--unit cargo:rightkit-time]"); return 2; }
  const candidate = path.join(managed, "trusted-gates", "candidates", sealed);
  const release = path.join(candidate, "node_modules", "@rightkit", "release");
  const gate = path.join(release, "bin", "rightkit-gate.mjs");
  const source = JSON.parse(fs.readFileSync(path.join(candidate, "trusted-gate-source.json"), "utf8"));
  const archive = { version: String(source.version ?? ""), integrity: String(source.integrity ?? "") };
  fs.mkdirSync(path.join(outDir, "job-claims"), { recursive: true });
  const problems = [];
  const printed = spawnSync(process.execPath, [gate, "--version"], { encoding: "utf8", windowsHide: true });
  const versionPrinted = String(printed.stdout ?? "").trim().split(/\r?\n/).pop() ?? "";
  // The candidate's own runtimeManifestPathOf (follows the agent shim to the client it starts), so the smoke reads the manifest the way that gate will.
  let runtimeBrokerDigest = null;
  const client = rustrightOnPath();
  if (!client) problems.push("no rustright client on PATH");
  else {
    try {
      const { runtimeManifestPathOf } = await import(pathToFileURL(path.join(release, "seam-broker.mjs")).href);
      runtimeBrokerDigest = JSON.parse(fs.readFileSync(runtimeManifestPathOf(client), "utf8")).brokerDigest ?? null;
    } catch (error) { problems.push(`the runtime manifest beside ${client} cannot be read: ${error.message}`); }
  }
  const git = (args) => String(spawnSync("git", ["-C", repo, ...args], { encoding: "utf8", windowsHide: true }).stdout ?? "").trim();
  const commit = git(["rev-parse", "HEAD"]); const tree = git(["rev-parse", "HEAD^{tree}"]);
  const runId = crypto.randomBytes(32).toString("hex");
  const jobsFile = path.join(path.resolve(outDir), "jobs.jsonl");
  const log = fs.openSync(path.join(outDir, "compile.log"), "w");
  const compiled = spawnSync(process.execPath, [gate, "--to", "compile", "--unit", unit, "--commit", commit], {
    cwd: repo, windowsHide: true, stdio: ["ignore", log, log],
    env: { ...process.env, [CONTRACT_RUN_ID_ENV]: runId, [CONTRACT_SEALED_ENV]: sealed, [CONTRACT_STEP_ENV]: SMOKE_STEP, [CONTRACT_JOBS_ENV]: jobsFile },
  });
  fs.closeSync(log);
  const { claims, problems: jobProblems } = claimsOfJobs(jobsFile);
  for (const c of claims) fs.writeFileSync(path.join(outDir, "job-claims", `${c.id}.json`), c.bytes);
  const smoke = buildSmoke({ runId, host, sealed, archive, versionPrinted, runtimeBrokerDigest, commit, tree, compileExit: compiled.status, claims, problems: [...problems, ...jobProblems] });
  fs.writeFileSync(path.join(outDir, "smoke.json"), `${JSON.stringify(smoke, null, 2)}\n`);
  out(`smoke written: ${path.join(outDir, "smoke.json")} (pass ${smoke.pass}, ${claims.length} signed job(s)${smoke.problems.length ? `; ${smoke.problems.join("; ")}` : ""})`);
  return 0;
}

if (import.meta.url === pathToFileURL(process.argv[1] ?? "").href) process.exitCode = await main();
