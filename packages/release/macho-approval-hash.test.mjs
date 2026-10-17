import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { readFileSync, writeFileSync } from "node:fs";
import path from "node:path";
import test from "node:test";
import { APPROVAL_MACHO_STRIP_LINKEDIT_V1, APPROVAL_RAW_SHA256, ApprovalHashError, approvalDigest, machoApprovalDigest, parseMachO } from "./macho-approval-hash.mjs";
import { tempRoot } from "./test-support/temp-roots.mjs";

const DATA_OFFSET = 0x1000;
const DATA = Buffer.from("linkedit symbol and string tables".repeat(4));
const SIGNATURE = Buffer.from("SUPERBLOB".repeat(10));
const VM_SIZE = 16384n;

/** A thin arm64 Mach-O with one __LINKEDIT segment; `signed` adds LC_CODE_SIGNATURE and a trailing signature blob. */
function machO({ signed, padding = 3, cpu = 0x0100000c }) {
  const commandBytes = 72 + (signed ? 16 : 0);
  const linkeditSize = DATA.length + (signed ? padding + SIGNATURE.length : 0);
  const bytes = Buffer.alloc(DATA_OFFSET + linkeditSize);
  bytes.writeUInt32LE(0xfeedfacf, 0);
  bytes.writeUInt32LE(cpu, 4);
  bytes.writeUInt32LE(2, 12);
  bytes.writeUInt32LE(signed ? 2 : 1, 16);
  bytes.writeUInt32LE(commandBytes, 20);
  bytes.writeUInt32LE(0x19, 32);
  bytes.writeUInt32LE(72, 36);
  bytes.write("__LINKEDIT", 40, "utf8");
  bytes.writeBigUInt64LE(VM_SIZE, 64);
  bytes.writeBigUInt64LE(BigInt(DATA_OFFSET), 72);
  bytes.writeBigUInt64LE(BigInt(linkeditSize), 80);
  DATA.copy(bytes, DATA_OFFSET);
  if (signed) {
    const signatureOffset = DATA_OFFSET + DATA.length + padding;
    bytes.writeUInt32LE(0x1d, 104);
    bytes.writeUInt32LE(16, 108);
    bytes.writeUInt32LE(signatureOffset, 112);
    bytes.writeUInt32LE(SIGNATURE.length, 116);
    SIGNATURE.copy(bytes, signatureOffset);
  }
  return bytes;
}

/** What `codesign --remove-signature` does to the fixture: drop the command and the trailing blob, shrink __LINKEDIT. */
function stripSignature(file) {
  const bytes = readFileSync(file);
  const stripped = Buffer.from(bytes.subarray(0, DATA_OFFSET + DATA.length));
  stripped.fill(0, 104, 120);
  stripped.writeUInt32LE(1, 16);
  stripped.writeUInt32LE(72, 20);
  stripped.writeBigUInt64LE(BigInt(DATA.length), 80);
  writeFileSync(file, stripped);
}

const sha = (bytes) => createHash("sha256").update(bytes).digest("hex");
const write = (name, bytes) => { const file = path.join(tempRoot("macho-approval-"), name); writeFileSync(file, bytes); return file; };

test("raw-sha256 is the file digest on any host", () => {
  const file = write("tool.exe", "windows bytes");
  assert.equal(approvalDigest(file, APPROVAL_RAW_SHA256, { platform: "win32" }), sha("windows bytes"));
});

test("a signed and an unsigned copy of the same build have the same Mach-O approval digest", () => {
  const signed = write("signed", machO({ signed: true }));
  const unsigned = write("unsigned", machO({ signed: false }));
  const options = { platform: "darwin", removeSignature: stripSignature };
  const signedDigest = approvalDigest(signed, APPROVAL_MACHO_STRIP_LINKEDIT_V1, options);
  assert.equal(signedDigest, approvalDigest(unsigned, APPROVAL_MACHO_STRIP_LINKEDIT_V1, options));
  assert.notEqual(signedDigest, sha(readFileSync(signed)), "the approval digest is not the raw digest");
  assert.equal(readFileSync(signed).length, DATA_OFFSET + DATA.length + 3 + SIGNATURE.length, "the input file is never modified");
});

test("changed content changes the approval digest", () => {
  const options = { platform: "darwin", removeSignature: stripSignature };
  const changed = machO({ signed: true });
  changed[DATA_OFFSET + 5] ^= 0xff;
  assert.notEqual(approvalDigest(write("a", machO({ signed: true })), APPROVAL_MACHO_STRIP_LINKEDIT_V1, options), approvalDigest(write("b", changed), APPROVAL_MACHO_STRIP_LINKEDIT_V1, options));
});

test("layout surprises are typed refusals", () => {
  const options = { platform: "darwin", removeSignature: stripSignature };
  const refuses = (bytes, pattern) => assert.throws(() => approvalDigest(write("bad", bytes), APPROVAL_MACHO_STRIP_LINKEDIT_V1, options), (error) => error instanceof ApprovalHashError && pattern.test(error.message));
  refuses(Buffer.from("not a macho"), /thin Mach-O/);
  refuses(machO({ signed: false, cpu: 7 }), /CPU type/);
  const nonzeroPadding = machO({ signed: true, padding: 4 });
  nonzeroPadding[DATA_OFFSET + DATA.length + 1] = 1;
  refuses(nonzeroPadding, /nonzero alignment padding/);
  const longPadding = machO({ signed: true, padding: 16 });
  refuses(longPadding, /outside trailing __LINKEDIT/);
  const notTerminal = machO({ signed: true });
  notTerminal.writeUInt32LE(DATA_OFFSET - 8, 112);
  refuses(notTerminal, /outside|terminal|extent/);
});

test("signature removal that changes other load commands is refused", () => {
  const file = write("signed", machO({ signed: true }));
  const damaging = (copy) => { stripSignature(copy); const bytes = readFileSync(copy); bytes[40] = 0x5f; bytes[41] = 0x58; writeFileSync(copy, bytes); };
  assert.throws(() => approvalDigest(file, APPROVAL_MACHO_STRIP_LINKEDIT_V1, { platform: "darwin", removeSignature: damaging }), /changed non-signature load commands|missing __LINKEDIT/);
});

test("the Mach-O algorithm needs macOS; an unknown algorithm is refused", () => {
  const file = write("signed", machO({ signed: true }));
  assert.throws(() => approvalDigest(file, APPROVAL_MACHO_STRIP_LINKEDIT_V1, { platform: "win32" }), { code: "APPROVAL_HASH_HOST_UNSUPPORTED" });
  assert.throws(() => approvalDigest(file, "md5", { platform: "darwin" }), { code: "APPROVAL_HASH_ALGORITHM_UNKNOWN" });
});

test("parseMachO and machoApprovalDigest expose the layout checks directly", () => {
  const signed = machO({ signed: true });
  const parsed = parseMachO(signed);
  assert.equal(parsed.signature.size, SIGNATURE.length);
  assert.equal(parsed.linkedit.fileoff, BigInt(DATA_OFFSET));
  const unsigned = machO({ signed: false });
  assert.equal(typeof machoApprovalDigest(unsigned, unsigned), "string");
  assert.throws(() => machoApprovalDigest(signed, signed), /signature remains/);
});
