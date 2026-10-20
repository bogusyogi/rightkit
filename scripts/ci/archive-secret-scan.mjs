// Secretless qualification only: never import this scanner into privileged upload execution.
// Archive paths never become filesystem paths; only generated flat filenames are written.
import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import { gunzipSync } from "node:zlib";

const LIMITS = Object.freeze({ compressedBytes: 64 * 1024 * 1024, expandedBytes: 256 * 1024 * 1024, fileBytes: 32 * 1024 * 1024, entries: 20_000 });
const decoder = new TextDecoder("utf-8", { fatal: true });
const refuse = () => { throw new Error("archive refused"); };
const zero = (bytes) => bytes.every((byte) => byte === 0);
function boundedLimits(overrides = {}) {
  const limits = { ...LIMITS, ...overrides };
  if (Object.keys(limits).some((key) => !Object.hasOwn(LIMITS, key) || !Number.isSafeInteger(limits[key]) || limits[key] < 1 || limits[key] > LIMITS[key])) refuse();
  return limits;
}
function text(bytes) {
  const end = bytes.indexOf(0);
  if (end >= 0 && !zero(bytes.subarray(end))) refuse();
  return decoder.decode(end < 0 ? bytes : bytes.subarray(0, end));
}
function octal(bytes) {
  if (!bytes.every((byte) => byte === 0 || byte === 32 || (byte >= 48 && byte <= 55))) refuse();
  const value = bytes.toString("ascii").replace(/\0.*$/, "").trim();
  if (!/^[0-7]+$/.test(value)) refuse();
  const parsed = Number.parseInt(value, 8);
  if (!Number.isSafeInteger(parsed)) refuse();
  return parsed;
}
function safeName(name, directory = false) {
  if (directory && name.endsWith("/")) name = name.slice(0, -1);
  if (!name || name.length > 4096 || /[\\\x00-\x1f\x7f:]/.test(name)) refuse();
  const parts = name.split("/");
  if (parts.some((part) => !part || part === "." || part === ".." || /[. ]$/.test(part) || /^(?:con|prn|aux|nul|com[1-9]|lpt[1-9])(?:\.|$)/i.test(part))) refuse();
  return name;
}
function paxFields(bytes) {
  const fields = {};
  let offset = 0;
  while (offset < bytes.length) {
    const space = bytes.indexOf(32, offset);
    if (space < 0) refuse();
    const lengthText = bytes.subarray(offset, space).toString("ascii");
    if (!/^[1-9][0-9]*$/.test(lengthText)) refuse();
    const length = Number(lengthText);
    const end = offset + length;
    if (!Number.isSafeInteger(length) || end > bytes.length || end <= space + 2 || bytes[end - 1] !== 10) refuse();
    const record = decoder.decode(bytes.subarray(space + 1, end - 1));
    const equals = record.indexOf("=");
    if (equals < 1) refuse();
    const key = record.slice(0, equals);
    // Sparse, link & vendor extraction semantics are deliberately unsupported.
    if (!["path", "size", "mtime", "atime", "ctime", "uid", "gid", "uname", "gname"].includes(key) || Object.hasOwn(fields, key)) refuse();
    fields[key] = record.slice(equals + 1);
    offset = end;
  }
  return fields;
}

/** Strict tar reader for npm .tgz & Cargo .crate; no system tar, filesystem extraction or archived code. */
export function readArchiveEntries(bytes, { root, limits: overrides } = {}) {
  const limits = boundedLimits(overrides);
  if (!Buffer.isBuffer(bytes) || bytes.length > limits.compressedBytes || !root || safeName(root) !== root || root.includes("/")) refuse();
  const tar = gunzipSync(bytes, { maxOutputLength: limits.expandedBytes });
  const entries = [];
  const seen = new Map();
  const parents = new Set();
  let offset = 0;
  let headers = 0;
  let pending = null;
  let longName = null;
  let ended = false;
  while (offset + 512 <= tar.length) {
    const header = tar.subarray(offset, offset + 512);
    if (zero(header)) {
      if (pending || longName || offset + 1024 > tar.length || !zero(tar.subarray(offset))) refuse();
      ended = true;
      break;
    }
    if (++headers > limits.entries) refuse();
    const expected = octal(header.subarray(148, 156));
    let checksum = 0;
    for (let i = 0; i < 512; i++) checksum += i >= 148 && i < 156 ? 32 : header[i];
    if (checksum !== expected) refuse();
    const magic = text(header.subarray(257, 263));
    if (magic !== "ustar" && magic !== "ustar " && magic !== "") refuse();
    const type = header[156];
    let size = octal(header.subarray(124, 136));
    if (pending?.size !== undefined) {
      if (!/^(?:0|[1-9][0-9]*)$/.test(pending.size)) refuse();
      size = Number(pending.size);
    }
    if (!Number.isSafeInteger(size) || size > limits.fileBytes) refuse();
    const start = offset + 512;
    const next = start + Math.ceil(size / 512) * 512;
    if (next > tar.length) refuse();
    const content = tar.subarray(start, start + size);
    if (!zero(tar.subarray(start + size, next))) refuse();
    offset = next;
    if (type === 120 || type === 76) {
      if (pending || longName) refuse();
      if (type === 120) pending = paxFields(content);
      else longName = text(content);
      continue;
    }
    if (type !== 0 && type !== 48 && type !== 53) refuse(); // Includes symlinks, hardlinks & devices.
    if (text(header.subarray(157, 257))) refuse();
    // Old GNU headers put timestamps here; only POSIX ustar defines a path prefix.
    const prefix = magic === "ustar" ? text(header.subarray(345, 500)) : "";
    const raw = text(header.subarray(0, 100));
    const name = safeName(pending?.path ?? longName ?? (prefix ? `${prefix}/${raw}` : raw), type === 53);
    pending = null;
    longName = null;
    if (name !== root && !name.startsWith(`${root}/`)) refuse();
    if (name === root && type !== 53) refuse();
    if (type === 53 && size !== 0) refuse();
    const key = name.normalize("NFC").toLowerCase();
    if (seen.has(key)) refuse();
    const ancestors = key.split("/");
    ancestors.pop();
    while (ancestors.length) {
      const parent = ancestors.join("/");
      if (seen.get(parent) === "file") refuse();
      parents.add(parent);
      ancestors.pop();
    }
    if (type !== 53 && parents.has(key)) refuse();
    seen.set(key, type === 53 ? "directory" : "file");
    if (name !== root) entries.push({ path: name.slice(root.length + 1), directory: type === 53, bytes: content });
  }
  if (!ended || !entries.some((entry) => !entry.directory)) refuse();
  return entries;
}

