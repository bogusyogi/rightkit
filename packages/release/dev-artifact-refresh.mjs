// Declarative metadata refresh for right-release sign-dev-artifact (Orthic-Labs/rightkit#257).
//
// Signing changes bytes, so a JSON file that records a digest of signed bytes goes stale. The sign manifest declares, per value, which
// JSON pointer of which file is recomputed from which artifact path:
//
//   { "schema": 1, "refreshRoot": "<dir inside the artifact>", "entries": [...],
//     "refresh": [ { "file": "share/app/release.json", "pointer": "/runtime/sha256", "compute": "sha256-file", "of": "bin/app" } ] }
//
// `file` and `of` are relative to `refreshRoot` (default: the artifact root). Computes:
//   sha256-file       hex sha256 of the bytes of `of`
//   sha256-tree-v1    hex sha256 over the regular files below the directory `of`: sorted by relative POSIX path (UTF-8 bytes), each
//                     contributing path bytes, 0x00, the length as 8 big-endian bytes, then the file bytes. Symlinks and
//                     non-regular files are refused.
//   verify-sha256-file  does not write: refuses unless the value at the pointer already equals the sha256 of `of` (a digest that must
//                     survive signing unchanged)
// Optional per operation: `format` (a template with one `{value}`, applied to the computed digest, for locator strings that embed it) and
// `currentMatches` (a regular expression the CURRENT string value must match before it is overwritten, so a value of another kind, such as
// a signed attestation locator, is refused instead of replaced).
//
// Ordering is enforced, never reordered: every operation runs after the manifest entries it reads (the entries equal to or below `of`)
// are signed and before the first entry that contains a file the refresh writes, so the bundle holding refreshed files is signed last
// over their final bytes. An operation may not read a file a later operation writes. JSON is rewritten in place keeping key order, and a
// file the serializer cannot reproduce byte for byte (apart from the edited value) is refused rather than reformatted.
import { createHash } from "node:crypto";
import { existsSync, lstatSync, readdirSync, readFileSync, renameSync, writeFileSync } from "node:fs";
import path from "node:path";
import { contained, fail, isWithin, toPosix } from "./dev-artifact-support.mjs";

export const REFRESH_COMPUTES = Object.freeze(["sha256-file", "sha256-tree-v1", "verify-sha256-file"]);
const OPERATION_KEYS = new Set(["file", "pointer", "compute", "of", "format", "currentMatches"]);

const sha256 = (bytes) => createHash("sha256").update(bytes).digest("hex");

/** Digest of the regular files below `root` (see the header). Refuses symlinks and anything that is not a regular file or directory. */
export function sha256TreeV1(root) {
  const files = [];
  const visit = (directory) => {
    for (const name of readdirSync(directory)) {
      const absolute = path.join(directory, name);
      const info = lstatSync(absolute);
      if (info.isSymbolicLink()) fail("SIGN_DEV_ARTIFACT_REFRESH_TREE_INVALID", `refresh tree contains a symlink: ${toPosix(path.relative(root, absolute))}`);
      if (info.isDirectory()) visit(absolute);
      else if (info.isFile()) files.push({ relative: toPosix(path.relative(root, absolute)), absolute });
      else fail("SIGN_DEV_ARTIFACT_REFRESH_TREE_INVALID", `refresh tree contains a non-regular file: ${toPosix(path.relative(root, absolute))}`);
    }
  };
  if (!lstatSync(root).isDirectory()) fail("SIGN_DEV_ARTIFACT_REFRESH_TREE_INVALID", `sha256-tree-v1 needs a directory: ${root}`);
  visit(root);
  files.sort((a, b) => Buffer.compare(Buffer.from(a.relative), Buffer.from(b.relative)));
  const digest = createHash("sha256");
  for (const { relative, absolute } of files) {
    const bytes = readFileSync(absolute);
    const length = Buffer.alloc(8);
    length.writeBigUInt64BE(BigInt(bytes.length));
    digest.update(Buffer.from(relative)).update(Buffer.from([0])).update(length).update(bytes);
  }
  return digest.digest("hex");
}

const decodePointer = (pointer) => pointer.slice(1).split("/").map((token) => token.replace(/~1/g, "/").replace(/~0/g, "~"));

