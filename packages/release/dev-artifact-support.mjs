// Shared by sign-dev-artifact.mjs and dev-artifact-refresh.mjs (Orthic-Labs/rightkit#257): the typed error and the path helpers.
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

/** A path relative to `base` that stays inside it: no absolute path, no `..`, not empty. Returns the absolute path. */
export function contained(base, relative, label) {
  if (typeof relative !== "string" || !relative || path.isAbsolute(relative) || path.win32.isAbsolute(relative) || relative.split(/[\\/]/).includes("..")) {
    fail("SIGN_DEV_ARTIFACT_PATH_INVALID", `${label} must be a relative path inside the artifact directory: ${JSON.stringify(relative)}`);
  }
  return path.resolve(base, relative);
}

/** True when `child` is `parent` or lies below it. Both absolute. */
export const isWithin = (parent, child) => child === parent || child.startsWith(parent + path.sep);
