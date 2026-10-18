#!/usr/bin/env node
// Two forms:
//   right-release mirror-root-artifact --file <artifact> --package-root <dir> [--name <filename>]
//   right-release mirror-root-artifact --config <right-release.config.mjs> --platform mac|win [--package-root <dir>]
// The config form mirrors every entry of `targets.<platform>.mirror`, a list of `"<app-relative file>"` or
// `{ file, name?, pruneSuffix? }`. `pruneSuffix` (for example "-setup.exe") then removes the other files with that suffix
// from the managed directory, so it holds exactly the current artifact. Run it as `targets.<platform>.postSign`.
// Output goes only to RIGHT_RELEASE_ARTIFACT_DIR (outside the repository); it never writes into the repository root.
import { configuredManagedRootSpellings } from "./artifact-storage.mjs";
import { copyFileSync, lstatSync, mkdirSync, readdirSync, realpathSync, renameSync, rmSync, statSync } from "node:fs";
import { execFileSync } from "./background-process.mjs";
import path from "node:path";
import { pathToFileURL } from "node:url";

const args = parseArgs(process.argv.slice(2));
const usage = "Usage: right-release mirror-root-artifact (--file <artifact> --package-root <dir> [--name <filename>] | --config <file> --platform mac|win [--package-root <dir>])";
if (!args.config && (!args.file || !args.packageRoot)) {
  console.error(usage);
  process.exit(2);
}
if (args.config && (!args.platform || args.file)) {
  console.error(usage);
  process.exit(2);
}
if (!process.env.RIGHT_RELEASE_ARTIFACT_DIR) {
  console.error("right-release mirror-root-artifact: RIGHT_RELEASE_ARTIFACT_DIR is required; refusing repository-root output");
  process.exit(2);
}

let entries;
let rootArg = args.packageRoot;
if (args.config) {
  const configPath = path.resolve(args.config);
  const loaded = await import(pathToFileURL(configPath).href);
  const config = loaded.default ?? loaded.config ?? loaded;
  const declared = config?.targets?.[args.platform]?.mirror;
  if (!Array.isArray(declared) || declared.length === 0) {
    console.error(`right-release mirror-root-artifact: targets.${args.platform}.mirror declares no files in ${args.config}`);
    process.exit(2);
  }
  entries = declared.map((item) => (typeof item === "string" ? { file: item } : item));
  for (const entry of entries) {
    const unknown = entry && typeof entry === "object" ? Object.keys(entry).filter((key) => !["file", "name", "pruneSuffix"].includes(key)) : ["(not an object)"];
    if (typeof entry?.file !== "string" || !entry.file.trim() || unknown.length || path.isAbsolute(entry.file) || entry.file.replaceAll("\\", "/").split("/").includes("..")) {
      console.error(`right-release mirror-root-artifact: each mirror entry is an app-relative file or { file, name?, pruneSuffix? }: ${JSON.stringify(entry)}`);
      process.exit(2);
    }
  }
  rootArg ??= path.dirname(configPath);
} else {
  entries = [{ file: args.file, name: args.name }];
}

const packageRoot = realpathSync(path.resolve(rootArg));
const managedOutput = path.resolve(process.env.RIGHT_RELEASE_ARTIFACT_DIR);
const repoRoot = path.resolve(git(packageRoot, "rev-parse", "--show-toplevel"));
if (inside(managedOutput, repoRoot)) {
  console.error(`right-release mirror-root-artifact: managed output must be outside repository: ${managedOutput}`);
  process.exit(2);
}
const safeName = (name) => name === path.basename(name) && /^[A-Za-z0-9][A-Za-z0-9._+-]{0,159}$/.test(name);

for (const entry of entries) {
  const source = path.resolve(packageRoot, entry.file);
  const name = entry.name || path.basename(source);
  if (!safeName(name)) {
    console.error(`right-release mirror-root-artifact: unsafe artifact name: ${name}`);
    process.exit(2);
  }
  if (entry.pruneSuffix !== undefined && (typeof entry.pruneSuffix !== "string" || !/^[A-Za-z0-9._+-]{1,40}$/.test(entry.pruneSuffix) || !name.endsWith(entry.pruneSuffix))) {
    console.error(`right-release mirror-root-artifact: pruneSuffix must be a plain suffix of the artifact name: ${entry.pruneSuffix}`);
    process.exit(2);
  }
  assertSafePath(source);
  if (!statSync(source).isFile()) {
    console.error(`right-release mirror-root-artifact: source is not a regular file: ${source}`);
    process.exit(2);
  }
  const destination = path.join(managedOutput, name);
  if (path.resolve(destination) !== source) atomicCopy(source, destination);
  if (entry.pruneSuffix) {
    for (const stale of readdirSync(managedOutput)) {
      if (stale !== name && stale.endsWith(entry.pruneSuffix) && safeName(stale)) {
        assertSafePath(path.join(managedOutput, stale));
        rmSync(path.join(managedOutput, stale), { force: true });
      }
    }
  }
  console.log(`right-release: managed artifact -> ${destination}`);
}

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
  // Only the configured managed root itself may be a symlink (same rule as artifact-storage assertSafeMutation).
  const managedRoots = configuredManagedRootSpellings();
  const configured = (candidate) => managedRoots.has(process.platform === "win32" ? path.resolve(candidate).toLowerCase() : path.resolve(candidate));
  let cursor = root;
  for (const segment of path.relative(root, absolute).split(path.sep).filter(Boolean)) {
    cursor = path.join(cursor, segment);
    try {
      if (lstatSync(cursor).isSymbolicLink() && !configured(cursor)) throw new Error(`refusing filesystem mutation through symlink: ${cursor}`);
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
