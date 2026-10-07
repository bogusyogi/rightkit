//! Content validators for generated artifacts: a file existing is not a result.
//! Pure Rust for WAV/PNG/loudness; ffmpeg (a caller-supplied binary path) only where a
//! compressed container must be decoded. See also [`crate::loudness`].
use crate::process::{run_owned, RunOptions, Tracker};
use crate::util::{err, sha256_hex, Result};
use serde::Serialize;
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub fn file_stat(path: &Path) -> Value {
    match fs::read(path) {
        Ok(b) => json!({"exists": true, "size": b.len(), "sha256": sha256_hex(&b)}),
        Err(_) => json!({"exists": false, "size": 0, "sha256": null}),
    }
}

pub fn png_size(path: &Path) -> Result<Value> {
    let b = fs::read(path)?;
    if b.len() < 24 || b[..8] != [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a] {
        return err(format!("{} is not a PNG", path.display()));
    }
    Ok(
        json!({"width": u32::from_be_bytes(b[16..20].try_into().unwrap()), "height": u32::from_be_bytes(b[20..24].try_into().unwrap())}),
    )
}

/// Parse a WAV: channels, rate, duration, RMS/peak dB, silence flag (JSON form of [`wav_info`]).
/// Keys: `channels`, `rate`, `bits`, `duration_s`, `rms_db`, `peak_db`, `silent`, plus
/// `sample_rate`, `frames`, `float`.
pub fn wav_stats(path: &Path) -> Result<Value> {
    let w = wav_info(path)?;
    Ok(json!({
        "channels": w.channels as f64, "rate": w.sample_rate as f64, "sample_rate": w.sample_rate,
        "bits": w.bits, "float": w.float, "frames": w.frames, "duration_s": w.duration_s,
        "rms_db": w.rms_dbfs, "peak_db": w.peak_dbfs, "silent": w.silent,
    }))
}

/// Deterministic 16 kHz mono speech-like WAV (same generator the Node suite used).
pub fn write_reference_wav(path: &Path, seconds: f64) -> Result<()> {
    let rate = 16000f64;
    let n = (rate * seconds) as usize;
    let mut data = Vec::with_capacity(n * 2);
    let (mut s, mut lp) = (12345i64, 0f64);
    for i in 0..n {
        s = (s * 1103515245 + 12345) & 0x7fffffff;
        let rnd = s as f64 / 0x40000000 as f64 - 1.0;
        let t = i as f64;
        let env = 0.4 + 0.6 * (2.0 * std::f64::consts::PI * 3.3 * t / rate).sin().abs();
        lp = 0.85 * lp + 0.15 * rnd;
        let v = env
            * (0.35 * (2.0 * std::f64::consts::PI * 190.0 * t / rate).sin()
                + 0.2 * (2.0 * std::f64::consts::PI * 380.0 * t / rate + 1.0).sin()
                + 0.25 * lp);
        data.extend_from_slice(&((v.clamp(-1.0, 1.0) * 32000.0) as i16).to_le_bytes());
    }
    let mut out = Vec::with_capacity(44 + data.len());
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&16000u32.to_le_bytes());
    out.extend_from_slice(&32000u32.to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(&data);
    if let Some(p) = path.parent() {
        fs::create_dir_all(p)?;
    }
    fs::write(path, out)?;
    Ok(())
}

pub fn ffprobe_bin() -> String {
    std::env::var("RIGHTKIT_QA_FFPROBE")
        .or_else(|_| std::env::var("GENRIGHT_FFPROBE"))
        .unwrap_or_else(|_| "ffprobe".into())
}

