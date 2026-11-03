//! the host's own media probe (pure Rust, no external tools): dimensions and
//! duration for the formats providers return. Unknown formats yield nulls.

use std::io::{Read, Seek, SeekFrom};

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

/// Leading bytes read from a file for `probe_prefix`.
pub const PROBE_PREFIX_BYTES: usize = 1 << 20;

/// Probe a file from its first bytes (`prefix`) when its full length is `total_len`.
/// Dimensions come from headers inside the prefix, so an MP4 whose `moov` box sits
/// past the prefix yields nulls here; use [`probe_media`] for a file on disk, which
/// seeks to `moov`. FLAC and MP4 durations come from headers and stay
/// exact. WAV and MP3 durations sum the bytes present, so they are dropped when the
/// prefix is not the whole file.
pub fn probe_prefix(prefix: &[u8], total_len: u64) -> Probe {
    let mut p = probe(prefix);
    let whole = prefix.len() as u64 >= total_len;
    let sums_bytes =
        prefix.starts_with(b"RIFF") || prefix.starts_with(b"ID3") || mp3_sync(prefix, 0);
    if !whole && sums_bytes {
        p.duration_s = None;
    }
    p
}

/// Largest file [`probe_file`] reads whole (WAV and MP3 durations sum the bytes present).
pub const PROBE_FILE_MAX_BYTES: u64 = 512 << 20;
/// Largest `moov` box [`probe_file`] reads from an MP4/MOV.
const PROBE_MOOV_MAX_BYTES: u64 = 64 << 20;

fn read_up_to<R: Read>(f: &mut R, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut got = 0;
    while got < buf.len() {
        match f.read(&mut buf[got..])? {
            0 => break,
            n => got += n,
        }
    }
    Ok(got)
}

/// Probe a local file for its real dimensions and duration, which price a request whose billed
/// seconds come from this media. An MP4/MOV is read box by box, so only its `ftyp` and `moov`
/// boxes are loaded wherever the `moov` sits; any other format is read whole up to
/// [`PROBE_FILE_MAX_BYTES`]. A format or file the probe cannot read yields nulls, never a guess.
pub fn probe_file(path: &std::path::Path) -> std::io::Result<Probe> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path)?;
    let len = f.metadata()?.len();
    let mut head = [0u8; 12];
    let n = read_up_to(&mut f, &mut head)?;
    if n >= 12 && &head[4..8] == b"ftyp" {
        return mp4_from_reader(&mut f, len);
    }
    f.seek(SeekFrom::Start(0))?;
    if len <= PROBE_FILE_MAX_BYTES {
        let mut all = Vec::with_capacity(len as usize);
        f.read_to_end(&mut all)?;
        return Ok(probe(&all));
    }
    let mut prefix = vec![0u8; PROBE_PREFIX_BYTES];
    let got = read_up_to(&mut f, &mut prefix)?;
    prefix.truncate(got);
    Ok(probe_prefix(&prefix, len))
}

/// Walk the top-level boxes of an MP4/MOV of `len` bytes read from `r`, by size, and join the
/// `ftyp` and `moov` boxes wherever they sit. Every other box is skipped by seeking past its
/// payload, `mdat` included, so media data is never read. A 64-bit `largesize` header and a
/// size of 0 (the box runs to end of file) are both honored.
fn mp4_from_reader<R: Read + Seek>(r: &mut R, len: u64) -> std::io::Result<Probe> {
    let mut joined: Vec<u8> = Vec::new();
    let mut at = 0u64;
    while at + 8 <= len {
        r.seek(SeekFrom::Start(at))?;
        let mut h = [0u8; 16];
        let got = read_up_to(r, &mut h)?;
        if got < 8 {
            break;
        }
        let size32 = u64::from(u32::from_be_bytes([h[0], h[1], h[2], h[3]]));
        let size = match size32 {
            0 => len - at,
            1 if got >= 16 => {
                u64::from_be_bytes([h[8], h[9], h[10], h[11], h[12], h[13], h[14], h[15]])
            }
            1 => break,
            s => s,
        };
        if size < 8 || (size32 == 1 && size < 16) {
            break;
        }
        let end = at.saturating_add(size).min(len);
        if &h[4..8] == b"ftyp" || &h[4..8] == b"moov" {
            if end - at > PROBE_MOOV_MAX_BYTES {
                break;
            }
            r.seek(SeekFrom::Start(at))?;
            let mut boxed = vec![0u8; (end - at) as usize];
            r.read_exact(&mut boxed)?;
            joined.extend_from_slice(&boxed);
        }
        at = end;
    }
    Ok(probe(&joined))
}

/// Probe a file whose leading bytes are already in memory (`prefix`, of a file `total_len`
/// long). An MP4/MOV is re-read from disk through [`probe_file`], so a `moov` box past the
/// prefix is still found; any other format is probed from `prefix` by [`probe_prefix`].
pub fn probe_media(
    path: &std::path::Path,
    prefix: &[u8],
    total_len: u64,
) -> std::io::Result<Probe> {
    if prefix.len() >= 12 && &prefix[4..8] == b"ftyp" {
        return probe_file(path);
    }
    Ok(probe_prefix(prefix, total_len))
}

/// Media type of `data` by magic bytes, for exactly the formats [`probe`] reads (PNG, JPEG, GIF,
/// WebP, WAV, FLAC, MP4/MOV/M4A, MP3). `None` for anything else; the declared extension is never
/// trusted. Used by `asset.import` to refuse unsupported files.
pub fn sniff_mime(data: &[u8]) -> Option<&'static str> {
    if data.starts_with(&[0x89, b'P', b'N', b'G']) {
        Some("image/png")
    } else if data.starts_with(&[0xFF, 0xD8]) {
        Some("image/jpeg")
    } else if data.starts_with(b"GIF8") {
        Some("image/gif")
    } else if data.len() >= 12 && &data[0..4] == b"RIFF" && &data[8..12] == b"WEBP" {
        Some("image/webp")
    } else if data.len() >= 12 && &data[0..4] == b"RIFF" && &data[8..12] == b"WAVE" {
        Some("audio/wav")
    } else if data.starts_with(b"fLaC") {
        Some("audio/flac")
    } else if data.len() >= 12 && &data[4..8] == b"ftyp" {
        Some(match &data[8..12] {
            b"qt  " => "video/quicktime",
            b"M4A " | b"M4B " => "audio/mp4",
            _ => "video/mp4",
        })
    } else if data.starts_with(b"ID3") || (mp3_sync(data, 0) && mp3(data).is_some()) {
        Some("audio/mpeg")
    } else {
        None
    }
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
