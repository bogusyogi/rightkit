//! Local Wav2Lip talking-head lip-sync — native ONNX Runtime inference with no
//! Python, no API key, and no spend. One portrait image + one audio file in,
//! one MP4 out, synchronously.
//!
//! Model: `wav2lip.onnx` (96×96 Wav2Lip export, `bluefoxcreation/Wav2lip-Onnx`
//! on Hugging Face — the upstream Rudrabha/Wav2Lip checkpoint is
//! research/non-commercial, so this route is permanently `quality:
//! experimental`; see the label). The file is manifest-verified by size and
//! SHA-256 before first use, like the QTS manifest.
//!
//! Pipeline (mirrors Rudrabha/Wav2Lip `audio.py` + `inference.py`):
//! 1. ffmpeg decodes any audio input to mono 16 kHz s16.
//! 2. Mel spectrogram: pre-emphasis 0.97 → STFT (n_fft/win 800, hop 200, Hann,
//!    center pad) → Slaney mel basis (80 bands, 55–7600 Hz) → amp_to_db − 20
//!    dB ref → symmetric normalize to ±4.
//! 3. Face region: YuNet face detection (opencv_zoo `face_detection_yunet_
//!    2023mar.onnx`, free official OpenCV model) → rect padded per Wav2Lip
//!    `--pads 0 10 0 0` (bottom +10 px for the chin) → resized to 96×96.
//!    Falls back to a centered crop only when the detector model is absent
//!    or finds no face. The network input is `[masked_lower_half, face]`
//!    in **BGR**, /255, NCHW (6,96,96) — the export keeps the cv2 channel
//!    order of the original checkpoint. `mel` input is (1,80,16) — 16 mel
//!    columns per 25 fps output frame.
//! 4. Predicted BGR 96×96 face is colour-matched to the source crop and
//!    pasted back inside a feathered elliptical mask (no hard seam), then
//!    frames stream as PPM into ffmpeg, which muxes the audio to MP4.
//!
//! Params (`GenRequest.params`):
//! - `image_path` (required): portrait file (png/jpg), face roughly centered.
//! - `audio_path` (required): audio file ffmpeg can decode (wav/mp3/…).
//! - `face_size`/`face_cx`/`face_cy` (optional): manual crop override as
//!   fractions; also the fallback when detection is unavailable.
//! - `fps` (optional): output framerate, default 25 (the model's native rate).
//!
//! Ready when: model file present+manifest-verified, an ONNX Runtime shared
//! library is found (`find_onnxruntime`, same rules as QTS), and an ffmpeg
//! binary is runnable.

use super::{
    model_load_failed, ort_init_failure, Durations, GenRequest, Kind, ModelInfo, Output,
    PollOutcome, Provider, ProviderError, SubmitOutcome,
};
use image::{imageops, RgbImage};
use rightkit_child_process::{run as run_captured, RunOptions};
use rightkit_ffmpeg::h264_rule as h264;
use rightkit_ffmpeg::{FfmpegBinary, FfmpegCommand};
use rightkit_onnx::ort::session::{builder::GraphOptimizationLevel, Session};
use rightkit_onnx::ort::value::Tensor;
use serde_json::Value;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

pub const ID: &str = "local-wav2lip";
pub const MODEL: &str = "wav2lip-96";

pub const MODEL_FILE: &str = "wav2lip.onnx";
pub const MODEL_BYTES: u64 = 145_175_471;
pub const MODEL_SHA256: &str = "902d7719f0ebbd461f956da8f603f02e84c019d8d64cfe25402a217605b2e690";
pub const HF_REPO: &str = "bluefoxcreation/Wav2lip-Onnx";

/// YuNet face detector (official opencv_zoo model, MIT/Apache, GitHub).
pub const DETECTOR_FILE: &str = "face_detection_yunet_2023mar.onnx";
pub const DETECTOR_BYTES: u64 = 232_589;
pub const DETECTOR_SHA256: &str =
    "8f2383e4dd3cfbb4553ea8718107fc0423210dc964f9f4280604804ed2552fa4";
pub const DETECTOR_REPO: &str = "github.com/opencv/opencv_zoo";
const DETECT_IN: u32 = 640;
const DETECT_SCORE: f32 = 0.5;

const SR: usize = 16_000;
const N_FFT: usize = 800;
const HOP: usize = 200;
const N_MELS: usize = 80;
const FMIN: f64 = 55.0;
const FMAX: f64 = 7600.0;
const MIN_LEVEL_DB: f64 = -100.0;
const REF_LEVEL_DB: f64 = 20.0;
const MAX_ABS: f64 = 4.0;
const MEL_STEP: usize = 16;
const FACE: usize = 96;
const DEFAULT_FPS: u32 = 25;

static MODELS: [ModelInfo; 1] = [ModelInfo {
    provider: ID,
    model: MODEL,
    kind: Kind::Avatar,
    label: "Wav2Lip 96×96 + YuNet face detect (local, free) — talking-head from one portrait + audio; EXPERIMENTAL & NON-COMMERCIAL: Wav2Lip is research-only licensed",
    est_usd: 0.0,
    durations: Durations::NONE,
}];

pub struct LocalLipsync;

impl LocalLipsync {
    pub fn new() -> Self {
        Self
    }
}

impl Default for LocalLipsync {
    fn default() -> Self {
        Self::new()
    }
}

fn bad(code: &'static str, msg: impl Into<String>) -> ProviderError {
    ProviderError::new(code, msg.into(), false)
}

// ---- model / runtime discovery --------------------------------------------

