//! Pure-Rust container probe (no external tools): dimensions, exact duration, file size,
//! codec and (GIF) frame count for the formats providers return (PNG, JPEG, GIF, WebP, WAV,
//! FLAC, MP4/MOV, MP3). Unknown formats yield nulls (and the size). Donor: GenRight
//! `genright-gen/src/media.rs`.
//!
//! A "slide count" is not a property of a media container; the nearest is `frame_count`
//! (GIF image frames). Deck formats (PDF, PPTX) are not probed here.

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Probe {
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub duration_s: Option<f64>,
    /// Byte length of the probed input. Always set by [`probe`] and [`probe_path`].
    pub size_bytes: u64,
    /// Primary codec or format name from the container header (video track if there is
    /// one): `png`, `jpeg`, `gif`, `webp`, `pcm_s16le`, `flac`, `mp3`, `h264`, `hevc`,
    /// `aac`, ... An unrecognised MP4/MOV sample entry is `unknown`.
    pub codec: Option<&'static str>,
    /// Audio codec of an MP4/MOV that also has a video track; `None` otherwise.
    pub audio_codec: Option<&'static str>,
    /// Number of image frames (GIF only).
    pub frame_count: Option<u32>,
}

impl Probe {
    fn dims(w: u32, h: u32) -> Self {
        Self {
            width: Some(w),
            height: Some(h),
            ..Self::default()
        }
    }
    fn dur(d: f64) -> Self {
        Self {
            duration_s: Some(d),
            ..Self::default()
        }
    }
    fn with_codec(mut self, codec: &'static str) -> Self {
        self.codec = Some(codec);
        self
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
    let mut out = found.unwrap_or_default();
    out.size_bytes = data.len() as u64;
    out
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
        return Some(Probe::dims(be32(b, 16)?, be32(b, 20)?).with_codec("png"));
    }
    None
}

fn gif(b: &[u8]) -> Option<Probe> {
    let mut p = Probe::dims(le16(b, 6)? as u32, le16(b, 8)? as u32).with_codec("gif");
    p.frame_count = gif_frames(b);
    Some(p)
}

/// Count image descriptors (0x2C) by walking the block structure.
fn gif_frames(b: &[u8]) -> Option<u32> {
    let flags = *b.get(10)?;
    let mut i = 13usize;
    if flags & 0x80 != 0 {
        i += 3 * (1usize << ((flags & 7) + 1));
    }
    let mut frames = 0u32;
    loop {
        match *b.get(i)? {
            0x3B => break,
            0x21 => {
                i += 2;
                i = skip_sub_blocks(b, i)?;
            }
            0x2C => {
                frames += 1;
                let f = *b.get(i + 9)?;
                i += 10;
                if f & 0x80 != 0 {
                    i += 3 * (1usize << ((f & 7) + 1));
                }
                i += 1; // LZW minimum code size
                i = skip_sub_blocks(b, i)?;
            }
            _ => return if frames > 0 { Some(frames) } else { None },
        }
    }
    Some(frames)
}

fn skip_sub_blocks(b: &[u8], mut i: usize) -> Option<usize> {
    loop {
        let n = *b.get(i)? as usize;
        i += 1;
        if n == 0 {
            return Some(i);
        }
        i = i.checked_add(n)?;
    }
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
            return Some(
                Probe::dims(be16(b, i + 7)? as u32, be16(b, i + 5)? as u32).with_codec("jpeg"),
            );
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
            Some(
                Probe::dims(
                    (le16(b, 26)? & 0x3fff) as u32,
                    (le16(b, 28)? & 0x3fff) as u32,
                )
                .with_codec("webp"),
            )
        }
        b"VP8L" => {
            if *b.get(20)? != 0x2f {
                return None;
            }
            let bits = le32(b, 21)?;
            Some(
                Probe::dims((bits & 0x3fff) + 1, ((bits >> 14) & 0x3fff) + 1).with_codec("webp"),
            )
        }
        b"VP8X" => {
            let w = u32::from_le_bytes([*b.get(24)?, *b.get(25)?, *b.get(26)?, 0]) + 1;
            let h = u32::from_le_bytes([*b.get(27)?, *b.get(28)?, *b.get(29)?, 0]) + 1;
            Some(Probe::dims(w, h).with_codec("webp"))
        }
        _ => None,
    }
}

fn wav(b: &[u8]) -> Option<Probe> {
    let mut i = 12;
    let mut byte_rate: Option<u32> = None;
    let mut codec: &'static str = "pcm";
    while i + 8 <= b.len() {
        let id = &b[i..i + 4];
        let size = le32(b, i + 4)? as usize;
        let body = i + 8;
        if id == b"fmt " {
            byte_rate = Some(le32(b, body + 8)?);
            codec = wav_codec(le16(b, body)?, le16(b, body + 14)?);
        } else if id == b"data" {
            let rate = byte_rate.filter(|r| *r > 0)?;
            let avail = b.len().saturating_sub(body);
            // Streamed files carry 0 or 0xFFFFFFFF; trust what is present.
            let n = if size == 0 || size > avail {
                avail
            } else {
                size
            };
            return Some(Probe::dur(n as f64 / rate as f64).with_codec(codec));
        }
        i = body.checked_add(size)?.checked_add(size & 1)?;
    }
    None
}

