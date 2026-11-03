// Refresh a bundled runtime manifest after nested code was re-signed (rightkit#722).
//
// An app that ships native runtime libraries or models (ffmpeg, ffprobe, onnxruntime, DirectML, ONNX models) describes them in a
// bundled `runtime/manifest.json`: { schema, ..., files: [{ path, sha256, bytes }] } with every `path` relative to the manifest's
// directory. Signing rewrites a binary's bytes, so a manifest written before signing no longer matches, and the app's fail-closed
// runtime check rejects the shipped files. The sign step therefore calls this between "nested code signed" and "app sealed": it
// re-hashes every listed file in full and rewrites ONLY `sha256` and `bytes` of the rows (every other field, and the row order,
// stay as the app wrote them). A listed file that is missing, outside the manifest directory, a symlink or a directory refuses.
//
// Config: `sign.runtimeManifests: ["<path to manifest.json>", ...]`. On macOS each path is relative to the signed .app bundle
// (`Contents/Resources/runtime/manifest.json`); on Windows it is relative to the app root (the staged resource folder that the
// installer wraps). The check is `validateRuntimeManifestPaths`; the effect is `refreshRuntimeManifest`.
import { createHash } from "node:crypto";
import { lstatSync, readFileSync, renameSync, writeFileSync, rmSync } from "node:fs";
import path from "node:path";

export class RuntimeManifestError extends Error {
  constructor(message) { super(message); this.name = "RuntimeManifestError"; this.code = "RK_RUNTIME_MANIFEST"; }
}
const fail = (message) => { throw new RuntimeManifestError(message); };
const SHA256 = /^[0-9a-f]{64}$/;

/** `sign.runtimeManifests`: a non-empty list of unique relative .json paths with no `..`. Returns the normalized list (empty when absent). */
export function validateRuntimeManifestPaths(value, label = "sign.runtimeManifests") {
  if (value === undefined) return [];
  if (!Array.isArray(value) || value.length === 0) fail(`${label} must be a non-empty array of relative manifest.json paths`);
  const seen = new Set();
  return value.map((entry, index) => {
    if (typeof entry !== "string" || !entry.trim() || entry !== entry.trim() || entry.includes("\0")) fail(`${label}[${index}] must be a non-empty path string`);
    const normalized = entry.replaceAll("\\", "/");
    if (path.posix.isAbsolute(normalized) || /^[A-Za-z]:/.test(normalized) || normalized.split("/").includes("..")) fail(`${label}[${index}] must be relative and stay inside the bundle: ${entry}`);
    if (!/\.json$/i.test(normalized)) fail(`${label}[${index}] must be a .json file: ${entry}`);
    if (seen.has(normalized)) fail(`${label}[${index}] duplicates an earlier entry: ${entry}`);
    seen.add(normalized);
    return normalized;
  });
}

const sha256Of = (bytes) => createHash("sha256").update(bytes).digest("hex");

/**
 * Re-describe every file of one manifest. Returns { manifest, changed: [paths whose sha256 or bytes changed], files: n }.
 * The file is rewritten (temp file beside it, then rename) only when a row changed.
 */
export function refreshRuntimeManifest({ manifest, fs = {} }) {
  const lstat = fs.lstatSync ?? lstatSync;
  const read = fs.readFileSync ?? readFileSync;
  const write = fs.writeFileSync ?? writeFileSync;
  const rename = fs.renameSync ?? renameSync;
  const remove = fs.rmSync ?? rmSync;
  let parsed;
  try { parsed = JSON.parse(String(read(manifest))); } catch (error) { fail(`cannot read runtime manifest ${manifest}: ${error.message}`); }
  if (parsed === null || typeof parsed !== "object" || Array.isArray(parsed) || !Array.isArray(parsed.files) || parsed.files.length === 0) fail(`${manifest} must be an object with a non-empty files array`);
  const dir = path.dirname(manifest);
  const changed = [];
  const seen = new Set();
  const files = parsed.files.map((row, index) => {
    if (row === null || typeof row !== "object" || Array.isArray(row) || typeof row.path !== "string" || !row.path) fail(`${manifest} files[${index}] needs a path`);
    const rel = row.path.replaceAll("\\", "/");
    if (path.posix.isAbsolute(rel) || /^[A-Za-z]:/.test(rel) || rel.split("/").includes("..")) fail(`${manifest} files[${index}].path must stay inside the manifest directory: ${row.path}`);
    if (seen.has(rel.toLowerCase())) fail(`${manifest} lists ${row.path} twice`);
    seen.add(rel.toLowerCase());
    const file = path.join(dir, ...rel.split("/"));
    let stat;
    try { stat = lstat(file); } catch { fail(`${manifest} lists ${row.path} but ${file} does not exist`); }
    if (stat.isSymbolicLink() || !stat.isFile()) fail(`${manifest} lists ${row.path}, which is not a regular file`);
    const bytes = read(file);
    const sha256 = sha256Of(bytes);
    if (row.sha256 !== undefined && !SHA256.test(String(row.sha256))) fail(`${manifest} files[${index}].sha256 is not a lowercase sha256`);
    if (row.sha256 !== sha256 || row.bytes !== bytes.length) changed.push(row.path);
    return { ...row, sha256, bytes: bytes.length };
  });
  if (changed.length) {
    const next = `${JSON.stringify({ ...parsed, files }, null, 2)}\n`;
    const temp = path.join(dir, `.${path.basename(manifest)}.refresh-${process.pid}.tmp`);
    try { write(temp, next); rename(temp, manifest); } finally { remove(temp, { force: true }); }
  }
  return { manifest, changed, files: files.length };
}

/** Refresh every declared manifest under `base` (the signed .app on macOS, the app root on Windows). */
export function refreshRuntimeManifests({ base, manifests, fs = {}, log = () => {} }) {
  return manifests.map((relative) => {
    const result = refreshRuntimeManifest({ manifest: path.join(base, ...relative.split("/")), fs });
    log(`right-release: runtime manifest ${relative}: ${result.files} file(s), ${result.changed.length} re-described${result.changed.length ? ` (${result.changed.join(", ")})` : ""}`);
    return result;
  });
}
