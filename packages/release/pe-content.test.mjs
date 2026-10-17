import test from "node:test";
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { peContentDigest, verifyImportClosure, main } from "./pe-content.mjs";

// Minimal PE32+ with one .idata section importing the given DLLs; optional fake certificate blob and CheckSum.
function makePe({ dlls = ["KERNEL32.dll"], checksum = 0, cert = null, trailing = null } = {}) {
  const raw = Buffer.alloc(0x200 + 0x200);
  raw.writeUInt16LE(0x5a4d, 0);
  raw.writeUInt32LE(0x40, 0x3c);
  raw.writeUInt32LE(0x4550, 0x40);
  raw.writeUInt16LE(0x8664, 0x44);
  raw.writeUInt16LE(1, 0x46);
  raw.writeUInt16LE(240, 0x54);
  const opt = 0x58;
  raw.writeUInt16LE(0x20b, opt);
  raw.writeUInt32LE(0x200, opt + 60);
  raw.writeUInt32LE(checksum, opt + 64);
  raw.writeUInt16LE(3, opt + 68);
  raw.writeUInt32LE(16, opt + 108);
  const dirs = opt + 112;
  raw.writeUInt32LE(0x1000, dirs + 8);
  raw.writeUInt32LE(20 * (dlls.length + 1), dirs + 12);
  const sec = opt + 240;
  raw.write(".idata", sec);
  raw.writeUInt32LE(0x200, sec + 8);
  raw.writeUInt32LE(0x1000, sec + 12);
  raw.writeUInt32LE(0x200, sec + 16);
  raw.writeUInt32LE(0x200, sec + 20);
  let nameAt = 0x200 + 20 * (dlls.length + 1);
  dlls.forEach((dll, i) => {
    raw.writeUInt32LE(0x1000 + (nameAt - 0x200), 0x200 + i * 20 + 12);
    raw.write(dll, nameAt, "latin1");
    nameAt += dll.length + 1;
  });
  const parts = [raw];
  if (trailing) parts.push(trailing);
  if (cert) {
    const offset = parts.reduce((n, p) => n + p.length, 0);
    raw.writeUInt32LE(offset, dirs + 4 * 8);
    raw.writeUInt32LE(cert.length, dirs + 4 * 8 + 4);
    parts.push(cert);
  }
  return Buffer.concat(parts);
}

test("content digest ignores CheckSum, certificate table and certificate data", () => {
  const plain = makePe();
  const signed = makePe({ checksum: 0xdeadbeef, cert: Buffer.alloc(64, 7) });
  const resigned = makePe({ checksum: 1, cert: Buffer.alloc(128, 9) });
  const digest = peContentDigest(plain);
  assert.match(digest, /^[0-9a-f]{64}$/);
  assert.equal(peContentDigest(signed), digest);
  assert.equal(peContentDigest(resigned), digest);
  assert.equal(peContentDigest(plain, { algorithm: "sha512" }).length, 128);
});

test("content digest changes with section bytes and with appended non-certificate data", () => {
  const base = makePe();
  const digest = peContentDigest(base);
  const tampered = Buffer.from(base);
  tampered[0x200 + 0x1f0] ^= 1;
  assert.notEqual(peContentDigest(tampered), digest);
  assert.notEqual(peContentDigest(makePe({ trailing: Buffer.from("overlay") })), digest);
  assert.equal(peContentDigest(makePe({ trailing: Buffer.from("overlay"), cert: Buffer.alloc(16, 1) })), peContentDigest(makePe({ trailing: Buffer.from("overlay") })));
  assert.notEqual(digest, createHash("sha256").update(base).digest("hex"));
});

test("content digest refuses bytes appended after the certificate table and overlapping layouts", () => {
  const signed = makePe({ cert: Buffer.alloc(64, 7) });
  const appended = Buffer.concat([signed, Buffer.from("tail")]);
  assert.throws(() => peContentDigest(appended), { code: "PE_INVALID" });
  const overlapping = Buffer.from(signed);
  overlapping.writeUInt32LE(0x100, 0x58 + 112 + 4 * 8);
  assert.throws(() => peContentDigest(overlapping), { code: "PE_INVALID" });
  const unsignedAppended = Buffer.concat([makePe(), Buffer.from("tail")]);
  assert.notEqual(peContentDigest(unsignedAppended), peContentDigest(makePe()));
});

test("content digest refuses non-PE input and bad algorithms", () => {
  assert.throws(() => peContentDigest(Buffer.from("hello world, definitely not a PE file at all......................")), { code: "PE_INVALID" });
  assert.throws(() => peContentDigest(makePe(), { algorithm: "md5" }), { code: "PE_USAGE" });
});

test("import closure reports unexpected DLLs, case-insensitively, with prefix allowances", () => {
  const pe = makePe({ dlls: ["KERNEL32.dll", "api-ms-win-core-synch-l1-2-0.dll", "evil.dll"] });
  const result = verifyImportClosure(pe, ["kernel32.dll", "api-ms-win-*"]);
  assert.equal(result.ok, false);
  assert.deepEqual(result.unexpected, ["evil.dll"]);
  assert.equal(verifyImportClosure(pe, ["kernel32.dll", "api-ms-win-*", "EVIL.DLL"]).ok, true);
  assert.equal(verifyImportClosure(pe, []).ok, false);
  assert.throws(() => verifyImportClosure(pe, "kernel32.dll"), { code: "PE_USAGE" });
});

test("CLI exit codes: 0 allowed, 1 unexpected import, 2 usage", async () => {
  const { mkdtempSync, writeFileSync } = await import("node:fs");
  const { tmpdir } = await import("node:os");
  const path = await import("node:path");
  const dir = mkdtempSync(path.join(tmpdir(), "pe-content-"));
  const file = path.join(dir, "a.exe");
  writeFileSync(file, makePe());
  const sink = { lines: [], log(m) { this.lines.push(m); }, error(m) { this.lines.push(m); } };
  assert.equal(main(["digest", file], sink), 0);
  assert.match(sink.lines.at(-1), /^sha256:[0-9a-f]{64}$/);
  assert.equal(main(["imports", file, "--allow", "kernel32.dll"], sink), 0);
  assert.equal(main(["imports", file, "--allow", "other.dll"], sink), 1);
  assert.equal(main(["imports"], sink), 2);
  assert.equal(main(["digest", file, "--bogus"], sink), 2);
});
