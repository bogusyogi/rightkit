// Signature-independent content digest: exact JS port of crates/rightkit-ffmpeg/src/content_digest.rs.
// PE (Authenticode rule), thin 64-bit LE Mach-O (canonical unsigned image) & universal Mach-O (per-slice).
// A malformed recognized format FAILS (ContentDigestError); never falls back to a plain hash.
// Anything else is plain SHA-256. Archive/download SHA-256 remains a separate, independent value.
import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";

export class ContentDigestError extends Error {
  constructor(message) { super(message); this.name = "ContentDigestError"; this.code = "CONTENT_DIGEST_MALFORMED"; }
}
const malformed = (why) => { throw new ContentDigestError(why); };

const MH_MAGIC_64 = 0xfeedfacf, LC_SEGMENT_64 = 0x19, LC_CODE_SIGNATURE = 0x1d;
const FAT_MAGIC = 0xcafebabe, FAT_MAGIC_64 = 0xcafebabf;
const CPU_X86_64 = 0x01000007, CPU_ARM64 = 0x0100000c;
const MAX_FAT_ARCHS = 16;

const sha256Parts = (parts) => { const h = createHash("sha256"); for (const p of parts) h.update(p); return h.digest("hex"); };
export const sha256Bytes = (bytes) => sha256Parts([Buffer.isBuffer(bytes) ? bytes : Buffer.from(bytes)]);

const le32 = (b, at) => (at + 4 <= b.length ? b.readUInt32LE(at) : null);
const le16 = (b, at) => (at + 2 <= b.length ? b.readUInt16LE(at) : null);
const be32 = (b, at) => (at + 4 <= b.length ? b.readUInt32BE(at) : null);
const le64n = (b, at) => (at + 8 <= b.length ? Number(b.readBigUInt64LE(at)) : null);
const be64n = (b, at) => (at + 8 <= b.length ? Number(b.readBigUInt64BE(at)) : null);

function archName(cputype) {
  if (cputype === CPU_ARM64) return "aarch64";
  if (cputype === CPU_X86_64) return "x86_64";
  malformed("unsupported CPU type");
}
const roundUp = (v, to) => Math.floor((v + to - 1) / to) * to;

/** @returns {{format:"macho"|"macho-universal"|"pe"|"plain", slices:{arch:string|null, sha256:string}[]}} */
export function contentIdentity(input) {
  const bytes = Buffer.isBuffer(input) ? input : Buffer.from(input);
  const magicBe = be32(bytes, 0), magicLe = le32(bytes, 0);
  if (magicBe === FAT_MAGIC || magicBe === FAT_MAGIC_64) return fatIdentity(bytes);
  if (magicLe === MH_MAGIC_64) {
    const { arch, sha256 } = machoSliceDigest(bytes);
    return { format: "macho", slices: [{ arch, sha256 }] };
  }
  if ([0xfeedface, 0xfeedfacf, 0xcefaedfe, 0xcffaedfe, 0xbebafeca, 0xbfbafeca].includes(magicBe)) {
    malformed("unsupported Mach-O flavour (only 64-bit little-endian is catalogued)");
  }
  if (bytes.subarray(0, 2).toString("latin1") === "MZ") {
    const { arch, sha256 } = peDigest(bytes);
    return { format: "pe", slices: [{ arch, sha256 }] };
  }
  return { format: "plain", slices: [{ arch: null, sha256: sha256Bytes(bytes) }] };
}

export function contentIdentityFile(file) { return contentIdentity(readFileSync(file)); }
export function digestFor(identity, arch) {
  return identity.slices.find((s) => s.arch?.toLowerCase() === String(arch).toLowerCase())?.sha256 ?? null;
}