/// `ffprobe -show_format -show_streams` as JSON; errors if ffprobe is absent.
pub fn ffprobe(path: &Path, tracker: &Tracker) -> Result<Value> {
    let out = run_owned(
        &ffprobe_bin(),
        &[
            "-v".into(),
            "quiet".into(),
            "-print_format".into(),
            "json".into(),
            "-show_format".into(),
            "-show_streams".into(),
            path.to_string_lossy().into(),
        ],
        &RunOptions {
            timeout: Some(Duration::from_secs(60)),
            label: "ffprobe".into(),
            ..Default::default()
        },
        tracker,
    )?;
    if out.code != Some(0) {
        return err(format!(
            "ffprobe failed ({:?}): {}",
            out.code,
            crate::util::tail(&out.stderr, 500)
        ));
    }
    Ok(serde_json::from_str(&out.stdout)?)
}

// ------------------------------------------------------------------ PCM audio

/// Decoded audio: interleaved samples in `[-1, 1]`.
#[derive(Debug, Clone, PartialEq)]
pub struct Pcm {
    pub sample_rate: u32,
    pub channels: u16,
    pub samples: Vec<f32>,
}

impl Pcm {
    pub fn frames(&self) -> usize {
        self.samples.len() / self.channels.max(1) as usize
    }
    pub fn duration_s(&self) -> f64 {
        self.frames() as f64 / self.sample_rate.max(1) as f64
    }
}

/// WAV header facts plus signal level.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WavInfo {
    pub channels: u16,
    pub sample_rate: u32,
    pub bits: u16,
    /// IEEE float samples (format 3) rather than integer PCM.
    pub float: bool,
    pub frames: u64,
    pub duration_s: f64,
    /// Largest absolute sample, dBFS (-180 for digital silence).
    pub peak_dbfs: f64,
    /// RMS over every sample of every channel, dBFS.
    pub rms_dbfs: f64,
    /// RMS below 1e-4 (-80 dBFS).
    pub silent: bool,
}

/// Read and decode a WAV file (PCM 8/16/24/32-bit, float 32/64-bit, WAVE_FORMAT_EXTENSIBLE).
pub fn read_wav(path: &Path) -> Result<Pcm> {
    parse_wav(&fs::read(path)?).map_err(|e| crate::util::Error(format!("{}: {e}", path.display())))
}

/// Decode WAV bytes. A streamed header (data size 0 or 0xFFFFFFFF) reads to end of file.
pub fn parse_wav(b: &[u8]) -> Result<Pcm> {
    if b.len() < 12 || &b[0..4] != b"RIFF" || &b[8..12] != b"WAVE" {
        return err("not a RIFF/WAVE file");
    }
    let (mut off, mut fmt, mut data) = (12usize, None, None);
    while off + 8 <= b.len() {
        let id = &b[off..off + 4];
        let mut size = u32::from_le_bytes(b[off + 4..off + 8].try_into().unwrap()) as usize;
        if id == b"data" && (size == 0 || size == u32::MAX as usize) {
            size = b.len() - (off + 8);
        }
        let end = (off + 8).saturating_add(size).min(b.len());
        if id == b"fmt " {
            fmt = Some(&b[off + 8..end]);
        }
        if id == b"data" {
            data = Some(&b[off + 8..end]);
        }
        off = off.saturating_add(8 + size + (size % 2));
    }
    let (Some(fmt), Some(data)) = (fmt, data) else {
        return err("WAV is missing fmt or data chunk");
    };
    if fmt.len() < 16 {
        return err("WAV fmt chunk too short");
    }
    let mut tag = u16::from_le_bytes([fmt[0], fmt[1]]);
    if tag == 0xFFFE && fmt.len() >= 26 {
        tag = u16::from_le_bytes([fmt[24], fmt[25]]); // SubFormat GUID's leading format code
    }
    let channels = u16::from_le_bytes([fmt[2], fmt[3]]);
    let sample_rate = u32::from_le_bytes(fmt[4..8].try_into().unwrap());
    let bits = u16::from_le_bytes([fmt[14], fmt[15]]);
    if channels == 0 || sample_rate == 0 {
        return err("WAV declares zero channels or zero sample rate");
    }
    let width = (bits as usize).div_ceil(8);
    let usable = data.len() - data.len() % (width.max(1) * channels as usize);
    let data = &data[..usable];
    let samples: Vec<f32> = match (tag, bits) {
        (1, 8) => data.iter().map(|&v| (v as f32 - 128.0) / 128.0).collect(),
        (1, 16) => data
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| i16::from_le_bytes([c[0], c[1]]) as f32 / 32768.0)
            .collect(),
        (1, 24) => data
            .as_chunks::<3>()
            .0
            .iter()
            .map(|c| (i32::from_le_bytes([0, c[0], c[1], c[2]]) >> 8) as f32 / 8_388_608.0)
            .collect(),
        (1, 32) => data
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| (i32::from_le_bytes(*c) as f64 / 2_147_483_648.0) as f32)
            .collect(),
        (3, 32) => data
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| f32::from_le_bytes(*c))
            .collect(),
        (3, 64) => data
            .as_chunks::<8>()
            .0
            .iter()
            .map(|c| f64::from_le_bytes(*c) as f32)
            .collect(),
        _ => {
            return err(format!(
                "unsupported WAV encoding: format {tag}, {bits} bits"
            ))
        }
    };
    Ok(Pcm {
        sample_rate,
        channels,
        samples,
    })
}

