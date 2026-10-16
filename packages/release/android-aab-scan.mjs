// Release-tier scan of an Android App Bundle (.aab), read in-process: an AAB is a ZIP, so no unzip, bundletool or
// jarsigner binary is run. It applies the same refusals the desktop lane applies to its binaries:
//   - every native library (`<module>/lib/<abi>/*.so`) is an ELF file and goes through the shared QA/debug rules of
//     qa-control-scan.mjs (QA control marker, wdio WebDriver, remote debugging, devtools, dev-inspect);
//   - every frontend text file inside the bundle goes through the web-bundle mock markers of qa-control-scan.mjs;
//   - the bundle must be a well-formed AAB (BundleConfig.pb and a base manifest) with at least one native library, so an
//     empty or wrong file never reads as "clean";
//   - the bundle must carry exactly one jarsigner signature block, whose first certificate is returned as a SHA-256
//     fingerprint (the caller pins it to the expected upload key) and which must not be the Android debug certificate.
// Fails closed: anything that cannot be read is a finding or a problem, never a pass.
import { createHash, X509Certificate } from "node:crypto";
import { mkdirSync, readFileSync, statSync, writeFileSync } from "node:fs";
import path from "node:path";
import zlib from "node:zlib";
import { bufferQaFindings, webBundleMockFindings } from "./qa-control-scan.mjs";

const ELF_MAGIC = Buffer.from([0x7f, 0x45, 0x4c, 0x46]);

const DEFAULT_LIMITS = Object.freeze({
  maxEntries: 50_000,
  maxFileBytes: 2 * 1024 * 1024 * 1024 - 1, // readFileSync ceiling
  maxEntryBytes: 1024 * 1024 * 1024,
  totalInflated: 6 * 1024 * 1024 * 1024,
});
const limitsOf = (limits) => ({ ...DEFAULT_LIMITS, ...(limits ?? {}) });

export const AAB_REQUIRED_ENTRIES = Object.freeze(["BundleConfig.pb", "base/manifest/AndroidManifest.xml"]);
const NATIVE_LIB = /^([^/]+)\/lib\/([^/]+)\/([^/]+\.so)$/i;
const SIGNATURE_BLOCK = /^META-INF\/([^/]+)\.(RSA|DSA|EC)$/i;
const WEB_FILE = /\.(?:m?js|cjs|html?|css|map)$/i;
const DEBUG_CERT_MARKER = Buffer.from("Android Debug", "latin1");

// ---- CRC-32 (verifies that an entry was read whole and unmodified) -----------------------------------------------
const CRC_TABLE = (() => {
  const table = new Uint32Array(256);
  for (let n = 0; n < 256; n += 1) {
    let c = n;
    for (let k = 0; k < 8; k += 1) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    table[n] = c >>> 0;
  }
  return table;
})();
export function crc32(buffer) {
  if (typeof zlib.crc32 === "function") return zlib.crc32(buffer) >>> 0;
  let c = 0xffffffff;
  for (let i = 0; i < buffer.length; i += 1) c = CRC_TABLE[(c ^ buffer[i]) & 0xff] ^ (c >>> 8);
  return (c ^ 0xffffffff) >>> 0;
}

// ---- ZIP reader (stored and deflate; ZIP64 sizes and offsets) ----------------------------------------------------
function u64(buffer, offset) {
  const value = buffer.readBigUInt64LE(offset);
  if (value > BigInt(Number.MAX_SAFE_INTEGER)) throw new Error("zip64 value out of range");
  return Number(value);
}

