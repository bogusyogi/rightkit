#!/usr/bin/env node
// publish.yml `scans` job (rightkit#888): run the shared deterministic scan module over requested publication units & shared qualification inputs,
// class A only (Ubuntu cannot compile the native crates; class B compile and lint run in the native verify jobs on their
// own hosts), full selected content rather than a diff, no deferral, then write the scan receipt that publish-reviewed.yml verifies.
// Every planned class A scan must pass. Any failure, missing tool or deferral exits 1 and leaves no receipt artifact.
//   run-deterministic-scans.mjs --content-root <dir> --live-root <dir> --out <dir>
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { parseUnitNames, readCrateWorkspace, readNpmWorkspace } from "./publish.mjs";
import { externalInputs } from "../../packages/release/bin/rightkit-gate.mjs";
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

/** Full selected packages plus shared qualification inputs; never a changed-file diff. */
export function publicationContent({ registry, units: text, files, workspace, crateFeatures = {}, readBlob }) {
  if (!["crates", "npm"].includes(registry)) throw new Error("publication scans require PUBLISH_REGISTRY=crates or npm");
  const units = parseUnitNames(text, registry).sort();
  const dirs = ["scripts/ci", "packages/release", "packages/ci", ".github", ".cargo", "crates/rightkit-ownership/catalog", "vendor/upstream-files.json"];
  const selected = units.map((name) => {
    const unit = workspace.get(name);
    if (!unit || unit.private) throw new Error(`publication scan unit is not publishable: ${name}`);
    if (!/^[A-Za-z0-9_@+.-]+(?:\/[A-Za-z0-9_@+.-]+)*$/.test(unit.dir) || unit.dir.split("/").includes("..")) throw new Error(`unsafe publication unit directory: ${unit.dir}`);
    dirs.push(unit.dir);
    if (registry === "crates") {
      dirs.push(`ci/consumers/${name}`);
      for (const row of crateFeatures[name]?.consumerRows ?? []) {
        if (!/^ci\/consumers\/[A-Za-z0-9_+.-]+\/Cargo\.toml$/.test(row.manifestPath)) throw new Error(`unsafe consumer manifest for ${name}`);
        dirs.push(path.posix.dirname(row.manifestPath));
      }
      const reach = externalInputs({ dir: unit.dir, files: files.filter((f) => f.startsWith(`${unit.dir}/`)), readBlob, includeRuntimePaths: false, exists: (p) => files.some((f) => f === p || f.startsWith(`${p}/`)) });
      if (reach.unresolved) throw new Error(`publication scan inputs unresolved for ${name}: ${reach.unresolved}`);
      dirs.push(...reach.paths);
    }
    return { name: registry === "npm" ? path.posix.basename(unit.dir) : name, dir: unit.dir };
  });
  return {
    scope: { registry, units },
    files: files.filter((f) => !f.includes("/") || f.endsWith("/Cargo.toml") || (f.startsWith("ci/") && !f.startsWith("ci/consumers/")) || dirs.some((d) => f === d || f.startsWith(`${d}/`))),
    rustUnits: registry === "crates" ? selected : [],
    npmUnits: registry === "npm" ? selected : [],
  };
}

export async function runCandidateScans({ contentRoot, liveRoot, outDir, env = process.env, spawn = spawnSync, files = candidateFiles(contentRoot) }) {
  const sourceCommit = env.GITHUB_SHA;
  const head = git(contentRoot, "rev-parse", "HEAD").trim();
  if (!/^[0-9a-f]{40}$/.test(sourceCommit ?? "") || head !== sourceCommit) throw new Error(`candidate checkout ${head} is not the dispatched commit ${sourceCommit}`);
  const liveHead = git(liveRoot, "rev-parse", "HEAD").trim();
  if (liveHead !== sourceCommit) throw new Error(`live checkout ${liveHead} is not the dispatched commit ${sourceCommit}`);
  // Mutually exclusive features (rightkit-db: sqlcipher vs sqlcipher-system) are checked per set from the candidate's own gate config.
  const crateFeatures = JSON.parse(fs.readFileSync(path.join(contentRoot, "ci", "gate.config.json"), "utf8")).rust ?? {};
  const selection = publicationContent({ registry: env.PUBLISH_REGISTRY, units: env.PUBLISH_UNITS, files, crateFeatures,
    workspace: env.PUBLISH_REGISTRY === "crates" ? readCrateWorkspace(contentRoot, spawn) : readNpmWorkspace(contentRoot),
    readBlob: (f) => fs.readFileSync(path.join(contentRoot, f), "utf8") });
  const plan = planScans({ changed: selection.files, rustUnits: selection.rustUnits, npmUnits: selection.npmUnits, contentRoot, liveRoot, includeTests: false, localCompile: true, cargoCommand: ["cargo"], crateFeatures, readBytes: (f) => fs.readFileSync(f) });
  const plannedClassA = plan.scans.filter((x) => x.class === "A");
  console.log(`scans: ${plannedClassA.length} class A scans planned over ${selection.files.length} tracked files for ${selection.scope.registry}:${selection.scope.units.join(",")} (class B runs in the native verify jobs)`);
  const observedTools = observeToolVersions(spawn);
  const result = await runScans(plan, { classes: ["A"] });
  for (const line of scanDeferrals(result)) console.log(`DEFERRED ${line}`);
  for (const line of scanFailures(result)) console.log(`FAIL ${line}`);
  const receipt = buildScanReceipt({
    result, plannedClassA, sourceCommit, headCommit: head, observedTools, scope: selection.scope,
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
