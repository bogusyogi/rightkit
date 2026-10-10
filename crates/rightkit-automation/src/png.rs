//! Minimal dependency-free PNG encoder (RGBA8, stored deflate blocks). Used by
//! the Windows window capture; also validates macOS captures in tests.

fn crc32(data: &[u8]) -> u32 {
    let mut table = [0u32; 256];
    for (n, slot) in table.iter_mut().enumerate() {
        let mut c = n as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 {
                0xEDB8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
        }
        *slot = c;
    }
    let mut c = 0xFFFF_FFFFu32;
    for b in data {
        c = table[((c ^ u32::from(*b)) & 0xFF) as usize] ^ (c >> 8);
    }
    c ^ 0xFFFF_FFFF
}

fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for chunk in data.chunks(5552) {
        for x in chunk {
            a += u32::from(*x);
            b += a;
        }
        a %= 65521;
        b %= 65521;
    }
    (b << 16) | a
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], body: &[u8]) {
    out.extend_from_slice(&(body.len() as u32).to_be_bytes());
    let mut crc_in = kind.to_vec();
    crc_in.extend_from_slice(body);
    out.extend_from_slice(&crc_in);
    out.extend_from_slice(&crc32(&crc_in).to_be_bytes());
}

/// `rgba` is `width*height*4` bytes, top row first.
pub fn encode_rgba(width: u32, height: u32, rgba: &[u8]) -> Option<Vec<u8>> {
    if width == 0 || height == 0 || rgba.len() != width as usize * height as usize * 4 {
        return None;
    }
    let stride = width as usize * 4;
    let mut raw = Vec::with_capacity((stride + 1) * height as usize);
    for row in rgba.chunks(stride) {
        raw.push(0); // filter: none
        raw.extend_from_slice(row);
    }
    let mut z = vec![0x78, 0x01];
    let mut chunks = raw.chunks(65535).peekable();
    while let Some(c) = chunks.next() {
        z.push(u8::from(chunks.peek().is_none()));
        z.extend_from_slice(&(c.len() as u16).to_le_bytes());
        z.extend_from_slice(&(!(c.len() as u16)).to_le_bytes());
        z.extend_from_slice(c);
    }
    z.extend_from_slice(&adler32(&raw).to_be_bytes());

    let mut out = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &z);
    chunk(&mut out, b"IEND", &[]);
    Some(out)
}

/// Dimensions from a PNG header (for validation).
pub fn dimensions(png: &[u8]) -> Option<(u32, u32)> {
    if png.len() < 24
        || png[..8] != [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]
        || &png[12..16] != b"IHDR"
    {
        return None;
    }
    Some((
        u32::from_be_bytes(png[16..20].try_into().ok()?),
        u32::from_be_bytes(png[20..24].try_into().ok()?),
    ))
}

// ------------------------------------------------------------ decoder -----

struct Bits<'a> {
    d: &'a [u8],
    pos: usize,
    bit: u32,
    cnt: u32,
}
impl Bits<'_> {
    fn need(&mut self, n: u32) -> Option<u32> {
        while self.cnt < n {
            let b = *self.d.get(self.pos)?;
            self.pos += 1;
            self.bit |= u32::from(b) << self.cnt;
            self.cnt += 8;
        }
        let v = self.bit & ((1u32 << n) - 1);
        self.bit >>= n;
        self.cnt -= n;
        Some(v)
    }
}

struct Huff {
    count: [u16; 16],
    symbol: Vec<u16>,
}
impl Huff {
    fn new(lengths: &[u8]) -> Huff {
        let mut count = [0u16; 16];
        for &l in lengths {
            count[l as usize] += 1;
        }
        let mut offs = [0u16; 16];
        for i in 1..15 {
            offs[i + 1] = offs[i] + count[i];
        }
        let mut symbol = vec![0u16; lengths.len()];
        for (sym, &l) in lengths.iter().enumerate() {
            if l != 0 {
                symbol[offs[l as usize] as usize] = sym as u16;
                offs[l as usize] += 1;
            }
        }
        Huff { count, symbol }
    }
    fn decode(&self, b: &mut Bits) -> Option<u16> {
        let (mut code, mut first, mut index) = (0i32, 0i32, 0i32);
        for len in 1..16 {
            code |= b.need(1)? as i32;
            let c = i32::from(self.count[len]);
            if code - c < first {
                return self.symbol.get((index + (code - first)) as usize).copied();
            }
            index += c;
            first += c;
            first <<= 1;
            code <<= 1;
        }
        None
    }
}

