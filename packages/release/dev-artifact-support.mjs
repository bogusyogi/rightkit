// Shared by sign-dev-artifact.mjs and dev-artifact-refresh.mjs (Orthic-Labs/rightkit#257): the typed error and the path helpers.
import { lstatSync, realpathSync } from "node:fs";
import path from "node:path";

export class SignDevArtifactError extends Error {
  constructor(code, message) {
    super(message);
    this.name = "SignDevArtifactError";
    this.code = code;
  }
}

export const fail = (code, message) => { throw new SignDevArtifactError(code, message); };
export const toPosix = (value) => value.split(path.sep).join("/");

/** True when `child` is `parent` or lies below it. Both absolute. */
export const isWithin = (parent, child) => child === parent || child.startsWith(parent + path.sep);

/**
 * Refuse `absolute` unless every existing component between `base` and it stays inside `base` on disk. Each component is lstat'ed; a
 * symlink (or junction) must resolve, through realpath, to a location inside the real `base`, and a dangling or unreadable one is
 * refused. A path whose tail does not exist yet is judged by its existing prefix. Call it again immediately before every read or
 * write, since a link can appear after the lexical check. Returns `absolute`.
 */
export function assertInsideOnDisk(base, absolute, label) {
  let realBase;
  try { realBase = realpathSync(base); } catch { fail("SIGN_DEV_ARTIFACT_PATH_INVALID", `${label}: the artifact directory cannot be resolved: ${base}`); }
  let current = path.resolve(base);
  const parts = path.relative(current, absolute).split(path.sep).filter(Boolean);
  for (const part of parts) {
    current = path.join(current, part);
    let info;
    try {
      info = lstatSync(current);
    } catch (error) {
      if (error.code === "ENOENT" || error.code === "ENOTDIR") return absolute;
      fail("SIGN_DEV_ARTIFACT_PATH_INVALID", `${label}: cannot inspect ${toPosix(path.relative(base, current))} (${error.code ?? error.message})`);
    }
    if (!info.isSymbolicLink()) continue;
    let real;
    try { real = realpathSync(current); } catch { fail("SIGN_DEV_ARTIFACT_PATH_ESCAPE", `${label}: ${toPosix(path.relative(base, current))} is a symlink that does not resolve`); }
    if (!isWithin(realBase, real)) fail("SIGN_DEV_ARTIFACT_PATH_ESCAPE", `${label}: ${toPosix(path.relative(base, current))} is a symlink that leaves the artifact directory`);
  }
  return absolute;
}

/**
 * A path relative to `base` that stays inside it: no absolute path, no `..`, not empty, and not `base` itself (so "." and "a/.."
 * are refused). Lexical first, then every existing component is checked on disk (see assertInsideOnDisk), so a symlink inside the
 * artifact cannot redirect outside it. Returns the absolute path.
 */
export function contained(base, relative, label) {
  if (typeof relative !== "string" || !relative || path.isAbsolute(relative) || path.win32.isAbsolute(relative) || relative.split(/[\\/]/).includes("..")) {
    fail("SIGN_DEV_ARTIFACT_PATH_INVALID", `${label} must be a relative path inside the artifact directory: ${JSON.stringify(relative)}`);
  }
  const absolute = path.resolve(base, relative);
  if (absolute === path.resolve(base) || !isWithin(path.resolve(base), absolute)) {
    fail("SIGN_DEV_ARTIFACT_PATH_INVALID", `${label} must name a path below the artifact directory, not the directory itself: ${JSON.stringify(relative)}`);
  }
  return assertInsideOnDisk(base, absolute, label);
}
