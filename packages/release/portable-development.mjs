import fs from "node:fs";
import path from "node:path";
import { createHash } from "node:crypto";
import { stagingPathOf, swapInPlace } from "./in-place-install.mjs";

const CHUNK_BYTES = 1024 * 1024;

/**
 * Install an unsigned development payload in place at the fixed path `<installRoot>/current` (a real directory).
 * No versions/ folder, no digest folder, no symlink or junction. NO old copy is kept (Orthic-Labs/rightkit#348): the
 * old `current` is removed and the verified staging copy is renamed over it, and a stale `current.previous` left by an
 * older RightKit is removed. A failed install leaves `current` absent or partial; reinstall from an older build. A
 * legacy `current` symlink or junction from the old layout is removed the same way.
 */
export function installPortableDevelopment(descriptor, { root, platform = process.platform, env = process.env } = {}) {
  const emit = (event, fields = {}) => process.stdout.write(`${JSON.stringify({ event: `DEV_INSTALL_${event}`, ...fields })}
`);
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
    digest = digestTree(source);
    current = path.join(installRoot, "current");
    const staging = stagingPathOf(current);
    emit("STAGE", { sourceDirectory: source, current, staging, digest });
    if (exists(current) && !fs.lstatSync(current).isDirectory() && !fs.lstatSync(current).isSymbolicLink()) throw new Error(`development current is not a directory; refusing to replace: ${current}`);

    fs.mkdirSync(installRoot, { recursive: true });
    removeIfExists(staging);
    try {
      copyTree(source, staging);
      if (digestTree(staging) !== digest) throw new Error("staged development payload did not match source digest");
    } catch (error) {
      removeIfExists(staging);
      throw error;
    }
    const sourceEntryDigest = hashFile(sourceEntrypoint);
    let entryDigest;
    swapInPlace({
      staging, destination: current, previous: null,
      verify: (destination) => {
        entryDigest = hashFile(resolveContained(destination, descriptor.entrypoint, "entrypoint"));
        if (entryDigest !== sourceEntryDigest) throw new Error("development entrypoint readback did not match source");
      },
    });
    emit("ACTIVATE", { current });
    emit("SUCCEEDED", { installRoot, current, digest, entrypoint: descriptor.entrypoint, entrypointDigest: entryDigest });
    return { installRoot, current, versionRoot: current, digest, entrypoint: path.join(current, descriptor.entrypoint), entrypointDigest: entryDigest };
  } catch (error) {
    emit("FAILED", { installRoot: descriptor?.installRoot, current, digest, message: error?.message ?? String(error) });
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

function removeIfExists(target) {
  if (!exists(target)) return;
  fs.rmSync(target, { recursive: true, force: true });
}

