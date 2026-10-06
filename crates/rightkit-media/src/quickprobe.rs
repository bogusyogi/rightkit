//! Pure-Rust container probe (no external tools): dimensions and exact duration for the
//! formats providers return (PNG, JPEG, GIF, WebP, WAV, FLAC, MP4/MOV, MP3). Unknown
//! formats yield nulls. Donor: GenRight `genright-gen/src/media.rs`.

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Probe {
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub duration_s: Option<f64>,
}

impl Probe {
    fn dims(w: u32, h: u32) -> Self {
        Self {
            width: Some(w),
            height: Some(h),
            duration_s: None,
        }
    }
    fn dur(d: f64) -> Self {
        Self {
            width: None,
            height: None,
            duration_s: Some(d),
        }
    }
}

/// Probe `data` by magic bytes (the declared mime type is not trusted).
pub fn probe(data: &[u8]) -> Probe {
    let found = if data.starts_with(&[0x89, b'P', b'N', b'G']) {
        png(data)
    } else if data.starts_with(&[0xFF, 0xD8]) {
        jpeg(data)
    } else if data.starts_with(b"GIF8") {
        gif(data)
    } else if data.len() >= 12 && &data[0..4] == b"RIFF" && &data[8..12] == b"WEBP" {
        webp(data)
    } else if data.len() >= 12 && &data[0..4] == b"RIFF" && &data[8..12] == b"WAVE" {
        wav(data)
    } else if data.starts_with(b"fLaC") {
        flac(data)
    } else if data.len() >= 12 && &data[4..8] == b"ftyp" {
        mp4(data)
    } else if data.starts_with(b"ID3") || mp3_sync(data, 0) {
        mp3(data)
    } else {
        None
    };
    found.unwrap_or_default()
}

fn be32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(b.get(at..at + 4)?.try_into().ok()?))
}
fn be64(b: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_be_bytes(b.get(at..at + 8)?.try_into().ok()?))
}
fn be16(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes(b.get(at..at + 2)?.try_into().ok()?))
}
fn le32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}
fn le16(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?))
}

fn png(b: &[u8]) -> Option<Probe> {
    if b.len() >= 24 && &b[12..16] == b"IHDR" {
        return Some(Probe::dims(be32(b, 16)?, be32(b, 20)?));
    }
    None
}

fn gif(b: &[u8]) -> Option<Probe> {
    Some(Probe::dims(le16(b, 6)? as u32, le16(b, 8)? as u32))
}

fn jpeg(b: &[u8]) -> Option<Probe> {
    let mut i = 2;
    while i + 4 <= b.len() {
        if b[i] != 0xFF {
            i += 1;
            continue;
        }
        let marker = b[i + 1];
        if marker == 0xFF {
            i += 1;
            continue;
        }
        if marker == 0xD8 || marker == 0x01 || (0xD0..=0xD7).contains(&marker) {
            i += 2;
            continue;
        }
        let len = be16(b, i + 2)? as usize;
        let sof = (0xC0..=0xCF).contains(&marker) && ![0xC4, 0xC8, 0xCC].contains(&marker);
        if sof {
            return Some(Probe::dims(be16(b, i + 7)? as u32, be16(b, i + 5)? as u32));
        }
        if marker == 0xDA {
            return None;
        }
        i += 2 + len;
    }
    None
}

fn webp(b: &[u8]) -> Option<Probe> {
    match b.get(12..16)? {
        b"VP8 " => {
            if b.get(23..26)? != [0x9d, 0x01, 0x2a] {
                return None;
            }
            Some(Probe::dims(
                (le16(b, 26)? & 0x3fff) as u32,
                (le16(b, 28)? & 0x3fff) as u32,
            ))
        }
        b"VP8L" => {
            if *b.get(20)? != 0x2f {
                return None;
            }
            let bits = le32(b, 21)?;
            Some(Probe::dims(
                (bits & 0x3fff) + 1,
                ((bits >> 14) & 0x3fff) + 1,
            ))
        }
        b"VP8X" => {
            let w = u32::from_le_bytes([*b.get(24)?, *b.get(25)?, *b.get(26)?, 0]) + 1;
            let h = u32::from_le_bytes([*b.get(27)?, *b.get(28)?, *b.get(29)?, 0]) + 1;
            Some(Probe::dims(w, h))
        }
        _ => None,
    }
}

fn wav(b: &[u8]) -> Option<Probe> {
    let mut i = 12;
    let mut byte_rate: Option<u32> = None;
    while i + 8 <= b.len() {
        let id = &b[i..i + 4];
        let size = le32(b, i + 4)? as usize;
        let body = i + 8;
        if id == b"fmt " {
            byte_rate = Some(le32(b, body + 8)?);
        } else if id == b"data" {
            let rate = byte_rate.filter(|r| *r > 0)?;
            let avail = b.len().saturating_sub(body);
            // Streamed files carry 0 or 0xFFFFFFFF; trust what is present.
            let n = if size == 0 || size > avail {
                avail
            } else {
                size
            };
            return Some(Probe::dur(n as f64 / rate as f64));
        }
        i = body.checked_add(size)?.checked_add(size & 1)?;
    }
    None
}

fn flac(b: &[u8]) -> Option<Probe> {
    // First block must be STREAMINFO (type 0), 34 bytes.
    if *b.get(4)? & 0x7f != 0 {
        return None;
    }
    let s = b.get(8..42)?;
    let rate = ((s[10] as u32) << 12) | ((s[11] as u32) << 4) | ((s[12] as u32) >> 4);
    let total = (((s[13] & 0x0f) as u64) << 32)
        | ((s[14] as u64) << 24)
        | ((s[15] as u64) << 16)
        | ((s[16] as u64) << 8)
        | s[17] as u64;
    if rate == 0 || total == 0 {
        return None;
    }
    Some(Probe::dur(total as f64 / rate as f64))
}