function machoSliceDigest(slice) {
  const bad = (why) => malformed(`Mach-O: ${why}`);
  if (le32(slice, 0) !== MH_MAGIC_64) bad("not a 64-bit little-endian Mach-O");
  const cputype = le32(slice, 4); if (cputype === null) bad("truncated header");
  const arch = archName(cputype);
  const ncmds = le32(slice, 16), sizeofcmds = le32(slice, 20);
  if (ncmds === null || sizeofcmds === null) bad("truncated header");
  const cmdsEnd = 32 + sizeofcmds;
  if (cmdsEnd > slice.length) bad("load commands run past the end of the file");
  const commands = [];
  let offset = 32;
  for (let i = 0; i < ncmds; i += 1) {
    const cmd = le32(slice, offset), size = le32(slice, offset + 4);
    if (cmd === null || size === null) bad("truncated load command");
    if (size < 8 || size % 8 !== 0 || offset + size > cmdsEnd) bad("load command size is invalid");
    commands.push({ offset, size, cmd });
    offset += size;
  }
  if (offset !== cmdsEnd) bad("load commands do not fill sizeofcmds");
  let signature = null, linkedit = null;
  commands.forEach((c, index) => {
    if (c.cmd === LC_CODE_SIGNATURE) {
      if (c.size !== 16 || signature !== null) bad("invalid or repeated LC_CODE_SIGNATURE");
      signature = index;
    } else if (c.cmd === LC_SEGMENT_64) {
      if (c.size < 72) bad("short LC_SEGMENT_64");
      const name = slice.subarray(c.offset + 8, c.offset + 24);
      const end = name.indexOf(0) === -1 ? name.length : name.indexOf(0);
      if (name.subarray(0, end).toString("latin1") === "__LINKEDIT") {
        if (linkedit !== null) bad("repeated __LINKEDIT");
        linkedit = index;
      }
    }
  });
  if (linkedit === null) bad("no __LINKEDIT segment");
  const leOff = commands[linkedit].offset;
  const vmsize = le64n(slice, leOff + 32), fileoff = le64n(slice, leOff + 40), filesize = le64n(slice, leOff + 48);
  if (vmsize === null || fileoff === null || filesize === null) bad("truncated __LINKEDIT");
  const len = slice.length;
  if (fileoff < cmdsEnd || fileoff + filesize !== len || vmsize < filesize || vmsize % 0x1000 !== 0) bad("__LINKEDIT does not run to the end of the file");
  let contentEnd = len;
  if (signature !== null) {
    const off = commands[signature].offset;
    const dataoff = le32(slice, off + 8), datasize = le32(slice, off + 12);
    if (dataoff === null || datasize === null) bad("truncated signature");
    if (dataoff < fileoff || dataoff + datasize !== len) bad("code signature is not the tail of __LINKEDIT");
    contentEnd = dataoff;
  }
  let end = contentEnd;
  while (end > fileoff && slice[end - 1] === 0) end -= 1;
  const canonicalFilesize = end - fileoff;
  const page = cputype === CPU_ARM64 ? 0x4000 : 0x1000;
  const canonicalVmsize = roundUp(canonicalFilesize, page);
  const head = Buffer.alloc(cmdsEnd);
  slice.copy(head, 0, 0, 32);
  let cursor = 32, newLinkeditAt = 0, removed = 0;
  commands.forEach((c, index) => {
    if (index === signature) { removed = c.size; return; }
    if (index === linkedit) newLinkeditAt = cursor;
    slice.copy(head, cursor, c.offset, c.offset + c.size);
    cursor += c.size;
  });
  head.writeBigUInt64LE(BigInt(canonicalVmsize), newLinkeditAt + 32);
  head.writeBigUInt64LE(BigInt(canonicalFilesize), newLinkeditAt + 48);
  head.writeUInt32LE(ncmds - (signature === null ? 0 : 1), 16);
  head.writeUInt32LE(sizeofcmds - removed, 20);
  return { arch, sha256: sha256Parts([head, slice.subarray(cmdsEnd, end)]) };
}

