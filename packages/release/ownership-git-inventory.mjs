// Working-tree inventory constrained to Git's tracked & non-ignored path set.
import { lstatSync, readFileSync, realpathSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "./background-process.mjs";
import { sha256 } from "./ownership-approval.mjs";

const MAX_LIST_BYTES = 32 * 1024 * 1024;
const MAX_BLOB_BATCH_BYTES = 256 * 1024 * 1024;
const DEFAULT_MAX_FILE_BYTES = 2 * 1024 * 1024;
const CACHE_LIMIT = 128 * 1024 * 1024;
const blobCache = new Map();
let cacheBytes = 0;

function gitEnv() {
  const env = Object.fromEntries(Object.entries(process.env).filter(([key]) => !/^GIT_/i.test(key)));
  Object.assign(env, { GIT_CONFIG_NOSYSTEM: "1", GIT_CONFIG_GLOBAL: process.platform === "win32" ? "NUL" : "/dev/null", GIT_OPTIONAL_LOCKS: "0", GIT_TERMINAL_PROMPT: "0" });
  return env;
}

function runGit(root, repositoryRoot, args, { input, maxBuffer = MAX_LIST_BYTES } = {}) {
  const executable = process.platform === "win32" ? "C:\\Program Files\\Git\\cmd\\git.exe" : "/usr/bin/git";
  const result = spawnSync(executable, ["-C", root, "--no-replace-objects", "-c", "core.fsmonitor=false", "-c", `core.hooksPath=${os.devNull}`, "-c", `safe.directory=${repositoryRoot}`, ...args], {
    encoding: "buffer", env: gitEnv(), windowsHide: true, maxBuffer, timeout: 2500, ...(input ? { input } : {}),
  });
  if (result?.error || result?.status !== 0) throw new Error(`ownership inventory: git ${args[0]} failed${result?.error?.code ? ` (${result.error.code})` : ""}`);
  return result.stdout;
}

function remember(oid, bytes) {
  if (bytes.length > CACHE_LIMIT) return;
  const key = `gitblob:${oid}`;
  if (blobCache.has(key)) { cacheBytes -= blobCache.get(key).length; blobCache.delete(key); }
  blobCache.set(key, bytes); cacheBytes += bytes.length;
  while (cacheBytes > CACHE_LIMIT && blobCache.size) {
    const first = blobCache.keys().next().value;
    cacheBytes -= blobCache.get(first).length; blobCache.delete(first);
  }
  return blobCache.get(key);
}

function decodePath(bytes) {
  return new TextDecoder("utf-8", { fatal: true }).decode(bytes);
}

function readGitBlobs(root, repositoryRoot, oids) {
  if (!oids.length) return new Map();
  const input = Buffer.from(`${oids.join("\n")}\n`);
  const output = runGit(root, repositoryRoot, ["cat-file", "--batch"], { input, maxBuffer: MAX_BLOB_BATCH_BYTES });
  const result = new Map(); let offset = 0;
  for (const expected of oids) {
    const lineEnd = output.indexOf(0x0a, offset);
    if (lineEnd < 0) throw new Error("ownership inventory: malformed Git blob header");
    const [oid, type, sizeText] = output.subarray(offset, lineEnd).toString("ascii").split(" ");
    const size = Number(sizeText), start = lineEnd + 1, end = start + size;
    if (oid !== expected || type !== "blob" || !Number.isSafeInteger(size) || size < 0 || size > MAX_BLOB_BATCH_BYTES || end >= output.length || output[end] !== 0x0a) throw new Error("ownership inventory: invalid Git blob");
    result.set(expected, Buffer.from(output.subarray(start, end))); offset = end + 1;
  }
  if (offset !== output.length) throw new Error("ownership inventory: unexpected Git blob output");
  return result;
}

export function gitListedInventory(appRoot, { relevant = () => true, protectedPaths = [], maxFileBytes = DEFAULT_MAX_FILE_BYTES, repositoryRoot = appRoot } = {}) {
  const root = path.resolve(appRoot);
  let safeRoot;
  try { safeRoot = realpathSync(repositoryRoot); }
  catch (error) { return { files: [], coverage: [{ path: ".", status: "unscanned", reason: `repository-root-unavailable:${error.code ?? "error"}` }] }; }
  let listing, dirtyBytes;
  try {
    listing = runGit(root, safeRoot, ["ls-files", "--stage", "--cached", "--others", "--exclude-standard", "-z"]);
    dirtyBytes = runGit(root, safeRoot, ["diff-files", "--name-only", "-z", "--no-ext-diff", "--no-textconv", "--relative"]);
  } catch (error) { return { files: [], coverage: [{ path: ".", status: "unscanned", reason: `git-inventory-failed:${error.message}` }] }; }
  const dirty = new Set(dirtyBytes.length ? dirtyBytes.subarray(0, dirtyBytes.length - (dirtyBytes.at(-1) === 0 ? 1 : 0)).toString("utf8").split("\0") : []);
  const entries = new Map(), coverage = [];
  try {
    let offset = 0;
    while (offset < listing.length) {
      const end = listing.indexOf(0, offset); if (end < 0) throw new Error("malformed NUL listing");
      const record = listing.subarray(offset, end); offset = end + 1;
      const tab = record.indexOf(0x09);
      const candidate = tab < 0 ? "" : record.subarray(0, tab).toString("ascii");
      if (tab >= 0 && /^(?:100644|100755|120000|160000) [0-9a-f]{40,64} \d$/.test(candidate)) {
        const header = record.subarray(0, tab).toString("ascii").split(" ");
        const rel = decodePath(record.subarray(tab + 1));
        if (header.length !== 3 || !/^(?:100644|100755|120000|160000)$/.test(header[0]) || !/^[0-9a-f]{40,64}$/.test(header[1]) || !/^\d$/.test(header[2])) throw new Error("malformed stage entry");
        const entry = entries.get(rel) ?? { rel, stages: [] };
        entry.stages.push({ mode: header[0], oid: header[1], stage: Number(header[2]) }); entries.set(rel, entry);
      } else {
        const rel = decodePath(record);
        if (!entries.has(rel)) entries.set(rel, { rel, stages: [], untracked: true });
      }
    }
  } catch (error) {
    coverage.push({ path: ".", status: "unscanned", reason: `git-listing-invalid:${error.message}` });
    return { files: [], coverage };
  }
  const protectedSet = new Set(protectedPaths);
  const relevantEntries = [...entries.values()].filter((entry) => {
    if (entry.stages.some((item) => item.stage !== 0) || entry.stages.length > 1) return false;
    let selected = false;
    try { selected = protectedSet.has(entry.rel) || relevant(entry.rel); }
    catch (error) { coverage.push({ path: entry.rel, status: "unscanned", reason: `relevance-check-failed:${error.message}` }); }
    entry.relevant = selected;
    if (selected) {
      let sizeLimit;
      try { sizeLimit = typeof maxFileBytes === "function" ? maxFileBytes(entry.rel) : maxFileBytes; }
      catch (error) {
        entry.overLimit = true;
        coverage.push({ path: entry.rel, status: "unscanned", reason: `size-limit-check-failed:${error.message}` });
      }
      entry.maxFileBytes = Number.isSafeInteger(sizeLimit) && sizeLimit >= 0 ? sizeLimit : DEFAULT_MAX_FILE_BYTES;
      try { entry.stat = lstatSync(path.join(root, ...entry.rel.split("/"))); }
      catch (error) {
        entry.statError = error;
        if (error.code === "ENOENT") coverage.push({ path: entry.rel, status: "unscanned", reason: "deleted" });
        else coverage.push({ path: entry.rel, status: "unscanned", reason: `stat-failed:${error.code ?? "error"}` });
      }
      if (entry.stat?.isSymbolicLink()) {
        entry.symlink = true; coverage.push({ path: entry.rel, status: "unscanned", reason: "symlink" });
      } else if (entry.stat && !entry.stat.isFile()) {
        entry.nonFile = true; coverage.push({ path: entry.rel, status: "unscanned", reason: "not-regular-file" });
      }
      if (entry.stat?.isFile() && dirty.has(entry.rel) && entry.stat.size > entry.maxFileBytes) {
        entry.overLimit = true;
        coverage.push({ path: entry.rel, status: "unscanned", reason: "file-over-size-limit" });
      }
    }
    return selected && !entry.statError && !entry.symlink && !entry.nonFile && !entry.overLimit && entry.stages[0]?.mode !== "120000" && entry.stages[0]?.mode !== "160000";
  });
  const blobOids = [...new Set(relevantEntries.filter((entry) => !entry.overLimit && entry.stages[0]?.oid && !dirty.has(entry.rel) && !blobCache.has(`gitblob:${entry.stages[0].oid}`)).map((entry) => entry.stages[0].oid))];
  let fetched;
  try { fetched = readGitBlobs(root, safeRoot, blobOids); }
  catch (error) { return { files: [], coverage: [{ path: ".", status: "unscanned", reason: `git-blob-read-failed:${error.message}` }] }; }
  for (const entry of relevantEntries) {
    const oid = entry.stages[0]?.oid;
    if (!oid || dirty.has(entry.rel) || entry.overLimit) continue;
    const bytes = blobCache.get(`gitblob:${oid}`) ?? fetched.get(oid);
    if (!bytes) continue;
    entry.cleanBlobSize = bytes.length;
    if (bytes.length > entry.maxFileBytes) {
      entry.overLimit = true;
      coverage.push({ path: entry.rel, status: "unscanned", reason: "file-over-size-limit" });
    } else remember(oid, bytes);
  }
  const files = [];
  for (const entry of entries.values()) {
    const { rel } = entry, abs = path.join(root, ...rel.split("/"));
    if (entry.stages.some((item) => item.stage !== 0) || entry.stages.length > 1) { coverage.push({ path: rel, status: "unscanned", reason: "git-index-conflict" }); continue; }
    const indexed = entry.stages[0];
    if (indexed?.mode === "160000") { coverage.push({ path: rel, status: "unscanned", reason: "gitlink" }); continue; }
    const mode = indexed?.mode;
    if (mode === "120000") { coverage.push({ path: rel, status: "unscanned", reason: "symlink" }); continue; }
    let stat = entry.stat;
    try { stat ??= lstatSync(abs); }
    catch (error) {
      if (error.code === "ENOENT") coverage.push({ path: rel, status: "unscanned", reason: "deleted" });
      else coverage.push({ path: rel, status: "unscanned", reason: `stat-failed:${error.code ?? "error"}` });
      continue;
    }
    if (stat.isSymbolicLink()) { coverage.push({ path: rel, status: "unscanned", reason: "symlink" }); continue; }
    if (!stat.isFile()) { coverage.push({ path: rel, status: "unscanned", reason: "not-regular-file" }); continue; }
    const inNodeModules = rel.split("/").includes("node_modules");
    const isRelevant = entry.relevant ?? false;
    let bytes, oid = indexed?.oid, digest, cacheKey;
    const isDirty = !indexed || dirty.has(rel);
    if (isRelevant && entry.overLimit) {
      // Size-limit coverage was already recorded before any blob or file read.
    } else if (isRelevant) {
      try {
        if (!isDirty && oid && blobCache.has(`gitblob:${oid}`)) bytes = blobCache.get(`gitblob:${oid}`);
        else if (!isDirty && oid && fetched.has(oid)) bytes = fetched.get(oid);
        else if (isDirty) {
          if (stat.size > entry.maxFileBytes) {
            coverage.push({ path: rel, status: "unscanned", reason: "file-over-size-limit" });
          } else bytes = readFileSync(abs);
        } else {
          coverage.push({ path: rel, status: "unscanned", reason: "git-blob-cache-miss" });
        }
      } catch (error) { coverage.push({ path: rel, status: "unscanned", reason: `file-read-failed:${error.code ?? "error"}` }); continue; }
      if (bytes && bytes.length > entry.maxFileBytes) {
        coverage.push({ path: rel, status: "unscanned", reason: "file-over-size-limit" });
        bytes = undefined;
      }
      if (bytes && isDirty) { digest = sha256(bytes); cacheKey = `sha256:${digest}`; }
      else if (bytes) { digest = `git:${oid}`; cacheKey = `gitblob:${oid}`; }
    } else if (!isDirty && oid) { digest = `git:${oid}`; cacheKey = `gitblob:${oid}`; }
    files.push({ rel, abs, size: bytes?.length ?? entry.cleanBlobSize ?? stat.size, bytes, oid, digest, cacheKey, inNodeModules });
  }
  return { files, coverage };
}

export function clearGitListedInventoryCacheForTests() { blobCache.clear(); cacheBytes = 0; }
