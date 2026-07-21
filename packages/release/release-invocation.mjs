import { spawnSync } from "node:child_process";
import { existsSync, readFileSync, realpathSync } from "node:fs";
import path from "node:path";

function git(cwd, args) {
  const result = spawnSync("git", args, { cwd, encoding: "utf8", windowsHide: true });
  if (result.status !== 0) {
    throw new Error(`git ${args.join(" ")} failed: ${(result.stderr || result.stdout).trim()}`);
  }
  return result.stdout;
}

function canonical(file) {
  return path.normalize(realpathSync(file)).toLowerCase();
}

export function assertPrimaryReleaseCheckout(repoRoot) {
  const worktrees = git(repoRoot, ["worktree", "list", "--porcelain"]);
  const primary = worktrees.match(/^worktree (.+)$/m)?.[1];
  if (!primary) throw new Error("unable to resolve the primary Git worktree");
  if (canonical(repoRoot) !== canonical(primary)) {
    throw new Error(`right-release must be invoked from the primary Git worktree: ${primary}`);
  }
  const branch = spawnSync("git", ["symbolic-ref", "--quiet", "HEAD"], { cwd: repoRoot, encoding: "utf8", windowsHide: true });
  if (branch.status !== 0) throw new Error("right-release requires a branch-attached primary Git checkout; detached HEAD is forbidden");
}

function pathspec(pattern, exclude = false) {
  const normalized = pattern.replaceAll("\\", "/").replace(/^\.\//, "");
  return `:(top,${exclude ? "exclude," : ""}glob)${normalized}`;
}

function jsonProjectionChanged(repoRoot, file, fields, baseline) {
  try {
    const working = JSON.parse(readFileSync(path.join(repoRoot, file), "utf8"));
    const committed = JSON.parse(git(repoRoot, ["show", `${baseline}:${file}`]));
    const project = (value) => fields.map((segments) => segments.reduce((current, segment) => current?.[segment], value));
    return JSON.stringify(project(working)) !== JSON.stringify(project(committed));
  } catch {
    return true;
  }
}

function fileChangedFromBaseline(repoRoot, file, baseline) {
  const workingPath = path.join(repoRoot, file);
  if (!existsSync(workingPath)) return true;
  try {
    const workingHash = git(repoRoot, ["hash-object", "--", file]).trim();
    const baselineHash = git(repoRoot, ["rev-parse", `${baseline}:${file}`]).trim();
    return workingHash !== baselineHash;
  } catch {
    return true;
  }
}

export function dirtyBuildInputs(repoRoot, inputs, baseline = "HEAD") {
  if (!Array.isArray(inputs?.include) || inputs.include.length === 0) {
    throw new Error("release config must declare non-empty buildInputs.include paths");
  }
  const required = new Set(inputs.required ?? []);
  const files = [
    ...statusFiles(repoRoot, inputs.include, inputs.exclude),
    ...statusFiles(repoRoot, [...required], []),
  ];
  return [...new Set(files)]
    .filter((file) => required.has(file)
      ? fileChangedFromBaseline(repoRoot, file, baseline)
      : inputs.json?.[file]
      ? jsonProjectionChanged(repoRoot, file, inputs.json[file], baseline)
      : fileChangedFromBaseline(repoRoot, file, baseline))
    .sort();
}

function statusFiles(repoRoot, include, exclude = []) {
  if (!include.length) return [];
  const specs = [
    ...include.map((pattern) => pathspec(pattern)),
    ...exclude.map((pattern) => pathspec(pattern, true)),
  ];
  const output = git(repoRoot, ["status", "--porcelain=v1", "-z", "--untracked-files=all", "--", ...specs]);
  const records = output.split("\0");
  const files = [];
  for (let index = 0; index < records.length; index += 1) {
    const record = records[index];
    if (!record) continue;
    const status = record.slice(0, 2);
    files.push(record.slice(3).replaceAll("\\", "/"));
    if (/[RC]/.test(status)) index += 1;
  }
  return files;
}