/// `RIGHTKIT_LIPSYNC_MODEL_DIR` or `<data>/models/lipsync`.
pub fn model_dir() -> PathBuf {
    match std::env::var_os("RIGHTKIT_LIPSYNC_MODEL_DIR").filter(|v| !v.is_empty()) {
        Some(p) => PathBuf::from(p),
        None => crate::qts::models::data_root()
            .join("models")
            .join("lipsync"),
    }
}

pub fn model_path() -> PathBuf {
    model_dir().join(MODEL_FILE)
}

/// Size+SHA-256 manifest check (same contract as the QTS manifest).
fn sha_verified(path: &Path, bytes: u64, sha: &str) -> Result<(), String> {
    let meta =
        std::fs::metadata(path).map_err(|_| format!("missing model file {}", path.display()))?;
    if meta.len() != bytes {
        return Err(format!(
            "{} has {} bytes, expected {}",
            path.display(),
            meta.len(),
            bytes
        ));
    }
    let got = rightkit_fs::digest::sha256_file(path).map_err(|e| e.to_string())?;
    if got != sha {
        return Err(format!("{} failed its SHA-256 check", path.display()));
    }
    Ok(())
}

pub fn model_verified(path: &Path) -> Result<(), String> {
    sha_verified(path, MODEL_BYTES, MODEL_SHA256)
}

pub fn detector_path() -> PathBuf {
    model_dir().join(DETECTOR_FILE)
}

/// YuNet detector present and manifest-verified.
pub fn detector_verified() -> bool {
    sha_verified(&detector_path(), DETECTOR_BYTES, DETECTOR_SHA256).is_ok()
}

/// The owner's ffmpeg handle (verified, or a development build's labelled binary); spawns take `.ffmpeg_path()`.
fn ffmpeg() -> Result<crate::host::ffmpeg::FfmpegHandle, ProviderError> {
    crate::host::ffmpeg::resolve_ffmpeg()
        .map_err(|e| bad("not_installed", format!("no verified ffmpeg binary (bundled; RIGHTKIT_FFMPEG or PATH in development builds): {e}")))
}

fn ffmpeg_ok() -> bool {
    ffmpeg().is_ok_and(|h| {
        run_captured(
            h.ffmpeg_path(),
            ["-hide_banner", "-version"],
            &RunOptions::new(Duration::from_secs(30)),
        )
        .map(|o| !o.timed_out && o.status.success())
        .unwrap_or(false)
    })
}

fn runtime_check() -> Result<(), ProviderError> {
    let path = model_path();
    let meta = std::fs::metadata(&path);
    if !meta
        .map(|m| m.is_file() && m.len() == MODEL_BYTES)
        .unwrap_or(false)
    {
        return Err(bad(
            "not_installed",
            format!(
                "Wav2Lip model not found at {} ({} MB download from HF repo {})",
                path.display(),
                MODEL_BYTES / 1_000_000,
                HF_REPO
            ),
        ));
    }
    if crate::qts::models::find_onnxruntime().is_none() {
        return Err(bad(
            "not_installed",
            if cfg!(target_os = "macos") {
                "ONNX Runtime shared library not found (it ships inside the app bundle, Contents/Resources/runtime)"
            } else {
                "ONNX Runtime shared library not found (install the QTS runtime in app data)"
            },
        ));
    }
    if !ffmpeg_ok() {
        return Err(bad(
            "not_installed",
            "ffmpeg binary not found or not runnable",
        ));
    }
    Ok(())
}

// ---- audio front-end --------------------------------------------------------

/// Hann window (periodic, matches numpy/librosa `hann(N, sym=False)`).
fn hann() -> &'static [f32; N_FFT] {
    static W: OnceLock<Vec<f32>> = OnceLock::new();
    W.get_or_init(|| {
        (0..N_FFT)
            .map(|n| 0.5 * (1.0 - (2.0 * std::f32::consts::PI * n as f32 / N_FFT as f32).cos()))
            .collect()
    })
    .as_slice()
    .try_into()
    .unwrap()
}

/// Pre-emphasis: y[n] − 0.97·y[n−1] (Wav2Lip `preemphasis=0.97`).
fn preemphasis(x: &[f32]) -> Vec<f32> {
    let mut out = Vec::with_capacity(x.len());
    let mut prev = 0.0f32;
    for &s in x {
        out.push(s - 0.97 * prev);
        prev = s;
    }
    out
}

