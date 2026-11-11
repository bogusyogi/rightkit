/** A small dependency-free QR Code encoder: byte mode, error correction M, versions 1 to 20, mask chosen by penalty.
 *  The pairing QR holds a public URI of about 250 bytes (version 11). Returns the module matrix (true = dark). */

const ECC_PER_BLOCK_M = [-1, 10, 16, 26, 18, 24, 16, 18, 22, 22, 26, 30, 22, 22, 24, 24, 28, 28, 26, 26, 26];
const BLOCKS_M = [-1, 1, 1, 1, 2, 2, 4, 4, 4, 5, 5, 5, 8, 9, 9, 10, 10, 11, 13, 14, 16];
const MAX_VERSION = 20;

function rawModules(ver: number): number {
  let r = (16 * ver + 128) * ver + 64;
  if (ver >= 2) {
    const n = Math.floor(ver / 7) + 2;
    r -= (25 * n - 10) * n - 55;
    if (ver >= 7) r -= 36;
  }
  return r;
}

const dataCodewords = (ver: number): number =>
  Math.floor(rawModules(ver) / 8) - ECC_PER_BLOCK_M[ver] * BLOCKS_M[ver];

// GF(256) arithmetic, polynomial 0x11D.
const EXP = new Array<number>(512);
const LOG = new Array<number>(256);
(() => {
  let x = 1;
  for (let i = 0; i < 255; i++) {
    EXP[i] = x;
    LOG[x] = i;
    x <<= 1;
    if (x & 0x100) x ^= 0x11d;
  }
  for (let i = 255; i < 512; i++) EXP[i] = EXP[i - 255];
})();

const mul = (a: number, b: number): number => (a === 0 || b === 0 ? 0 : EXP[LOG[a] + LOG[b]]);

function generator(degree: number): number[] {
  let poly = [1];
  for (let i = 0; i < degree; i++) {
    const next = new Array<number>(poly.length + 1).fill(0);
    for (let j = 0; j < poly.length; j++) {
      next[j] ^= poly[j];
      next[j + 1] ^= mul(poly[j], EXP[i]);
    }
    poly = next;
  }
  return poly;
}

function remainder(data: number[], gen: number[]): number[] {
  const rem = new Array<number>(gen.length - 1).fill(0);
  for (const byte of data) {
    const factor = byte ^ rem.shift()!;
    rem.push(0);
    for (let i = 0; i < rem.length; i++) rem[i] ^= mul(gen[i + 1], factor);
  }
  return rem;
}

function interleave(data: number[], ver: number): number[] {
  const blocks = BLOCKS_M[ver];
  const eccLen = ECC_PER_BLOCK_M[ver];
  const rawCodewords = Math.floor(rawModules(ver) / 8);
  const shortBlocks = blocks - (rawCodewords % blocks);
  const shortLen = Math.floor(rawCodewords / blocks);
  const gen = generator(eccLen);
  const dataBlocks: number[][] = [];
  const eccBlocks: number[][] = [];
  for (let i = 0, k = 0; i < blocks; i++) {
    const len = shortLen - eccLen + (i < shortBlocks ? 0 : 1);
    const chunk = data.slice(k, k + len);
    k += len;
    dataBlocks.push(chunk);
    eccBlocks.push(remainder(chunk, gen));
  }
  const out: number[] = [];
  for (let i = 0; i <= shortLen - eccLen; i++) {
    dataBlocks.forEach((b) => { if (i < b.length) out.push(b[i]); });
  }
  for (let i = 0; i < eccLen; i++) eccBlocks.forEach((b) => out.push(b[i]));
  return out;
}

