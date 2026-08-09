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
  const gitDir = git(repoRoot, ["rev-parse", "--git-dir"]).trim();
  const configured = spawnSync("git", ["config", "--path", "--get", "core.worktree"], {
    cwd: repoRoot, encoding: "utf8", windowsHide: true,
  });
  const primary = resolvePrimaryWorktree({
    repoRoot,
    worktrees,
    gitDir,
    coreWorktree: configured.status === 0 ? configured.stdout.trim() : "",
  });
  if (!primary) throw new Error("unable to resolve the primary Git worktree");
  if (canonical(repoRoot) !== canonical(primary)) {
    throw new Error(`right-release must be invoked from the primary Git worktree: ${primary}`);
  }
  const branch = spawnSync("git", ["symbolic-ref", "--quiet", "HEAD"], { cwd: repoRoot, encoding: "utf8", windowsHide: true });
  if (branch.status !== 0) throw new Error("right-release requires a branch-attached primary Git checkout; detached HEAD is forbidden");
}

export function resolvePrimaryWorktree({ repoRoot, worktrees, gitDir, coreWorktree }) {
  const listed = worktrees.match(/^worktree (.+)$/m)?.[1];
  if (!listed) return undefined;
  const resolvedGitDir = path.resolve(repoRoot, gitDir);
  if (path.resolve(listed) === resolvedGitDir && coreWorktree) {
    return path.resolve(resolvedGitDir, coreWorktree);
  }
  return listed;
}

export function resolveConfiguredBuildInputs(config, target, label = "release config") {
  const configured = target?.buildInputs ?? config?.buildInputs;
  if (!Array.isArray(configured?.include) || configured.include.length === 0) {
    throw new Error(`${label} must declare non-empty buildInputs.include paths`);
  }
  return configured;
}

export function resolveReleaseBuildInputs({ repoRoot, appRoot, configPath, config, target, env = process.env }) {
  const requiredInputs = [
    configPath,
    path.join(appRoot, "package.json"),
    path.join(appRoot, "pnpm-lock.yaml"),
    path.join(appRoot, "src-tauri", "Cargo.toml"),
    path.join(appRoot, "src-tauri", "Cargo.lock"),
    ...(target?.preflight?.files ?? []).map((file) => path.resolve(appRoot, expandEnv(file, env))),
  ].filter((file, index, files) => files.indexOf(file) === index);
  for (const file of requiredInputs) {
    if (!existsSync(file)) throw new Error(`missing required release input: ${file}`);
  }

  const configured = resolveConfiguredBuildInputs(config, target);
  const appPrefix = path.relative(repoRoot, appRoot).replaceAll("\\", "/");
  const qualify = (pattern) => path.posix.normalize([
    appPrefix,
    pattern.replaceAll("\\", "/").replace(/^\.\//, ""),
  ].filter(Boolean).join("/"));
  const required = requiredInputs
    .map((file) => path.relative(repoRoot, file).replaceAll("\\", "/"))
    .filter((file) => file && file !== ".." && !file.startsWith("../") && !path.isAbsolute(file));

  return {
    requiredInputs,
    buildInputs: {
      include: configured.include.map(qualify),
      exclude: (configured.exclude ?? []).map(qualify),
      required,
      json: Object.fromEntries(Object.entries(configured.json ?? {}).map(([file, fields]) => [qualify(file), fields])),
    },
  };
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

function expandEnv(value, env) {
  return String(value)
    .replace(/%([^%]+)%/g, (_, name) => env[name] ?? "")
    .replace(/\$\{([^}]+)\}/g, (_, name) => env[name] ?? "");
}