/// |STFT| magnitudes, (n_fft/2+1, T). Center padding is reflect-pad by n_fft/2
/// on each side (librosa default). The 800-point DFT is computed directly —
/// O(N²) is ~300 M ops for a few seconds of speech and needs no FFT crate.
fn stft_mag(y: &[f32]) -> Vec<Vec<f32>> {
    const BINS: usize = N_FFT / 2 + 1; // 401
    static COS: OnceLock<Vec<f32>> = OnceLock::new();
    static SIN: OnceLock<Vec<f32>> = OnceLock::new();
    let cos = COS.get_or_init(|| {
        let mut t = Vec::with_capacity(BINS * N_FFT);
        for k in 0..BINS {
            for n in 0..N_FFT {
                t.push((-2.0 * std::f32::consts::PI * k as f32 * n as f32 / N_FFT as f32).cos());
            }
        }
        t
    });
    let sin = SIN.get_or_init(|| {
        let mut t = Vec::with_capacity(BINS * N_FFT);
        for k in 0..BINS {
            for n in 0..N_FFT {
                t.push((-2.0 * std::f32::consts::PI * k as f32 * n as f32 / N_FFT as f32).sin());
            }
        }
        t
    });
    let pad = N_FFT / 2;
    let mut padded = Vec::with_capacity(y.len() + 2 * pad);
    for i in 0..pad {
        padded.push(y.get(pad - i).copied().unwrap_or(0.0));
    }
    padded.extend_from_slice(y);
    for i in 0..pad {
        padded.push(y.get(y.len().saturating_sub(2 + i)).copied().unwrap_or(0.0));
    }
    let w = hann();
    let frames = 1 + (padded.len() as isize - N_FFT as isize).max(0) as usize / HOP;
    let mut mags = vec![vec![0.0f32; frames]; BINS];
    for t in 0..frames {
        let off = t * HOP;
        let mut frame = [0f32; N_FFT];
        for n in 0..N_FFT {
            frame[n] = padded.get(off + n).copied().unwrap_or(0.0) * w[n];
        }
        for k in 0..BINS {
            let (mut re, mut im) = (0.0f64, 0.0f64);
            for n in 0..N_FFT {
                re += frame[n] as f64 * cos[k * N_FFT + n] as f64;
                im += frame[n] as f64 * sin[k * N_FFT + n] as f64;
            }
            mags[k][t] = (re * re + im * im).sqrt() as f32;
        }
    }
    mags
}

/// librosa.filters.mel(sr=16000, n_fft=800, n_mels=80, fmin=55, fmax=7600):
/// Slaney mel scale (linear under 1 kHz, log above) + slaney area norm.
/// Returns row-major [n_mels][n_fft/2+1].
fn mel_basis() -> &'static Vec<Vec<f64>> {
    static B: OnceLock<Vec<Vec<f64>>> = OnceLock::new();
    B.get_or_init(|| {
        let hz_to_mel = |f: f64| {
            let f_sp = 200.0 / 3.0;
            let min_log_hz = 1000.0;
            let logstep = 6.4f64.ln() / 27.0;
            if f < min_log_hz {
                f / f_sp
            } else {
                (min_log_hz / f_sp) + (f / min_log_hz).ln() / logstep
            }
        };
        let mel_to_hz = |m: f64| {
            let f_sp = 200.0 / 3.0;
            let min_log_hz = 1000.0;
            let min_log_mel = min_log_hz / f_sp;
            let logstep = 6.4f64.ln() / 27.0;
            if m < min_log_mel {
                m * f_sp
            } else {
                min_log_hz * (logstep * (m - min_log_mel)).exp()
            }
        };
        let bins = N_FFT / 2 + 1;
        let fft_freqs: Vec<f64> = (0..bins)
            .map(|i| i as f64 * SR as f64 / N_FFT as f64)
            .collect();
        let (lo, hi) = (hz_to_mel(FMIN), hz_to_mel(FMAX));
        let mel_pts: Vec<f64> = (0..N_MELS + 2)
            .map(|i| lo + (hi - lo) * i as f64 / (N_MELS + 1) as f64)
            .collect();
        let hz_pts: Vec<f64> = mel_pts.iter().map(|&m| mel_to_hz(m)).collect();
        let mut basis = vec![vec![0.0f64; bins]; N_MELS];
        for m in 0..N_MELS {
            let (l, c, r) = (hz_pts[m], hz_pts[m + 1], hz_pts[m + 2]);
            let enorm = 2.0 / (hz_pts[m + 2] - hz_pts[m]);
            for k in 0..bins {
                let f = fft_freqs[k];
                basis[m][k] = enorm * (((f - l) / (c - l)).min((r - f) / (r - c))).max(0.0);
            }
        }
        basis
    })
}

/// 80×T normalized mel spectrogram, ready for the model (values in ±4).
fn mel_spectrogram(wav: &[f32]) -> Vec<Vec<f32>> {
    let y = preemphasis(wav);
    let mag = stft_mag(&y);
    let basis = mel_basis();
    let t = mag[0].len();
    let min_level = (MIN_LEVEL_DB / 20.0 * 10f64.ln()).exp() as f32; // 1e-5
    let mut mel = vec![vec![0f32; t]; N_MELS];
    for m in 0..N_MELS {
        for tt in 0..t {
            let mut s = 0f64;
            for k in 0..mag.len() {
                s += basis[m][k] * mag[k][tt] as f64;
            }
            let db = 20.0 * (s.max(min_level as f64)).log10() - REF_LEVEL_DB;
            mel[m][tt] = ((2.0 * MAX_ABS) * ((db - MIN_LEVEL_DB) / -MIN_LEVEL_DB) - MAX_ABS)
                .clamp(-MAX_ABS, MAX_ABS) as f32;
        }
    }
    mel
}

/// `mel_idx_multiplier = 80 / fps` per output frame; last chunk pinned to the
/// tail — same slicing as `inference.py`.
fn mel_chunks(mel: &[Vec<f32>], fps: u32) -> Vec<Vec<f32>> {
    let t = mel[0].len();
    let mult = 80.0 / fps as f64;
    let mut chunks = Vec::new();
    let mut i = 0usize;
    loop {
        let start = (i as f64 * mult) as usize;
        if start + MEL_STEP > t {
            let begin = t.saturating_sub(MEL_STEP);
            chunks.push(
                mel.iter()
                    .map(|r| r[begin..begin + MEL_STEP].to_vec())
                    .collect::<Vec<_>>()
                    .concat(),
            );
            break;
        }
        chunks.push(
            mel.iter()
                .map(|r| r[start..start + MEL_STEP].to_vec())
                .collect::<Vec<_>>()
                .concat(),
        );
        i += 1;
    }
    chunks
}