/// Header facts and levels of a WAV file.
pub fn wav_info(path: &Path) -> Result<WavInfo> {
    let b = fs::read(path)?;
    let pcm = parse_wav(&b).map_err(|e| crate::util::Error(format!("{}: {e}", path.display())))?;
    // parse_wav validated the fmt chunk; re-read bits/float for the report.
    let fmt_at = b.windows(4).position(|w| w == b"fmt ").unwrap_or(12) + 8;
    let mut tag = u16::from_le_bytes([b[fmt_at], b[fmt_at + 1]]);
    if tag == 0xFFFE && b.len() > fmt_at + 25 {
        tag = u16::from_le_bytes([b[fmt_at + 24], b[fmt_at + 25]]);
    }
    let bits = u16::from_le_bytes([b[fmt_at + 14], b[fmt_at + 15]]);
    let (mut sum_sq, mut peak) = (0f64, 0f64);
    for &v in &pcm.samples {
        let v = v as f64;
        sum_sq += v * v;
        peak = peak.max(v.abs());
    }
    let rms = (sum_sq / pcm.samples.len().max(1) as f64).sqrt();
    Ok(WavInfo {
        channels: pcm.channels,
        sample_rate: pcm.sample_rate,
        bits,
        float: tag == 3,
        frames: pcm.frames() as u64,
        duration_s: pcm.duration_s(),
        peak_dbfs: dbfs(peak),
        rms_dbfs: dbfs(rms),
        silent: rms < 0.0001,
    })
}

fn dbfs(v: f64) -> f64 {
    20.0 * (if v > 0.0 { v } else { 1e-9 }).log10()
}

