import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import test from "node:test";
import { contentIdentity, digestFor, sha256Bytes } from "./content-digest.mjs";

// Fixtures mirror crates/rightkit-ffmpeg/src/content_digest.rs tests.
const CPU_ARM64 = 0x0100000c, CPU_X86_64 = 0x01000007;
const u32le = (v) => { const b = Buffer.alloc(4); b.writeUInt32LE(v); return b; };
const u64le = (v) => { const b = Buffer.alloc(8); b.writeBigUInt64LE(BigInt(v)); return b; };
const u32be = (v) => { const b = Buffer.alloc(4); b.writeUInt32BE(v); return b; };
const roundUp = (v, t) => Math.floor((v + t - 1) / t) * t;

function macho(cputype, payload, signature) {
  const ncmds = 2 + (signature ? 1 : 0);
  const sizeofcmds = 144 + (signature ? 16 : 0);
  const cmdsEnd = 32 + sizeofcmds;
  const textOff = Math.max(0x1000, cmdsEnd);
  const text = Buffer.alloc(64, 0xaa);
  const linkeditOff = textOff + text.length;
  const sigOff = signature ? roundUp(linkeditOff + payload.length, 16) : null;
  const fileLen = signature ? sigOff + signature.length : linkeditOff + payload.length;
  const filesize = fileLen - linkeditOff;
  const page = cputype === CPU_ARM64 ? 0x4000 : 0x1000;
  const segment = (name, vm, vmsize, off, size) => {
    const n = Buffer.alloc(16); n.write(name, "latin1");
    return Buffer.concat([u32le(0x19), u32le(72), n, u64le(vm), u64le(vmsize), u64le(off), u64le(size), Buffer.alloc(16)]);
  };
  const out = [u32le(0xfeedfacf), u32le(cputype), u32le(0), u32le(2), u32le(ncmds), u32le(sizeofcmds), u32le(0), u32le(0),
    segment("__TEXT", 0, 0x4000, textOff, text.length),
    segment("__LINKEDIT", 0x4000, roundUp(filesize, page), linkeditOff, filesize)];
  if (signature) out.push(u32le(0x1d), u32le(16), u32le(sigOff), u32le(signature.length));
  const body = Buffer.concat(out);
  const file = Buffer.alloc(fileLen);
  body.copy(file); text.copy(file, textOff); payload.copy(file, linkeditOff);
  if (signature) signature.copy(file, sigOff);
  return file;
}
const PAYLOAD = Buffer.from("symtab-and-strtab-bytes!");

function fat(slices, wide) {
  const entry = wide ? 32 : 20;
  const head = [u32be(wide ? 0xcafebabf : 0xcafebabe), u32be(slices.length)];
  let offset = roundUp(8 + entry * slices.length, 0x4000);
  const body = Buffer.alloc(0);
  const parts = [body];
  let cursor = 8 + entry * slices.length;
  for (const [cputype, slice] of slices) {
    head.push(u32be(cputype), u32be(0));
    if (wide) { const o = Buffer.alloc(8); o.writeBigUInt64BE(BigInt(offset)); const s = Buffer.alloc(8); s.writeBigUInt64BE(BigInt(slice.length)); head.push(o, s, u32be(14), u32be(0)); }
    else head.push(u32be(offset), u32be(slice.length), u32be(14));
    parts.push(Buffer.alloc(offset - cursor), slice);
    cursor = offset + slice.length;
    offset = roundUp(cursor, 0x4000);
  }
  return Buffer.concat([Buffer.concat(head), ...parts]);
}

function pe(machine, sectionA, cert, checksum) {
  const out = Buffer.alloc(0x400);
  out.write("MZ", 0, "latin1"); out.writeUInt32LE(0x80, 0x3c);
  out.write("PE\0\0", 0x80, "latin1"); out.writeUInt16LE(machine, 0x84); out.writeUInt16LE(2, 0x86);
  out.writeUInt16LE(240, 0x94);
  const opt = 0x98;
  out.writeUInt16LE(0x20b, opt); out.writeUInt32LE(0x400, opt + 60); out.writeUInt32LE(checksum, opt + 64);
  out.writeUInt32LE(16, opt + 108);
  [[0x400, 0x200], [0x600, 0x200]].forEach(([off, size], i) => {
    const at = opt + 240 + i * 40; out.writeUInt32LE(size, at + 16); out.writeUInt32LE(off, at + 20);
  });
  const a = Buffer.alloc(0x200, sectionA); a[0] = 0x90;
  const parts = [out, a, Buffer.alloc(0x200, 0x22)];
  if (cert) {
    const full = Buffer.concat(parts);
    const entry = opt + 112 + 32;
    full.writeUInt32LE(full.length, entry); full.writeUInt32LE(cert.length, entry + 4);
    return Buffer.concat([full, cert]);
  }
  return Buffer.concat(parts);
}