// ---- face crop / paste ------------------------------------------------------

/// Pixel-space face rectangle (detected box + Wav2Lip pads, clamped).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FaceRect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

/// Centered square fallback crop — used only when the YuNet detector is
/// missing or finds no face. `size`/`cy` are fractions; box clamped.
pub fn face_box(w: u32, h: u32, size: f64, cx: f64, cy: f64) -> FaceRect {
    let side = ((w.min(h) as f64) * size.clamp(0.2, 1.0)).round().max(1.0) as u32;
    let x = (w as f64 * cx.clamp(0.0, 1.0) - side as f64 / 2.0)
        .round()
        .clamp(0.0, (w - side) as f64) as u32;
    let y = (h as f64 * cy.clamp(0.0, 1.0) - side as f64 / 2.0)
        .round()
        .clamp(0.0, (h - side) as f64) as u32;
    FaceRect {
        x,
        y,
        w: side.min(w),
        h: side.min(h),
    }
}

/// Wav2Lip `inference.py` pads `0 10 0 0` = (top, bottom, left, right) added
/// to the detected rect at the original frame scale, then clamped.
fn pad_rect(r: FaceRect, top: i64, bottom: i64, left: i64, right: i64, w: u32, h: u32) -> FaceRect {
    let x = (r.x as i64 - left).max(0) as u32;
    let y = (r.y as i64 - top).max(0) as u32;
    let x2 = (r.x as i64 + r.w as i64 + right).min(w as i64) as u32;
    let y2 = (r.y as i64 + r.h as i64 + bottom).min(h as i64) as u32;
    FaceRect {
        x,
        y,
        w: x2.saturating_sub(x).max(1),
        h: y2.saturating_sub(y).max(1),
    }
}

// ---- YuNet detection --------------------------------------------------------

/// One decoded detection: pixel-space rect + score (image coordinates).
fn yunet_decode(
    outs: &rightkit_onnx::ort::session::SessionOutputs<'_>,
    iw: u32,
    ih: u32,
) -> Vec<(FaceRect, f32)> {
    let strides = [8usize, 16, 32];
    let mut dets = Vec::new();
    let sig = |v: f32| 1.0 / (1.0 + (-v).exp());
    for s in strides {
        let get = |name: &str| outs[name].try_extract_tensor::<f32>().ok().map(|(_, v)| v);
        let (Some(cls), Some(obj), Some(bbox)) = (
            get(&format!("cls_{s}")),
            get(&format!("obj_{s}")),
            get(&format!("bbox_{s}")),
        ) else {
            continue;
        };
        let gw = (iw / s as u32) as usize;
        let gh = (ih / s as u32) as usize;
        for gy in 0..gh {
            for gx in 0..gw {
                let i = gy * gw + gx;
                let score = sig(cls[i]) * sig(obj[i]);
                if score < DETECT_SCORE {
                    continue;
                }
                let (l, t, r, b) = (
                    bbox[i * 4],
                    bbox[i * 4 + 1],
                    bbox[i * 4 + 2],
                    bbox[i * 4 + 3],
                );
                let sf = s as f32;
                let x1 = (gx as f32 + 0.5 - l) * sf;
                let y1 = (gy as f32 + 0.5 - t) * sf;
                let x2 = (gx as f32 + 0.5 + r) * sf;
                let y2 = (gy as f32 + 0.5 + b) * sf;
                let (x, y) = (x1.max(0.0) as u32, y1.max(0.0) as u32);
                dets.push((
                    FaceRect {
                        x,
                        y,
                        w: (x2.min(iw as f32) as u32).saturating_sub(x).max(1),
                        h: (y2.min(ih as f32) as u32).saturating_sub(y).max(1),
                    },
                    score,
                ));
            }
        }
    }
    dets.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
    // Greedy NMS (IoU 0.3), keep best few — single-face portraits only need #1.
    let mut keep: Vec<(FaceRect, f32)> = Vec::new();
    'next: for d in dets {
        for k in &keep {
            let (a, b) = (d.0, k.0);
            let ix = (a.x + a.w).min(b.x + b.w).saturating_sub(a.x.max(b.x));
            let iy = (a.y + a.h).min(b.y + b.h).saturating_sub(a.y.max(b.y));
            let inter = (ix * iy) as f32;
            let union = (a.w * a.h + b.w * b.h) as f32 - inter;
            if union > 0.0 && inter / union > 0.3 {
                continue 'next;
            }
        }
        keep.push(d);
    }
    keep
}

