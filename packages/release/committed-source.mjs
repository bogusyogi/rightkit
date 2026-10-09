import { createHash } from "node:crypto";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "./background-process.mjs";

const MAX_TREE_BYTES = 32 * 1024 * 1024;
const MAX_TOTAL_BLOB_BYTES = 128 * 1024 * 1024;
const MAX_BLOB_BYTES = 64 * 1024 * 1024;
const COMMAND_TIMEOUT_MS = 15_000;
const BLOB_TIMEOUT_MS = 30_000;
const acquiredSources = new WeakMap();

export class CommittedSourceIncompleteError extends Error {
  constructor(message, cause) {
    super(message, cause ? { cause } : undefined);
    this.name = "CommittedSourceIncompleteError";
    this.code = "COMMITTED_SOURCE_INCOMPLETE";
    this.coverage = [{ path: ".", status: "unscanned", reason: "committed-source-acquisition-failed" }];
  }
}

function nativeGitPath(platform = process.platform) {
  if (platform === "win32") return "C:\\Program Files\\Git\\cmd\\git.exe";
  if (platform === "darwin") return "/usr/bin/git";
  return "/usr/bin/git";
}

function gitEnv(env = process.env, platform = process.platform) {
  const clean = Object.fromEntries(Object.entries(env).filter(([key]) => !/^GIT_/i.test(key)));
  clean.GIT_CONFIG_NOSYSTEM = "1";
  clean.GIT_CONFIG_GLOBAL = platform === "win32" ? "NUL" : "/dev/null";
  clean.GIT_OPTIONAL_LOCKS = "0";
  clean.GIT_TERMINAL_PROMPT = "0";
  return clean;
}

function runGit(root, args, { encoding = "buffer", maxBuffer = 4 * 1024 * 1024, timeout = COMMAND_TIMEOUT_MS, runner = spawnSync, env = gitEnv(), input } = {}) {
  const result = runner(nativeGitPath(), ["-C", root, "--no-replace-objects", "-c", "core.fsmonitor=false", "-c", `core.hooksPath=${os.devNull}`, "-c", `safe.directory=${root}`, ...args], {
    encoding, env, windowsHide: true, maxBuffer, timeout, ...(input ? { input } : {}),
  });
  if (result?.error || result?.status !== 0) throw new Error(`committed ownership: git ${args[0] ?? "command"} failed${result?.error?.code ? ` (${result.error.code})` : ""}`);
  return result.stdout;
}

function validatePath(value) {
  if (!value || value.includes("\\") || value.startsWith("/") || /^[A-Za-z]:/.test(value)) return false;
  const parts = value.split("/");
  return parts.every((part) => part && part !== "." && part !== "..") && path.posix.normalize(value) === value;
}

function hashGitObject(type, bytes, algorithm) {
  return createHash(algorithm).update(`${type} ${bytes.length}\0`).update(bytes).digest("hex");
}

function parseTree(bytes, objectFormat) {
  if (bytes.length > MAX_TREE_BYTES) throw new Error("committed ownership: tree listing exceeds 32MiB");
  const text = new TextDecoder("utf-8", { fatal: true }).decode(bytes);
  const rows = [];
  for (const record of text.split("\0")) {
    if (!record) continue;
    const tab = record.indexOf("\t");
    if (tab < 0) throw new Error("committed ownership: malformed tree entry");
    const [mode, type, oid] = record.slice(0, tab).split(" ");
    const rel = record.slice(tab + 1);
    const digestLength = objectFormat === "sha256" ? 64 : 40;
    if (!validatePath(rel) || !new RegExp(`^[0-9a-f]{${digestLength}}$`).test(oid ?? "")) throw new Error("committed ownership: invalid tree path or object id");
    if (type !== "blob" && type !== "commit") throw new Error(`committed ownership: unsupported tree object type at ${rel}`);
    if (!new Set(["100644", "100755", "120000", "160000"]).has(mode)) throw new Error(`committed ownership: unsupported tree mode at ${rel}`);
    if ((type === "commit") !== (mode === "160000") || (mode === "160000" && type !== "commit")) throw new Error(`committed ownership: mismatched tree mode and object type at ${rel}`);
    rows.push({ rel, mode, type, oid });
  }
  rows.sort((a, b) => a.rel.localeCompare(b.rel));
  return rows;
}

function parseBatchCheck(bytes, expectedOids) {
  if (!expectedOids.length) {
    if (bytes.length) throw new Error("committed ownership: unexpected blob size response");
    return [];
  }
  const lines = bytes.toString("utf8").trimEnd().split("\n");
  if (lines.length !== expectedOids.length) throw new Error("committed ownership: malformed blob size response");
  return lines.map((line, index) => {
    const [oid, type, sizeText] = line.trim().split(" ");
    const size = Number(sizeText);
    if (oid !== expectedOids[index] || type !== "blob" || !Number.isSafeInteger(size) || size < 0) throw new Error("committed ownership: invalid blob size response");
    return size;
  });
}

