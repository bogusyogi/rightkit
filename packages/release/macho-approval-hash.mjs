// Signing-independent approval hashes for staged prebuilt binaries (Orthic-Labs/rightkit#490).
//
// `raw-sha256`                        the sha256 of the file bytes (Windows and Linux binaries).
// `macho-codesign-strip-linkedit-v1`  the sha256 of a thin little-endian 64-bit Mach-O after its code signature is removed:
//                                      LC_CODE_SIGNATURE and the trailing signature blob are gone, the __LINKEDIT filesize is
//                                      whatever `codesign --remove-signature` leaves, and the __LINKEDIT vmsize is normalised to
//                                      the page-rounded filesize, so a signed and an unsigned copy of the same build hash alike.
//
// This is the algorithm GenRight's `local-image-approval-hash.mjs` recorded as `approval_hash_algorithm` in its runtime
// manifest, moved here so apps stop carrying it. Every layout surprise (a fat or big-endian file, a duplicate or non-terminal
// signature, a load command that changed, padding that is not zero) is a typed refusal, never a guess. The signature is removed
// from a private copy with `/usr/bin/codesign --remove-signature`; the input file is never modified. macOS only for Mach-O.
import { createHash, randomBytes } from "node:crypto";
import { chmodSync, lstatSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "./background-process.mjs";

export const APPROVAL_RAW_SHA256 = "raw-sha256";
export const APPROVAL_MACHO_STRIP_LINKEDIT_V1 = "macho-codesign-strip-linkedit-v1";
export const APPROVAL_ALGORITHMS = Object.freeze([APPROVAL_RAW_SHA256, APPROVAL_MACHO_STRIP_LINKEDIT_V1]);

const LC_CODE_SIGNATURE = 0x1d;
const LC_SEGMENT_64 = 0x19;
const MH_MAGIC_64 = 0xfeedfacf;
const CPU_TYPE_X86_64 = 0x01000007;
const CPU_TYPE_ARM64 = 0x0100000c;

export class ApprovalHashError extends Error {
  constructor(code, message) { super(message); this.name = "ApprovalHashError"; this.code = code; }
}
const fail = (message, code = "APPROVAL_HASH_INVALID_MACHO") => { throw new ApprovalHashError(code, `backend integrity: ${message}`); };

/** Parse a thin 64-bit little-endian Mach-O. Exported for tests. */
export function parseMachO(bytes) {
  if (bytes.length < 32 || bytes.readUInt32LE(0) !== MH_MAGIC_64) fail("only little-endian 64-bit thin Mach-O is supported");
  const cpuType = bytes.readUInt32LE(4);
  if (![CPU_TYPE_X86_64, CPU_TYPE_ARM64].includes(cpuType)) fail("unsupported Mach-O CPU type");
  const ncmds = bytes.readUInt32LE(16);
  const sizeofcmds = bytes.readUInt32LE(20);
  const commandEnd = 32 + sizeofcmds;
  if (ncmds > 65_536 || commandEnd > bytes.length || sizeofcmds > bytes.length / 2) fail("invalid Mach-O load command table");
  let cursor = 32;
  let signature = null;
  let linkedit = null;
  const normalizedCommands = [];
  for (let index = 0; index < ncmds; index += 1) {
    if (cursor + 8 > commandEnd) fail("truncated Mach-O load command");
    const command = bytes.readUInt32LE(cursor);
    const commandSize = bytes.readUInt32LE(cursor + 4);
    if (commandSize < 8 || commandSize % 4 !== 0 || cursor + commandSize > commandEnd) fail("invalid Mach-O load command size");
    const commandBytes = Buffer.from(bytes.subarray(cursor, cursor + commandSize));
    if (command === LC_CODE_SIGNATURE) {
      if (commandSize !== 16 || signature) fail("invalid duplicate code signature command");
      const offset = bytes.readUInt32LE(cursor + 8);
      const size = bytes.readUInt32LE(cursor + 12);
      if (!size || offset + size > bytes.length) fail("invalid code signature extent");
      signature = { offset, size };
    } else if (command === LC_SEGMENT_64) {
      if (commandSize < 72) fail("truncated Mach-O segment command");
      const sectionCount = commandBytes.readUInt32LE(64);
      if (sectionCount > 65_536 || 72 + sectionCount * 80 > commandSize) fail("invalid Mach-O segment sections");
      const segmentName = commandBytes.subarray(8, 24).toString("utf8").replace(/\0.*$/s, "");
      if (segmentName === "__LINKEDIT") {
        if (linkedit) fail("duplicate __LINKEDIT segment");
        const vmsize = commandBytes.readBigUInt64LE(32);
        const fileoff = commandBytes.readBigUInt64LE(40);
        const filesize = commandBytes.readBigUInt64LE(48);
        if (fileoff + filesize > BigInt(bytes.length) || vmsize < filesize) fail("invalid __LINKEDIT extent");
        // vmsize and filesize legitimately differ after the signature is removed: they are normalised, never compared raw.
        commandBytes.fill(0, 32, 40);
        commandBytes.fill(0, 48, 56);
        linkedit = { fileoff, filesize, vmsize, vmsizeOffset: cursor + 32 };
      }
    }
    if (command !== LC_CODE_SIGNATURE) normalizedCommands.push(commandBytes);
    cursor += commandSize;
  }
  if (cursor !== commandEnd) fail("load commands do not fill declared table");
  if (!linkedit) fail("missing __LINKEDIT segment");
  if (signature && (BigInt(signature.offset) < linkedit.fileoff
      || signature.offset + signature.size !== bytes.length
      || linkedit.fileoff + linkedit.filesize !== BigInt(bytes.length))) {
    fail("signature is not terminal __LINKEDIT data");
  }
  return { bytes, cpuType, ncmds, sizeofcmds, signature, linkedit, normalizedCommands };
}

/**
 * Every byte outside the explicitly permitted ranges must be identical between the captured original and the stripped copy.
 * Permitted: header ncmds/sizeofcmds (checked by the caller), the load-command table (compared command by command, with only
 * the signature command removed and __LINKEDIT vmsize/filesize normalised), the zeroed tail the shorter table leaves, and the
 * signature blob plus its alignment padding at the end of __LINKEDIT. Code, data and all __LINKEDIT content before the
 * signature are compared byte for byte.
 */
function assertOnlySignatureDiffers(before, after) {
  const a = before.bytes;
  const b = after.bytes;
  if (!before.signature) {
    if (a.length !== b.length || !a.equals(b)) fail("unsigned Mach-O changed during normalization");
    return;
  }
  const same = (from, to, label) => {
    if (from === to) return;
    if (!a.subarray(from, to).equals(b.subarray(from, to))) fail(`signature removal changed ${label}`);
  };
  const beforeTable = 32 + before.sizeofcmds;
  const afterTable = 32 + after.sizeofcmds;
  same(0, 16, "Mach-O header");
  same(24, 32, "Mach-O header");
  if (b.subarray(afterTable, beforeTable).some((byte) => byte !== 0)) fail("signature removal left nonzero bytes after the load commands");
  const keptEnd = Number(after.linkedit.fileoff + after.linkedit.filesize);
  if (b.length !== keptEnd) fail("stripped Mach-O does not end at __LINKEDIT");
  same(beforeTable, keptEnd, "executable or data bytes");
}

/**
 * The approval digest of an already-unsigned copy and its signed original: verifies that signature removal changed
 * nothing but the signature, then hashes the unsigned bytes with the normalised __LINKEDIT vmsize. Exported for tests.
 */
export function machoApprovalDigest(beforeBytes, afterBytes) {
  const before = parseMachO(beforeBytes);
  const after = parseMachO(Buffer.from(afterBytes));
  if (after.signature) fail("signature remains after normalization");
  if (before.signature && (before.ncmds !== after.ncmds + 1 || before.sizeofcmds !== after.sizeofcmds + 16)) fail("signature removal changed Mach-O command layout");
  if (before.signature) {
    const beforeLinkeditEnd = before.linkedit.fileoff + before.linkedit.filesize;
    const afterLinkeditEnd = after.linkedit.fileoff + after.linkedit.filesize;
    const removedPadding = BigInt(before.signature.offset) - afterLinkeditEnd;
    if (beforeLinkeditEnd !== BigInt(before.signature.offset + before.signature.size)
      || after.linkedit.fileoff !== before.linkedit.fileoff
      || afterLinkeditEnd > BigInt(before.signature.offset)
      || removedPadding < 0n || removedPadding > 15n
      || BigInt(after.bytes.length) !== afterLinkeditEnd) {
      fail("code signature is outside trailing __LINKEDIT data");
    }
    const start = Number(afterLinkeditEnd);
    const end = before.signature.offset;
    if (before.bytes.subarray(start, end).some((byte) => byte !== 0)) fail("nonzero alignment padding before code signature");
  }
  assertOnlySignatureDiffers(before, after);
  if (before.normalizedCommands.length !== after.normalizedCommands.length
    || before.normalizedCommands.some((command, index) => !command.equals(after.normalizedCommands[index]))) {
    fail("signature removal changed non-signature load commands");
  }
  const pageSize = before.cpuType === CPU_TYPE_ARM64 ? 16n * 1024n : 4096n;
  const roundedSize = ((after.linkedit.filesize + pageSize - 1n) / pageSize) * pageSize;
  if (after.linkedit.vmsize < roundedSize) fail("invalid __LINKEDIT virtual size");
  after.bytes.writeBigUInt64LE(roundedSize, after.linkedit.vmsizeOffset);
  return createHash("sha256").update(after.bytes).digest("hex");
}

/** Default signature removal: `/usr/bin/codesign --remove-signature <copy>` (macOS only). */
function removeSignatureWithCodesign(copy) {
  const result = spawnSync("/usr/bin/codesign", ["--remove-signature", copy], { stdio: "ignore", timeout: 15_000, windowsHide: true });
  if (result.error || result.status !== 0) throw new ApprovalHashError("APPROVAL_HASH_CODESIGN_FAILED", `backend integrity: codesign normalization failed (${result.error?.message ?? result.status})`);
}

/**
 * The approval digest of `file` under `algorithm`. `removeSignature(copyPath)` is injectable for tests and for hosts that
 * strip signatures another way; the default needs macOS. The input file is never modified.
 */
export function approvalDigest(file, algorithm, { platform = process.platform, removeSignature = removeSignatureWithCodesign, tmpdir = os.tmpdir() } = {}) {
  if (algorithm === APPROVAL_RAW_SHA256) return createHash("sha256").update(readFileSync(file)).digest("hex");
  if (algorithm !== APPROVAL_MACHO_STRIP_LINKEDIT_V1) throw new ApprovalHashError("APPROVAL_HASH_ALGORITHM_UNKNOWN", `unknown approval hash algorithm ${JSON.stringify(algorithm)} (supported: ${APPROVAL_ALGORITHMS.join(", ")})`);
  if (platform !== "darwin") throw new ApprovalHashError("APPROVAL_HASH_HOST_UNSUPPORTED", `${APPROVAL_MACHO_STRIP_LINKEDIT_V1} needs macOS (codesign); this host is ${platform}`);
  if (!lstatSync(file).isFile()) fail("expected regular Mach-O file");
  const original = readFileSync(file);
  const before = parseMachO(original);
  const directory = mkdtempSync(path.join(tmpdir, `rightkit-macho-${process.pid}-${randomBytes(8).toString("hex")}-`), { mode: 0o700 });
  const copy = path.join(directory, "binary");
  try {
    // The copy is written from the captured bytes, never re-read from the path, so the validated snapshot is the one stripped.
    writeFileSync(copy, original, { mode: 0o700 });
    chmodSync(copy, 0o700);
    if (before.signature) removeSignature(copy);
    return machoApprovalDigest(original, readFileSync(copy));
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
}