/// Detect the largest face in `portrait`. Returns a rect in portrait pixels.
fn detect_face(portrait: &RgbImage) -> Option<FaceRect> {
    let path = detector_path();
    sha_verified(&path, DETECTOR_BYTES, DETECTOR_SHA256).ok()?;
    ensure_ort().ok()?;
    let mut session = Session::builder(rightkit_onnx::ort_environment().ok()?)
        .ok()?
        .with_optimization_level(GraphOptimizationLevel::Level3)
        .ok()?
        .commit_from_file(&path)
        .ok()?;
    let (w, h) = portrait.dimensions();
    let resized = imageops::resize(
        portrait,
        DETECT_IN,
        DETECT_IN,
        imageops::FilterType::Triangle,
    );
    // YuNet takes BGR, unnormalised 0..255 (mmdet img_norm_cfg mean=0 std=1).
    let mut data = Vec::with_capacity(3 * (DETECT_IN * DETECT_IN) as usize);
    for c in 0..3usize {
        for p in resized.pixels() {
            data.push(p[2 - c] as f32);
        }
    }
    let input =
        Tensor::from_array((vec![1i64, 3, DETECT_IN as i64, DETECT_IN as i64], data)).ok()?;
    let outs = session
        .run(rightkit_onnx::ort::inputs!["input" => input])
        .ok()?;
    let best = yunet_decode(&outs, DETECT_IN, DETECT_IN)
        .into_iter()
        .next()?;
    // Scale back to portrait coordinates.
    let (sx, sy) = (w as f32 / DETECT_IN as f32, h as f32 / DETECT_IN as f32);
    let r = best.0;
    Some(FaceRect {
        x: (r.x as f32 * sx) as u32,
        y: (r.y as f32 * sy) as u32,
        w: ((r.w as f32 * sx) as u32).max(1).min(w),
        h: ((r.h as f32 * sy) as u32).max(1).min(h),
    })
}

/// NCHW `(6,96,96)` input in **BGR** (cv2 order, as the checkpoint was
/// trained): masked face (lower half zeroed) then full face, /255.
fn vid_tensor(face: &RgbImage) -> Vec<f32> {
    let mut out = Vec::with_capacity(6 * FACE * FACE);
    for c in 0..6 {
        let masked = c < 3;
        let bgr = c % 3; // tensor channel c%3 is B,G,R → pixel index 2-c%3
        for y in 0..FACE {
            for x in 0..FACE {
                let p = face.get_pixel(x as u32, y as u32);
                out.push(if masked && y >= FACE / 2 {
                    0.0
                } else {
                    p[2 - bgr] as f32 / 255.0
                });
            }
        }
    }
    out
}

/// Decode the model output `(3,96,96)` **BGR** floats into an RGB image.
fn pred_image(gen: &[f32]) -> RgbImage {
    let mut pred = RgbImage::new(FACE as u32, FACE as u32);
    for c in 0..3 {
        for y in 0..FACE {
            for x in 0..FACE {
                let v = (gen[c * FACE * FACE + y * FACE + x] * 255.0).clamp(0.0, 255.0) as u8;
                pred.get_pixel_mut(x as u32, y as u32)[2 - c] = v;
            }
        }
    }
    pred
}

/// Gain-bias colour match: shift `patch` per-channel mean/std toward `src`
/// so the model output blends into the frame's lighting.
fn color_match(patch: &mut RgbImage, src: &RgbImage) {
    for c in 0..3 {
        let stat = |img: &RgbImage| {
            let (mut n, mut s1, mut s2) = (0f64, 0f64, 0f64);
            for p in img.pixels() {
                let v = p[c] as f64;
                n += 1.0;
                s1 += v;
                s2 += v * v;
            }
            let mean = s1 / n;
            let std = ((s2 / n) - mean * mean).sqrt();
            (mean, std)
        };
        let (ms, ss) = stat(src);
        let (mp, sp) = stat(patch);
        let gain = if sp > 1e-3 {
            (ss / sp).clamp(0.5, 2.0)
        } else {
            1.0
        };
        for p in patch.pixels_mut() {
            p[c] = ((p[c] as f64 - mp) * gain + ms).clamp(0.0, 255.0) as u8;
        }
    }
}

/// Feathered elliptical paste: alpha is 1 deep inside the face rect and
/// rolls off smoothly toward the edge — no rectangular seam anywhere.
fn feathered_paste(frame: &mut RgbImage, patch: &RgbImage, r: FaceRect) {
    let (cx, cy) = (r.w as f64 / 2.0, r.h as f64 / 2.0);
    let (rx, ry) = (r.w as f64 / 2.0, r.h as f64 / 2.0);
    // Edge feather width as a fraction of the rect (~12%).
    const FEATHER: f64 = 0.12;
    for y in 0..r.h {
        for x in 0..r.w {
            let dx = (x as f64 + 0.5 - cx) / rx;
            let dy = (y as f64 + 0.5 - cy) / ry;
            let d = (dx * dx + dy * dy).sqrt();
            // alpha: 1 for d <= 1-FEATHER, smooth cosine ramp to 0 at d=1.
            let a = if d >= 1.0 {
                0.0
            } else if d <= 1.0 - FEATHER {
                1.0
            } else {
                0.5 * (1.0 + (std::f64::consts::PI * (d - (1.0 - FEATHER)) / FEATHER).cos())
            };
            if a <= 0.0 {
                continue;
            }
            let sp = patch.get_pixel(x, y);
            let dp = frame.get_pixel_mut(r.x + x, r.y + y);
            for c in 0..3 {
                dp[c] = (dp[c] as f64 * (1.0 - a) + sp[c] as f64 * a).round() as u8;
            }
        }
    }
}

// ---- ffmpeg -----------------------------------------------------------------

