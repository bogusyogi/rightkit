import { inflateSync } from 'node:zlib';
import fs from 'node:fs';
import path from 'node:path';
import type { QaPage } from './page.js';

/**
 * Pixel-level visual baselines for page QA. A baseline is a PNG captured from the real installed app and kept in
 * the repository; `visualBaseline` compares a fresh viewport screenshot with it pixel by pixel. Decoding is done here
 * with `node:zlib`, so no image dependency is needed. Supported: 8-bit, non-interlaced PNG of colour types gray,
 * gray+alpha, RGB and RGBA (what the browser's screenshot and the platform capture write).
 */

export interface DecodedImage { width: number; height: number; /** RGBA, 4 bytes per pixel, row-major. */ data: Uint8Array }

export interface VisualOptions {
  /** Largest per-channel difference (0-255) still counted as the same pixel. Default 0 (exact). */
  tolerance?: number;
  /** Largest share (0-1) of differing pixels that still passes. Default 0 (exact). */
  maxDiffRatio?: number;
  /** Write the baseline when none exists. Default false: a missing baseline is an error. */
  create?: boolean;
  /** Overwrite the baseline with the fresh capture. Only for a deliberate, reviewed change. Default false. */
  update?: boolean;
  /** Return the verdict instead of throwing `VisualMismatchError` on a failed comparison. Default false. */
  reportOnly?: boolean;
}

export interface VisualReport {
  passes: boolean;
  /** True when the baseline was written on this call (create or update). */
  written: boolean;
  width: number;
  height: number;
  differingPixels: number;
  totalPixels: number;
  ratio: number;
  /** Set when the two images have different dimensions; no pixel comparison is made then. */
  sizeMismatch: boolean;
  baseline: string;
  /** The fresh capture, saved next to the baseline as `<name>.actual.png` when the comparison fails. */
  actual?: string;
}

export class VisualMismatchError extends Error {
  readonly report: VisualReport;
  constructor(report: VisualReport) {
    super(
      report.sizeMismatch
        ? `visual baseline ${report.baseline} is ${report.width}x${report.height} in the capture; sizes differ`
        : `visual baseline ${report.baseline}: ${report.differingPixels} of ${report.totalPixels} pixels differ (${(report.ratio * 100).toFixed(3)}%)`,
    );
    this.name = 'VisualMismatchError';
    this.report = report;
  }
}

const PNG_SIGNATURE = Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);
const CHANNELS: Record<number, number> = { 0: 1, 2: 3, 4: 2, 6: 4 };

function paeth(a: number, b: number, c: number): number {
  const p = a + b - c;
  const pa = Math.abs(p - a), pb = Math.abs(p - b), pc = Math.abs(p - c);
  return pa <= pb && pa <= pc ? a : pb <= pc ? b : c;
}

/** Decode an 8-bit, non-interlaced PNG to RGBA. Throws on anything else rather than guessing. */
export function decodePng(buffer: Uint8Array): DecodedImage {
  const bytes = Buffer.from(buffer.buffer, buffer.byteOffset, buffer.byteLength);
  if (bytes.length < 8 || !bytes.subarray(0, 8).equals(PNG_SIGNATURE)) throw new Error('not a PNG file');
  let offset = 8;
  let header: { width: number; height: number; depth: number; colourType: number; interlace: number } | undefined;
  const idat: Buffer[] = [];
  while (offset + 8 <= bytes.length) {
    const length = bytes.readUInt32BE(offset);
    const type = bytes.toString('latin1', offset + 4, offset + 8);
    const body = bytes.subarray(offset + 8, offset + 8 + length);
    if (body.length !== length) throw new Error('truncated PNG chunk');
    if (type === 'IHDR') {
      header = { width: body.readUInt32BE(0), height: body.readUInt32BE(4), depth: body[8], colourType: body[9], interlace: body[12] };
    } else if (type === 'IDAT') idat.push(body);
    else if (type === 'IEND') break;
    offset += 12 + length;
  }
  if (!header) throw new Error('PNG has no IHDR chunk');
  const { width, height, depth, colourType, interlace } = header;
  const channels = CHANNELS[colourType];
  if (!channels || depth !== 8 || interlace !== 0) throw new Error(`unsupported PNG: colour type ${colourType}, bit depth ${depth}, interlace ${interlace}`);
  if (!width || !height) throw new Error('PNG has no pixels');
  const stride = width * channels;
  const raw = inflateSync(Buffer.concat(idat));
  if (raw.length !== height * (stride + 1)) throw new Error('PNG image data has the wrong length');
  const lines = new Uint8Array(height * stride);
  for (let y = 0; y < height; y++) {
    const filter = raw[y * (stride + 1)];
    const src = y * (stride + 1) + 1;
    const row = y * stride, prev = row - stride;
    for (let x = 0; x < stride; x++) {
      const left = x >= channels ? lines[row + x - channels] : 0;
      const up = y > 0 ? lines[prev + x] : 0;
      const corner = y > 0 && x >= channels ? lines[prev + x - channels] : 0;
      let value: number;
      switch (filter) {
        case 0: value = raw[src + x]; break;
        case 1: value = raw[src + x] + left; break;
        case 2: value = raw[src + x] + up; break;
        case 3: value = raw[src + x] + ((left + up) >> 1); break;
        case 4: value = raw[src + x] + paeth(left, up, corner); break;
        default: throw new Error(`unknown PNG filter ${filter}`);
      }
      lines[row + x] = value & 0xff;
    }
  }
  const data = new Uint8Array(width * height * 4);
  for (let i = 0; i < width * height; i++) {
    const s = i * channels, d = i * 4;
    if (colourType === 0) { data[d] = data[d + 1] = data[d + 2] = lines[s]; data[d + 3] = 255; }
    else if (colourType === 4) { data[d] = data[d + 1] = data[d + 2] = lines[s]; data[d + 3] = lines[s + 1]; }
    else if (colourType === 2) { data[d] = lines[s]; data[d + 1] = lines[s + 1]; data[d + 2] = lines[s + 2]; data[d + 3] = 255; }
    else { data[d] = lines[s]; data[d + 1] = lines[s + 1]; data[d + 2] = lines[s + 2]; data[d + 3] = lines[s + 3]; }
  }
  return { width, height, data };
}

