#!/usr/bin/env node
// Signature-independent PE content digest and import-closure check (bogusyogi/rightkit#445).
//   right-release pe digest <file> [--algorithm sha256|sha384|sha512] [--json]
//   right-release pe imports <file> [--allow <dll[,dll...]>]... [--allow-file <path>] [--json]
// Pure Buffer parsing, so both run on any host. The digest follows the Authenticode image hash: the CheckSum field,
// the Certificate Table directory entry and the certificate data itself are excluded, so re-signing or timestamping
// a binary never changes it.
import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import { readPeImports } from "./pe-imports.mjs";

const ALGORITHMS = new Set(["sha256", "sha384", "sha512"]);
function fail(message, code = "PE_INVALID") { throw Object.assign(new Error(message), { code }); }

function layout(buf) {
  if (buf.length < 0x40 || buf.readUInt16LE(0) !== 0x5a4d) fail("not a PE file (missing MZ)");
  const pe = buf.readUInt32LE(0x3c);
  if (pe + 24 > buf.length || buf.readUInt32LE(pe) !== 0x00004550) fail("not a PE file (missing PE signature)");
  const sectionCount = buf.readUInt16LE(pe + 6);
  const optionalSize = buf.readUInt16LE(pe + 20);
  const optional = pe + 24;
  const magic = buf.readUInt16LE(optional);
  if (magic !== 0x20b && magic !== 0x10b) fail("unsupported PE optional header");
  const directories = optional + (magic === 0x20b ? 112 : 96);
  const numberOfDirectories = buf.readUInt32LE(directories - 4);
  const checksumAt = optional + 64;
  const sizeOfHeaders = buf.readUInt32LE(optional + 60);
  const certEntryAt = directories + 4 * 8;
  const hasCert = numberOfDirectories > 4;
  if (checksumAt + 4 > buf.length || sizeOfHeaders > buf.length || (hasCert && certEntryAt + 8 > buf.length)) fail("truncated PE headers");
  const sectionsAt = optional + optionalSize;
  const sections = [];
  for (let i = 0; i < sectionCount; i++) {
    const at = sectionsAt + i * 40;
    if (at + 40 > buf.length) fail("truncated PE section table");
    const rawSize = buf.readUInt32LE(at + 16);
    const rawPtr = buf.readUInt32LE(at + 20);
    if (rawSize > 0 && rawPtr + rawSize > buf.length) fail("PE section extends past the end of the file");
    sections.push({ rawSize, rawPtr });
  }
  // The certificate table directory holds a FILE OFFSET (not an RVA).
  const cert = hasCert ? { offset: buf.readUInt32LE(certEntryAt), size: buf.readUInt32LE(certEntryAt + 4) } : { offset: 0, size: 0 };
  if (cert.size === 0 && cert.offset !== 0) fail("PE certificate table has an offset but no size");
  if (cert.size > 0) {
    if (cert.offset + cert.size > buf.length) fail("PE certificate table extends past the end of the file");
    // The certificate table must be the terminal region: bytes after it would otherwise escape the digest.
    if (cert.offset + cert.size !== buf.length) fail("PE certificate table is not the terminal region of the file (bytes follow it)");
    if (cert.offset < sizeOfHeaders) fail("PE certificate table overlaps the headers");
    for (const s of sections) if (s.rawSize > 0 && s.rawPtr + s.rawSize > cert.offset) fail("PE certificate table overlaps a section");
  }
  return { checksumAt, sizeOfHeaders, certEntryAt, hasCert, sections, cert };
}

/** Authenticode-style SHA-2 digest of a PE image, independent of its signature, timestamp and CheckSum. */
export function peContentDigest(bytes, { algorithm = "sha256" } = {}) {
  if (!ALGORITHMS.has(algorithm)) fail(`unsupported digest algorithm: ${algorithm}`, "PE_USAGE");
  const buf = Buffer.isBuffer(bytes) ? bytes : Buffer.from(bytes);
  const l = layout(buf);
  const hash = createHash(algorithm);
  hash.update(buf.subarray(0, l.checksumAt));
  if (l.hasCert) {
    hash.update(buf.subarray(l.checksumAt + 4, l.certEntryAt));
    hash.update(buf.subarray(l.certEntryAt + 8, l.sizeOfHeaders));
  } else {
    hash.update(buf.subarray(l.checksumAt + 4, l.sizeOfHeaders));
  }
  // Every byte after the headers up to the certificate table (or end of file) is hashed: sections, inter-section gaps and overlay.
  const trailEnd = l.cert.size > 0 ? l.cert.offset : buf.length;
  if (trailEnd > l.sizeOfHeaders) hash.update(buf.subarray(l.sizeOfHeaders, trailEnd));
  return hash.digest("hex");
}

