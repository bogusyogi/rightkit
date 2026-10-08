//! Recoverable media. A take whose helper dies leaves `clean.mp4` behind. The
//! helpers write a fragmented MP4 (a `moof`/`mdat` pair roughly every second), so
//! the file plays up to the last whole fragment. This module checks that, with no
//! decoder: it walks the box structure, counts the samples of every complete
//! fragment, and sums their durations. A fragment whose `mdat` was cut short is
//! not counted, because its samples are not all on disk.
//!
//! The probe is structural. It proves the container is intact and says how many
//! video samples and how much time it holds. Decoding is proved separately by the
//! live kill test, which runs the file through `ffprobe`.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// What a media file holds: whole fragments only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MediaProbe {
    /// Video samples (frames) in complete fragments.
    pub samples: u64,
    /// Their summed duration, milliseconds.
    pub duration_ms: u64,
    /// Complete `moof`/`mdat` pairs.
    pub fragments: u64,
    /// Bytes after the last complete box, from a write the kill interrupted.
    pub torn_tail_bytes: u64,
}

/// What a non-complete take's media still plays, recorded when the take is sealed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecoveredMedia {
    /// Video frames in the playable part of `clean.mp4`.
    pub samples: u64,
    /// Their duration, milliseconds.
    pub duration_ms: u64,
    /// Frames the bundle's clock table accepted. Media may hold fewer: the last
    /// fragment, up to one interval, dies with the helper.
    pub accepted_frames: u64,
}

const MAX_HEADER_BOX: u64 = 64 * 1024 * 1024;

struct BoxHead {
    kind: [u8; 4],
    /// Offset of the box payload.
    body: u64,
    /// Offset one past the box, from its declared size.
    end: u64,
}

fn read_head(file: &mut File, at: u64, file_len: u64) -> Result<Option<BoxHead>, String> {
    if at + 8 > file_len {
        return Ok(None);
    }
    file.seek(SeekFrom::Start(at)).map_err(|e| e.to_string())?;
    let mut head = [0u8; 8];
    file.read_exact(&mut head).map_err(|e| e.to_string())?;
    let size = u32::from_be_bytes([head[0], head[1], head[2], head[3]]) as u64;
    let kind = [head[4], head[5], head[6], head[7]];
    let (body, end) = match size {
        0 => (at + 8, file_len),
        1 => {
            if at + 16 > file_len {
                return Ok(None);
            }
            let mut large = [0u8; 8];
            file.read_exact(&mut large).map_err(|e| e.to_string())?;
            (at + 16, u64::from_be_bytes(large))
        }
        n if n < 8 => return Err(format!("box at {at} has size {n}")),
        n => (at + 8, at + n),
    };
    if end < body {
        return Err(format!("box at {at} ends before its payload"));
    }
    Ok(Some(BoxHead { kind, body, end }))
}

fn read_range(file: &mut File, from: u64, to: u64) -> Result<Vec<u8>, String> {
    let mut buf = vec![0u8; (to - from) as usize];
    file.seek(SeekFrom::Start(from))
        .map_err(|e| e.to_string())?;
    file.read_exact(&mut buf).map_err(|e| e.to_string())?;
    Ok(buf)
}

/// Child boxes of `data`, as `(kind, payload)`. A child that overruns `data` ends the walk.
fn children(data: &[u8]) -> Vec<([u8; 4], &[u8])> {
    let mut out = Vec::new();
    let mut at = 0usize;
    while at + 8 <= data.len() {
        let size =
            u32::from_be_bytes([data[at], data[at + 1], data[at + 2], data[at + 3]]) as usize;
        let kind = [data[at + 4], data[at + 5], data[at + 6], data[at + 7]];
        if size < 8 || at + size > data.len() {
            break;
        }
        out.push((kind, &data[at + 8..at + size]));
        at += size;
    }
    out
}

fn be32(data: &[u8], at: usize) -> Option<u32> {
    data.get(at..at + 4)
        .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
}