function fatIdentity(bytes) {
  const bad = (why) => malformed(`universal Mach-O: ${why}`);
  const wide = be32(bytes, 0) === FAT_MAGIC_64;
  const count = be32(bytes, 4);
  if (count === null) bad("truncated header");
  if (count === 0 || count > MAX_FAT_ARCHS) bad("implausible architecture count");
  const entrySize = wide ? 32 : 20;
  const tableEnd = 8 + entrySize * count;
  if (tableEnd > bytes.length) bad("architecture table runs past the end of the file");
  const slices = [], seen = [], ranges = [];
  for (let index = 0; index < count; index += 1) {
    const at = 8 + index * entrySize;
    const cputype = be32(bytes, at);
    const off = wide ? be64n(bytes, at + 8) : be32(bytes, at + 8);
    const size = wide ? be64n(bytes, at + 16) : be32(bytes, at + 12);
    if (cputype === null || off === null || size === null) bad("truncated entry");
    const end = off + size;
    if (size === 0 || off < tableEnd || end > bytes.length) bad("slice is empty or out of bounds");
    if (seen.includes(cputype)) bad("duplicate architecture");
    if (ranges.some(([s, e]) => off < e && s < end)) bad("slices overlap");
    seen.push(cputype); ranges.push([off, end]);
    const { arch, sha256 } = machoSliceDigest(bytes.subarray(off, end));
    if (arch !== archName(cputype)) bad("slice architecture differs from the fat header");
    slices.push({ arch, sha256 });
  }
  return { format: "macho-universal", slices };
}

function peDigest(bytes) {
  const bad = (why) => malformed(`PE: ${why}`);
  const len = bytes.length;
  const pe = le32(bytes, 0x3c);
  if (pe === null) bad("no PE header offset");
  if (bytes.subarray(pe, pe + 4).toString("latin1") !== "PE\0\0") bad("missing PE signature");
  const machine = le16(bytes, pe + 4);
  const arch = machine === 0x8664 ? "x86_64" : machine === 0xaa64 ? "aarch64" : bad("unsupported machine type");
  const nsections = le16(bytes, pe + 6), optSize = le16(bytes, pe + 20);
  if (nsections === null || optSize === null) bad("truncated COFF header");
  const opt = pe + 24;
  const sectionsAt = opt + optSize;
  if (sectionsAt > len) bad("optional header runs past the end of the file");
  const magic = le16(bytes, opt);
  const dirs = magic === 0x10b ? 96 : magic === 0x20b ? 112 : bad("unknown optional header magic");
  if (optSize < dirs) bad("optional header too short");
  const checksumAt = opt + 64;
  const sizeOfHeaders = le32(bytes, opt + 60), dirCount = le32(bytes, opt + dirs - 4);
  if (sizeOfHeaders === null || dirCount === null) bad("truncated optional header");
  const certEntry = dirCount > 4 && optSize >= dirs + 40 ? opt + dirs + 32 : null;
  const tableEnd = sectionsAt + nsections * 40;
  if (tableEnd > len) bad("section table runs past the end of the file");
  let floor = checksumAt + 4;
  if (certEntry !== null) floor = Math.max(floor, certEntry + 8);
  if (sizeOfHeaders > len || sizeOfHeaders < Math.max(floor, tableEnd)) bad("SizeOfHeaders is inconsistent");
  let certOffset = 0, certSize = 0;
  if (certEntry !== null) {
    certOffset = le32(bytes, certEntry); certSize = le32(bytes, certEntry + 4);
    if (certOffset === null || certSize === null) bad("truncated directory");
  }
  if (certSize > 0 && (certOffset < sizeOfHeaders || certOffset + certSize !== len)) bad("certificate table is not the tail of the file");
  const sections = [];
  for (let i = 0; i < nsections; i += 1) {
    const at = sectionsAt + i * 40;
    const size = le32(bytes, at + 16), off = le32(bytes, at + 20);
    if (size === null || off === null) bad("truncated section");
    if (size > 0) sections.push([off, size]);
  }
  sections.sort((a, b) => a[0] - b[0] || a[1] - b[1]);
  const parts = [bytes.subarray(0, checksumAt)];
  if (certEntry !== null) {
    parts.push(bytes.subarray(checksumAt + 4, certEntry), bytes.subarray(certEntry + 8, sizeOfHeaders));
  } else parts.push(bytes.subarray(checksumAt + 4, sizeOfHeaders));
  let hashed = sizeOfHeaders, previousEnd = sizeOfHeaders;
  const bodyEnd = len - certSize;
  for (const [off, size] of sections) {
    const end = off + size;
    if (off < previousEnd || end > bodyEnd) bad("section overlaps the headers or the certificate table");
    parts.push(bytes.subarray(off, end));
    hashed += size; previousEnd = end;
  }
  if (hashed > bodyEnd) bad("hashed bytes exceed the file");
  parts.push(bytes.subarray(hashed, bodyEnd));
  return { arch, sha256: sha256Parts(parts) };
}