/// Write 32-bit float PCM WAV (fixtures, decoded intermediates).
pub fn write_wav_f32(path: &Path, pcm: &Pcm) -> Result<()> {
    let data_len = (pcm.samples.len() * 4) as u32;
    let mut out = Vec::with_capacity(44 + data_len as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&3u16.to_le_bytes());
    out.extend_from_slice(&pcm.channels.to_le_bytes());
    out.extend_from_slice(&pcm.sample_rate.to_le_bytes());
    out.extend_from_slice(&(pcm.sample_rate * pcm.channels as u32 * 4).to_le_bytes());
    out.extend_from_slice(&(pcm.channels * 4).to_le_bytes());
    out.extend_from_slice(&32u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for s in &pcm.samples {
        out.extend_from_slice(&s.to_le_bytes());
    }
    if let Some(p) = path.parent() {
        fs::create_dir_all(p)?;
    }
    fs::write(path, out)?;
    Ok(())
}

fn scratch_file(ext: &str) -> PathBuf {
    std::env::temp_dir().join(format!("rkqa-{}.{ext}", crate::util::new_id()))
}

fn ffmpeg_to_file(
    ffmpeg: &Path,
    args: &[String],
    out: &Path,
    label: &str,
    tracker: &Tracker,
) -> Result<Vec<u8>> {
    let mut all: Vec<String> = ["-hide_banner", "-nostdin", "-v", "error", "-y"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    all.extend_from_slice(args);
    all.push(out.to_string_lossy().into());
    let o = run_owned(
        &ffmpeg.to_string_lossy(),
        &all,
        &RunOptions {
            timeout: Some(Duration::from_secs(300)),
            label: label.into(),
            ..Default::default()
        },
        tracker,
    )?;
    let bytes = fs::read(out).unwrap_or_default();
    let _ = fs::remove_file(out);
    if o.code != Some(0) {
        return err(format!(
            "{label} failed ({:?}): {}",
            o.code,
            crate::util::tail(&o.stderr, 800)
        ));
    }
    Ok(bytes)
}

/// Decode any audio ffmpeg reads (FLAC, MP3, AAC, MP4 audio track) to float PCM at its native
/// rate and channel count. `ffmpeg` is the binary path.
pub fn decode_audio_ffmpeg(ffmpeg: &Path, input: &Path, tracker: &Tracker) -> Result<Pcm> {
    let tmp = scratch_file("wav");
    let args: Vec<String> = vec![
        "-i".into(),
        input.to_string_lossy().into(),
        "-vn".into(),
        "-map".into(),
        "0:a:0".into(),
        "-c:a".into(),
        "pcm_f32le".into(),
        "-f".into(),
        "wav".into(),
    ];
    let bytes = ffmpeg_to_file(ffmpeg, &args, &tmp, "ffmpeg decode audio", tracker)?;
    parse_wav(&bytes)
}

/// WAV read directly; anything else decoded through `ffmpeg` (an error when not given).
pub fn read_audio(path: &Path, ffmpeg: Option<&Path>, tracker: &Tracker) -> Result<Pcm> {
    let b = fs::read(path)?;
    if b.len() >= 12 && &b[0..4] == b"RIFF" && &b[8..12] == b"WAVE" {
        if let Ok(p) = parse_wav(&b) {
            return Ok(p);
        }
    }
    match ffmpeg {
        Some(f) => decode_audio_ffmpeg(f, path, tracker),
        None => err(format!(
            "{} is not a decodable WAV and no ffmpeg path was given",
            path.display()
        )),
    }
}

// ------------------------------------------------------------------ pixels

/// Minimum per-frame standard deviation (largest of the R, G, B channels, 0-255 scale) for a
/// decoded 64x64 RGB24 video frame to count as non-uniform. GenRight's `framesNonTrivial`
/// threshold; GenRight pooled all channels, which let a flat non-grey colour pass, so the
/// deviation here is taken per channel.
pub const FRAME_STDDEV_MIN: f64 = 4.0;
/// Minimum channel range (max - min, 0-255) after a 32x32 box downscale for a PNG to count as
/// non-trivial (GenRight's `pngNonTrivial` threshold).
pub const IMAGE_RANGE_MIN: u8 = 24;
/// Edge length video frames are scaled to before [`pixel_stats`].
pub const FRAME_EDGE: u32 = 64;
/// Edge length images are box-downscaled to before the range test.
pub const IMAGE_EDGE: u32 = 32;

/// Statistics of an RGB24 buffer.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct PixelStats {
    /// Mean of every byte.
    pub mean: f64,
    /// Largest per-channel population standard deviation: spatial variation, so a flat colour
    /// scores 0 whatever its hue.
    pub stddev: f64,
    /// Per-channel standard deviation (R, G, B).
    pub channel_stddev: [f64; 3],
    /// Per-channel max - min.
    pub range: [u8; 3],
}

/// Mean, per-channel standard deviation and ranges of RGB24 pixels.
pub fn pixel_stats(rgb: &[u8]) -> PixelStats {
    let (mut sum, mut sq) = ([0f64; 3], [0f64; 3]);
    let (mut lo, mut hi) = ([255u8; 3], [0u8; 3]);
    let px = rgb.as_chunks::<3>().0;
    for p in px {
        for c in 0..3 {
            let v = p[c];
            lo[c] = lo[c].min(v);
            hi[c] = hi[c].max(v);
            sum[c] += v as f64;
            sq[c] += (v as f64) * (v as f64);
        }
    }
    let n = px.len().max(1) as f64;
    let channel_stddev = [0, 1, 2].map(|c| {
        let m = sum[c] / n;
        (sq[c] / n - m * m).max(0.0).sqrt()
    });
    PixelStats {
        mean: (sum[0] + sum[1] + sum[2]) / (3.0 * n),
        stddev: channel_stddev.iter().copied().fold(0.0, f64::max),
        channel_stddev,
        range: [0, 1, 2].map(|c| hi[c].saturating_sub(lo[c])),
    }
}

/// Decoded-video verdict.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FramesReport {
    pub frames: Vec<PixelStats>,
    /// At least one frame decoded and every decoded frame has stddev > [`FRAME_STDDEV_MIN`].
    pub non_trivial: bool,
}