/** Parse and validate `refresh` and `refreshRoot` of a raw sign manifest. Returns { root, operations } with absolute paths. */
export function readRefreshPlan(raw, artifact) {
  if (raw.refresh === undefined && raw.refreshRoot === undefined) return { root: artifact, operations: [] };
  if (!Array.isArray(raw.refresh) || raw.refresh.length === 0) fail("SIGN_DEV_ARTIFACT_REFRESH_INVALID", "sign manifest refresh must be a non-empty array of operations");
  const root = raw.refreshRoot === undefined ? artifact : contained(artifact, raw.refreshRoot, "sign manifest refreshRoot");
  const operations = raw.refresh.map((item, index) => {
    const label = `sign manifest refresh[${index}]`;
    if (!item || typeof item !== "object" || Array.isArray(item)) fail("SIGN_DEV_ARTIFACT_REFRESH_INVALID", `${label} must be an object`);
    for (const key of Object.keys(item)) if (!OPERATION_KEYS.has(key)) fail("SIGN_DEV_ARTIFACT_REFRESH_INVALID", `${label} has an unknown field: ${key}`);
    if (!REFRESH_COMPUTES.includes(item.compute)) fail("SIGN_DEV_ARTIFACT_REFRESH_INVALID", `${label}.compute must be one of ${REFRESH_COMPUTES.join(", ")}`);
    if (typeof item.pointer !== "string" || !item.pointer.startsWith("/") || /~(?![01])/.test(item.pointer)) fail("SIGN_DEV_ARTIFACT_REFRESH_INVALID", `${label}.pointer must be a JSON pointer starting with "/"`);
    if (item.format !== undefined && (typeof item.format !== "string" || item.format.split("{value}").length !== 2)) fail("SIGN_DEV_ARTIFACT_REFRESH_INVALID", `${label}.format must be a string with exactly one {value}`);
    if (item.format !== undefined && item.compute === "verify-sha256-file") fail("SIGN_DEV_ARTIFACT_REFRESH_INVALID", `${label}.format does not apply to verify-sha256-file`);
    let currentMatches;
    if (item.currentMatches !== undefined) {
      if (typeof item.currentMatches !== "string" || item.currentMatches.length > 200) fail("SIGN_DEV_ARTIFACT_REFRESH_INVALID", `${label}.currentMatches must be a short regular expression string`);
      try { currentMatches = new RegExp(item.currentMatches); } catch { fail("SIGN_DEV_ARTIFACT_REFRESH_INVALID", `${label}.currentMatches is not a valid regular expression`); }
    }
    return {
      index, file: contained(root, item.file, `${label}.file`), of: contained(root, item.of, `${label}.of`),
      pointer: item.pointer, compute: item.compute, format: item.format, currentMatches, writes: item.compute !== "verify-sha256-file",
      declared: { file: item.file, pointer: item.pointer, compute: item.compute, of: item.of },
    };
  });
  return { root, operations };
}

/**
 * Split the manifest entries around the refresh and enforce the ordering rules. Returns `{ before, after }`: the entries to sign
 * before the refresh runs and the entries (the first one containing a refreshed file, and everything after it) to sign after it.
 * Also refuses a payload metadata file that sits inside a signed entry, since refreshing it after that entry is signed would break
 * the signature. `entries` carry absolute `file` paths.
 */
export function planRefresh(entries, operations, metadataFile) {
  const insideSigned = entries.find((entry) => isWithin(entry.file, metadataFile));
  if (insideSigned) {
    fail("SIGN_DEV_ARTIFACT_METADATA_IN_SIGNED_ENTRY", `payload metadata ${path.basename(metadataFile)} lies inside the signed entry ${path.basename(insideSigned.file)}, and refreshing its hashes after signing would break that signature. Keep the payload metadata outside every signed bundle (for example at the artifact root).`);
  }
  if (operations.length === 0) return { before: entries, after: [] };
  const written = operations.filter((operation) => operation.writes).map((operation) => operation.file);
  const firstContaining = entries.findIndex((entry) => written.some((file) => isWithin(entry.file, file)));
  const split = firstContaining < 0 ? entries.length : firstContaining;
  for (const operation of operations) {
    entries.forEach((entry, index) => {
      if (isWithin(operation.of, entry.file) && index >= split) {
        fail("SIGN_DEV_ARTIFACT_REFRESH_ORDER", `refresh reads ${toPosix(path.relative(path.dirname(operation.of), entry.file)) || path.basename(entry.file)}, which is signed after the bundle holding the refreshed files; list it before that bundle so its final bytes are read`);
      }
    });
    operations.forEach((later) => {
      if (later.index > operation.index && later.writes && isWithin(operation.of, later.file)) {
        fail("SIGN_DEV_ARTIFACT_REFRESH_ORDER", `refresh[${operation.index}] reads a file that refresh[${later.index}] writes later; list the writer first`);
      }
    });
  }
  for (const operation of operations) {
    if (!existsSync(operation.of)) fail("SIGN_DEV_ARTIFACT_MISSING", `refresh input is missing: ${operation.declared.of}`);
    if (!existsSync(operation.file)) fail("SIGN_DEV_ARTIFACT_MISSING", `refresh file is missing: ${operation.declared.file}`);
  }
  return { before: entries.slice(0, split), after: entries.slice(split) };
}