function findEndOfCentralDirectory(buffer) {
  const floor = Math.max(0, buffer.length - (22 + 65535));
  for (let i = buffer.length - 22; i >= floor; i -= 1) {
    if (buffer.readUInt32LE(i) !== 0x06054b50) continue;
    if (i + 22 + buffer.readUInt16LE(i + 20) > buffer.length) continue;
    let entries = buffer.readUInt16LE(i + 10); let size = buffer.readUInt32LE(i + 12); let offset = buffer.readUInt32LE(i + 16);
    if (entries === 0xffff || size === 0xffffffff || offset === 0xffffffff) {
      if (i < 20 || buffer.readUInt32LE(i - 20) !== 0x07064b50) throw new Error("zip64 locator missing");
      const record = u64(buffer, i - 12);
      if (record + 56 > buffer.length || buffer.readUInt32LE(record) !== 0x06064b50) throw new Error("zip64 end record missing");
      entries = u64(buffer, record + 32); size = u64(buffer, record + 40); offset = u64(buffer, record + 48);
    }
    return { entries, size, offset };
  }
  throw new Error("end of central directory not found");
}

export function listZipEntries(buffer, limits = DEFAULT_LIMITS) {
  const eocd = findEndOfCentralDirectory(buffer);
  if (eocd.entries > limits.maxEntries) throw new Error(`more than ${limits.maxEntries} entries`);
  if (eocd.offset + eocd.size > buffer.length) throw new Error("central directory out of bounds");
  const out = [];
  let at = eocd.offset;
  const end = eocd.offset + eocd.size;
  for (let n = 0; n < eocd.entries; n += 1) {
    if (at + 46 > end || buffer.readUInt32LE(at) !== 0x02014b50) throw new Error("bad central directory entry");
    const flags = buffer.readUInt16LE(at + 8); const method = buffer.readUInt16LE(at + 10);
    const crc = buffer.readUInt32LE(at + 16);
    let compressed = buffer.readUInt32LE(at + 20); let uncompressed = buffer.readUInt32LE(at + 24);
    const nameLength = buffer.readUInt16LE(at + 28); const extraLength = buffer.readUInt16LE(at + 30);
    const commentLength = buffer.readUInt16LE(at + 32); let localOffset = buffer.readUInt32LE(at + 42);
    const next = at + 46 + nameLength + extraLength + commentLength;
    if (next > end) throw new Error("central directory entry out of bounds");
    const name = buffer.subarray(at + 46, at + 46 + nameLength).toString("utf8");
    const extra = buffer.subarray(at + 46 + nameLength, at + 46 + nameLength + extraLength);
    for (let e = 0; e + 4 <= extra.length;) {
      const id = extra.readUInt16LE(e); const size = extra.readUInt16LE(e + 2);
      if (id === 0x0001) {
        let p = e + 4;
        if (uncompressed === 0xffffffff) { uncompressed = u64(extra, p); p += 8; }
        if (compressed === 0xffffffff) { compressed = u64(extra, p); p += 8; }
        if (localOffset === 0xffffffff) localOffset = u64(extra, p);
      }
      e += 4 + size;
    }
    out.push({ name, flags, method, crc, compressed, uncompressed, localOffset });
    at = next;
  }
  return out;
}

export function readZipEntry(buffer, entry, limits = DEFAULT_LIMITS) {
  if (entry.flags & 1) throw new Error("encrypted entry");
  const at = entry.localOffset;
  if (at + 30 > buffer.length || buffer.readUInt32LE(at) !== 0x04034b50) throw new Error("bad local header");
  const start = at + 30 + buffer.readUInt16LE(at + 26) + buffer.readUInt16LE(at + 28);
  if (start + entry.compressed > buffer.length) throw new Error("entry data out of bounds");
  if (entry.uncompressed > limits.maxEntryBytes || entry.compressed > limits.maxEntryBytes) throw new Error(`entry larger than ${limits.maxEntryBytes} bytes`);
  const raw = buffer.subarray(start, start + entry.compressed);
  let data;
  if (entry.method === 0) data = raw;
  else if (entry.method === 8) data = zlib.inflateRawSync(raw, { maxOutputLength: Math.max(1, entry.uncompressed + 1) });
  else throw new Error(`unsupported compression method ${entry.method}`);
  if (data.length !== entry.uncompressed) throw new Error("entry size does not match its directory record");
  if (crc32(data) !== entry.crc) throw new Error("entry CRC-32 mismatch");
  return data;
}