/// Decode any audio container to mono 16 kHz s16 samples.
fn decode_audio_16k(audio: &Path) -> Result<Vec<f32>, ProviderError> {
    // Raw PCM to stdout, not a WAV file, so `pcm_s16le_16khz_mono_args` does not apply here.
    let mut decode = FfmpegCommand::base("error");
    decode
        .input(audio)
        .texts([
            "-f",
            "s16le",
            "-ac",
            "1",
            "-acodec",
            "pcm_s16le",
            "-ar",
            "16000",
        ])
        .text("-");
    let out = run_captured(
        ffmpeg()?.ffmpeg_path(),
        decode.to_os_args(),
        &RunOptions::new(Duration::from_secs(600))
            .stdout_cap(256 * 1024 * 1024)
            .stderr_cap(256 * 1024 * 1024),
    )
    .map_err(|e| bad("ffmpeg_missing", format!("cannot run ffmpeg: {e}")))?;
    if out.timed_out {
        return Err(bad(
            "ffmpeg_missing",
            "cannot run ffmpeg: child exceeded its time limit and was terminated",
        ));
    }
    if !out.status.success() {
        return Err(bad(
            "invalid_request",
            format!(
                "ffmpeg could not decode {}: {}",
                audio.display(),
                String::from_utf8_lossy(&out.stderr.bytes)
            ),
        ));
    }
    Ok(out
        .stdout
        .bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| i16::from_le_bytes([c[0], c[1]]) as f32 / i16::MAX as f32)
        .collect())
}

/// `ffmpeg -encoders` for the located binary, cached once a probe succeeds. A failed probe is
/// reported as itself (`ffmpeg_failed`), never as a missing encoder.
fn encoder_listing() -> Result<String, ProviderError> {
    static CAPS: OnceLock<String> = OnceLock::new();
    if let Some(text) = CAPS.get() {
        return Ok(text.clone());
    }
    let handle = ffmpeg()?;
    let text = h264::probe_listing(&handle).map_err(|e| bad("ffmpeg_failed", e))?;
    let _ = CAPS.set(text.clone());
    Ok(text)
}

/// `h264_encoder_unavailable` for lip-sync, with the same details as compose and Motion. The
/// message says why MPEG-4 is not offered here, so the caller is not pointed at a flag that
/// lip-sync does not honour.
fn h264_unavailable(listing: &str, ffmpeg_message: Option<&str>) -> ProviderError {
    let handle = ffmpeg().ok();
    let path = handle.as_ref().map(|h| h.ffmpeg_path());
    ProviderError::new(
        h264::CODE,
        format!(
            "local-wav2lip needs the platform H.264 encoder ({}) in the resolved ffmpeg; it does not produce MPEG-4 because its output cannot report a fallback; details={}",
            h264::platform_encoder(),
            h264::details(path, listing, ffmpeg_message)
        ),
        false,
    )
}

/// Renders the lip-sync video through the shared H.264 rule (`rightkit_ffmpeg::h264_rule`): the
/// platform hardware encoder; an encoder that refuses to start (an initialisation error in
/// ffmpeg's own output) gets one more H.264 candidate, if any. Lip-sync never produces MPEG-4:
/// the provider output has no field that could report the fallback, so the typed error is the
/// honest answer. Any other encode failure is returned as `encoder_failed`, unchanged.
fn render(req: &GenRequest) -> Result<Vec<u8>, ProviderError> {
    let listing = encoder_listing()?;
    let first = match h264::decide(&listing, None, false) {
        h264::Pick::H264(name) => name,
        _ => return Err(h264_unavailable(&listing, None)),
    };
    match render_with_encoder(req, first) {
        Err(e) if e.code == ENCODER_INIT_FAILED => match h264::decide(&listing, Some(first), false)
        {
            h264::Pick::H264(next) => match render_with_encoder(req, next) {
                Err(again) if again.code == ENCODER_INIT_FAILED => {
                    Err(h264_unavailable(&listing, Some(again.message.as_str())))
                }
                other => other,
            },
            _ => Err(h264_unavailable(&listing, Some(e.message.as_str()))),
        },
        other => other,
    }
}

/// Internal code: the H.264 encoder refused to start. Never returned to a caller.
const ENCODER_INIT_FAILED: &str = "encoder_init_failed";

/// Last lines of an encoder's stderr, truncated for job logs and error messages.
fn stderr_tail(text: &str) -> String {
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    let tail = lines[lines.len().saturating_sub(8)..].join(" | ");
    if tail.chars().count() > 1200 {
        tail.chars()
            .rev()
            .take(1200)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect()
    } else {
        tail
    }
}

/// The encode command. Frames arrive on stdin (`-i -`), so stdin is piped (`FfmpegCommand::stdin_piped`,
/// which drops `-nostdin`) and the owner's `run_with_stdin` feeds it, as Motion does.
fn ffmpeg_command(fps: u32, audio: &Path, out: &Path, enc: &str) -> FfmpegCommand {
    let mut cmd = FfmpegCommand::base("error");
    cmd.stdin_piped(true);
    cmd.texts(["-f", "image2pipe", "-framerate"])
        .text(fps.to_string())
        .texts(["-c:v", "ppm"])
        .texts(["-i", "-"])
        .input(audio)
        .texts(["-map", "0:v", "-map", "1:a"])
        // RightKit ruling 12b1a381: #14 valid composition
        // rightkit-ffmpeg 0.2.9 has no even-dimension/yuv420p helper (checked); plain builder filter kept.
        .texts(["-vf", "scale=trunc(iw/2)*2:trunc(ih/2)*2,format=yuv420p"])
        .text("-c:v")
        .text(enc)
        .texts(["-c:a", "aac", "-b:a", "192k", "-shortest"]);
    if enc == "h264_videotoolbox" {
        cmd.texts(["-allow_sw", "1"]);
    }
    if enc == "libx264" {
        cmd.texts(["-preset", "veryfast", "-crf", "20"]);
    }
    cmd.texts(["-movflags", "+faststart"]).path(out);
    cmd
}

fn ppm_header(w: u32, h: u32) -> Vec<u8> {
    format!("P6\n{w} {h}\n255\n").into_bytes()
}

// ---- session / render -------------------------------------------------------

