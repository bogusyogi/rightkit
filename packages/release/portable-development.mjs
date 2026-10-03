import fs from "node:fs";
import path from "node:path";
import { createHash } from "node:crypto";

const CHUNK_BYTES = 1024 * 1024;

/** Install an unsigned development payload through an owned, versioned `current` pointer. */
export function installPortableDevelopment(descriptor, { root, platform = process.platform, env = process.env } = {}) {
  const emit = (event, fields = {}) => process.stdout.write(`${JSON.stringify({ event: `DEV_INSTALL_${event}`, ...fields })}\n`);
  let backup = null;
  let switched = false;
  let versionRoot;
  let digest;
  let current;
  try {
    validateDescriptor(descriptor, root, platform);
    const installRoot = path.resolve(descriptor.installRoot);
    if (!path.isAbsolute(descriptor.installRoot) || isFilesystemRoot(installRoot, platform)) {
      throw new Error("development installRoot must be an absolute non-filesystem-root path");
    }
    const source = resolveContained(root, descriptor.sourceDirectory, "sourceDirectory");
    const sourceReal = fs.realpathSync(source);
    if (!isWithin(fs.realpathSync(root), sourceReal, platform)) throw new Error("development sourceDirectory resolves outside repository root");
    const sourceInfo = fs.lstatSync(source);
    if (!sourceInfo.isDirectory() || sourceInfo.isSymbolicLink()) throw new Error("development sourceDirectory must be a real directory");
    const sourceEntrypoint = resolveContained(source, descriptor.entrypoint, "entrypoint");
    const sourceEntrypointInfo = fs.lstatSync(sourceEntrypoint);
    if (!sourceEntrypointInfo.isFile() || sourceEntrypointInfo.isSymbolicLink()) throw new Error("development entrypoint must be a regular file inside sourceDirectory");
    const sourceDigest = digestTree(source);
    digest = sourceDigest;
    versionRoot = path.join(installRoot, "versions", `dev-${digest}`);
    current = path.join(installRoot, "current");
    emit("STAGE", { sourceDirectory: source, versionRoot, digest });

    fs.mkdirSync(path.dirname(versionRoot), { recursive: true });
    if (exists(versionRoot)) {
      if (digestTree(versionRoot) !== digest) throw new Error(`development version digest collision: ${versionRoot}`);
    } else {
      const staging = path.join(path.dirname(versionRoot), `.stage-${process.pid}-${randomSuffix()}`);
      try {
        copyTree(source, staging);
        if (digestTree(staging) !== digest) throw new Error("staged development payload did not match source digest");
        fs.renameSync(staging, versionRoot);
      } catch (error) {
        removeIfExists(staging);
        throw error;
      }
    }

    validateCurrent(current, installRoot, platform);
    if (exists(current)) {
      backup = path.join(installRoot, `.current-backup-${process.pid}-${randomSuffix()}`);
      fs.renameSync(current, backup);
    }
    try {
      fs.symlinkSync(versionRoot, current, platform === "win32" ? "junction" : "dir");
      switched = true;
      emit("ACTIVATE", { current, versionRoot });
      const activeRoot = fs.realpathSync(current);
      if (!samePath(activeRoot, fs.realpathSync(versionRoot), platform)) throw new Error("development current pointer did not resolve to staged version");
      const entrypoint = resolveContained(versionRoot, descriptor.entrypoint, "entrypoint");
      const entryDigest = hashFile(entrypoint);
      if (entryDigest !== hashFile(path.join(source, descriptor.entrypoint))) throw new Error("development entrypoint readback did not match source");
      if (backup) removeIfExists(backup);
      emit("SUCCEEDED", { installRoot, current, versionRoot, digest, entrypoint: descriptor.entrypoint, entrypointDigest: entryDigest });
      return { installRoot, current, versionRoot, digest, entrypoint: path.join(current, descriptor.entrypoint), entrypointDigest: entryDigest };
    } catch (error) {
      if (switched) removeIfExists(current);
      if (backup && exists(backup)) fs.renameSync(backup, current);
      throw error;
    }
  } catch (error) {
    emit("FAILED", { installRoot: descriptor?.installRoot, versionRoot, digest, message: error?.message ?? String(error) });
    throw error;
  }
}

function validateDescriptor(descriptor, root, platform) {
  if (!descriptor || descriptor.kind !== "portable" || descriptor.activation?.kind !== "current") throw new Error("unsupported portable development descriptor");
  if (typeof root !== "string" || !path.isAbsolute(root)) throw new Error("repository root must be absolute");
  if (typeof descriptor.installRoot !== "string" || !path.isAbsolute(descriptor.installRoot)) throw new Error("development installRoot must be absolute");
  resolveContained(root, descriptor.sourceDirectory, "sourceDirectory");
  resolveContained(root, descriptor.entrypoint, "entrypoint");
  if (!isRelativePath(descriptor.sourceDirectory) || !isRelativePath(descriptor.entrypoint)) throw new Error("development sourceDirectory and entrypoint must be relative paths");
  if (platform !== "win32" && platform !== "darwin" && platform !== "linux") throw new Error(`unsupported development install platform: ${platform}`);
}