/// Verdict over already-decoded RGB24 frames of `FRAME_EDGE x FRAME_EDGE` (or any equal size).
pub fn frames_report(frames: &[Vec<u8>]) -> FramesReport {
    let stats: Vec<PixelStats> = frames.iter().map(|f| pixel_stats(f)).collect();
    let non_trivial = !stats.is_empty() && stats.iter().all(|s| s.stddev > FRAME_STDDEV_MIN);
    FramesReport {
        frames: stats,
        non_trivial,
    }
}

/// Decode `count` frames of a video (every `count`-th frame from the start, as GenRight does),
/// scale each to 64x64 RGB24 and require visible variance in all of them. `ffmpeg` is the
/// binary path. An undecodable file is an error; a decodable flat one is `non_trivial: false`.
pub fn frames_non_trivial(
    ffmpeg: &Path,
    video: &Path,
    count: usize,
    tracker: &Tracker,
) -> Result<FramesReport> {
    let count = count.max(1);
    let tmp = scratch_file("rgb");
    let vf = format!("select='not(mod(n\\,{count}))',scale={FRAME_EDGE}:{FRAME_EDGE}");
    let args: Vec<String> = [
        "-i",
        &video.to_string_lossy(),
        "-an",
        "-vf",
        &vf,
        "-frames:v",
        &count.to_string(),
        "-f",
        "rawvideo",
        "-pix_fmt",
        "rgb24",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let buf = ffmpeg_to_file(ffmpeg, &args, &tmp, "ffmpeg decode frames", tracker)?;
    let size = (FRAME_EDGE * FRAME_EDGE * 3) as usize;
    if buf.len() < size {
        return err(format!("{} decoded no video frames", video.display()));
    }
    let frames: Vec<Vec<u8>> = buf.chunks_exact(size).map(<[u8]>::to_vec).collect();
    Ok(frames_report(&frames))
}

/// Decoded-image verdict.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ImageReport {
    pub width: u32,
    pub height: u32,
    /// Stats of the [`IMAGE_EDGE`] box-downscaled RGB (alpha composited over black).
    pub stats: PixelStats,
    /// Every pixel has alpha 0.
    pub fully_transparent: bool,
    /// Not fully transparent, and some channel's range exceeds [`IMAGE_RANGE_MIN`].
    pub non_trivial: bool,
}