#[derive(Default)]
struct Track {
    id: u32,
    timescale: u32,
    video: bool,
    /// `trex` default sample duration.
    default_duration: u32,
    /// Samples and ticks a non-fragmented sample table holds.
    table_samples: u64,
    table_ticks: u64,
}

fn parse_moov(moov: &[u8]) -> Vec<Track> {
    let mut tracks: Vec<Track> = Vec::new();
    let mut trex: Vec<(u32, u32)> = Vec::new();
    for (kind, body) in children(moov) {
        match &kind {
            b"trak" => {
                let mut track = Track::default();
                for (k, b) in children(body) {
                    match &k {
                        b"tkhd" => {
                            let version = b.first().copied().unwrap_or(0);
                            let at = if version == 1 { 20 } else { 12 };
                            track.id = be32(b, at).unwrap_or(0);
                        }
                        b"mdia" => parse_mdia(b, &mut track),
                        _ => {}
                    }
                }
                tracks.push(track);
            }
            b"mvex" => {
                for (k, b) in children(body) {
                    if &k == b"trex" {
                        if let (Some(id), Some(duration)) = (be32(b, 4), be32(b, 12)) {
                            trex.push((id, duration));
                        }
                    }
                }
            }
            _ => {}
        }
    }
    for track in &mut tracks {
        if let Some((_, duration)) = trex.iter().find(|(id, _)| *id == track.id) {
            track.default_duration = *duration;
        }
    }
    tracks
}