const LBASE: [u16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258,
];
const LEXT: [u8; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
const DBASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
const DEXT: [u8; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];

/// Raw DEFLATE (RFC 1951). `limit` bounds the output so a hostile stream cannot exhaust memory.
fn inflate(data: &[u8], limit: usize) -> Option<Vec<u8>> {
    let mut b = Bits {
        d: data,
        pos: 0,
        bit: 0,
        cnt: 0,
    };
    let mut out: Vec<u8> = Vec::new();
    loop {
        let last = b.need(1)?;
        match b.need(2)? {
            0 => {
                b.bit = 0;
                b.cnt = 0;
                let n = u16::from_le_bytes([*b.d.get(b.pos)?, *b.d.get(b.pos + 1)?]) as usize;
                b.pos += 4;
                out.extend_from_slice(b.d.get(b.pos..b.pos + n)?);
                b.pos += n;
            }
            t @ (1 | 2) => {
                let (lit, dist) = if t == 1 {
                    let mut l = [0u8; 288];
                    for (i, v) in l.iter_mut().enumerate() {
                        *v = match i {
                            0..=143 => 8,
                            144..=255 => 9,
                            256..=279 => 7,
                            _ => 8,
                        };
                    }
                    (Huff::new(&l), Huff::new(&[5u8; 30]))
                } else {
                    let nlen = b.need(5)? as usize + 257;
                    let ndist = b.need(5)? as usize + 1;
                    let ncode = b.need(4)? as usize + 4;
                    const ORDER: [usize; 19] = [
                        16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
                    ];
                    let mut cl = [0u8; 19];
                    for &o in ORDER.iter().take(ncode) {
                        cl[o] = b.need(3)? as u8;
                    }
                    let ch = Huff::new(&cl);
                    let mut lens = vec![0u8; nlen + ndist];
                    let mut i = 0;
                    while i < nlen + ndist {
                        let sym = ch.decode(&mut b)?;
                        if sym < 16 {
                            lens[i] = sym as u8;
                            i += 1;
                        } else {
                            let (prev, rep) = match sym {
                                16 => (*lens.get(i.checked_sub(1)?)?, 3 + b.need(2)? as usize),
                                17 => (0, 3 + b.need(3)? as usize),
                                _ => (0, 11 + b.need(7)? as usize),
                            };
                            if i + rep > nlen + ndist {
                                return None;
                            }
                            lens[i..i + rep].fill(prev);
                            i += rep;
                        }
                    }
                    (Huff::new(&lens[..nlen]), Huff::new(&lens[nlen..]))
                };
                loop {
                    let sym = lit.decode(&mut b)? as usize;
                    if sym < 256 {
                        out.push(sym as u8);
                    } else if sym == 256 {
                        break;
                    } else {
                        let s = sym - 257;
                        let len = usize::from(*LBASE.get(s)?)
                            + b.need(u32::from(*LEXT.get(s)?))? as usize;
                        let ds = dist.decode(&mut b)? as usize;
                        let d = usize::from(*DBASE.get(ds)?)
                            + b.need(u32::from(*DEXT.get(ds)?))? as usize;
                        if d > out.len() {
                            return None;
                        }
                        for _ in 0..len {
                            out.push(out[out.len() - d]);
                        }
                    }
                    if out.len() > limit {
                        return None;
                    }
                }
            }
            _ => return None,
        }
        if out.len() > limit {
            return None;
        }
        if last == 1 {
            return Some(out);
        }
    }
}

/// Decode an 8-bit, non-interlaced PNG (grey, grey+alpha, RGB, RGBA) to RGBA8.
/// Anything else returns `None`, and callers fail closed.
pub fn decode_rgba(png: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    let (w, h) = dimensions(png)?;
    if w == 0 || h == 0 || u64::from(w) * u64::from(h) > 64 * 1024 * 1024 {
        return None;
    }
    let (mut i, mut idat, mut ihdr) = (8usize, Vec::new(), None);
    while i + 12 <= png.len() {
        let len = u32::from_be_bytes(png[i..i + 4].try_into().ok()?) as usize;
        let kind = &png[i + 4..i + 8];
        let body = png.get(i + 8..i + 8 + len)?;
        match kind {
            b"IHDR" => ihdr = Some((body[8], body[9], body[12])),
            b"IDAT" => idat.extend_from_slice(body),
            _ => {}
        }
        i += 12 + len;
    }
    let (depth, ctype, interlace) = ihdr?;
    let bpp = match (depth, ctype, interlace) {
        (8, 0, 0) => 1usize,
        (8, 4, 0) => 2,
        (8, 2, 0) => 3,
        (8, 6, 0) => 4,
        _ => return None,
    };
    if idat.len() < 6 {
        return None;
    }
    let stride = w as usize * bpp;
    let raw = inflate(&idat[2..], (stride + 1) * h as usize + 16)?;
    if raw.len() < (stride + 1) * h as usize {
        return None;
    }
    let mut cur = vec![0u8; stride];
    let mut prev = vec![0u8; stride];
    let mut rgba = Vec::with_capacity(w as usize * h as usize * 4);
    for y in 0..h as usize {
        let row = &raw[y * (stride + 1)..(y + 1) * (stride + 1)];
        let f = row[0];
        for x in 0..stride {
            let a = if x >= bpp { cur[x - bpp] } else { 0 };
            let b = prev[x];
            let c = if x >= bpp { prev[x - bpp] } else { 0 };
            let v = row[1 + x];
            cur[x] = match f {
                0 => v,
                1 => v.wrapping_add(a),
                2 => v.wrapping_add(b),
                3 => v.wrapping_add(((u16::from(a) + u16::from(b)) / 2) as u8),
                4 => {
                    let (pa, pb, pc) = (
                        i32::from(b) - i32::from(c),
                        i32::from(a) - i32::from(c),
                        i32::from(a) + i32::from(b) - 2 * i32::from(c),
                    );
                    let (da, db, dc) = (pa.abs(), pb.abs(), pc.abs());
                    let p = if da <= db && da <= dc {
                        a
                    } else if db <= dc {
                        b
                    } else {
                        c
                    };
                    v.wrapping_add(p)
                }
                _ => return None,
            };
        }
        for px in cur.chunks(bpp) {
            match bpp {
                1 => rgba.extend_from_slice(&[px[0], px[0], px[0], 255]),
                2 => rgba.extend_from_slice(&[px[0], px[0], px[0], px[1]]),
                3 => rgba.extend_from_slice(&[px[0], px[1], px[2], 255]),
                _ => rgba.extend_from_slice(px),
            }
        }
        std::mem::swap(&mut cur, &mut prev);
    }
    Some((w, h, rgba))
}

/// Paint `rects` (pixel coordinates, clamped) solid black in an RGBA buffer.
pub fn fill_rects(w: u32, h: u32, rgba: &mut [u8], rects: &[(i64, i64, i64, i64)]) -> u64 {
    let mut painted = 0u64;
    for &(x0, y0, x1, y1) in rects {
        let (x0, x1) = (x0.clamp(0, i64::from(w)), x1.clamp(0, i64::from(w)));
        let (y0, y1) = (y0.clamp(0, i64::from(h)), y1.clamp(0, i64::from(h)));
        for y in y0..y1 {
            for x in x0..x1 {
                let o = (y as usize * w as usize + x as usize) * 4;
                rgba[o..o + 4].copy_from_slice(&[0, 0, 0, 255]);
                painted += 1;
            }
        }
    }
    painted
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_checksums() {
        assert_eq!(crc32(b"IEND"), 0xAE42_6082);
        assert_eq!(adler32(b"Wikipedia"), 0x11E6_0398);
    }

    #[test]
    fn encodes_a_decodable_stored_png() {
        let (w, h) = (3u32, 2u32);
        let px: Vec<u8> = (0..w * h * 4).map(|i| i as u8).collect();
        let png = encode_rgba(w, h, &px).unwrap();
        assert_eq!(dimensions(&png), Some((3, 2)));
        // walk chunks, inflate the stored blocks, compare pixels
        let mut i = 8;
        let mut idat = vec![];
        while i < png.len() {
            let len = u32::from_be_bytes(png[i..i + 4].try_into().unwrap()) as usize;
            let kind = &png[i + 4..i + 8];
            let body = &png[i + 8..i + 8 + len];
            let crc = u32::from_be_bytes(png[i + 8 + len..i + 12 + len].try_into().unwrap());
            assert_eq!(crc, crc32(&png[i + 4..i + 8 + len]));
            if kind == b"IDAT" {
                idat.extend_from_slice(body);
            }
            i += 12 + len;
        }
        let mut raw = vec![];
        let mut j = 2;
        loop {
            let last = idat[j] & 1 == 1;
            let n = u16::from_le_bytes([idat[j + 1], idat[j + 2]]) as usize;
            raw.extend_from_slice(&idat[j + 5..j + 5 + n]);
            j += 5 + n;
            if last {
                break;
            }
        }
        assert_eq!(
            adler32(&raw),
            u32::from_be_bytes(idat[j..j + 4].try_into().unwrap())
        );
        let rows: Vec<u8> = raw.chunks(13).flat_map(|r| r[1..].to_vec()).collect();
        assert_eq!(rows, px);
    }

    #[test]
    fn decoder_round_trips_encoder_and_fills_rects() {
        let (w, h) = (40u32, 30u32);
        let px: Vec<u8> = (0..w * h * 4).map(|i| (i % 251) as u8).collect();
        let png = encode_rgba(w, h, &px).unwrap();
        let (dw, dh, mut back) = decode_rgba(&png).unwrap();
        assert_eq!((dw, dh), (w, h));
        assert_eq!(back, px);
        let n = fill_rects(
            w,
            h,
            &mut back,
            &[(5, 5, 10, 8), (-4, -4, 2, 2), (38, 28, 99, 99)],
        );
        assert_eq!(n, 15 + 4 + 4);
        assert_eq!(
            &back[(5 * 40 + 5) * 4..(5 * 40 + 5) * 4 + 4],
            &[0, 0, 0, 255]
        );
        assert_ne!(
            &back[(5 * 40 + 10) * 4..(5 * 40 + 10) * 4 + 4],
            &[0, 0, 0, 255]
        );
        assert!(decode_rgba(b"not a png at all, no header").is_none());
    }

    #[test]
    fn rejects_bad_sizes_and_spans_blocks() {
        assert!(encode_rgba(0, 1, &[]).is_none());
        assert!(encode_rgba(1, 1, &[0; 3]).is_none());
        let big = vec![7u8; 200 * 200 * 4];
        let png = encode_rgba(200, 200, &big).unwrap();
        assert_eq!(dimensions(&png), Some((200, 200)));
        assert!(dimensions(b"nope").is_none());
    }
}