/// Verdict over RGBA8 pixels. Downscaling (area average) first means single-pixel noise or a
/// one-pixel border does not make a blank image pass.
pub fn image_report(rgba: &[u8], width: u32, height: u32) -> ImageReport {
    let (w, h) = (width.max(1) as usize, height.max(1) as usize);
    let (tw, th) = (w.min(IMAGE_EDGE as usize), h.min(IMAGE_EDGE as usize));
    let mut acc = vec![[0f64; 4]; tw * th];
    let mut cnt = vec![0f64; tw * th];
    let mut any_alpha = false;
    for y in 0..h {
        for x in 0..w {
            let i = (y * w + x) * 4;
            let Some(px) = rgba.get(i..i + 4) else {
                continue;
            };
            any_alpha |= px[3] > 0;
            let a = px[3] as f64 / 255.0;
            let t = (y * th / h) * tw + (x * tw / w);
            for c in 0..3 {
                acc[t][c] += px[c] as f64 * a;
            }
            cnt[t] += 1.0;
        }
    }
    let small: Vec<u8> = acc
        .iter()
        .zip(&cnt)
        .flat_map(|(a, &n)| (0..3).map(move |c| (a[c] / n.max(1.0)).round() as u8))
        .collect();
    let stats = pixel_stats(&small);
    let fully_transparent = !any_alpha;
    ImageReport {
        width,
        height,
        stats,
        fully_transparent,
        non_trivial: !fully_transparent && stats.range.iter().any(|&r| r > IMAGE_RANGE_MIN),
    }
}

/// Decode a PNG (pure Rust) to RGBA8.
pub fn decode_png(path: &Path) -> Result<(u32, u32, Vec<u8>)> {
    let bytes = fs::read(path)?;
    let mut dec = png::Decoder::new(std::io::Cursor::new(bytes));
    dec.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = dec
        .read_info()
        .map_err(|e| crate::util::Error(format!("{}: PNG decode: {e}", path.display())))?;
    let mut buf = vec![0u8; reader.output_buffer_size().unwrap_or(0)];
    let info = reader
        .next_frame(&mut buf)
        .map_err(|e| crate::util::Error(format!("{}: PNG decode: {e}", path.display())))?;
    buf.truncate(info.buffer_size());
    let rgba = match info.color_type {
        png::ColorType::Rgba => buf,
        png::ColorType::Rgb => buf
            .as_chunks::<3>()
            .0
            .iter()
            .flat_map(|p| [p[0], p[1], p[2], 255])
            .collect(),
        png::ColorType::GrayscaleAlpha => buf
            .as_chunks::<2>()
            .0
            .iter()
            .flat_map(|p| [p[0], p[0], p[0], p[1]])
            .collect(),
        png::ColorType::Grayscale => buf.iter().flat_map(|&g| [g, g, g, 255]).collect(),
        png::ColorType::Indexed => return err("PNG palette was not expanded"),
    };
    Ok((info.width, info.height, rgba))
}

/// A PNG that decodes and is not blank, uniform or fully transparent (pure Rust).
pub fn png_non_trivial(path: &Path) -> Result<ImageReport> {
    let (w, h, rgba) = decode_png(path)?;
    Ok(image_report(&rgba, w, h))
}

/// Write a deterministic non-trivial RGB PNG (gradients plus a checkerboard), the pure-Rust
/// stand-in for ffmpeg `testsrc2` input images.
pub fn write_test_png(path: &Path, width: u32, height: u32) -> Result<PathBuf> {
    let (w, h) = (width.max(1), height.max(1));
    let mut px = Vec::with_capacity((w * h * 3) as usize);
    for y in 0..h {
        for x in 0..w {
            let check = if ((x * 8 / w) + (y * 8 / h)) % 2 == 0 {
                0
            } else {
                96
            };
            px.extend_from_slice(&[
                (x * 255 / w.max(2).saturating_sub(1)) as u8,
                (y * 255 / h.max(2).saturating_sub(1)) as u8,
                check,
            ]);
        }
    }
    write_png_rgb(path, w, h, &px)?;
    Ok(path.to_path_buf())
}

