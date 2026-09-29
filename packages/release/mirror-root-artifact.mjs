#!/usr/bin/env node
import { copyFileSync, mkdirSync, realpathSync, renameSync, rmSync } from "node:fs";
import { execFileSync } from "./background-process.mjs";
import path from "node:path";

const args = parseArgs(process.argv.slice(2));
if (!args.file || !args.packageRoot) {
  console.error("Usage: right-release mirror-root-artifact --file <artifact> --package-root <dir> [--name <filename>]");
  process.exit(2);
}

const packageRoot = realpathSync(path.resolve(args.packageRoot));
const source = path.resolve(packageRoot, args.file);
const name = args.name || path.basename(source);
const repoRoot = git(packageRoot, "rev-parse", "--show-toplevel");
const commonDir = git(packageRoot, "rev-parse", "--path-format=absolute", "--git-common-dir");
const canonicalRepoRoot = path.basename(commonDir) === ".git" ? path.dirname(commonDir) : repoRoot;
const relativePackageRoot = path.relative(repoRoot, packageRoot);
const destinations = new Set([
  path.join(packageRoot, name),
  path.join(canonicalRepoRoot, relativePackageRoot, name),
]);

for (const destination of destinations) {
  if (path.resolve(destination) === source) continue;
  atomicCopy(source, destination);
}

const canonicalArtifact = path.join(canonicalRepoRoot, relativePackageRoot, name);
console.log(`right-release: repository-root artifact -> ${canonicalArtifact}`);

function git(cwd, ...commandArgs) {
  return execFileSync("git", commandArgs, { cwd, encoding: "utf8" }).trim();
}

function atomicCopy(from, to) {
  mkdirSync(path.dirname(to), { recursive: true });
  const temporary = `${to}.tmp-${process.pid}`;
  rmSync(temporary, { force: true });
  try {
    copyFileSync(from, temporary);
    renameSync(temporary, to);
  } finally {
    rmSync(temporary, { force: true });
  }
}

function parseArgs(values) {
  const parsed = {};
  for (let i = 0; i < values.length; i += 1) {
    const key = values[i];
    if (!key.startsWith("--")) continue;
    parsed[key.slice(2).replace(/-([a-z])/g, (_, letter) => letter.toUpperCase())] = values[++i];
  }
  return parsed;
}