fn parse_mdia(mdia: &[u8], track: &mut Track) {
    for (k, b) in children(mdia) {
        match &k {
            b"mdhd" => {
                let version = b.first().copied().unwrap_or(0);
                let at = if version == 1 { 20 } else { 12 };
                track.timescale = be32(b, at).unwrap_or(0);
            }
            b"hdlr" => track.video = b.get(8..12) == Some(b"vide"),
            b"minf" => {
                for (k2, b2) in children(b) {
                    if &k2 == b"stbl" {
                        for (k3, b3) in children(b2) {
                            if &k3 == b"stts" {
                                let entries = be32(b3, 4).unwrap_or(0) as usize;
                                for i in 0..entries {
                                    let at = 8 + i * 8;
                                    if let (Some(count), Some(delta)) =
                                        (be32(b3, at), be32(b3, at + 4))
                                    {
                                        track.table_samples += count as u64;
                                        track.table_ticks += count as u64 * delta as u64;
                                    }
                                }
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

/// `(samples, ticks)` for one track fragment of `moof`.
fn parse_traf(traf: &[u8], tracks: &[Track]) -> Option<(u32, u64, u64)> {
    let mut track_id = 0u32;
    let mut default_duration: Option<u32> = None;
    let mut samples = 0u64;
    let mut ticks = 0u64;
    for (kind, body) in children(traf) {
        match &kind {
            b"tfhd" => {
                let flags = be32(body, 0)? & 0x00ff_ffff;
                track_id = be32(body, 4)?;
                let mut at = 8usize;
                if flags & 0x1 != 0 {
                    at += 8;
                }
                if flags & 0x2 != 0 {
                    at += 4;
                }
                if flags & 0x8 != 0 {
                    default_duration = be32(body, at);
                }
            }
            b"trun" => {
                let flags = be32(body, 0)? & 0x00ff_ffff;
                let count = be32(body, 4)? as usize;
                let mut at = 8usize;
                if flags & 0x1 != 0 {
                    at += 4;
                }
                if flags & 0x4 != 0 {
                    at += 4;
                }
                let per_sample = [0x100, 0x200, 0x400, 0x800]
                    .iter()
                    .filter(|bit| flags & **bit != 0)
                    .count()
                    * 4;
                let fallback = default_duration
                    .or_else(|| {
                        tracks
                            .iter()
                            .find(|t| t.id == track_id)
                            .map(|t| t.default_duration)
                    })
                    .unwrap_or(0) as u64;
                for i in 0..count {
                    let base = at + i * per_sample;
                    // A trun that claims more samples than it lists is damaged.
                    if base + per_sample > body.len() {
                        return None;
                    }
                    let duration = if flags & 0x100 != 0 {
                        be32(body, base)? as u64
                    } else {
                        fallback
                    };
                    ticks += duration;
                    samples += 1;
                }
            }
            _ => {}
        }
    }
    Some((track_id, samples, ticks))
}

/// Probes a (fragmented) MP4. `Err` means the file is not playable: no `ftyp`, no
/// `moov`, no video track, or not one whole sample.
pub fn probe_mp4(path: &Path) -> Result<MediaProbe, String> {
    let mut file = File::open(path).map_err(|e| format!("cannot open media: {e}"))?;
    let file_len = file.metadata().map_err(|e| e.to_string())?.len();
    let mut at = 0u64;
    let mut saw_ftyp = false;
    let mut tracks: Vec<Track> = Vec::new();
    let mut pending: Option<Vec<(u32, u64, u64)>> = None;
    let mut counted: Vec<(u32, u64, u64)> = Vec::new();
    let mut fragments = 0u64;
    let mut good_end = 0u64;

    while let Some(head) = read_head(&mut file, at, file_len)? {
        if head.end > file_len && &head.kind != b"mdat" {
            break; // a header box cut short: nothing after it counts
        }
        match &head.kind {
            b"ftyp" => saw_ftyp = true,
            b"moov" => {
                if head.end - head.body > MAX_HEADER_BOX {
                    return Err("moov is implausibly large".into());
                }
                tracks = parse_moov(&read_range(&mut file, head.body, head.end)?);
            }
            b"moof" => {
                if head.end - head.body > MAX_HEADER_BOX {
                    return Err("moof is implausibly large".into());
                }
                let body = read_range(&mut file, head.body, head.end)?;
                let mut parsed = Vec::new();
                for (kind, traf) in children(&body) {
                    if &kind == b"traf" {
                        if let Some(entry) = parse_traf(traf, &tracks) {
                            parsed.push(entry);
                        }
                    }
                }
                pending = Some(parsed);
            }
            b"mdat" => {
                // A fragment counts only when its media data is whole.
                if let Some(entries) = pending.take() {
                    if head.end <= file_len {
                        counted.extend(entries);
                        fragments += 1;
                    }
                }
            }
            _ => {}
        }
        if head.end > file_len {
            break;
        }
        good_end = head.end;
        at = head.end;
    }

    if !saw_ftyp {
        return Err("not an MP4: no ftyp box".into());
    }
    if tracks.is_empty() {
        return Err("no moov box: the file has no track header".into());
    }
    let video: Vec<&Track> = tracks.iter().filter(|t| t.video).collect();
    let video = if video.is_empty() {
        tracks.iter().collect()
    } else {
        video
    };
    let mut samples = 0u64;
    let mut duration_ms = 0u64;
    for track in video {
        if track.timescale == 0 {
            return Err("video track has no timescale".into());
        }
        let (mut n, mut ticks) = (track.table_samples, track.table_ticks);
        for (id, s, t) in &counted {
            if *id == track.id {
                n += s;
                ticks += t;
            }
        }
        samples += n;
        duration_ms = duration_ms.max(ticks * 1000 / track.timescale as u64);
    }
    if samples == 0 {
        return Err("no complete fragment: the file holds no whole sample".into());
    }
    Ok(MediaProbe {
        samples,
        duration_ms,
        fragments,
        torn_tail_bytes: file_len.saturating_sub(good_end),
    })
}

fn put_box(out: &mut Vec<u8>, kind: &[u8; 4], body: &[u8]) {
    out.extend_from_slice(&((body.len() + 8) as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(body);
}

/// A synthetic fragmented MP4: one video track at timescale 1000, `samples`
/// samples of `sample_ms` each, one fragment per `per_fragment` samples. It is
/// structurally valid for [`probe_mp4`], not decodable. Test fixtures only.
#[doc(hidden)]
pub fn synthetic_fragmented_mp4(samples: u32, sample_ms: u32, per_fragment: u32) -> Vec<u8> {
    let mut out = Vec::new();
    put_box(&mut out, b"ftyp", b"isom\0\0\0\0isomiso5");

    let mut tkhd = vec![0u8; 84];
    tkhd[12..16].copy_from_slice(&1u32.to_be_bytes());
    let mut mdhd = vec![0u8; 24];
    mdhd[12..16].copy_from_slice(&1000u32.to_be_bytes());
    let mut hdlr = vec![0u8; 8];
    hdlr.extend_from_slice(b"vide");
    hdlr.extend_from_slice(&[0u8; 13]);
    let mut mdia = Vec::new();
    put_box(&mut mdia, b"mdhd", &mdhd);
    put_box(&mut mdia, b"hdlr", &hdlr);
    let mut trak = Vec::new();
    put_box(&mut trak, b"tkhd", &tkhd);
    put_box(&mut trak, b"mdia", &mdia);
    let mut trex = vec![0u8; 24];
    trex[4..8].copy_from_slice(&1u32.to_be_bytes());
    let mut mvex = Vec::new();
    put_box(&mut mvex, b"trex", &trex);
    let mut moov = Vec::new();
    put_box(&mut moov, b"trak", &trak);
    put_box(&mut moov, b"mvex", &mvex);
    put_box(&mut out, b"moov", &moov);

    let mut left = samples;
    while left > 0 {
        let n = left.min(per_fragment.max(1));
        left -= n;
        let mut tfhd = vec![0u8; 8];
        tfhd[4..8].copy_from_slice(&1u32.to_be_bytes());
        // flags: duration and size present in each sample
        let mut trun = vec![0u8, 0, 3, 0];
        trun.extend_from_slice(&n.to_be_bytes());
        for _ in 0..n {
            trun.extend_from_slice(&sample_ms.to_be_bytes());
            trun.extend_from_slice(&4u32.to_be_bytes());
        }
        let mut traf = Vec::new();
        put_box(&mut traf, b"tfhd", &tfhd);
        put_box(&mut traf, b"trun", &trun);
        let mut moof = Vec::new();
        put_box(&mut moof, b"traf", &traf);
        put_box(&mut out, b"moof", &moof);
        put_box(&mut out, b"mdat", &vec![0u8; (n * 4) as usize]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write(name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("rk-media-{name}-{}.mp4", std::process::id()));
        fs::write(&path, bytes).unwrap();
        path
    }

    #[test]
    fn counts_whole_fragments() {
        let path = write("whole", &synthetic_fragmented_mp4(90, 33, 30));
        let probe = probe_mp4(&path).unwrap();
        assert_eq!(probe.samples, 90);
        assert_eq!(probe.fragments, 3);
        assert_eq!(probe.duration_ms, 90 * 33);
        assert_eq!(probe.torn_tail_bytes, 0);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn a_kill_mid_fragment_keeps_the_fragments_before_it() {
        let bytes = synthetic_fragmented_mp4(90, 33, 30);
        // Cut inside the last fragment's mdat.
        let cut = bytes.len() - 20;
        let path = write("torn", &bytes[..cut]);
        let probe = probe_mp4(&path).unwrap();
        assert_eq!(probe.samples, 60);
        assert_eq!(probe.fragments, 2);
        assert_eq!(probe.duration_ms, 60 * 33);
        assert!(probe.torn_tail_bytes > 0);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn a_header_only_file_is_not_playable() {
        let bytes = synthetic_fragmented_mp4(30, 33, 30);
        let moof_at = bytes.windows(4).position(|w| w == b"moof").unwrap() - 4;
        let path = write("header", &bytes[..moof_at]);
        assert!(probe_mp4(&path)
            .unwrap_err()
            .contains("no complete fragment"));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn garbage_is_not_playable() {
        let path = write("junk", b"fake-media-bytes");
        assert!(probe_mp4(&path).is_err());
        let _ = fs::remove_file(path);
    }
}
