#!/usr/bin/env node
// App icon packers (bogusyogi/rightkit#675): assemble a Windows ICO or a macOS ICNS from per-size PNG frames, so
// hand-tuned small frames (16/24/32/48) are kept exactly as drawn instead of being down-scaled from one master.
//   right-release icons ico  --out <file.ico>  --frame <PNG>... [--dir <folder of <size>.png>]
//   right-release icons icns --out <file.icns> --frame <PNG>... [--dir <folder of <size>.png>]
// Every frame must be a square PNG; its size comes from its IHDR, never from the file name. ICO frames are stored
// as PNG (Vista+), sizes 1..256. ICNS accepts 16, 32, 64, 128, 256, 512 and 1024. Pure Buffer work, any host.
// Rendering a master image to PNG frames and DMG background art stay with the app's design source
// (dmg background: generate-dmg-background.py).
import { readFileSync, readdirSync, writeFileSync, mkdirSync } from "node:fs";
import path from "node:path";

const PNG_MAGIC = Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);
export const ICNS_TYPES = Object.freeze({ 16: "icp4", 32: "icp5", 64: "icp6", 128: "ic07", 256: "ic08", 512: "ic09", 1024: "ic10" });
function fail(message, code = "ICON_INVALID") { throw Object.assign(new Error(message), { code }); }

/** Read the pixel size of a square PNG from its IHDR. */
export function pngSize(bytes, label = "frame") {
  const buf = Buffer.isBuffer(bytes) ? bytes : Buffer.from(bytes);
  if (buf.length < 33 || !buf.subarray(0, 8).equals(PNG_MAGIC) || buf.toString("latin1", 12, 16) !== "IHDR") fail(`${label} is not a PNG`);
  const width = buf.readUInt32BE(16), height = buf.readUInt32BE(20);
  if (width !== height) fail(`${label} must be square, got ${width}x${height}`);
  // Walk the chunk list: IHDR first (13 bytes), at least one IDAT, IEND last with nothing after it.
  if (buf.readUInt32BE(8) !== 13) fail(`${label} has a malformed IHDR chunk`);
  let at = 8, idat = false, iend = false, first = true;
  while (at < buf.length) {
    if (at + 12 > buf.length) fail(`${label} is a truncated PNG`);
    const length = buf.readUInt32BE(at), type = buf.toString("latin1", at + 4, at + 8);
    if (at + 12 + length > buf.length) fail(`${label} is a truncated PNG`);
    if (iend) fail(`${label} has data after IEND`);
    if (first && type !== "IHDR") fail(`${label} is not a PNG`);
    first = false;
    if (type === "IDAT") idat = true;
    if (type === "IEND") iend = true;
    at += 12 + length;
  }
  if (!idat) fail(`${label} has no image data (IDAT)`);
  if (!iend) fail(`${label} is missing IEND`);
  const depth = buf[24], colorType = buf[25];
  if (depth !== 8 || colorType !== 6) fail(`${label} must be 8-bit RGBA (color type 6, bit depth 8), got bit depth ${depth}, color type ${colorType}`);
  return width;
}

function normalizeFrames(frames, { min = 1, max = 1024, only = null } = {}) {
  if (!Array.isArray(frames) || frames.length === 0) fail("at least one PNG frame is required", "ICON_USAGE");
  const bySize = new Map();
  for (const frame of frames) {
    const bytes = Buffer.isBuffer(frame) ? frame : Buffer.from(frame);
    const size = pngSize(bytes);
    if (size < min || size > max || (only && !only.includes(size))) fail(`unsupported frame size ${size}${only ? ` (allowed: ${only.join(", ")})` : ` (allowed ${min}..${max})`}`);
    if (bySize.has(size)) fail(`duplicate ${size}x${size} frame`);
    bySize.set(size, bytes);
  }
  return [...bySize.entries()].sort((a, b) => a[0] - b[0]);
}

/** @param {Buffer[]} frames PNG buffers, one per size (1..256). @returns {Buffer} ICO file. */
export function packIco(frames) {
  const sorted = normalizeFrames(frames, { max: 256 });
  const header = Buffer.alloc(6 + 16 * sorted.length);
  header.writeUInt16LE(0, 0); header.writeUInt16LE(1, 2); header.writeUInt16LE(sorted.length, 4);
  let offset = header.length;
  sorted.forEach(([size, png], i) => {
    const at = 6 + i * 16;
    header[at] = size === 256 ? 0 : size; header[at + 1] = size === 256 ? 0 : size;
    header.writeUInt16LE(1, at + 4); header.writeUInt16LE(32, at + 6);
    header.writeUInt32LE(png.length, at + 8); header.writeUInt32LE(offset, at + 12);
    offset += png.length;
  });
  return Buffer.concat([header, ...sorted.map(([, png]) => png)]);
}

/** @param {Buffer[]} frames PNG buffers (16, 32, 64, 128, 256, 512, 1024). @returns {Buffer} ICNS file. */
export function packIcns(frames) {
  const sorted = normalizeFrames(frames, { only: Object.keys(ICNS_TYPES).map(Number) });
  const chunks = sorted.map(([size, png]) => {
    const head = Buffer.alloc(8);
    head.write(ICNS_TYPES[size], 0, "latin1"); head.writeUInt32BE(png.length + 8, 4);
    return Buffer.concat([head, png]);
  });
  const total = 8 + chunks.reduce((n, c) => n + c.length, 0);
  const head = Buffer.alloc(8);
  head.write("icns", 0, "latin1"); head.writeUInt32BE(total, 4);
  return Buffer.concat([head, ...chunks]);
}

function parseArgs(argv) {
  const [kind, ...rest] = argv;
  const opts = { kind, out: null, frames: [], dirs: [] };
  for (let i = 0; i < rest.length; i++) {
    const arg = rest[i], value = rest[i + 1];
    if (!["--out", "--frame", "--dir"].includes(arg)) fail(`unknown icons argument: ${arg}`, "ICON_USAGE");
    if (value === undefined || value.startsWith("--")) fail(`${arg} needs a value`, "ICON_USAGE");
    i += 1;
    if (arg === "--out") opts.out = value; else if (arg === "--frame") opts.frames.push(value); else opts.dirs.push(value);
  }
  if (!["ico", "icns"].includes(kind) || !opts.out) fail("usage: right-release icons ico|icns --out <file> --frame <PNG>... [--dir <folder>]", "ICON_USAGE");
  for (const dir of opts.dirs) for (const name of readdirSync(dir).filter((n) => n.toLowerCase().endsWith(".png")).sort()) opts.frames.push(path.join(dir, name));
  return opts;
}

export function main(argv = process.argv.slice(2), out = console) {
  try {
    const opts = parseArgs(argv);
    const frames = opts.frames.map((file) => readFileSync(file));
    const bytes = opts.kind === "ico" ? packIco(frames) : packIcns(frames);
    mkdirSync(path.dirname(path.resolve(opts.out)), { recursive: true });
    writeFileSync(opts.out, bytes);
    out.log(JSON.stringify({ out: opts.out, kind: opts.kind, frames: opts.frames.length, bytes: bytes.length }));
    return 0;
  } catch (error) {
    out.error(error.message);
    return error.code === "ICON_USAGE" ? 2 : 1;
  }
}

if (process.argv[1] && import.meta.url === new URL(`file://${process.argv[1]}`).href) process.exit(main());
