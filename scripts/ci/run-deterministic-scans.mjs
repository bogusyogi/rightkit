#!/usr/bin/env node
// publish.yml `scans` job (rightkit#888): run the shared deterministic scan module over the whole candidate,
// class A only (Ubuntu cannot compile the native crates; class B compile and lint run in the native verify jobs on their
// own hosts), no changed-file narrowing, no deferral, then write the scan receipt that publish-reviewed.yml verifies.
// Every planned class A scan must pass. Any failure, missing tool or deferral exits 1 and leaves no receipt artifact.
//   run-deterministic-scans.mjs --content-root <dir> --live-root <dir> --out <dir>
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { planScans, runScans, scanDeferrals, scanFailures } from "../../packages/release/deterministic-scans.mjs";
import { buildScanReceipt, observeToolVersions, resultProblems, SCAN_RECEIPT_FILE, sha256Hex, toolVersionProblems } from "./scan-receipt.mjs";

const git = (cwd, ...args) => {
  const r = spawnSync("git", args, { cwd, encoding: "utf8", maxBuffer: 256 * 1024 * 1024 });
  if (r.error || r.status !== 0) throw new Error(`git ${args.join(" ")} failed: ${r.error?.message ?? r.stderr}`);
  return r.stdout;
};

/** Every tracked file of the candidate: the scans cover the candidate in full, never a diff. */
export function candidateFiles(contentRoot) {
  return git(contentRoot, "ls-files", "-z").split("\0").filter(Boolean);
}

export async function runCandidateScans({ contentRoot, liveRoot, outDir, env = process.env, spawn = spawnSync, files = candidateFiles(contentRoot) }) {
  const sourceCommit = env.GITHUB_SHA;
  const head = git(contentRoot, "rev-parse", "HEAD").trim();
  if (!/^[0-9a-f]{40}$/.test(sourceCommit ?? "") || head !== sourceCommit) throw new Error(`candidate checkout ${head} is not the dispatched commit ${sourceCommit}`);
  const liveHead = git(liveRoot, "rev-parse", "HEAD").trim();
  if (liveHead !== sourceCommit) throw new Error(`live checkout ${liveHead} is not the dispatched commit ${sourceCommit}`);
  // Mutually exclusive features (rightkit-db: sqlcipher vs sqlcipher-system) are checked per set from the candidate's own gate config.
  const crateFeatures = JSON.parse(fs.readFileSync(path.join(contentRoot, "ci", "gate.config.json"), "utf8")).rust ?? {};
  const plan = planScans({ changed: files, contentRoot, liveRoot, includeTests: false, localCompile: true, cargoCommand: ["cargo"], crateFeatures, readBytes: (f) => fs.readFileSync(f) });
  const plannedClassA = plan.scans.filter((x) => x.class === "A");
  console.log(`scans: ${plannedClassA.length} class A scans planned over ${files.length} tracked files (class B runs in the native verify jobs)`);
  const observedTools = observeToolVersions(spawn);
  const result = await runScans(plan, { classes: ["A"] });
  for (const line of scanDeferrals(result)) console.log(`DEFERRED ${line}`);
  for (const line of scanFailures(result)) console.log(`FAIL ${line}`);
  const receipt = buildScanReceipt({
    result, plannedClassA, sourceCommit, headCommit: head, observedTools,
    cargoLockSha256: sha256Hex(fs.readFileSync(path.join(contentRoot, "Cargo.lock"))),
    run: { repository: env.GITHUB_REPOSITORY, runId: env.GITHUB_RUN_ID, runAttempt: env.GITHUB_RUN_ATTEMPT },
  });
  const problems = [...resultProblems(result, plannedClassA), ...toolVersionProblems(observedTools)];
  if (problems.length === 0) {
    fs.mkdirSync(outDir, { recursive: true });
    fs.writeFileSync(path.join(outDir, SCAN_RECEIPT_FILE), `${JSON.stringify(receipt, null, 2)}\n`, { flag: "wx" });
  }
  return { receipt, problems };
}

if (process.argv[1] && fs.realpathSync(process.argv[1]) === fs.realpathSync(fileURLToPath(import.meta.url))) {
  const arg = (flag) => { const i = process.argv.indexOf(flag); return i > 0 ? process.argv[i + 1] : undefined; };
  const [contentRoot, liveRoot, outDir] = [arg("--content-root"), arg("--live-root"), arg("--out")].map((p) => (p ? path.resolve(p) : p));
  if (!contentRoot || !liveRoot || !outDir || process.argv.length !== 8) { console.error("usage: run-deterministic-scans.mjs --content-root <dir> --live-root <dir> --out <dir>"); process.exit(2); }
  try {
    const { problems } = await runCandidateScans({ contentRoot, liveRoot, outDir });
    if (problems.length) { for (const p of problems) console.error(`scans refused: ${p}`); process.exit(1); }
    console.log(`scans passed; receipt written to ${path.join(outDir, SCAN_RECEIPT_FILE)}`);
  } catch (error) { console.error(`scans refused: ${error.message}`); process.exit(1); }
}