/// Iterate boxes in `b`, yielding `(type, payload)`.
fn boxes(b: &[u8]) -> Vec<([u8; 4], &[u8])> {
    let mut out = Vec::new();
    let mut i = 0;
    while i + 8 <= b.len() {
        let size32 = be32(b, i).unwrap_or(0) as u64;
        let ty: [u8; 4] = b[i + 4..i + 8].try_into().unwrap_or([0; 4]);
        let (hdr, size) = match size32 {
            0 => (8usize, (b.len() - i) as u64),
            1 => match be64(b, i + 8) {
                Some(s) => (16usize, s),
                None => break,
            },
            s => (8usize, s),
        };
        if size < hdr as u64 {
            break;
        }
        let end = (i as u64).saturating_add(size).min(b.len() as u64) as usize;
        if i + hdr > end {
            break;
        }
        out.push((ty, &b[i + hdr..end]));
        i = end;
    }
    out
}

fn mp4(b: &[u8]) -> Option<Probe> {
    let top = boxes(b);
    let moov = top.iter().find(|(t, _)| t == b"moov")?.1;
    let mut p = Probe::default();
    for (ty, body) in boxes(moov) {
        if &ty == b"mvhd" {
            let version = *body.first()?;
            let (scale, dur) = if version == 1 {
                (be32(body, 20)? as f64, be64(body, 24)? as f64)
            } else {
                (be32(body, 12)? as f64, be32(body, 16)? as f64)
            };
            if scale > 0.0 && dur > 0.0 {
                p.duration_s = Some(dur / scale);
            }
        } else if &ty == b"trak" && p.width.is_none() {
            for (tt, tb) in boxes(body) {
                if &tt == b"tkhd" {
                    let off = if *tb.first()? == 1 { 88 } else { 76 };
                    let w = be32(tb, off)? >> 16;
                    let h = be32(tb, off + 4)? >> 16;
                    if w > 0 && h > 0 {
                        p.width = Some(w);
                        p.height = Some(h);
                    }
                }
            }
        }
    }
    Some(p)
}

fn mp3_sync(b: &[u8], at: usize) -> bool {
    matches!((b.get(at), b.get(at + 1)), (Some(0xFF), Some(x)) if x & 0xE0 == 0xE0)
}

/// `(frame_len_bytes, samples_per_frame, sample_rate)` for the header at `at`.
fn mp3_frame(b: &[u8], at: usize) -> Option<(usize, u32, u32)> {
    const BR_V1: [[u32; 15]; 3] = [
        [
            0, 32, 64, 96, 128, 160, 192, 224, 256, 288, 320, 352, 384, 416, 448,
        ],
        [
            0, 32, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384,
        ],
        [
            0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320,
        ],
    ];
    const BR_V2: [[u32; 15]; 2] = [
        [
            0, 32, 48, 56, 64, 80, 96, 112, 128, 144, 160, 176, 192, 224, 256,
        ],
        [0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160],
    ];
    let h = be32(b, at)?;
    if h >> 21 != 0x7FF {
        return None;
    }
    let version = (h >> 19) & 3; // 0 = 2.5, 2 = 2, 3 = 1
    let layer = (h >> 17) & 3; // 1 = III, 2 = II, 3 = I
    let br_idx = ((h >> 12) & 0xF) as usize;
    let sr_idx = ((h >> 10) & 3) as usize;
    let pad = (h >> 9) & 1;
    if version == 1 || layer == 0 || br_idx == 0 || br_idx == 15 || sr_idx == 3 {
        return None;
    }
    let rate = [
        [11025, 12000, 8000],
        [0, 0, 0],
        [22050, 24000, 16000],
        [44100, 48000, 32000],
    ][version as usize][sr_idx];
    let v1 = version == 3;
    let kbps = if v1 {
        BR_V1[(3 - layer) as usize][br_idx]
    } else if layer == 3 {
        BR_V2[0][br_idx]
    } else {
        BR_V2[1][br_idx]
    };
    let bitrate = kbps * 1000;
    let (len, samples) = if layer == 3 {
        (((12 * bitrate / rate + pad) * 4) as usize, 384)
    } else if layer == 2 || v1 {
        ((144 * bitrate / rate + pad) as usize, 1152)
    } else {
        ((72 * bitrate / rate + pad) as usize, 576)
    };
    if len < 4 {
        return None;
    }
    Some((len, samples, rate))
}

fn mp3(b: &[u8]) -> Option<Probe> {
    let mut i = 0;
    if b.starts_with(b"ID3") && b.len() >= 10 {
        let sz = b[6..10]
            .iter()
            .fold(0usize, |a, x| (a << 7) | (*x as usize & 0x7f));
        i = 10 + sz;
    }
    // Find the first frame header.
    while i + 4 <= b.len() && mp3_frame(b, i).is_none() {
        i += 1;
    }
    let mut total = 0f64;
    let mut frames = 0u32;
    while let Some((len, samples, rate)) = mp3_frame(b, i) {
        total += samples as f64 / rate as f64;
        frames += 1;
        i += len;
        if i > b.len() {
            break;
        }
    }
    if frames == 0 {
        return None;
    }
    Some(Probe::dur(total))
}

/// Probe a file on disk. Reads the whole file (MP4 `moov` may trail the media data);
/// callers with very large inputs should prefer the FFmpeg-backed
/// [`crate::probe::probe_media`].
pub fn probe_path(path: &std::path::Path) -> std::io::Result<Probe> {
    Ok(probe(&std::fs::read(path)?))
}