fn wav_codec(tag: u16, bits: u16) -> &'static str {
    match (tag, bits) {
        (1, 8) => "pcm_u8",
        (1, 16) => "pcm_s16le",
        (1, 24) => "pcm_s24le",
        (1, 32) => "pcm_s32le",
        (3, 32) => "pcm_f32le",
        (3, 64) => "pcm_f64le",
        (1, _) | (3, _) => "pcm",
        (2, _) => "adpcm_ms",
        (6, _) => "pcm_alaw",
        (7, _) => "pcm_mulaw",
        (0x55, _) => "mp3",
        (0xFFFE, _) => "pcm_extensible",
        _ => "unknown",
    }
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
    Some(Probe::dur(total as f64 / rate as f64).with_codec("flac"))
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
    let mut video: Option<&'static str> = None;
    let mut audio: Option<&'static str> = None;
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
        } else if &ty == b"trak" {
            if let Some((kind, name)) = mp4_track_codec(body) {
                match &kind {
                    b"vide" => video = video.or(Some(name)),
                    b"soun" => audio = audio.or(Some(name)),
                    _ => {}
                }
            }
            if p.width.is_some() {
                continue;
            }
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
    p.codec = video.or(audio);
    if video.is_some() {
        p.audio_codec = audio;
    }
    Some(p)
}

/// `(handler type, codec name)` of one `trak`: `mdia/hdlr` and the first `stsd` entry.
fn mp4_track_codec(trak: &[u8]) -> Option<([u8; 4], &'static str)> {
    let mdia = boxes(trak).into_iter().find(|(t, _)| t == b"mdia")?.1;
    let mut kind = None;
    let mut fourcc = None;
    for (t, body) in boxes(mdia) {
        if &t == b"hdlr" {
            kind = body.get(8..12).and_then(|x| <[u8; 4]>::try_from(x).ok());
        } else if &t == b"minf" {
            let stbl = boxes(body).into_iter().find(|(t, _)| t == b"stbl")?.1;
            let stsd = boxes(stbl).into_iter().find(|(t, _)| t == b"stsd")?.1;
            fourcc = stsd.get(12..16).and_then(|x| <[u8; 4]>::try_from(x).ok());
        }
    }
    Some((kind?, codec_name(fourcc?)))
}

fn codec_name(f: [u8; 4]) -> &'static str {
    match &f {
        b"avc1" | b"avc3" => "h264",
        b"hvc1" | b"hev1" => "hevc",
        b"av01" => "av1",
        b"vp09" => "vp9",
        b"mp4v" => "mpeg4",
        b"jpeg" | b"mjpa" => "mjpeg",
        b"apch" | b"apcn" | b"apcs" | b"apco" | b"ap4h" => "prores",
        b"mp4a" => "aac",
        b"ac-3" => "ac3",
        b"ec-3" => "eac3",
        b"alac" => "alac",
        b"Opus" => "opus",
        b"fLaC" => "flac",
        b".mp3" => "mp3",
        b"sowt" | b"twos" | b"lpcm" => "pcm",
        _ => "unknown",
    }
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
    Some(Probe::dur(total).with_codec("mp3"))
}

/// Probe a file on disk. Reads the whole file (MP4 `moov` may trail the media data);
/// callers with very large inputs should prefer the FFmpeg-backed
/// [`crate::probe::probe_media`].
pub fn probe_path(path: &std::path::Path) -> std::io::Result<Probe> {
    Ok(probe(&std::fs::read(path)?))
}

#[cfg(test)]
mod size_codec_tests {
    use super::*;

    #[test]
    fn png_reports_size_and_codec() {
        let mut d = vec![0x89, b'P', b'N', b'G', 13, 10, 26, 10, 0, 0, 0, 13];
        d.extend_from_slice(b"IHDR");
        d.extend_from_slice(&7u32.to_be_bytes());
        d.extend_from_slice(&5u32.to_be_bytes());
        let p = probe(&d);
        assert_eq!((p.width, p.height), (Some(7), Some(5)));
        assert_eq!(p.codec, Some("png"));
        assert_eq!(p.size_bytes, d.len() as u64);
    }

    #[test]
    fn unknown_input_keeps_size_only() {
        let p = probe(b"not media at all");
        assert_eq!(p.size_bytes, 16);
        assert_eq!(p.codec, None);
    }

    #[test]
    fn gif_counts_frames() {
        let mut d = b"GIF89a".to_vec();
        d.extend_from_slice(&[2, 0, 2, 0, 0, 0, 0]);
        for _ in 0..3 {
            d.extend_from_slice(&[0x2C, 0, 0, 0, 0, 2, 0, 2, 0, 0, 2, 2, 0x44, 0x01, 0]);
        }
        d.push(0x3B);
        let p = probe(&d);
        assert_eq!(p.frame_count, Some(3));
        assert_eq!(p.codec, Some("gif"));
    }

    #[test]
    fn wav_codec_from_format_tag() {
        assert_eq!(wav_codec(1, 16), "pcm_s16le");
        assert_eq!(wav_codec(3, 32), "pcm_f32le");
    }
}