/// Write RGB8 pixels as a PNG.
pub fn write_png_rgb(path: &Path, width: u32, height: u32, rgb: &[u8]) -> Result<()> {
    if let Some(p) = path.parent() {
        fs::create_dir_all(p)?;
    }
    let file = fs::File::create(path)?;
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), width, height);
    enc.set_color(png::ColorType::Rgb);
    enc.set_depth(png::BitDepth::Eight);
    let e = |e: png::EncodingError| crate::util::Error(format!("PNG encode: {e}"));
    let mut w = enc.write_header().map_err(e)?;
    w.write_image_data(rgb).map_err(e)?;
    w.finish().map_err(e)?;
    Ok(())
}

/// Find a tool: the settings key (path) first, then `PATH`, then common install directories
/// (`/opt/homebrew/bin`, `/usr/local/bin`, `/usr/bin`). Under `rightkit cargo` the environment,
/// including `PATH`, may be scrubbed, so tests should pass the result explicitly.
pub fn locate_tool(name: &str, settings: &crate::settings::Settings, key: &str) -> Option<PathBuf> {
    if let Some(p) = settings.get(key) {
        let p = PathBuf::from(p);
        return p.is_file().then_some(p);
    }
    let exe = format!("{name}{}", std::env::consts::EXE_SUFFIX);
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    dirs.extend(["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"].map(PathBuf::from));
    dirs.into_iter().map(|d| d.join(&exe)).find(|p| p.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reference_wav_round_trips_through_stats() {
        let p = std::env::temp_dir().join(format!("rkqa-{}.wav", crate::util::new_id()));
        write_reference_wav(&p, 1.0).unwrap();
        let s = wav_stats(&p).unwrap();
        assert_eq!(s["rate"], 16000.0);
        assert_eq!(s["silent"], false);
        assert!((s["duration_s"].as_f64().unwrap() - 1.0).abs() < 0.01);
        assert!(png_size(&p).is_err());
        let info = wav_info(&p).unwrap();
        assert_eq!(
            (info.channels, info.sample_rate, info.bits, info.float),
            (1, 16000, 16, false)
        );
        assert_eq!(info.frames, 16000);
        assert!(info.peak_dbfs < 0.0 && info.peak_dbfs > -6.0, "{info:?}");
        assert!(info.rms_dbfs < info.peak_dbfs);
        let _ = fs::remove_file(p);
    }

    fn tmp(ext: &str) -> PathBuf {
        scratch_file(ext)
    }

    #[test]
    fn wav_decodes_float_24bit_extensible_and_streamed_headers() {
        let pcm = Pcm {
            sample_rate: 48000,
            channels: 2,
            samples: (0..9600)
                .map(|i| ((i % 200) as f32 / 100.0 - 1.0) * 0.5)
                .collect(),
        };
        let p = tmp("wav");
        write_wav_f32(&p, &pcm).unwrap();
        let back = read_wav(&p).unwrap();
        assert_eq!(back, pcm);
        let info = wav_info(&p).unwrap();
        assert!(info.float && info.bits == 32 && info.channels == 2);
        assert!((info.duration_s - 0.1).abs() < 1e-9);
        assert!((info.peak_dbfs - dbfs(0.5)).abs() < 0.01);
        let _ = fs::remove_file(&p);

        // 24-bit WAVE_FORMAT_EXTENSIBLE, data size 0xFFFFFFFF (ffmpeg writing to a pipe).
        let mut fmt = Vec::new();
        fmt.extend_from_slice(&0xFFFEu16.to_le_bytes());
        fmt.extend_from_slice(&1u16.to_le_bytes());
        fmt.extend_from_slice(&8000u32.to_le_bytes());
        fmt.extend_from_slice(&24000u32.to_le_bytes());
        fmt.extend_from_slice(&3u16.to_le_bytes());
        fmt.extend_from_slice(&24u16.to_le_bytes());
        fmt.extend_from_slice(&22u16.to_le_bytes());
        fmt.extend_from_slice(&24u16.to_le_bytes());
        fmt.extend_from_slice(&4u32.to_le_bytes());
        fmt.extend_from_slice(&1u16.to_le_bytes()); // SubFormat: PCM
        fmt.extend_from_slice(&[0u8; 14]);
        let mut b = b"RIFF\xff\xff\xff\xffWAVEfmt ".to_vec();
        b.extend_from_slice(&(fmt.len() as u32).to_le_bytes());
        b.extend_from_slice(&fmt);
        b.extend_from_slice(b"data\xff\xff\xff\xff");
        for v in [0x400000i32, -0x400000, 0x7fffff] {
            b.extend_from_slice(&v.to_le_bytes()[..3]);
        }
        let p = parse_wav(&b).unwrap();
        assert_eq!(p.samples.len(), 3);
        assert!((p.samples[0] - 0.5).abs() < 1e-6 && (p.samples[1] + 0.5).abs() < 1e-6);
        assert!(parse_wav(b"RIFF....WAVE").is_err());
    }

    #[test]
    fn image_report_rejects_flat_transparent_and_speckled() {
        let flat: Vec<u8> = [40u8, 80, 120, 255].repeat(64 * 64);
        let r = image_report(&flat, 64, 64);
        assert!(
            !r.non_trivial && !r.fully_transparent && r.stats.stddev < 0.5,
            "{r:?}"
        );
        let clear = vec![0u8; 64 * 64 * 4];
        assert!(image_report(&clear, 64, 64).fully_transparent);
        assert!(!image_report(&clear, 64, 64).non_trivial);
        // One white pixel on black: a 32x32 area average dilutes it below the threshold.
        let mut speck = [0u8, 0, 0, 255].repeat(256 * 256);
        speck[..4].copy_from_slice(&[255, 255, 255, 255]);
        assert!(!image_report(&speck, 256, 256).non_trivial);
        let grad: Vec<u8> = (0..64 * 64)
            .flat_map(|i| [(i % 64 * 4) as u8, 0, 0, 255])
            .collect();
        let g = image_report(&grad, 64, 64);
        assert!(g.non_trivial && g.stats.range[0] > 200, "{g:?}");
    }

    #[test]
    fn png_round_trip_and_non_trivial_verdicts() {
        let good = write_test_png(&tmp("png"), 96, 64).unwrap();
        let r = png_non_trivial(&good).unwrap();
        assert!(r.non_trivial && (r.width, r.height) == (96, 64), "{r:?}");
        assert_eq!(png_size(&good).unwrap()["width"], 96);
        let flat = tmp("png");
        write_png_rgb(&flat, 16, 16, &[200u8; 16 * 16 * 3]).unwrap();
        assert!(!png_non_trivial(&flat).unwrap().non_trivial);
        let junk = tmp("png");
        fs::write(&junk, b"\x89PNG\r\n\x1a\nnot really").unwrap();
        assert!(
            png_non_trivial(&junk).is_err(),
            "undecodable PNG is an error, not trivial"
        );
        for p in [good, flat, junk] {
            let _ = fs::remove_file(p);
        }
    }

    #[test]
    fn frames_report_requires_every_frame_to_vary() {
        let edge = (FRAME_EDGE * FRAME_EDGE * 3) as usize;
        let busy: Vec<u8> = (0..edge).map(|i| (i * 37 % 256) as u8).collect();
        let flat = vec![16u8; edge];
        assert!(frames_report(&[busy.clone(), busy.clone()]).non_trivial);
        assert!(!frames_report(&[busy, flat]).non_trivial);
        assert!(!frames_report(&[]).non_trivial);
    }

    #[test]
    fn read_audio_without_ffmpeg_needs_wav() {
        let p = tmp("flac");
        fs::write(&p, b"fLaC....").unwrap();
        let e = read_audio(&p, None, &Tracker::new()).unwrap_err();
        assert!(e.0.contains("no ffmpeg path"), "{e}");
        let _ = fs::remove_file(p);
    }
}