fn ensure_ort() -> Result<(), ProviderError> {
    // Cached per process: the classified failure (not_installed / runtime_load_failed) is replayed.
    static INIT: OnceLock<Result<(), ProviderError>> = OnceLock::new();
    INIT.get_or_init(|| {
        crate::qts::models::init_onnxruntime()
            .map(|_| ())
            .map_err(|e| ort_init_failure(&e))
    })
    .clone()
}

/// Session-builder and model-load failures on a file that verified by size. Non-retryable.
fn ort_err(e: impl std::fmt::Display) -> ProviderError {
    ProviderError::new("model_load_failed", format!("onnxruntime: {e}"), false)
}

fn open_session() -> Result<Session, ProviderError> {
    ensure_ort()?;
    let path = model_path();
    model_verified(&path).map_err(|e| bad("models_invalid", e))?;
    Session::builder(rightkit_onnx::ort_environment().map_err(ort_err)?)
        .map_err(ort_err)?
        .with_optimization_level(GraphOptimizationLevel::Level3)
        .map_err(ort_err)?
        .with_intra_threads(num_cpus())
        .map_err(ort_err)?
        .commit_from_file(&path)
        .map_err(|e| model_load_failed(&path, format!("onnxruntime: {e}")))
}

fn num_cpus() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
}

