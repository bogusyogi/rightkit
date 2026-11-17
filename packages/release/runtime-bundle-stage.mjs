// Internal planner for a receipt-backed mixed runtime bundle. It performs no writes.
import { createHash } from "node:crypto";
import fs from "node:fs";
import path from "node:path";

const HASH = /^[0-9a-f]{64}$/;
const relative = (value) => typeof value === "string" && value.length > 0 &&
  !value.includes("\\") && !value.includes("\0") && !path.posix.isAbsolute(value) &&
  !/^[A-Za-z]:/.test(value) && value.split("/").every((part) => part && part !== "." && part !== "..");
const fail = (message) => { throw new Error(`runtime bundle: ${message}`); };

// Check every existing path component before any staging writer runs. Missing
// components are allowed; links, reparse points and special files are not.
export function checkRuntimeBundlePath(root, dir, io = fs) {
  if (!path.isAbsolute(root) || !relative(dir)) fail("invalid root or directory");
  let current = path.parse(root).root;
  const parts = path.relative(current, path.resolve(root, ...dir.split("/"))).split(path.sep);
  for (const part of parts) {
    current = path.join(current, part);
    let stat;
    try { stat = io.lstatSync(current); } catch (error) {
      if (error.code === "ENOENT") continue;
      throw error;
    }
    if (stat.isSymbolicLink() || !stat.isDirectory()) fail(`unsafe directory ${current}`);
  }
  const bundle = path.resolve(root, ...dir.split("/"));
  const walk = (folder) => {
    for (const item of io.readdirSync(folder, { withFileTypes: true })) {
      const full = path.join(folder, item.name);
      const stat = io.lstatSync(full);
      if (stat.isSymbolicLink()) fail(`symbolic link ${full}`);
      if (stat.isDirectory()) walk(full);
      else if (!stat.isFile()) fail(`non-regular file ${full}`);
    }
  };
  if (io.existsSync(bundle)) walk(bundle);
}

/** Build strict rightkit-onnx schema 1 from outputs already verified by staging.
 * `catalogRow` is the actual selected, validated catalog row, not a caller-supplied substitute.
 * `rows` are earlier stage receipt rows; nested notices carry their own digest.
 */
export function planRuntimeBundle({ root, dir, target, identity, catalogRow, rows, io = fs }) {
  checkRuntimeBundlePath(root, dir, io);
  if (!identity || ![identity.version, identity.variant, identity.catalogRow].every((v) => typeof v === "string" && v.trim() === v && v)) fail("invalid ONNX identity");
  if (!target || !["macos", "windows", "linux"].includes(target.os) || !["aarch64", "x86_64", "universal"].includes(target.arch)) fail("invalid target");
  const row = catalogRow;
  if (!row || row.library !== "onnxruntime" || row.id !== identity.catalogRow || row.version !== identity.version || row.os !== target.os || row.arch !== target.arch || !["approved", "pinned"].includes(row.status)) fail("ONNX identity does not match selected catalog row");
  if (row.variant !== undefined && row.variant !== identity.variant) fail("ONNX variant does not match catalog");
  const directml = Array.isArray(row.companions) && row.companions.some((id) => typeof id === "string" && /^directml-/.test(id));
  if (directml && identity.variant !== "directml") fail("ONNX variant conflicts with DirectML companion");
  if (identity.variant === "directml" && !directml) fail("ONNX catalog row has no DirectML companion");
  if (!Array.isArray(rows)) fail("missing stage rows");
  const base = `${dir}/`;
  const candidates = [];
  for (const item of rows) {
    if (item.path?.startsWith(base)) candidates.push(item);
    for (const notice of item.notices ?? []) if (notice.path?.startsWith(base)) candidates.push(notice);
  }
  const onnx = rows.filter((item) => (item.kind === "onnxruntime" || (item.kind === "native-library" && item.library === "onnxruntime")) && item.path?.startsWith(base) && !item.companionOf);
  if (onnx.length !== 1 || onnx[0].catalogRow !== row.id || onnx[0].version !== row.version) fail("expected exactly one matching staged ONNX row in bundle");
  const listed = new Map();
  for (const item of candidates) {
    const name = item.path.slice(base.length);
    if (!relative(name) || name.toLowerCase() === "manifest.json" || !HASH.test(item.sha256 ?? "")) fail("invalid staged file row");
    const key = name.toLowerCase();
    const previous = listed.get(key);
    if (previous) {
      if (previous.path !== name || previous.sha256 !== item.sha256) fail(`conflicting staged file ${name}`);
      continue;
    }
    const full = path.resolve(root, ...item.path.split("/"));
    const stat = io.lstatSync(full);
    if (!stat.isFile() || stat.isSymbolicLink()) fail(`not a regular file: ${name}`);
    const bytes = io.readFileSync(full);
    if (createHash("sha256").update(bytes).digest("hex") !== item.sha256 || (item.sizeBytes !== undefined && item.sizeBytes !== bytes.length)) fail(`staged file changed: ${name}`);
    listed.set(key, { path: name, sha256: item.sha256, bytes: bytes.length });
  }
  if (!listed.size) fail("empty bundle");
  const bundle = path.resolve(root, ...dir.split("/"));
  const walk = (folder, prefix = "") => {
    for (const item of io.readdirSync(folder, { withFileTypes: true })) {
      const name = prefix ? `${prefix}/${item.name}` : item.name;
      if (item.isDirectory()) walk(path.join(folder, item.name), name);
      else if (name !== "manifest.json" && listed.get(name.toLowerCase())?.path !== name) fail(`unlisted file: ${name}`);
    }
  };
  walk(bundle);
  const files = [...listed.values()].sort((a, b) => a.path < b.path ? -1 : a.path > b.path ? 1 : 0);
  return { schema: 1, platform: `${target.os}-${target.arch.replaceAll("_", "-")}`, onnxruntime: { version: identity.version, variant: identity.variant, catalogRow: identity.catalogRow }, files };
}