function checkOptions(options: VisualOptions): { tolerance: number; maxDiffRatio: number } {
  const tolerance = options.tolerance ?? 0;
  const maxDiffRatio = options.maxDiffRatio ?? 0;
  if (!Number.isInteger(tolerance) || tolerance < 0 || tolerance > 255) throw new Error('visual tolerance must be an integer from 0 to 255');
  if (!Number.isFinite(maxDiffRatio) || maxDiffRatio < 0 || maxDiffRatio > 1) throw new Error('visual maxDiffRatio must be a number from 0 to 1');
  return { tolerance, maxDiffRatio };
}

/** Compare two PNG files' pixels. Differing pixels are those with any RGBA channel further apart than `tolerance`. */
export function comparePng(baseline: Uint8Array, actual: Uint8Array, options: VisualOptions = {}): Omit<VisualReport, 'written' | 'baseline' | 'actual'> {
  const { tolerance, maxDiffRatio } = checkOptions(options);
  const a = decodePng(baseline), b = decodePng(actual);
  if (a.width !== b.width || a.height !== b.height) {
    return { passes: false, width: b.width, height: b.height, differingPixels: 0, totalPixels: b.width * b.height, ratio: 1, sizeMismatch: true };
  }
  const total = a.width * a.height;
  let differing = 0;
  for (let i = 0; i < total * 4; i += 4) {
    if (Math.abs(a.data[i] - b.data[i]) > tolerance || Math.abs(a.data[i + 1] - b.data[i + 1]) > tolerance
      || Math.abs(a.data[i + 2] - b.data[i + 2]) > tolerance || Math.abs(a.data[i + 3] - b.data[i + 3]) > tolerance) differing++;
  }
  const ratio = total === 0 ? 0 : differing / total;
  return { passes: ratio <= maxDiffRatio, width: a.width, height: a.height, differingPixels: differing, totalPixels: total, ratio, sizeMismatch: false };
}

/**
 * Compare a fresh viewport screenshot of `page` with the PNG baseline at `baselinePath`. A missing baseline is an
 * error unless `create` is set; `update` overwrites it. A failed comparison saves the capture as `<name>.actual.png`
 * beside the baseline and throws `VisualMismatchError`, unless `reportOnly` is set.
 */
export async function visualBaseline(page: QaPage, baselinePath: string, options: VisualOptions = {}): Promise<VisualReport> {
  checkOptions(options);
  if (typeof baselinePath !== 'string' || !baselinePath.endsWith('.png')) throw new Error('visualBaseline needs a .png baseline path');
  const target = path.resolve(baselinePath);
  const actual = await page.screenshot();
  const exists = fs.existsSync(target);
  if (!exists && !options.create && !options.update) throw new Error(`no visual baseline at ${target}; pass create: true to record it`);
  if (!exists || options.update) {
    fs.mkdirSync(path.dirname(target), { recursive: true });
    fs.writeFileSync(target, actual);
    const decoded = decodePng(actual);
    return { passes: true, written: true, width: decoded.width, height: decoded.height, differingPixels: 0, totalPixels: decoded.width * decoded.height, ratio: 0, sizeMismatch: false, baseline: target };
  }
  const verdict = comparePng(fs.readFileSync(target), actual, options);
  const report: VisualReport = { ...verdict, written: false, baseline: target };
  if (!report.passes) {
    const savedPath = target.replace(/\.png$/, '.actual.png');
    fs.writeFileSync(savedPath, actual);
    report.actual = savedPath;
    if (!options.reportOnly) throw new VisualMismatchError(report);
  }
  return report;
}
