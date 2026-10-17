import test from "node:test";
import assert from "node:assert/strict";
import { packIco, packIcns, pngSize, main } from "./icon-pack.mjs";
import { mkdtempSync, mkdirSync, writeFileSync, readFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";

function chunk(type, data) {
  const b = Buffer.alloc(12 + data.length);
  b.writeUInt32BE(data.length, 0); b.write(type, 4, "latin1"); data.copy(b, 8);
  return b;
}
function png(size, fill = 0, { colorType = 6, depth = 8, idat = true, iend = true } = {}) {
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(size, 0); ihdr.writeUInt32BE(size, 4); ihdr[8] = depth; ihdr[9] = colorType;
  return Buffer.concat([
    Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]), chunk("IHDR", ihdr),
    ...(idat ? [chunk("IDAT", Buffer.alloc(8, fill))] : []), ...(iend ? [chunk("IEND", Buffer.alloc(0))] : []),
  ]);
}

test("pngSize reads IHDR and refuses non-PNG and non-square", () => {
  assert.equal(pngSize(png(48)), 48);
  assert.throws(() => pngSize(Buffer.alloc(64)), { code: "ICON_INVALID" });
  const wide = png(16); wide.writeUInt32BE(32, 16);
  assert.throws(() => pngSize(wide), /square/);
});

test("PNG structure and pixel format are validated", () => {
  const header = Buffer.alloc(33);
  png(16).copy(header, 0, 0, 33);
  assert.throws(() => pngSize(header), /truncated|IDAT/);
  assert.throws(() => packIco([header]), { code: "ICON_INVALID" });
  assert.throws(() => pngSize(png(16, 0, { idat: false })), /no image data/);
  assert.throws(() => pngSize(png(16, 0, { iend: false })), /IEND/);
  assert.throws(() => pngSize(png(16, 0, { colorType: 2 })), /8-bit RGBA/);
  assert.throws(() => pngSize(png(16, 0, { depth: 16 })), /8-bit RGBA/);
  assert.throws(() => pngSize(Buffer.concat([png(16), Buffer.from("x")])), /truncated|after IEND/);
});

test("packIco keeps each hand-tuned frame byte for byte, sorted, 256 encoded as 0", () => {
  const frames = [png(256, 3), png(16, 1), png(32, 2)];
  const ico = packIco(frames);
  assert.equal(ico.readUInt16LE(2), 1);
  assert.equal(ico.readUInt16LE(4), 3);
  const entries = [0, 1, 2].map((i) => ({ w: ico[6 + i * 16], len: ico.readUInt32LE(6 + i * 16 + 8), off: ico.readUInt32LE(6 + i * 16 + 12) }));
  assert.deepEqual(entries.map((e) => e.w), [16, 32, 0]);
  assert.ok(ico.subarray(entries[0].off, entries[0].off + entries[0].len).equals(frames[1]));
  assert.ok(ico.subarray(entries[2].off, entries[2].off + entries[2].len).equals(frames[0]));
  assert.throws(() => packIco([png(16), png(16, 9)]), /duplicate/);
  assert.throws(() => packIco([png(512)]), /unsupported frame size/);
  assert.throws(() => packIco([]), { code: "ICON_USAGE" });
});

test("packIcns writes typed PNG chunks with a correct total length", () => {
  const icns = packIcns([png(128), png(16), png(1024)]);
  assert.equal(icns.toString("latin1", 0, 4), "icns");
  assert.equal(icns.readUInt32BE(4), icns.length);
  const types = [];
  for (let at = 8; at < icns.length; at += icns.readUInt32BE(at + 4)) types.push(icns.toString("latin1", at, at + 4));
  assert.deepEqual(types, ["icp4", "ic07", "ic10"]);
  assert.throws(() => packIcns([png(48)]), /unsupported frame size 48/);
});

test("CLI packs from --dir and --frame; usage errors exit 2", () => {
  const dir = mkdtempSync(path.join(tmpdir(), "icon-pack-"));
  mkdirSync(path.join(dir, "f"));
  writeFileSync(path.join(dir, "f", "16.png"), png(16));
  writeFileSync(path.join(dir, "f", "32.png"), png(32));
  const sink = { log() {}, error() {} };
  assert.equal(main(["ico", "--out", path.join(dir, "o", "a.ico"), "--dir", path.join(dir, "f")], sink), 0);
  assert.equal(readFileSync(path.join(dir, "o", "a.ico")).readUInt16LE(4), 2);
  assert.equal(main(["icns", "--out", path.join(dir, "a.icns"), "--frame", path.join(dir, "f", "16.png")], sink), 0);
  assert.equal(main(["ico"], sink), 2);
  assert.equal(main(["ico", "--out", path.join(dir, "x.ico"), "--frame", path.join(dir, "missing.png")], sink), 1);
});