function readBatch(root, oids, totalBytes, runner) {
  if (!oids.length) return new Map();
  const input = Buffer.from(`${oids.join("\n")}\n`);
  const outputLimit = totalBytes + oids.length * 128;
  const output = runGit(root, ["cat-file", "--batch"], { encoding: "buffer", maxBuffer: outputLimit, timeout: BLOB_TIMEOUT_MS, runner, env: gitEnv() , input });
  if (!Buffer.isBuffer(output)) throw new Error("committed ownership: Git returned non-binary blob data");
  const result = new Map();
  let offset = 0;
  for (const expected of oids) {
    const endHeader = output.indexOf(0x0a, offset);
    if (endHeader < 0) throw new Error("committed ownership: malformed blob header");
    const [oid, type, sizeText] = output.subarray(offset, endHeader).toString("ascii").split(" ");
    const size = Number(sizeText);
    if (oid !== expected || type !== "blob" || !Number.isSafeInteger(size) || size < 0 || size > MAX_BLOB_BYTES) throw new Error("committed ownership: invalid blob object");
    const start = endHeader + 1, end = start + size;
    if (end >= output.length || output[end] !== 0x0a) throw new Error("committed ownership: truncated blob object");
    const bytes = output.subarray(start, end);
    const algorithm = expected.length === 64 ? "sha256" : "sha1";
    if (hashGitObject("blob", bytes, algorithm) !== expected) throw new Error(`committed ownership: blob hash mismatch ${expected}`);
    result.set(expected, Buffer.from(bytes));
    offset = end + 1;
  }
  if (offset !== output.length) throw new Error("committed ownership: unexpected blob output");
  return result;
}

function acquire(root, { expectedCommit, expectedTree, requiredPaths = [], runner = spawnSync, env = gitEnv() } = {}) {
  const text = (args) => runGit(root, args, { encoding: "utf8", runner, env }).trim();
  const repositoryRoot = path.resolve(text(["rev-parse", "--show-toplevel"]));
  const pathPrefix = text(["rev-parse", "--show-prefix"]).replaceAll("\\", "/");
  const originResult = runner(nativeGitPath(), ["-C", repositoryRoot, "--no-replace-objects", "-c", "core.fsmonitor=false", "-c", `core.hooksPath=${os.devNull}`, "-c", `safe.directory=${repositoryRoot}`, "config", "--get", "remote.origin.url"], {
    encoding: "utf8", env, windowsHide: true, maxBuffer: 4 * 1024 * 1024, timeout: COMMAND_TIMEOUT_MS,
  });
  const origin = originResult?.status === 0 ? String(originResult.stdout).trim() : null;
  const objectFormat = text(["rev-parse", "--show-object-format"]);
  if (objectFormat !== "sha1" && objectFormat !== "sha256") throw new Error("committed ownership: unsupported Git object format");
  const commit = text(["rev-parse", "--verify", "HEAD^{commit}"]);
  const digestLength = objectFormat === "sha256" ? 64 : 40;
  if (!new RegExp(`^[0-9a-f]{${digestLength}}$`).test(commit) || (expectedCommit && commit !== expectedCommit)) throw new Error("committed ownership: expected commit is not current HEAD");
  const tree = text(["rev-parse", "--verify", `${commit}^{tree}`]);
  if (!new RegExp(`^[0-9a-f]{${digestLength}}$`).test(tree) || (expectedTree && tree !== expectedTree)) throw new Error("committed ownership: expected tree does not match commit");
  const rows = parseTree(runGit(root, ["ls-tree", "-r", "-z", "--full-tree", tree], { encoding: "buffer", maxBuffer: MAX_TREE_BYTES, timeout: COMMAND_TIMEOUT_MS, runner, env }), objectFormat);
  const wanted = new Set(requiredPaths.filter(validatePath));
  const shouldRead = (row) => row.type === "blob" && row.mode !== "120000" && (
    wanted.has(row.rel) || ["Cargo.toml", "package.json", "Package.swift", "Package.resolved", "ownership-exceptions.json", "Cargo.lock", "pnpm-lock.yaml", "package-lock.json", "npm-shrinkwrap.json", "yarn.lock"].includes(path.posix.basename(row.rel)) ||
    /\.(?:rs|ts|tsx|js|mjs|cjs|jsx|swift|m|mm|c|cc|cpp|h|py|go|kt|java|cs)$/i.test(row.rel)
  );
  const candidates = rows.filter(shouldRead);
  const uniqueOids = [...new Set(candidates.map((row) => row.oid))];
  const sizeInput = Buffer.from(`${uniqueOids.join("\n")}\n`);
  if (sizeInput.length > MAX_TREE_BYTES) throw new Error("committed ownership: blob size query exceeds 32MiB");
  const sizesOutput = uniqueOids.length ? runGit(root, ["cat-file", "--batch-check=%(objectname) %(objecttype) %(objectsize)"], {
    encoding: "buffer", maxBuffer: Math.min(MAX_TREE_BYTES, uniqueOids.length * 128 + 1), timeout: COMMAND_TIMEOUT_MS, runner, env, input: sizeInput,
  }) : Buffer.alloc(0);
  const sizes = parseBatchCheck(sizesOutput, uniqueOids);
  const sizeByOid = new Map(uniqueOids.map((oid, i) => [oid, sizes[i]]));
  const blobOids = [], coverage = [], blobsTotal = new Set();
  let totalBlobBytes = 0;
  for (const row of candidates) {
    const size = sizeByOid.get(row.oid);
    if (size > MAX_BLOB_BYTES) { coverage.push({ path: row.rel, status: "unscanned", reason: "committed-blob-over-64MiB" }); continue; }
    if (!blobsTotal.has(row.oid) && totalBlobBytes + size > MAX_TOTAL_BLOB_BYTES) {
      coverage.push({ path: row.rel, status: "unscanned", reason: "committed-blobs-over-128MiB" }); continue;
    }
    if (!blobsTotal.has(row.oid)) { blobsTotal.add(row.oid); totalBlobBytes += size; }
    blobOids.push(row.oid);
  }
  const blobs = readBatch(root, [...new Set(blobOids)], totalBlobBytes, runner);
  const entries = rows.map((row) => ({ ...row, size: row.type === "blob" ? sizeByOid.get(row.oid) : 0, bytes: blobs.get(row.oid) }));
  return Object.freeze({ repositoryRoot, pathPrefix, origin, commit, tree, objectFormat, entries, blobs, coverage });
}