function isRelativePath(value) {
  return typeof value === "string" && value.length > 0 && !path.isAbsolute(value) && !path.win32.isAbsolute(value) && !value.split(/[\\/]/).includes("..");
}

function resolveContained(base, relative, label) {
  if (!isRelativePath(relative)) throw new Error(`${label} must be a contained relative path`);
  const resolved = path.resolve(base, relative);
  if (!isWithin(path.resolve(base), resolved, process.platform)) throw new Error(`${label} escapes its parent directory`);
  return resolved;
}

function isFilesystemRoot(value, platform) {
  const parsed = path.parse(value);
  return samePath(parsed.root, value, platform);
}

function isWithin(parent, child, platform) {
  const relative = path.relative(parent, child);
  return relative === "" || (!relative.startsWith(`..${path.sep}`) && relative !== ".." && !path.isAbsolute(relative));
}

function samePath(left, right, platform) {
  const a = path.normalize(left);
  const b = path.normalize(right);
  return platform === "win32" ? a.toLowerCase() === b.toLowerCase() : a === b;
}

function exists(file) {
  try { fs.lstatSync(file); return true; } catch (error) { if (error?.code === "ENOENT") return false; throw error; }
}

function digestTree(directory) {
  const hash = createHash("sha256");
  const visit = (dir, relative = "") => {
    const entries = fs.readdirSync(dir, { withFileTypes: true }).sort((a, b) => a.name < b.name ? -1 : a.name > b.name ? 1 : 0);
    for (const entry of entries) {
      const rel = relative ? `${relative}/${entry.name}` : entry.name;
      const absolute = path.join(dir, entry.name);
      const stat = fs.lstatSync(absolute);
      if (stat.isSymbolicLink()) throw new Error(`development payload contains a symbolic link: ${rel}`);
      if (stat.isDirectory()) { hash.update(`D\0${rel}\0`); visit(absolute, rel); }
      else if (stat.isFile()) hash.update(`F\0${rel}\0${stat.size}\0${hashFile(absolute)}\0`);
      else throw new Error(`development payload contains a non-regular entry: ${rel}`);
    }
  };
  visit(directory);
  return hash.digest("hex");
}

function hashFile(file) {
  const fd = fs.openSync(file, "r");
  const hash = createHash("sha256");
  const buffer = Buffer.allocUnsafe(CHUNK_BYTES);
  try {
    let bytes;
    while ((bytes = fs.readSync(fd, buffer, 0, buffer.length, null)) > 0) hash.update(buffer.subarray(0, bytes));
  } finally { fs.closeSync(fd); }
  return hash.digest("hex");
}

function copyTree(source, target) {
  fs.mkdirSync(target, { recursive: true });
  for (const entry of fs.readdirSync(source, { withFileTypes: true })) {
    const from = path.join(source, entry.name);
    const to = path.join(target, entry.name);
    const stat = fs.lstatSync(from);
    if (stat.isSymbolicLink()) throw new Error(`development payload contains a symbolic link: ${from}`);
    if (stat.isDirectory()) copyTree(from, to);
    else if (stat.isFile()) fs.copyFileSync(from, to, fs.constants.COPYFILE_EXCL);
    else throw new Error(`development payload contains a non-regular entry: ${from}`);
  }
}

function validateCurrent(current, installRoot, platform) {
  if (!exists(current)) return;
  const stat = fs.lstatSync(current);
  if (!stat.isSymbolicLink()) throw new Error(`development current pointer is not a symlink or junction; refusing to replace: ${current}`);
  let target;
  try { target = fs.realpathSync(current); } catch (error) { throw new Error(`development current pointer is dangling or unreadable: ${current}`, { cause: error }); }
  const versionsRoot = path.join(installRoot, "versions");
  const versionName = path.basename(target);
  if (!isWithin(fs.realpathSync(installRoot), target, platform)
    || !samePath(path.dirname(target), fs.realpathSync(versionsRoot), platform)
    || !versionName || versionName === "." || versionName === "..") {
    throw new Error(`development current pointer does not resolve to an owned version: ${current}`);
  }
}

function removeIfExists(target) {
  if (!exists(target)) return;
  fs.rmSync(target, { recursive: true, force: true });
}

function randomSuffix() { return `${Date.now()}-${Math.random().toString(16).slice(2)}`; }
