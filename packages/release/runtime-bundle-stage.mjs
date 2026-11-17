// Internal planner for a receipt-backed mixed runtime bundle. It performs no writes.
import { createHash } from "node:crypto";
import fs from "node:fs";
import path from "node:path";

const HASH = /^[0-9a-f]{64}$/;
export const runtimeBundleRelative = (value) => typeof value === "string" && value.length > 0 &&
  !/[\\\x00-\x1f<>:"|?*]/.test(value) && !path.posix.isAbsolute(value) &&
  value.split("/").every((part) => part && part !== "." && part !== ".." &&
    !/[. ]$/.test(part) && !/^(?:con|conin\$|conout\$|prn|aux|nul|com[1-9¹²³]|lpt[1-9¹²³])(?:\.|$)/i.test(part));
const fail = (message) => { throw new Error(`runtime bundle: ${message}`); };

// Check every existing path component before any staging writer runs. Missing
// components are allowed; links, reparse points and special files are not.
export function checkRuntimeBundlePath(root, dir, io = fs) {
  if (!path.isAbsolute(root) || !runtimeBundleRelative(dir)) fail("invalid root or directory");
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
  if (!bundle.startsWith(`${path.resolve(root)}${path.sep}`)) fail("directory escapes root");
  let parent = path.resolve(root);
  for (const part of dir.split("/")) {
    if (io.existsSync(parent)) {
      const names = io.readdirSync(parent);
      const child = path.join(parent, part);
      if (io.existsSync(child) && !names.includes(part)) fail(`directory spelling differs: ${part}`);
      if (!names.includes(part) && names.some((name) => name.toLowerCase() === part.toLowerCase())) fail(`directory spelling differs: ${part}`);
    }
    parent = path.join(parent, part);
  }
  const walk = (folder) => {
    const entries = io.readdirSync(folder, { withFileTypes: true });
    const names = new Set();
    for (const item of entries) {
      const folded = item.name.toLowerCase();
      if (names.has(folded) || (folder === bundle && folded === "manifest.json" && item.name !== "manifest.json")) fail(`case-colliding entry: ${item.name}`);
      names.add(folded);
      if (!runtimeBundleRelative(item.name)) fail(`unsafe path component: ${item.name}`);
      const full = path.join(folder, item.name);
      const stat = io.lstatSync(full);
      if (stat.isSymbolicLink()) fail(`symbolic link ${full}`);
      if (stat.isDirectory()) walk(full);
      else if (!stat.isFile()) fail(`non-regular file ${full}`);
    }
  };
  if (io.existsSync(bundle)) walk(bundle);
}

/** Reject aliases and links in any existing component of a planned output. */
export function checkRuntimeBundleOutput(root, output, io = fs) {
  if (!path.isAbsolute(root) || !runtimeBundleRelative(output)) fail("invalid planned output");
  const base = path.resolve(root);
  const resolved = path.resolve(base, ...output.split("/"));
  if (!resolved.startsWith(`${base}${path.sep}`)) fail("planned output escapes root");
  let parent = base;
  const parts = output.split("/");
  for (let index = 0; index < parts.length; index += 1) {
    const part = parts[index];
    if (!io.existsSync(parent)) break;
    const names = io.readdirSync(parent);
    const child = path.join(parent, part);
    if ((io.existsSync(child) && !names.includes(part)) || (!names.includes(part) && names.some((name) => name.toLowerCase() === part.toLowerCase()))) fail(`output spelling differs: ${output}`);
    if (!names.includes(part)) break;
    const stat = io.lstatSync(child);
    if (stat.isSymbolicLink() || (index < parts.length - 1 && !stat.isDirectory()) || (index === parts.length - 1 && !stat.isDirectory() && !stat.isFile())) fail(`unsafe output: ${output}`);
    parent = child;
  }
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
  const servingArch = row?.arch === target.arch || (target.os === "macos" && target.arch !== "universal" && row?.arch === "universal");
  if (!row || row.library !== "onnxruntime" || row.id !== identity.catalogRow || row.version !== identity.version || row.os !== target.os || !servingArch || !["approved", "pinned"].includes(row.status)) fail("ONNX identity does not match selected catalog row");
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
  const canonicalOnnx = { windows: "onnxruntime.dll", macos: "libonnxruntime.dylib", linux: "libonnxruntime.so" }[target.os];
  if (onnx[0].path !== `${base}${canonicalOnnx}`) fail("ONNX file does not have the runtime loader's canonical name");
  if (directml) {
    const companions = row.companions.filter((id) => typeof id === "string" && /^directml-/.test(id));
    if (companions.length !== 1 || rows.filter((item) => ((item.kind === "native-library" && item.library === "directml") || (item.kind === "onnxruntime" && item.companionOf === row.id)) && item.catalogRow === companions[0] && item.path === `${base}DirectML.dll`).length !== 1) fail("selected DirectML companion was not staged in bundle");
  }
  const listed = new Map();
  for (const item of candidates) {
    const name = item.path.slice(base.length);
    if (!runtimeBundleRelative(name) || name.toLowerCase() === "manifest.json" || !HASH.test(item.sha256 ?? "")) fail("invalid staged file row");
    const key = name.toLowerCase();
    const previous = listed.get(key);
    if (previous) {
      if (previous.path !== name || previous.sha256 !== item.sha256) fail(`conflicting staged file ${name}`);
      continue;
    }
    const full = path.resolve(root, ...item.path.split("/"));
    const bundleRoot = path.resolve(root, ...dir.split("/"));
    if (!full.startsWith(`${bundleRoot}${path.sep}`)) fail(`file escapes bundle: ${name}`);
    let parent = bundleRoot;
    for (const part of name.split("/")) {
      if (!io.readdirSync(parent).includes(part)) fail(`file spelling differs: ${name}`);
      parent = path.join(parent, part);
    }
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
  const arch = { x86_64: "x64", aarch64: "arm64", universal: "universal" }[target.arch];
  return { schema: 1, platform: `${target.os}-${arch}`, onnxruntime: { version: identity.version, variant: identity.variant, catalogRow: identity.catalogRow }, files };
}