export function acquireCommittedSource(root, options = {}) {
  const { expectedCommit, expectedTree, requiredPaths = [] } = options;
  try { return createSourceHandle(acquire(path.resolve(root), { expectedCommit, expectedTree, requiredPaths })); }
  catch (error) { if (error instanceof CommittedSourceIncompleteError) throw error; throw new CommittedSourceIncompleteError(error.message, error); }
}

export function acquireCommittedSourceForTest(root, options = {}) {
  try { return createSourceHandle(acquire(path.resolve(root), options)); }
  catch (error) { if (error instanceof CommittedSourceIncompleteError) throw error; throw new CommittedSourceIncompleteError(error.message, error); }
}

function createSourceHandle(data) {
  const handle = Object.freeze({
    repositoryRoot: data.repositoryRoot, pathPrefix: data.pathPrefix, origin: data.origin,
    commit: data.commit, tree: data.tree, objectFormat: data.objectFormat,
  });
  acquiredSources.set(handle, data);
  return handle;
}

export function committedSourceDataForInternalUse(handle) {
  return acquiredSources.get(handle) ?? null;
}

export function assertCommittedCheckoutClean(root, { expectedCommit, expectedTree, runner = spawnSync } = {}) {
  if (expectedCommit || expectedTree) {
    const currentCommit = runGit(path.resolve(root), ["rev-parse", "--verify", "HEAD^{commit}"], { encoding: "utf8", timeout: COMMAND_TIMEOUT_MS, runner, env: gitEnv() }).trim();
    const currentTree = runGit(path.resolve(root), ["rev-parse", "--verify", `${currentCommit}^{tree}`], { encoding: "utf8", timeout: COMMAND_TIMEOUT_MS, runner, env: gitEnv() }).trim();
    if ((expectedCommit && currentCommit !== expectedCommit) || (expectedTree && currentTree !== expectedTree)) throw new CommittedSourceIncompleteError("committed ownership: HEAD changed during protected source acquisition");
  }
  const output = runGit(path.resolve(root), ["status", "--porcelain=v1", "-z", "--untracked-files=all"], {
    encoding: "buffer", maxBuffer: MAX_TREE_BYTES, timeout: COMMAND_TIMEOUT_MS, runner, env: gitEnv(),
  });
  if (output.length) throw new CommittedSourceIncompleteError("committed ownership: protected release checkout has tracked or untracked changes");
  return true;
}

export const committedOwnershipLimitsForTest = Object.freeze({ MAX_TREE_BYTES, MAX_TOTAL_BLOB_BYTES, MAX_BLOB_BYTES });
