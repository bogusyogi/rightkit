#!/usr/bin/env node
import { copyFileSync, lstatSync, mkdirSync, realpathSync, renameSync, rmSync, statSync } from "node:fs";
import { execFileSync } from "./background-process.mjs";
import path from "node:path";

const args = parseArgs(process.argv.slice(2));
if (!args.file || !args.packageRoot) {
  console.error("Usage: right-release mirror-root-artifact --file <artifact> --package-root <dir> [--name <filename>]");
  process.exit(2);
}
if (!process.env.RIGHT_RELEASE_ARTIFACT_DIR) {
  console.error("right-release mirror-root-artifact: RIGHT_RELEASE_ARTIFACT_DIR is required; refusing repository-root output");
  process.exit(2);
}

const packageRoot = realpathSync(path.resolve(args.packageRoot));
const source = path.resolve(packageRoot, args.file);
const name = args.name || path.basename(source);
const managedOutput = path.resolve(process.env.RIGHT_RELEASE_ARTIFACT_DIR);
const repoRoot = path.resolve(git(packageRoot, "rev-parse", "--show-toplevel"));
if (inside(managedOutput, repoRoot)) {
  console.error(`right-release mirror-root-artifact: managed output must be outside repository: ${managedOutput}`);
  process.exit(2);
}
if (name !== path.basename(name) || !/^[A-Za-z0-9][A-Za-z0-9._+-]{0,159}$/.test(name)) {
  console.error(`right-release mirror-root-artifact: unsafe artifact name: ${name}`);
  process.exit(2);
}
assertSafePath(source);
if (!statSync(source).isFile()) {
  console.error(`right-release mirror-root-artifact: source is not a regular file: ${source}`);
  process.exit(2);
}
const destinations = new Set([path.join(managedOutput, name)]);

for (const destination of destinations) {
  if (path.resolve(destination) === source) continue;
  atomicCopy(source, destination);
}

console.log(`right-release: managed artifact -> ${[...destinations][0]}`);

function git(cwd, ...commandArgs) {
  return execFileSync("git", commandArgs, { cwd, encoding: "utf8" }).trim();
}

function atomicCopy(from, to) {
  assertSafePath(to);
  mkdirSync(path.dirname(to), { recursive: true });
  assertSafePath(to);
  const temporary = `${to}.tmp-${process.pid}`;
  assertSafePath(temporary);
  rmSync(temporary, { force: true });
  try {
    copyFileSync(from, temporary);
    assertSafePath(temporary);
    assertSafePath(to);
    renameSync(temporary, to);
  } finally {
    assertSafePath(temporary);
    rmSync(temporary, { force: true });
  }
}

function assertSafePath(target) {
  const absolute = path.resolve(target);
  const root = path.parse(absolute).root;
  let cursor = root;
  for (const segment of path.relative(root, absolute).split(path.sep).filter(Boolean)) {
    cursor = path.join(cursor, segment);
    try {
      if (lstatSync(cursor).isSymbolicLink()) throw new Error(`refusing filesystem mutation through symlink: ${cursor}`);
    } catch (error) {
      if (error.code !== "ENOENT") throw error;
    }
  }
}

function inside(candidate, parent) {
  return candidate === parent || candidate.startsWith(`${parent}${path.sep}`);
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