// ---- PKCS#7 signature block --------------------------------------------------------------------------------------
function tlv(buffer, at, limit = buffer.length) {
  if (at + 2 > limit) throw new Error("DER truncated");
  const tag = buffer[at]; let length = buffer[at + 1]; let head = 2;
  if (length & 0x80) {
    const count = length & 0x7f;
    if (count === 0 || count > 4 || at + 2 + count > limit) throw new Error("DER length unsupported");
    length = 0;
    for (let i = 0; i < count; i += 1) length = length * 256 + buffer[at + 2 + i];
    head = 2 + count;
  }
  const end = at + head + length;
  if (end > limit) throw new Error("DER truncated");
  return { tag, start: at, body: at + head, end };
}
function children(buffer, node) {
  const out = [];
  for (let at = node.body; at < node.end;) {
    const child = tlv(buffer, at, node.end);
    out.push(child);
    at = child.end;
  }
  return out;
}

// DER certificates carried by a PKCS#7 SignedData block, in file order (jarsigner writes the signer first).
export function pkcs7Certificates(block) {
  const root = tlv(block, 0);
  if (root.tag !== 0x30) throw new Error("signature block is not a DER sequence");
  const [oid, wrap] = children(block, root);
  if (!oid || oid.tag !== 0x06 || !wrap || wrap.tag !== 0xa0) throw new Error("signature block is not PKCS#7 SignedData");
  const signed = children(block, wrap)[0];
  if (!signed || signed.tag !== 0x30) throw new Error("signature block has no SignedData");
  // SignedData: version, digestAlgorithms, contentInfo, [0] certificates, [1] crls, signerInfos.
  const certificates = children(block, signed).find((part, index) => index >= 3 && part.tag === 0xa0);
  if (!certificates) throw new Error("signature block carries no certificate");
  return children(block, certificates).filter((cert) => cert.tag === 0x30).map((cert) => block.subarray(cert.start, cert.end));
}

const sha256Hex = (bytes) => createHash("sha256").update(bytes).digest("hex");

function signerOf(blockName, block, problems) {
  try {
    const certificates = pkcs7Certificates(block);
    if (certificates.length === 0) throw new Error("signature block carries no certificate");
    const der = certificates[0];
    let subject = null; let notAfter = null;
    try { const x509 = new X509Certificate(der); subject = x509.subject.replace(/\n/g, ", "); notAfter = Date.parse(x509.validTo); } catch { /* the pinned fingerprint is the authority */ }
    return { block: blockName, certSha256: sha256Hex(der), certificates: certificates.length, subject, notAfter: Number.isFinite(notAfter) ? notAfter : null, debugCertificate: der.includes(DEBUG_CERT_MARKER) };
  } catch (error) {
    problems.push(`${blockName}: ${error.message}`);
    return null;
  }
}