fn render_with_encoder(req: &GenRequest, encoder: &str) -> Result<Vec<u8>, ProviderError> {
    let p = &req.params;
    let image_path = p
        .get("image_path")
        .and_then(Value::as_str)
        .ok_or_else(|| bad("invalid_request", "image_path is required"))?;
    let audio_path = p
        .get("audio_path")
        .and_then(Value::as_str)
        .ok_or_else(|| bad("invalid_request", "audio_path is required"))?;
    let image_path = Path::new(image_path);
    let audio_path = Path::new(audio_path);
    if !image_path.is_file() {
        return Err(bad(
            "invalid_request",
            format!("image not found: {}", image_path.display()),
        ));
    }
    if !audio_path.is_file() {
        return Err(bad(
            "invalid_request",
            format!("audio not found: {}", audio_path.display()),
        ));
    }
    let fps = p
        .get("fps")
        .and_then(Value::as_u64)
        .map(|f| f as u32)
        .unwrap_or(DEFAULT_FPS)
        .clamp(1, 60);
    let size = p.get("face_size").and_then(Value::as_f64).unwrap_or(0.62);
    let cx = p.get("face_cx").and_then(Value::as_f64).unwrap_or(0.5);
    let cy = p.get("face_cy").and_then(Value::as_f64).unwrap_or(0.42);

    let portrait = image::open(image_path)
        .map_err(|e| {
            bad(
                "invalid_request",
                format!("cannot decode {}: {e}", image_path.display()),
            )
        })?
        .to_rgb8();
    let (w, h) = portrait.dimensions();
    // YuNet detection + Wav2Lip pads (0,10,0,0); manual/center crop fallback.
    let box_ = match detect_face(&portrait) {
        Some(r) => pad_rect(r, 0, 10, 0, 0, w, h),
        None => face_box(w, h, size, cx, cy),
    };
    let crop = imageops::crop_imm(&portrait, box_.x, box_.y, box_.w, box_.h).to_image();
    let face96 = imageops::resize(
        &crop,
        FACE as u32,
        FACE as u32,
        imageops::FilterType::Triangle,
    );

    let wav = decode_audio_16k(audio_path)?;
    if wav.len() < N_FFT {
        return Err(bad("invalid_request", "audio is too short (< 50 ms)"));
    }
    let mel = mel_spectrogram(&wav);
    let chunks = mel_chunks(&mel, fps);
    let vid = vid_tensor(&face96);

    let mut session = open_session()?;

    // Encode pipe: frames stream in as PPM while audio muxes from file.
    let out_dir = std::env::temp_dir().join(format!("rightkit-lipsync-{}", std::process::id()));
    rightkit_fs::ensure_private_dir(&out_dir).map_err(|e| bad("io", e.to_string()))?;
    let out_mp4 = out_dir.join(format!("talking-{}.mp4", uuid::Uuid::new_v4()));
    struct TempVideo(PathBuf);
    impl Drop for TempVideo {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
    let _temp = TempVideo(out_mp4.clone());
    let command = ffmpeg_command(fps, audio_path, &out_mp4, encoder);
    // The owner's runner (`rightkit_ffmpeg::runner::run_with_stdin`) owns the child: spawn, hidden console,
    // bounded stderr tail, tree kill. Its feed thread writes the frames this loop sends over the channel.
    let handle = ffmpeg()?;
    let (frames_tx, frames_rx) = std::sync::mpsc::sync_channel::<Vec<u8>>(4);
    let runner = std::thread::spawn(move || {
        rightkit_ffmpeg::runner::run_with_stdin(
            &handle,
            &command,
            rightkit_ffmpeg::runner::RunOptions::unbounded(
                rightkit_ffmpeg::CancellationToken::new(),
            ),
            move |mut stdin| {
                for frame in frames_rx {
                    stdin.write_all(&frame)?;
                }
                Ok(())
            },
        )
    });
    let head = ppm_header(w, h);

    let mut frame_err: Option<String> = None;
    for chunk in &chunks {
        let mel_in = match Tensor::from_array((
            vec![1i64, 1, N_MELS as i64, MEL_STEP as i64],
            chunk.clone(),
        )) {
            Ok(t) => t,
            Err(e) => {
                frame_err = Some(format!("tensor: {e}"));
                break;
            }
        };
        let vid_in =
            match Tensor::from_array((vec![1i64, 6, FACE as i64, FACE as i64], vid.clone())) {
                Ok(t) => t,
                Err(e) => {
                    frame_err = Some(format!("tensor: {e}"));
                    break;
                }
            };
        let outs = match session.run(rightkit_onnx::ort::inputs!["mel" => mel_in, "vid" => vid_in])
        {
            Ok(o) => o,
            Err(e) => {
                frame_err = Some(format!("inference: {e}"));
                break;
            }
        };
        let gen = match outs[0].try_extract_tensor::<f32>() {
            Ok((_, v)) => v,
            Err(e) => {
                frame_err = Some(format!("output: {e}"));
                break;
            }
        };
        let mut resized = imageops::resize(
            &pred_image(gen),
            box_.w,
            box_.h,
            imageops::FilterType::Triangle,
        );
        color_match(&mut resized, &crop);
        let mut frame = portrait.clone();
        feathered_paste(&mut frame, &resized, box_);
        let mut message = Vec::with_capacity(head.len() + frame.len());
        message.extend_from_slice(&head);
        message.extend_from_slice(&frame);
        if frames_tx.send(message).is_err() {
            frame_err = Some("ffmpeg pipe broke".into());
            break;
        }
    }
    drop(frames_tx);
    let run = runner
        .join()
        .map_err(|_| bad("io", "the encode thread panicked"))?;
    let (exit_code, exited_ok, ff_stderr): (Option<i32>, bool, String) = match run {
        Ok(_) => (Some(0), true, String::new()),
        Err(rightkit_ffmpeg::runner::RunError::Failed {
            status,
            stderr_tail,
        }) => (status.code(), false, stderr_tail),
        Err(rightkit_ffmpeg::runner::RunError::Spawn(e)) => {
            return Err(bad("ffmpeg_missing", format!("cannot run ffmpeg: {e}")))
        }
        Err(other) => (None, false, other.to_string()),
    };
    // Only ffmpeg's own initialisation error marks the H.264 encoder as refused; a pipe closed
    // for any other reason, or any other exit, keeps the plain encoder_failed code and stderr.
    let init_refused = encoder != "mpeg4" && h264::is_init_failure(&ff_stderr);
    let failed_code = if init_refused {
        ENCODER_INIT_FAILED
    } else {
        "encoder_failed"
    };
    if let Some(e) = frame_err {
        if e == "ffmpeg pipe broke" {
            return Err(ProviderError::new(
                failed_code,
                format!(
                    "{encoder} encode failed (pipe closed, exit {:?}): {}",
                    exit_code,
                    stderr_tail(&ff_stderr)
                ),
                false,
            ));
        }
        return Err(ProviderError::new("unavailable", e, true));
    }
    if !exited_ok || !out_mp4.is_file() {
        return Err(ProviderError::new(
            failed_code,
            format!(
                "{encoder} encode failed (exit {:?}): {}",
                exit_code,
                stderr_tail(&ff_stderr)
            ),
            false,
        ));
    }
    let bytes = std::fs::read(&out_mp4).map_err(|e| bad("io", e.to_string()))?;
    let _ = std::fs::remove_file(&out_mp4);
    Ok(bytes)
}

impl Provider for LocalLipsync {
    fn id(&self) -> &'static str {
        ID
    }

    fn models(&self) -> &'static [ModelInfo] {
        &MODELS
    }

    fn estimate_usd(&self, req: &GenRequest) -> Result<f64, ProviderError> {
        if req.kind != Kind::Avatar {
            return Err(bad(
                "invalid_request",
                "local-wav2lip only produces avatar video",
            ));
        }
        if req.model != MODEL {
            return Err(bad(
                "invalid_request",
                format!("unknown local-wav2lip model {}", req.model),
            ));
        }
        Ok(0.0)
    }

    fn submit(&self, req: &GenRequest, _api_key: &str) -> Result<SubmitOutcome, ProviderError> {
        self.estimate_usd(req)?;
        runtime_check()?;
        let mp4 = render(req)?;
        Ok(SubmitOutcome::Completed {
            outputs: vec![Output::Bytes {
                bytes: mp4,
                mime: "video/mp4".into(),
            }],
        })
    }

    fn poll(&self, _req: &GenRequest, _id: &str, _key: &str) -> Result<PollOutcome, ProviderError> {
        Err(bad(
            "invalid_request",
            "local-wav2lip completes synchronously; nothing to poll",
        ))
    }

    /// `render`: `params.image_path` and `params.audio_path` are required local files.
    fn capabilities(&self, _model: &str) -> crate::capabilities::Capabilities {
        crate::capabilities::Capabilities::default()
            .mode("image_and_audio")
            .input("image", crate::capabilities::io("image_path", "path", true))
            .input("audio", crate::capabilities::io("audio_path", "path", true))
    }

    fn needs_key(&self) -> bool {
        false
    }
    /// Local engine: a submit has no remote charge, so a replay is free.
    fn submit_is_idempotent(&self) -> bool {
        true
    }

    fn test_key(&self, _api_key: &str) -> Result<(), ProviderError> {
        runtime_check()
    }

    fn runtime_ready(&self) -> Option<Result<(), ProviderError>> {
        Some(runtime_check())
    }

    fn fetch(&self, _output: &Output) -> Result<(Vec<u8>, String), ProviderError> {
        Err(bad(
            "invalid_request",
            "local-wav2lip returns bytes directly",
        ))
    }
}