test("unsigned and differently signed Mach-O copies share one content digest, not the plain hash", () => {
  const unsigned = macho(CPU_ARM64, PAYLOAD, null);
  const adhoc = macho(CPU_ARM64, PAYLOAD, Buffer.alloc(300, 0x11));
  const devid = macho(CPU_ARM64, PAYLOAD, Buffer.alloc(9000, 0x22));
  const a = contentIdentity(unsigned);
  assert.equal(a.format, "macho");
  assert.deepEqual(a, contentIdentity(adhoc));
  assert.deepEqual(a, contentIdentity(devid));
  assert.notEqual(sha256Bytes(unsigned), sha256Bytes(adhoc), "archive SHA-256 stays independent");
  assert.equal(a.slices[0].arch, "aarch64");
  const flip = macho(CPU_ARM64, PAYLOAD, Buffer.alloc(64, 1)); flip[0x1000] ^= 1;
  assert.notDeepEqual(contentIdentity(macho(CPU_ARM64, PAYLOAD, Buffer.alloc(64, 1))), contentIdentity(flip));
});

test("malformed Mach-O is refused, never plain-hashed", () => {
  const good = macho(CPU_ARM64, PAYLOAD, Buffer.alloc(64, 1));
  for (const cut of [4, 20, 31, 40, 100, 200, good.length - 1]) assert.throws(() => contentIdentity(good.subarray(0, cut)), /Mach-O|CPU/, `truncated at ${cut}`);
  assert.throws(() => contentIdentity(Buffer.concat([good, Buffer.from("extra")])));
  const ncmds = Buffer.from(good); ncmds[16] = 9; assert.throws(() => contentIdentity(ncmds));
  for (const magic of [0xfeedface, 0xfeedfacf, 0xcefaedfe]) { const o = Buffer.from(good); o.writeUInt32BE(magic); assert.throws(() => contentIdentity(o)); }
  const cpu = Buffer.from(good); cpu.writeUInt32LE(7, 4); assert.throws(() => contentIdentity(cpu));
});

test("universal digests each slice like its thin form; malformed fat headers refused", () => {
  const arm = macho(CPU_ARM64, PAYLOAD, null);
  const x86signed = macho(CPU_X86_64, Buffer.from("other-linkedit"), Buffer.alloc(100, 3));
  const x86 = macho(CPU_X86_64, Buffer.from("other-linkedit"), null);
  for (const wide of [false, true]) {
    const id = contentIdentity(fat([[CPU_ARM64, arm], [CPU_X86_64, x86signed]], wide));
    assert.equal(id.format, "macho-universal");
    assert.equal(digestFor(id, "aarch64"), contentIdentity(arm).slices[0].sha256);
    assert.equal(digestFor(id, "x86_64"), contentIdentity(x86).slices[0].sha256);
  }
  const good = fat([[CPU_ARM64, arm]], false);
  assert.throws(() => contentIdentity(good.subarray(0, good.length - 1)));
  const count = Buffer.from(good); count.writeUInt32BE(200, 4); assert.throws(() => contentIdentity(count));
  assert.throws(() => contentIdentity(fat([[CPU_ARM64, arm], [CPU_ARM64, arm]], false)));
});

test("PE hash ignores checksum and certificate table only; malformed PE refused", () => {
  const unsigned = pe(0x8664, 0x11, null, 0);
  const a = contentIdentity(unsigned);
  assert.equal(a.format, "pe"); assert.equal(a.slices[0].arch, "x86_64");
  assert.deepEqual(a, contentIdentity(pe(0x8664, 0x11, Buffer.alloc(24, 0x77), 0xdeadbeef)));
  assert.deepEqual(a, contentIdentity(pe(0x8664, 0x11, Buffer.alloc(4096, 0x66), 0x1234)));
  assert.notDeepEqual(a, contentIdentity(pe(0x8664, 0x12, null, 0)));
  const good = pe(0x8664, 0x11, Buffer.alloc(24, 0x77), 0);
  assert.throws(() => contentIdentity(good.subarray(0, 0x500)));
  assert.throws(() => contentIdentity(Buffer.concat([good, Buffer.from("junk")])));
  const machine = Buffer.from(good); machine.writeUInt16LE(0x14c, 0x84); assert.throws(() => contentIdentity(machine));
});

test("other files are plain SHA-256", () => {
  const id = contentIdentity(Buffer.from("#!/bin/sh\nexit 0\n"));
  assert.equal(id.format, "plain");
  assert.equal(id.slices[0].sha256, createHash("sha256").update("#!/bin/sh\nexit 0\n").digest("hex"));
  assert.ok(contentIdentity(Buffer.alloc(0)));
});