const normalize = (name) => String(name).trim().toLowerCase();

function allowMatcher(allowed) {
  const entries = allowed.map(normalize).filter(Boolean);
  return (dll) => entries.some((entry) => (entry.endsWith("*") ? dll.startsWith(entry.slice(0, -1)) : dll === entry));
}

/**
 * Check that every DLL a PE imports (regular and delay-load) is in `allowedDlls`. Names compare case-insensitively;
 * a trailing `*` allows a prefix (`api-ms-win-*`). An empty allow list refuses every import.
 * @returns {{ok: boolean, architecture: string, imports: string[], delayImports: string[], unexpected: string[]}}
 */
export function verifyImportClosure(bytes, allowedDlls) {
  if (!Array.isArray(allowedDlls)) fail("allowedDlls must be an array", "PE_USAGE");
  const info = readPeImports(bytes);
  const allowed = allowMatcher(allowedDlls);
  const all = [...new Set([...info.imports, ...info.delayImports].map(normalize))].sort();
  const unexpected = all.filter((dll) => !allowed(dll));
  return { ok: unexpected.length === 0, architecture: info.architecture, imports: info.imports, delayImports: info.delayImports, unexpected };
}

const USAGE = "usage: right-release pe digest <file> [--algorithm sha256|sha384|sha512] [--json] | pe imports <file> [--allow dll,...] [--allow-file <path>] [--json]";

function parseArgs(argv) {
  const [sub, file, ...rest] = argv;
  const opts = { sub, file, algorithm: "sha256", allow: [], json: false };
  for (let i = 0; i < rest.length; i++) {
    const arg = rest[i];
    if (arg === "--json") opts.json = true;
    else if (["--algorithm", "--allow", "--allow-file"].includes(arg)) {
      const value = rest[++i];
      if (value === undefined || value.startsWith("--")) fail(`${arg} needs a value`, "PE_USAGE");
      if (arg === "--algorithm") opts.algorithm = value;
      else if (arg === "--allow") opts.allow.push(...value.split(","));
      else opts.allow.push(...readFileSync(value, "utf8").split(/\r?\n/).map((l) => l.replace(/#.*/, "").trim()).filter(Boolean));
    } else fail(`unknown pe argument: ${arg}`, "PE_USAGE");
  }
  if (!["digest", "imports"].includes(sub) || !file) fail(USAGE, "PE_USAGE");
  return opts;
}

export function main(argv = process.argv.slice(2), out = console) {
  let opts;
  try { opts = parseArgs(argv); } catch (error) { out.error(error.message); return 2; }
  try {
    const bytes = readFileSync(opts.file);
    if (opts.sub === "digest") {
      const digest = peContentDigest(bytes, { algorithm: opts.algorithm });
      out.log(opts.json ? JSON.stringify({ file: opts.file, algorithm: opts.algorithm, digest }) : `${opts.algorithm}:${digest}`);
      return 0;
    }
    const result = verifyImportClosure(bytes, opts.allow);
    out.log(opts.json ? JSON.stringify({ file: opts.file, ...result }) : [`${opts.file} (${result.architecture})`, ...result.imports.map((d) => `  import ${d}`), ...result.delayImports.map((d) => `  delay-import ${d}`), ...result.unexpected.map((d) => `  UNEXPECTED ${d}`)].join("\n"));
    return result.ok ? 0 : 1;
  } catch (error) {
    out.error(error.message);
    return error.code === "PE_USAGE" ? 2 : 1;
  }
}

if (process.argv[1] && import.meta.url === new URL(`file://${process.argv[1]}`).href) process.exit(main());