function readBounded(file, maxBytes) {
  const fd = fs.openSync(file, "r");
  try {
    const stat = fs.fstatSync(fd);
    if (!stat.isFile() || stat.size > maxBytes) refuse();
    const out = Buffer.alloc(stat.size);
    let offset = 0;
    while (offset < out.length) {
      const read = fs.readSync(fd, out, offset, out.length - offset, offset);
      if (!read) refuse();
      offset += read;
    }
    if (fs.readSync(fd, Buffer.alloc(1), 0, 1, offset)) refuse();
    return out;
  } finally { fs.closeSync(fd); }
}

/**
 * Call before sealing or consumer verification in a token-free job. scratchRoot must be an existing
 * private scratch directory. scannerOwner may inject BOTH original publish-cargo exports in tests.
 * Reports contain paths only, never matching content or exception messages. Require status=pass &
 * archiveSha256 equal sealed unit SHA-256 in qualification receipts; upload need not load this module.
 */
export async function scanArchiveForSecrets({ file, registry, name, version, scratchRoot, scannerOwner, readArchive = readBounded, limits: overrides = {} }) {
  const report = { schema: 1, scanner: "publish-cargo/scanCrateForSecrets", archiveSha256: null, status: "fail", files: 0, violations: [], problems: [] };
  let scratch;
  try {
    const limits = boundedLimits(overrides);
    if (!file || !scratchRoot || !["npm", "cargo"].includes(registry)) refuse();
    if (registry === "cargo" && (!/^[a-zA-Z0-9_-]+$/.test(name ?? "") || !/^[0-9A-Za-z.+-]+$/.test(version ?? ""))) refuse();
    const bytes = Buffer.from(await readArchive(file, limits.compressedBytes));
    if (bytes.length > limits.compressedBytes) refuse();
    report.archiveSha256 = crypto.createHash("sha256").update(bytes).digest("hex");
    const entries = readArchiveEntries(bytes, { root: registry === "npm" ? "package" : `${name}-${version}`, limits });
    const owner = scannerOwner ?? await import("../../packages/release/publish-cargo.mjs");
    if (typeof owner.scanCrateForSecrets !== "function" || typeof owner.packageFileViolations !== "function") refuse();
    const forbidden = owner.packageFileViolations(entries.map((entry) => entry.path));
    if (!Array.isArray(forbidden) || forbidden.some((name) => !entries.some((entry) => entry.path === name))) refuse();
    const scratchParent = fs.realpathSync(scratchRoot);
    scratch = fs.mkdtempSync(path.join(scratchParent, "archive-secret-scan-"));
    fs.chmodSync(scratch, 0o700);
    const names = new Map();
    for (const entry of entries) {
      if (entry.directory) continue;
      const generated = `file-${names.size}`;
      fs.writeFileSync(path.join(scratch, generated), entry.bytes, { flag: "wx", mode: 0o600 });
      names.set(generated, entry.path);
    }
    report.files = names.size;
    const found = await owner.scanCrateForSecrets(scratch);
    if (!Array.isArray(found) || found.some((entry) => !names.has(entry))) refuse();
    report.violations = [...new Set([...forbidden, ...found.map((entry) => names.get(entry))])].sort();
    if (report.violations.length) report.problems.push("archive contains forbidden paths or secret patterns");
    else report.status = "pass";
  } catch {
    report.problems.push("archive could not be safely read or scanned");
  } finally {
    if (scratch) {
      try { fs.rmSync(scratch, { recursive: true, force: true }); }
      catch { report.status = "fail"; report.problems.push("archive scan scratch cleanup failed"); }
    }
  }
  return report;
}
