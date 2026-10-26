// A deterministic git history (fixed identity, dates and contents) standing in for Legion's: a pre-baseline commit, then the
// baseline the retirement range starts after. Its id is not c1d80c9d (that would need a 2^32 SHA-1 search), so tests bind the
// LG-04 rule to this baseline with retirementLedgerFor(); the shipped rule is bound to c1d80c9d.
import fs from "node:fs";
import path from "node:path";
import { spawnSync } from "node:child_process";

export const FIXTURE_IDENTITY = { GIT_AUTHOR_NAME: "Fixture", GIT_AUTHOR_EMAIL: "fixture@rightkit.invalid", GIT_AUTHOR_DATE: "1700000000 +0000", GIT_COMMITTER_NAME: "Fixture", GIT_COMMITTER_EMAIL: "fixture@rightkit.invalid", GIT_COMMITTER_DATE: "1700000000 +0000" };

// The eight paths Orthic-Labs/legion 3c11aa76 deleted without ledger rows (job 119412709257: "check-retirements failed: 8 deleted
// path(s) lack a ledger row"); their contents are not in the log, so these bytes are stand-ins.
export const HISTORICAL_DELETIONS = Object.freeze([
  "scripts/ci/fixtures/audit-reasoning-runtime-accuracy/fixtures/legacy-security-adjudication/clean/src/public-config/public.toml",
  "scripts/ci/fixtures/audit-reasoning-runtime-accuracy/fixtures/legacy-security-adjudication/planted/src/public-config/public.toml",
  "scripts/ci/fixtures/audit-reasoning-runtime-accuracy/fixtures/legacy-security-variant-analysis/clean/src/public-config/public.toml",
  "scripts/ci/fixtures/audit-reasoning-runtime-accuracy/fixtures/legacy-security-variant-analysis/planted/src/public-config/public.toml",
  "scripts/ci/fixtures/audit-reasoning-runtime-accuracy/fixtures/reasoning-architecture/clean/ARCHITECTURE.md",
  "scripts/ci/fixtures/audit-reasoning-runtime-accuracy/fixtures/reasoning-architecture/planted/ARCHITECTURE.md",
  "scripts/ci/fixtures/audit-reasoning-runtime-accuracy/fixtures/reasoning-release-readiness/clean/release/surface.json",
  "scripts/ci/fixtures/audit-reasoning-runtime-accuracy/fixtures/reasoning-release-readiness/planted/release/surface.json",
]);
export const LEDGER_HEAD = "# Retirements ledger\n\n| Path or rule | Retired in commit | Successor | Reason |\n|---|---|---|---|\n| `scripts/check-atomic-canons.mjs` | `ebb349ae` | none | intentionally dropped |\n";
const SKILL = Array.from({ length: 30 }, (_, i) => `Step ${i}: a line of the demo skill that keeps its identity through a rename.`).join("\n") + "\n";
export const BASELINE_FILES = Object.freeze({
  "docs/provenance/retirements.md": LEDGER_HEAD,
  ...Object.fromEntries(HISTORICAL_DELETIONS.map((p, i) => [p, `fixture = ${i}\n`])),
  "scripts/tools/old-tool.mjs": "export const old = 1;\n",
  "skills/manifests/demo.json": "{\"digest\":\"stale\"}\n",
  "skills/demo/SKILL.md": SKILL,
  "hooks/restored.sh": "#!/bin/sh\nexit 0\n",
  "docs/notes.md": "notes outside every ledger surface\n",
  "engine/bins/legion/src/commands/old_cmd.rs": "pub fn run() {}\n",
});
export const PRE_BASELINE_FILE = "scripts/pre-baseline.sh";

function run(cwd, args, { env = {}, input } = {}) {
  const r = spawnSync("git", ["-c", "commit.gpgsign=false", "-c", "core.autocrlf=false", ...args], { cwd, input, encoding: "utf8", env: { ...process.env, ...FIXTURE_IDENTITY, ...env } });
  if (r.status !== 0) throw new Error(`git ${args.join(" ")}: ${r.stderr}`);
  return r.stdout.trim();
}
export function writeFiles(root, files) {
  for (const [rel, body] of Object.entries(files)) {
    fs.mkdirSync(path.dirname(path.join(root, rel)), { recursive: true });
    fs.writeFileSync(path.join(root, rel), body);
  }
}
/** The tree id of `files` in `dir`'s object store (through a throwaway index). */
function treeOf(dir, files) {
  const index = path.join(path.resolve(dir), ".git", "fixture-index");
  fs.rmSync(index, { force: true });
  const env = { GIT_INDEX_FILE: index };
  for (const [rel, body] of Object.entries(files)) {
    const oid = run(dir, ["hash-object", "-w", "--stdin"], { input: body });
    run(dir, ["update-index", "--add", "--cacheinfo", `100644,${oid},${rel}`], { env });
  }
  const tree = run(dir, ["write-tree"], { env });
  fs.rmSync(index, { force: true });
  return tree;
}
function baselineObjects(dir) {
  const parentTree = treeOf(dir, { ...BASELINE_FILES, [PRE_BASELINE_FILE]: "echo pre-baseline\n" });
  const parent = run(dir, ["commit-tree", parentTree, "-m", "pre-baseline"]);
  return { parent, tree: treeOf(dir, BASELINE_FILES) };
}
/** A fresh repository at `dir` (on branch main) whose HEAD is the baseline, with its files checked out. */
export function legionHistory(dir) {
  fs.mkdirSync(dir, { recursive: true });
  run(dir, ["init", "-q", "-b", "main"]);
  const { parent, tree } = baselineObjects(dir);
  const baseline = run(dir, ["commit-tree", tree, "-p", parent, "-m", "baseline"]);
  run(dir, ["update-ref", "refs/heads/main", baseline]);
  run(dir, ["reset", "-q", "--hard", "main"]);
  return { baseline, git: (args, opts) => run(dir, args, opts) };
}