function readJson(file, label) {
  const text = readFileSync(file, "utf8");
  let value;
  try { value = JSON.parse(text); } catch { fail("SIGN_DEV_ARTIFACT_REFRESH_JSON_INVALID", `${label} is not readable JSON`); }
  const indent = /^\{\n( +)"/.exec(text)?.[1].length ?? 2;
  const newline = text.endsWith("\n") ? "\n" : "";
  const serialize = (candidate) => `${JSON.stringify(candidate, null, indent)}${newline}`;
  if (serialize(value) !== text) fail("SIGN_DEV_ARTIFACT_REFRESH_JSON_FORMAT", `${label} is not formatted the way this tool writes JSON (${indent}-space indent, key order kept), so refreshing it would reformat more than the edited value; refusing`);
  return { value, serialize };
}

function locate(root, pointer, label) {
  const tokens = decodePointer(pointer);
  let container = root;
  for (const token of tokens.slice(0, -1)) {
    if (container === null || typeof container !== "object" || !Object.hasOwn(container, token)) fail("SIGN_DEV_ARTIFACT_REFRESH_POINTER_MISSING", `${label}: pointer ${pointer} does not exist; the refresh only updates values that are already there`);
    container = container[token];
  }
  const key = tokens.at(-1);
  if (container === null || typeof container !== "object" || Array.isArray(container) || !Object.hasOwn(container, key)) fail("SIGN_DEV_ARTIFACT_REFRESH_POINTER_MISSING", `${label}: pointer ${pointer} does not exist; the refresh only updates values that are already there`);
  return { container, key };
}

/**
 * Run the operations in order. Returns one record per operation: `{ file, pointer, compute, of, before, after, changed }`
 * (`verified: true` and no change for verify-sha256-file). Writes atomically, one operation at a time.
 */
export function applyRefresh(operations, root) {
  return operations.map((operation) => {
    const label = `${operation.declared.file} ${operation.pointer}`;
    const digest = operation.compute === "sha256-tree-v1" ? sha256TreeV1(operation.of) : sha256(readFileSync(operation.of));
    const { value, serialize } = readJson(operation.file, operation.declared.file);
    const { container, key } = locate(value, operation.pointer, label);
    const before = container[key];
    if (typeof before !== "string") fail("SIGN_DEV_ARTIFACT_REFRESH_POINTER_INVALID", `${label} is not a string value`);
    const record = { file: toPosix(path.relative(root, operation.file)), pointer: operation.pointer, compute: operation.compute, of: toPosix(path.relative(root, operation.of)), before };
    if (operation.compute === "verify-sha256-file") {
      if (before !== digest) fail("SIGN_DEV_ARTIFACT_REFRESH_VERIFY_FAILED", `${label} records ${before}, but ${operation.declared.of} is now ${digest}; this digest must survive signing unchanged`);
      return { ...record, after: before, changed: false, verified: true };
    }
    if (operation.currentMatches && !operation.currentMatches.test(before)) fail("SIGN_DEV_ARTIFACT_REFRESH_UNRECOMPUTABLE", `${label} is ${JSON.stringify(before)}, which does not match ${operation.currentMatches}; it is not a value this refresh can recompute, so it is left alone and signing stops`);
    const after = operation.format === undefined ? digest : operation.format.replace("{value}", digest);
    container[key] = after;
    const staging = `${operation.file}.refresh`;
    writeFileSync(staging, serialize(value));
    renameSync(staging, operation.file);
    return { ...record, after, changed: after !== before };
  });
}
