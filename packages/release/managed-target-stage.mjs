// Broker-managed Cargo target directories (bogusyogi/rightkit#924). On a RustRight-managed host the broker owns the
// Cargo target directory outside the app, but release.mjs addresses every artifact by an app-relative path (no `..`).
// A recipe whose Cargo target directory is outside the app AND under a managed root therefore stages its declared
// artifacts: after the build they are copied from the real target directory to their default app-relative location,
// and before a later step that reads the signed raw EXE from the target directory (tauri bundle) the signed copy is
// copied back. A `fresh` source is deleted before the build and every source is polled until stable before it is
// copied, so only this build's finished output is staged. Every copy is atomic (temp file beside the destination, then rename) and verified by sha256 against its
// source; a stale destination is deleted first, so a failed copy can never leave an old artifact to be signed or sealed.
import { createHash } from "node:crypto";
import { cpSync, lstatSync, mkdirSync, readdirSync, readFileSync, readlinkSync, realpathSync, renameSync, rmSync } from "node:fs";
import path from "node:path";
import { managedControlCandidates } from "./managed-roots.mjs";

export class ManagedTargetStageError extends Error {
  constructor(code, message, details = {}) { super(message); this.name = "ManagedTargetStageError"; this.code = code; this.details = details; }
}
const fail = (code, message, details) => { throw new ManagedTargetStageError(code, message, details); };

const sha256Of = (bytes) => createHash("sha256").update(bytes).digest("hex");

function realOrSelf(value) {
  try { return realpathSync(value); } catch { return path.resolve(value); }
}

/** The directories the RustRight broker owns: the managed data and control roots (and the legacy root). */
export function defaultManagedRoots(env = process.env) {
  return managedControlCandidates({ env }).filter((root) => typeof root === "string" && path.isAbsolute(root));
}

/** True when `dir` is strictly under one of `roots` (case-insensitive on Windows). Symlinks are resolved when they exist. */
export function isUnderManagedRoot(dir, roots, { platform = process.platform } = {}) {
  const fold = (value) => (platform === "win32" ? value.toLowerCase() : value);
  const target = fold(realOrSelf(dir));
  return roots.some((root) => {
    if (typeof root !== "string" || !path.isAbsolute(root)) return false;
    const relative = path.relative(fold(realOrSelf(root)), target);
    return relative !== "" && !relative.startsWith("..") && !path.isAbsolute(relative) && !/^[A-Za-z]:/.test(relative);
  });
}

/**
 * A content digest of a file, or of a directory tree (relative path, kind, file sha256, symlink target and executable bit
 * of every entry, sorted). Symlinks are never followed.
 */
export function contentDigest(file, fs = {}) {
  const lstat = fs.lstatSync ?? lstatSync;
  const readDir = fs.readdirSync ?? readdirSync;
  const readFile = fs.readFileSync ?? readFileSync;
  const readLink = fs.readlinkSync ?? readlinkSync;
  const stat = lstat(file);
  if (stat.isSymbolicLink()) return { kind: "symlink", sha256: sha256Of(`link:${readLink(file)}`) };
  if (stat.isFile()) return { kind: "file", sha256: sha256Of(readFile(file)) };
  if (!stat.isDirectory()) fail("STAGE_ARTIFACT_UNSUPPORTED", `${file} is neither a file nor a directory`);
  const lines = [];
  const walk = (dir, prefix) => {
    for (const name of readDir(dir).sort()) {
      const full = path.join(dir, name);
      const relative = prefix ? `${prefix}/${name}` : name;
      const entry = lstat(full);
      if (entry.isSymbolicLink()) lines.push(`l ${relative} ${readLink(full)}`);
      else if (entry.isDirectory()) { lines.push(`d ${relative}`); walk(full, relative); }
      else if (entry.isFile()) lines.push(`f ${relative} ${(entry.mode & 0o111) !== 0 ? "x" : "-"} ${sha256Of(readFile(full))}`);
      else fail("STAGE_ARTIFACT_UNSUPPORTED", `${full} is neither a file, a directory nor a symlink`);
    }
  };
  walk(file, "");
  return { kind: "directory", sha256: sha256Of(lines.join("\n")) };
}

/**
 * Copy one artifact from `from` to `to` (both absolute): delete a stale destination, check the source exists, copy beside
 * the destination, verify the copy's sha256 against the source (read before and after the copy), then rename into place.
 * Returns { from, to, kind, sha256 }.
 */
export function stageArtifact({ from, to }, { fs = {}, log = () => {} } = {}) {
  const remove = fs.rmSync ?? rmSync;
  const copy = fs.cpSync ?? cpSync;
  const rename = fs.renameSync ?? renameSync;
  const makeDir = fs.mkdirSync ?? mkdirSync;
  // A destination left by an earlier build is never an acceptable result: remove it before anything else.
  remove(to, { recursive: true, force: true });
  if (!(fs.lstatSync ?? lstatSync)(from, { throwIfNoEntry: false })) {
    fail("STAGE_SOURCE_MISSING", `managed target stage: the build did not produce ${from}; ${to} was removed and nothing was copied, so no stale artifact can be packaged`, { from, to });
  }
  const before = contentDigest(from, fs);
  const temp = path.join(path.dirname(to), `.${path.basename(to)}.stage-${process.pid}-${Date.now()}.tmp`);
  makeDir(path.dirname(to), { recursive: true });
  remove(temp, { recursive: true, force: true });
  try {
    copy(from, temp, { recursive: true, verbatimSymlinks: true, preserveTimestamps: true, force: false, errorOnExist: true });
    const copied = contentDigest(temp, fs);
    const after = contentDigest(from, fs);
    if (copied.sha256 !== before.sha256 || after.sha256 !== before.sha256) {
      fail("STAGE_DIGEST_MISMATCH", `managed target stage: sha256 of the copy of ${from} does not match its source (source ${before.sha256}, copy ${copied.sha256}, source after copy ${after.sha256}); ${to} was not created`, { from, to });
    }
    rename(temp, to);
  } finally {
    remove(temp, { recursive: true, force: true });
  }
  log(`recipe-build: staged ${from} -> ${to} (${before.kind} sha256 ${before.sha256})`);
  return { from, to, kind: before.kind, sha256: before.sha256 };
}

/** Remove the sources named `fresh` before a build step, so a missing output after it cannot be a leftover. */
export function removeFreshSources(entries, { fs = {}, log = () => {} } = {}) {
  const remove = fs.rmSync ?? rmSync;
  for (const entry of entries) {
    if (!entry.fresh) continue;
    log(`recipe-build: stage removes stale source ${entry.from}`);
    remove(entry.from, { recursive: true, force: true });
  }
}

/** Stage every entry in order; the first failure throws and nothing after it is copied. */
export function stageArtifacts(entries, options = {}) {
  return entries.map((entry) => stageArtifact(entry, options));
}