/** The module matrix for `text` (UTF-8, byte mode). Throws when it does not fit version 20 at level M. */
export function qrMatrix(text: string): boolean[][] {
  const bytes = Array.from(new TextEncoder().encode(text));
  let ver = 1;
  for (; ver <= MAX_VERSION; ver++) {
    const countBits = ver < 10 ? 8 : 16;
    if (4 + countBits + bytes.length * 8 <= dataCodewords(ver) * 8) break;
  }
  if (ver > MAX_VERSION) throw new Error("QR payload is too long");

  const bits: number[] = [];
  const put = (value: number, len: number) => { for (let i = len - 1; i >= 0; i--) bits.push((value >>> i) & 1); };
  put(0b0100, 4);
  put(bytes.length, ver < 10 ? 8 : 16);
  bytes.forEach((b) => put(b, 8));
  const capacity = dataCodewords(ver) * 8;
  put(0, Math.min(4, capacity - bits.length));
  while (bits.length % 8) bits.push(0);
  for (let pad = 0xec; bits.length < capacity; pad ^= 0xec ^ 0x11) put(pad, 8);
  const data: number[] = [];
  for (let i = 0; i < bits.length; i += 8) data.push(parseInt(bits.slice(i, i + 8).join(""), 2));
  const codewords = interleave(data, ver);

  const size = ver * 4 + 17;
  const grid: boolean[][] = Array.from({ length: size }, () => new Array<boolean>(size).fill(false));
  const fn: boolean[][] = Array.from({ length: size }, () => new Array<boolean>(size).fill(false));
  const set = (x: number, y: number, dark: boolean) => {
    if (x < 0 || y < 0 || x >= size || y >= size) return;
    grid[y][x] = dark;
    fn[y][x] = true;
  };

  for (let i = 0; i < size; i++) { set(6, i, i % 2 === 0); set(i, 6, i % 2 === 0); }
  const finder = (cx: number, cy: number) => {
    for (let dy = -4; dy <= 4; dy++) for (let dx = -4; dx <= 4; dx++) {
      const d = Math.max(Math.abs(dx), Math.abs(dy));
      set(cx + dx, cy + dy, d !== 2 && d !== 4);
    }
  };
  finder(3, 3); finder(size - 4, 3); finder(3, size - 4);
  if (ver >= 2) {
    const n = Math.floor(ver / 7) + 2;
    const step = ver === 32 ? 26 : Math.ceil((ver * 4 + 4) / (n * 2 - 2)) * 2;
    const pos = [6];
    for (let p = size - 7; pos.length < n; p -= step) pos.splice(1, 0, p);
    for (let i = 0; i < n; i++) for (let j = 0; j < n; j++) {
      if ((i === 0 && j === 0) || (i === 0 && j === n - 1) || (i === n - 1 && j === 0)) continue;
      for (let dy = -2; dy <= 2; dy++) for (let dx = -2; dx <= 2; dx++) {
        set(pos[i] + dx, pos[j] + dy, Math.max(Math.abs(dx), Math.abs(dy)) !== 1);
      }
    }
  }

  const formatBits = (mask: number): number => {
    const data = (0b00 << 3) | mask; // level M = 00
    let rem = data;
    for (let i = 0; i < 10; i++) rem = (rem << 1) ^ ((rem >>> 9) * 0x537);
    return ((data << 10) | rem) ^ 0x5412;
  };
  const drawFormat = (mask: number) => {
    const b = formatBits(mask);
    const bit = (i: number) => ((b >>> i) & 1) !== 0;
    for (let i = 0; i <= 5; i++) set(8, i, bit(i));
    set(8, 7, bit(6)); set(8, 8, bit(7)); set(7, 8, bit(8));
    for (let i = 9; i < 15; i++) set(14 - i, 8, bit(i));
    for (let i = 0; i < 8; i++) set(size - 1 - i, 8, bit(i));
    for (let i = 8; i < 15; i++) set(8, size - 15 + i, bit(i));
    set(8, size - 8, true);
  };
  drawFormat(0);
  if (ver >= 7) {
    let rem = ver;
    for (let i = 0; i < 12; i++) rem = (rem << 1) ^ ((rem >>> 11) * 0x1f25);
    const b = (ver << 12) | rem;
    for (let i = 0; i < 18; i++) {
      const dark = ((b >>> i) & 1) !== 0;
      const a = size - 11 + (i % 3);
      const c = Math.floor(i / 3);
      set(a, c, dark);
      set(c, a, dark);
    }
  }

  // Place the data bits in the zigzag, skipping function modules.
  const total = codewords.length * 8;
  let k = 0;
  for (let right = size - 1; right >= 1; right -= 2) {
    if (right === 6) right = 5;
    for (let v = 0; v < size; v++) for (let j = 0; j < 2; j++) {
      const x = right - j;
      const y = ((right + 1) & 2) === 0 ? size - 1 - v : v;
      if (!fn[y][x] && k < total) {
        grid[y][x] = ((codewords[k >>> 3] >>> (7 - (k & 7))) & 1) !== 0;
        k++;
      }
    }
  }

  const masks: ((x: number, y: number) => boolean)[] = [
    (x, y) => (x + y) % 2 === 0,
    (_x, y) => y % 2 === 0,
    (x) => x % 3 === 0,
    (x, y) => (x + y) % 3 === 0,
    (x, y) => (Math.floor(x / 3) + Math.floor(y / 2)) % 2 === 0,
    (x, y) => ((x * y) % 2) + ((x * y) % 3) === 0,
    (x, y) => (((x * y) % 2) + ((x * y) % 3)) % 2 === 0,
    (x, y) => (((x + y) % 2) + ((x * y) % 3)) % 2 === 0,
  ];
  const apply = (mask: number) => {
    for (let y = 0; y < size; y++) for (let x = 0; x < size; x++) {
      if (!fn[y][x] && masks[mask](x, y)) grid[y][x] = !grid[y][x];
    }
  };

  const penalty = (): number => {
    let score = 0;
    const lines = [grid, grid[0].map((_, x) => grid.map((row) => row[x]))];
    for (const rows of lines) {
      for (const row of rows) {
        let run = 1;
        for (let i = 1; i <= size; i++) {
          if (i < size && row[i] === row[i - 1]) run++;
          else { if (run >= 5) score += run - 2; run = 1; }
        }
      }
    }
    for (let y = 0; y < size - 1; y++) for (let x = 0; x < size - 1; x++) {
      const c = grid[y][x];
      if (c === grid[y][x + 1] && c === grid[y + 1][x] && c === grid[y + 1][x + 1]) score += 3;
    }
    const pat = [true, false, true, true, true, false, true, false, false, false, false];
    const rev = [...pat].reverse();
    for (const rows of lines) {
      for (const row of rows) {
        for (let i = 0; i + 11 <= size; i++) {
          const win = row.slice(i, i + 11);
          if (win.every((v, n) => v === pat[n]) || win.every((v, n) => v === rev[n])) score += 40;
        }
      }
    }
    const dark = grid.reduce((n, row) => n + row.filter(Boolean).length, 0);
    score += Math.floor(Math.abs(dark * 20 - size * size * 10) / (size * size)) * 10;
    return score;
  };

  let best = 0;
  let bestScore = Infinity;
  for (let m = 0; m < 8; m++) {
    apply(m);
    drawFormat(m);
    const s = penalty();
    if (s < bestScore) { bestScore = s; best = m; }
    apply(m);
  }
  apply(best);
  drawFormat(best);
  return grid;
}

/** The matrix as one SVG path (quiet zone of 4 modules), for rendering at any size. */
export function qrSvgPath(matrix: boolean[][]): { path: string; size: number } {
  const n = matrix.length;
  let path = "";
  matrix.forEach((row, y) => {
    for (let x = 0; x < n; x++) {
      if (!row[x]) continue;
      let end = x;
      while (end + 1 < n && row[end + 1]) end++;
      path += `M${x + 4} ${y + 4}h${end - x + 1}v1h-${end - x + 1}z`;
      x = end;
    }
  });
  return { path, size: n + 8 };
}
