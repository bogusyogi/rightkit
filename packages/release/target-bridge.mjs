import { lstatSync, mkdirSync, realpathSync, symlinkSync, unlinkSync } from "node:fs";
import path from "node:path";

/**
 * @param ownedRoot Directory this bridge owns (the app's per-fingerprint cache
 *   parent). An existing link resolving INSIDE it was created by this system for
 *   an earlier fingerprint, so it is safe to relink: a symlink is a pointer, not
 *   data, and the directory it pointed at is untouched. Without this, every
 *   fingerprint change — i.e. every Cargo.lock or version bump — left a stale
 *   link that hard-stopped the next build until someone deleted it by hand.
 *   Omit it to keep the strict refuse-everything behavior.
 */
export function createTargetBridge({
  link,
  target,
  ownedRoot,
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
      let entry = readEntry(link, lstat);
      if (entry && !entry.isSymbolicLink()) {
        throw new Error(
          `primary checkout target is not a symbolic link; refusing to replace it: ${link}\n` +
            `  It is a real directory holding build output. Run right-release cache migrate before building.`,
        );
      }
      let pointsAtTarget = false;
      if (entry) {
        try {
          pointsAtTarget = sameRealPath(link, target, realpath, platform);
        } catch (error) {
          if (error?.code !== "ENOENT") throw error;
        }
        if (!pointsAtTarget && !isReclaimableLink(link, ownedRoot, realpath, platform)) {
          throw new Error(
            `primary checkout target is not the owned shared cache target; refusing to replace it: ${link}\n` +
              `  It is a symlink pointing outside this app's release cache, so it was not created by RightKit.\n` +
              `  Repoint or remove it yourself, then re-run.`,
          );
        }
      }
      mkdir(target, { recursive: true });
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
        if (!pointsAtTarget) {
          // Stale link from an earlier fingerprint, or one whose cache entry a
          // prune already removed. Both are ours to repoint.
          unlink(link);
          symlink(target, link, platform === "win32" ? "junction" : "dir");
          entry = lstat(link);
          if (!sameRealPath(link, target, realpath, platform)) {
            throw new Error(`RightKit target bridge did not repoint to the owned shared cache target: ${link}`);
          }
          ownership = entryIdentity(entry);
          created = true;
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

/**
 * True when `link` is a symlink RightKit may repoint: it resolves inside
 * `ownedRoot`, or it dangles entirely (its cache entry was pruned). A link
 * resolving anywhere else is someone else's and is never touched.
 */
function isReclaimableLink(link, ownedRoot, realpath, platform) {
  if (!ownedRoot) return false;
  const canonical = (value) => {
    const resolved = path.normalize(value);
    return platform === "win32" ? resolved.toLowerCase() : resolved;
  };
  let resolved;
  try {
    resolved = canonical(realpath(link));
  } catch (error) {
    // Dangling link: the entry it pointed at is gone, so nothing can be lost.
    if (error?.code === "ENOENT") return true;
    throw error;
  }
  const root = canonical(ownedRoot);
  return resolved === root || resolved.startsWith(root.endsWith(path.sep) ? root : `${root}${path.sep}`);
}

function sameRealPath(left, right, realpath, platform) {
  const canonical = (value) => {
    const resolved = path.normalize(realpath(value));
    return platform === "win32" ? resolved.toLowerCase() : resolved;
  };
  return canonical(left) === canonical(right);
}
