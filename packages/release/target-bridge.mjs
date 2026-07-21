import { lstatSync, mkdirSync, realpathSync, symlinkSync, unlinkSync } from "node:fs";
import path from "node:path";

export function createTargetBridge({
  link,
  target,
  platform = process.platform,
  lstat = lstatSync,
  mkdir = mkdirSync,
  realpath = realpathSync,
  symlink = symlinkSync,
  unlink = unlinkSync,
}) {
  let acquired = false;
  let created = false;
  let ownership = null;

  const bridge = {
    ensure() {
      mkdir(target, { recursive: true });
      let entry = readEntry(link, lstat);
      if (!entry) {
        symlink(target, link, platform === "win32" ? "junction" : "dir");
        entry = lstat(link);
        if (!entry.isSymbolicLink()) throw new Error(`RightKit target bridge was not created as a symbolic link: ${link}`);
        if (!sameRealPath(link, target, realpath, platform)) {
          throw new Error(`RightKit target bridge is not the owned shared cache target after creation: ${link}`);
        }
        ownership = entryIdentity(entry);
        created = true;
      } else {
        if (!entry.isSymbolicLink()) throw new Error(`primary checkout target is not a symbolic link; refusing to replace it: ${link}`);
        if (!sameRealPath(link, target, realpath, platform)) {
          throw new Error(`primary checkout target is not the owned shared cache target; refusing to replace it: ${link}`);
        }
      }
      acquired = true;
      return { created, link, target };
    },

    release() {
      if (!acquired) return false;
      if (!ownership) {
        acquired = false;
        return false;
      }
      const entry = readEntry(link, lstat);
      if (!entry) {
        acquired = false;
        return false;
      }
      if (!sameEntry(entry, ownership)) {
        acquired = false;
        return false;
      }
      let exactOwnedTarget = false;
      try {
        exactOwnedTarget = sameRealPath(link, target, realpath, platform);
      } catch (error) {
        if (error?.code !== "ENOENT") throw error;
      }
      if (!exactOwnedTarget) {
        acquired = false;
        return false;
      }
      unlink(link);
      acquired = false;
      ownership = null;
      return true;
    },

    async run(operation) {
      let operationError;
      try {
        return await operation(bridge);
      } catch (error) {
        operationError = error;
        throw error;
      } finally {
        try {
          bridge.release();
        } catch (cleanupError) {
          if (!operationError) throw cleanupError;
          operationError.cleanupError = cleanupError;
        }
      }
    },
  };

  return bridge;
}

function entryIdentity(stats) {
  return { dev: stats.dev, ino: stats.ino, symbolicLink: stats.isSymbolicLink() };
}

function sameEntry(stats, expected) {
  return stats.dev === expected.dev
    && stats.ino === expected.ino
    && stats.isSymbolicLink() === expected.symbolicLink
    && expected.symbolicLink;
}

function readEntry(file, lstat) {
  try {
    return lstat(file);
  } catch (error) {
    if (error?.code === "ENOENT") return null;
    throw error;
  }
}

function sameRealPath(left, right, realpath, platform) {
  const canonical = (value) => {
    const resolved = path.normalize(realpath(value));
    return platform === "win32" ? resolved.toLowerCase() : resolved;
  };
  return canonical(left) === canonical(right);
}