// ---- the AAB scan ------------------------------------------------------------------------------------------------
// Returns { entries, problems[], findings[], nativeLibs[], webFiles, signature }. `problems` are structural/signature
// defects, `findings` are test-automation or debugging surfaces; either one refuses the release.
export function scanAabBuffer(buffer, { allowContentOnlyDevtools = false, limits, label = "<aab>" } = {}) {
  const lim = limitsOf(limits);
  const result = { entries: 0, problems: [], findings: [], nativeLibs: [], webFiles: 0, signature: null };
  let entries;
  try { entries = listZipEntries(buffer, lim); } catch (error) { result.problems.push(`not a readable ZIP/AAB (${error.message})`); return result; }
  result.entries = entries.length;
  const names = new Map();
  for (const entry of entries) {
    if (names.has(entry.name)) result.problems.push(`duplicate entry name ${entry.name}`);
    names.set(entry.name, entry);
  }
  for (const required of AAB_REQUIRED_ENTRIES) if (!names.has(required)) result.problems.push(`missing required AAB entry ${required}`);
  let budget = lim.totalInflated;
  const read = (entry) => {
    if ((budget -= entry.uncompressed) < 0) throw new Error("bundle exceeds its inflate budget");
    return readZipEntry(buffer, entry, lim);
  };
  const unscannable = (entry, why) => result.findings.push({ file: `${label}!/${entry.name}`, rule: "unscannable", reason: `could not scan: ${why}` });

  for (const entry of entries) {
    if (entry.name.endsWith("/")) continue;
    const lib = NATIVE_LIB.exec(entry.name);
    if (lib) {
      try {
        const bytes = read(entry);
        if (!bytes.subarray(0, 4).equals(ELF_MAGIC)) { unscannable(entry, "native library is not an ELF file"); continue; }
        result.nativeLibs.push({ name: entry.name, module: lib[1], abi: lib[2], size: bytes.length, sha256: sha256Hex(bytes) });
        for (const finding of bufferQaFindings(bytes, { allowContentOnlyDevtools })) result.findings.push({ file: `${label}!/${entry.name}`, ...finding });
      } catch (error) { unscannable(entry, error.message); }
    } else if (WEB_FILE.test(entry.name)) {
      try {
        result.webFiles += 1;
        for (const finding of webBundleMockFindings(read(entry).toString("utf8"))) result.findings.push({ file: `${label}!/${entry.name}`, ...finding });
      } catch (error) { unscannable(entry, error.message); }
    }
  }
  if (result.nativeLibs.length === 0 && !result.findings.some((f) => f.rule === "unscannable")) {
    result.problems.push("no native library (<module>/lib/<abi>/*.so) found to scan; cannot verify");
  }

  const blocks = entries.filter((entry) => SIGNATURE_BLOCK.test(entry.name));
  if (blocks.length === 0) result.problems.push("bundle is not signed: no META-INF/*.RSA, *.DSA or *.EC signature block");
  else if (blocks.length > 1) result.problems.push(`bundle carries ${blocks.length} signature blocks; exactly one upload-key signature is expected`);
  else {
    const [block] = blocks;
    const stem = SIGNATURE_BLOCK.exec(block.name)[1];
    if (!names.has(`META-INF/${stem}.SF`)) result.problems.push(`signature block ${block.name} has no matching META-INF/${stem}.SF`);
    if (!names.has("META-INF/MANIFEST.MF")) result.problems.push("signed bundle has no META-INF/MANIFEST.MF");
    try { result.signature = signerOf(block.name, read(block), result.problems); } catch (error) { result.problems.push(`${block.name}: ${error.message}`); }
  }
  return result;
}

export function scanAab(file, options = {}) {
  const size = statSync(file).size;
  const lim = limitsOf(options.limits);
  if (size > lim.maxFileBytes) return { entries: 0, problems: [`bundle is larger than ${lim.maxFileBytes} bytes`], findings: [], nativeLibs: [], webFiles: 0, signature: null };
  return scanAabBuffer(readFileSync(file), { ...options, label: options.label ?? file });
}

// Writes every native library of the bundle to `dir` (flat, index-named, so an entry name can never escape it) and returns
// the written paths. Lets the caller run path-based layers (release-switch scan) over the same bytes.
export function extractNativeLibs(file, dir, { limits } = {}) {
  const lim = limitsOf(limits);
  const buffer = readFileSync(file);
  mkdirSync(dir, { recursive: true });
  const written = [];
  let index = 0;
  for (const entry of listZipEntries(buffer, lim)) {
    if (!NATIVE_LIB.test(entry.name)) continue;
    const target = path.join(dir, `${String(index += 1).padStart(3, "0")}-${path.basename(entry.name).replace(/[^A-Za-z0-9._-]/g, "_")}`);
    writeFileSync(target, readZipEntry(buffer, entry, lim));
    written.push(target);
  }
  return written;
}

export function describeAabProblems(scan) {
  return [
    ...scan.problems.map((problem) => `- ${problem}`),
    ...scan.findings.map((finding) => `- ${finding.file}: ${finding.reason}`),
  ];
}
