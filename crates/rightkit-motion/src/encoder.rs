//! Encoder selection and ffmpeg argument construction for a motion render.
//!
//! Everything here is pure: it builds argument vectors and reads `ffmpeg -encoders` listings, and
//! runs nothing except the one optional inventory probe.
//!
//! Rules (ported from GenRight's `motion_encode.rs` and the shared H.264 rule):
//!
//! 1. H.264 uses the platform hardware encoder the located ffmpeg lists (the approved LGPL builds
//!    carry no software H.264 encoder; `libx264` is GPL and never picked). The pick order is
//!    `rightkit_ffmpeg::pick_h264`.
//! 2. With no usable H.264 encoder, or one that refuses to start, the render fails with
//!    `h264_encoder_unavailable`. MPEG-4 Part 2 is produced only when the caller set
//!    `allow_mpeg4_fallback`, and the result then reports `fallback: true`.
//! 3. Alpha output is a separate pick: ProRes 4444 (`prores_ks`, `.mov`) or VP9 with alpha
//!    (`libvpx-vp9`, `.webm`). A missing alpha encoder is `alpha_encoder_unavailable`; it never
//!    falls back to an opaque format.

use std::path::PathBuf;
use std::time::Duration;

use rightkit_ffmpeg::{pick_h264, EncoderInventory, FfmpegBinary, FfmpegCommand};
use serde::Serialize;

use crate::render::RenderError;

/// Render quality. Drives the H.264 bit rate (bits per pixel), the MPEG-4 `-q:v` and the VP9 CRF.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Quality {
    Draft,
    Standard,
    High,
}

impl Quality {
    /// `draft`, `standard` or `high`.
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "draft" => Some(Self::Draft),
            "standard" => Some(Self::Standard),
            "high" => Some(Self::High),
            _ => None,
        }
    }

    /// Target bits per pixel per frame for hardware H.264: 0.06, 0.12, 0.25.
    pub fn bits_per_pixel(self) -> f64 {
        match self {
            Self::High => 0.25,
            Self::Standard => 0.12,
            Self::Draft => 0.06,
        }
    }

    /// MPEG-4 Part 2 `-q:v` (lower is better): 6, 3, 1.
    pub fn mpeg4_qscale(self) -> u32 {
        match self {
            Self::High => 1,
            Self::Standard => 3,
            Self::Draft => 6,
        }
    }

    /// VP9 constant-quality `-crf` (lower is better): 40, 32, 24. New in this crate; GenRight had
    /// no VP9 path.
    pub fn vp9_crf(self) -> u32 {
        match self {
            Self::High => 24,
            Self::Standard => 32,
            Self::Draft => 40,
        }
    }
}

/// The `-b:v` for hardware H.264: `width * height * fps * bits_per_pixel`, truncated. Computed in
/// integer hundredths of a bit per pixel so a float rounding error cannot cost a bit. Sizes are
/// used as given; [`build_command`] passes the even-rounded size.
pub fn target_bitrate(width: u32, height: u32, fps: u32, quality: Quality) -> u64 {
    let hundredths: u64 = match quality {
        Quality::High => 25,
        Quality::Standard => 12,
        Quality::Draft => 6,
    };
    width as u64 * height as u64 * fps as u64 * hundredths / 100
}

/// What an encoder produces.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EncoderKind {
    H264,
    Mpeg4,
    ProRes4444,
    Vp9Alpha,
}

impl EncoderKind {
    /// Whether frames must carry alpha (PAM input) and the output keeps it.
    pub fn has_alpha(self) -> bool {
        matches!(self, Self::ProRes4444 | Self::Vp9Alpha)
    }

    /// Output file extension (without the dot).
    pub fn extension(self) -> &'static str {
        match self {
            Self::H264 | Self::Mpeg4 => "mp4",
            Self::ProRes4444 => "mov",
            Self::Vp9Alpha => "webm",
        }
    }
}

/// A chosen encoder.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Encoder {
    /// The ffmpeg encoder name, for `-c:v`.
    pub name: &'static str,
    /// Human label.
    pub codec: &'static str,
    pub kind: EncoderKind,
    /// True only when MPEG-4 was chosen because `allow_mpeg4_fallback` was set.
    pub fallback: bool,
}

/// ffmpeg's ProRes encoder; supports the 4444 profile with alpha.
pub const PRORES_ENCODER: &str = "prores_ks";
/// libvpx VP9; the only VP9 encoder with an alpha plane (`yuva420p`).
pub const VP9_ENCODER: &str = "libvpx-vp9";
/// MPEG-4 Part 2, a native encoder every ffmpeg build lists.
pub const MPEG4_ENCODER: &str = "mpeg4";

impl Encoder {
    /// The MPEG-4 encoder, used only after the caller asked for the fallback.
    pub fn mpeg4_fallback() -> Self {
        Self {
            name: MPEG4_ENCODER,
            codec: "MPEG-4 Part 2",
            kind: EncoderKind::Mpeg4,
            fallback: true,
        }
    }

    fn h264(name: &'static str) -> Self {
        Self {
            name,
            codec: h264_label(name),
            kind: EncoderKind::H264,
            fallback: false,
        }
    }

    fn prores_4444() -> Self {
        Self {
            name: PRORES_ENCODER,
            codec: "Apple ProRes 4444",
            kind: EncoderKind::ProRes4444,
            fallback: false,
        }
    }

    fn vp9_alpha() -> Self {
        Self {
            name: VP9_ENCODER,
            codec: "VP9 with alpha",
            kind: EncoderKind::Vp9Alpha,
            fallback: false,
        }
    }
}

fn h264_label(name: &str) -> &'static str {
    match name {
        "h264_mf" => "H.264 (Media Foundation)",
        "h264_nvenc" => "H.264 (NVENC)",
        "h264_videotoolbox" => "H.264 (VideoToolbox)",
        "h264_vaapi" => "H.264 (VA-API)",
        "libopenh264" => "H.264 (OpenH264)",
        _ => "H.264",
    }
}

/// Run `ffmpeg -hide_banner -encoders` once. A failed run is [`RenderError::Probe`], a probe
/// failure, never an encoder finding.
pub fn probe_inventory(
    binary: &dyn FfmpegBinary,
    timeout: Duration,
) -> Result<EncoderInventory, RenderError> {
    EncoderInventory::probe(binary, timeout).map_err(|e| RenderError::Probe(e.to_string()))
}

/// Which alpha encoders a build lists.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct AlphaSupport {
    pub prores_4444: bool,
    pub vp9_alpha: bool,
}

impl AlphaSupport {
    /// Read the two alpha encoders out of an inventory. This is the availability probe: a name in
    /// `-encoders` means the build can try it; whether a given start succeeds is classified at
    /// render time as [`RenderError::EncoderInit`].
    pub fn from_inventory(inventory: &EncoderInventory) -> Self {
        Self {
            prores_4444: inventory.has(PRORES_ENCODER),
            vp9_alpha: inventory.has(VP9_ENCODER),
        }
    }

    /// [`probe_inventory`] then [`AlphaSupport::from_inventory`].
    pub fn probe(binary: &dyn FfmpegBinary, timeout: Duration) -> Result<Self, RenderError> {
        Ok(Self::from_inventory(&probe_inventory(binary, timeout)?))
    }
}

/// The encoder for an opaque (MP4) render on `os` (a `std::env::consts::OS` name).
///
/// Order: the first usable hardware H.264 encoder the build lists, then a listed software one
/// (`libopenh264`), then MPEG-4 when `allow_mpeg4` is set and the build lists it. `usable` is
/// called only for a listed hardware encoder; return `false` for one that already refused to start
/// (pass `&mut |_| true` otherwise). Otherwise [`RenderError::EncoderUnavailable`].
pub fn pick_encoder(
    inventory: &EncoderInventory,
    os: &str,
    allow_mpeg4: bool,
    usable: &mut dyn FnMut(&str) -> bool,
) -> Result<Encoder, RenderError> {
    if let Some(name) = pick_h264(inventory, os, usable) {
        return Ok(Encoder::h264(name));
    }
    if allow_mpeg4 && inventory.has(MPEG4_ENCODER) {
        return Ok(Encoder::mpeg4_fallback());
    }
    Err(RenderError::EncoderUnavailable {
        kind: EncoderKind::H264,
        listed: inventory.names().to_vec(),
        ffmpeg_message: None,
    })
}

/// The alpha encoder for `kind` (`ProRes4444` or `Vp9Alpha`), or
/// [`RenderError::EncoderUnavailable`]. Any other `kind` is [`RenderError::InvalidInput`].
pub fn pick_alpha_encoder(
    inventory: &EncoderInventory,
    kind: EncoderKind,
) -> Result<Encoder, RenderError> {
    let (name, encoder) = match kind {
        EncoderKind::ProRes4444 => (PRORES_ENCODER, Encoder::prores_4444()),
        EncoderKind::Vp9Alpha => (VP9_ENCODER, Encoder::vp9_alpha()),
        other => {
            return Err(RenderError::InvalidInput(format!(
                "{other:?} is not an alpha output"
            )))
        }
    };
    if inventory.has(name) {
        Ok(encoder)
    } else {
        Err(RenderError::EncoderUnavailable {
            kind,
            listed: inventory.names().to_vec(),
            ffmpeg_message: None,
        })
    }
}

/// Whether ffmpeg's own output shows an encoder would not start. Only these messages classify a
/// failure as the encoder's; any other failure passes through unchanged.
pub fn is_init_failure(ffmpeg_output: &str) -> bool {
    const MARKERS: [&str; 6] = [
        "error while opening encoder",
        "unknown encoder",
        "encoder not found",
        "could not open encoder",
        "cannot open encoder",
        "vtcompressionsession",
    ];
    let text = ffmpeg_output.to_ascii_lowercase();
    MARKERS.iter().any(|marker| text.contains(marker))
}

/// One audio input mixed into the render.
#[derive(Clone, Debug, PartialEq)]
pub struct AudioInput {
    pub path: PathBuf,
    /// Where the clip starts on the timeline, seconds.
    pub start: f64,
    /// How much of the clip to use, seconds; `None` plays it to its end.
    pub duration: Option<f64>,
    /// Offset into the clip, seconds.
    pub media_start: f64,
    pub volume: f64,
}

/// Everything the ffmpeg command depends on.
#[derive(Clone, Debug)]
pub struct EncodeSpec {
    pub encoder: Encoder,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    /// Timeline duration, seconds; trims the mixed audio.
    pub duration: f64,
    pub quality: Quality,
    pub audio: Vec<AudioInput>,
    pub output: PathBuf,
}

fn even(v: u32) -> u32 {
    (v & !1).max(2)
}

/// The ffmpeg command for `spec`: PPM (opaque) or PAM (alpha) frames on stdin, optional mixed
/// audio, the encoder's own arguments, then the output path. It is
/// [`FfmpegCommand::stdin_piped`], ready for `run_with_stdin`.
///
/// Sizes are rounded down to even for H.264, MPEG-4 and VP9 (4:2:0); ProRes keeps the exact size.
/// VP9 alpha is video-only: `webm` audio needs libopus or libvorbis, which the LGPL catalog
/// builds do not carry, so audio inputs are refused. ProRes takes PCM audio; MP4 takes AAC.
pub fn build_command(spec: &EncodeSpec) -> Result<FfmpegCommand, RenderError> {
    if spec.fps == 0 {
        return Err(RenderError::InvalidInput("fps must be at least 1".into()));
    }
    if spec.encoder.kind == EncoderKind::Vp9Alpha && !spec.audio.is_empty() {
        return Err(RenderError::InvalidInput(
            "VP9 alpha output is video-only; render audio separately".into(),
        ));
    }
    let kind = spec.encoder.kind;
    let (w, h) = if kind == EncoderKind::ProRes4444 {
        (spec.width.max(1), spec.height.max(1))
    } else {
        (even(spec.width), even(spec.height))
    };
    let pix_fmt = match kind {
        EncoderKind::H264 | EncoderKind::Mpeg4 => "yuv420p",
        EncoderKind::ProRes4444 => "yuva444p10le",
        EncoderKind::Vp9Alpha => "yuva420p",
    };
    let fps = spec.fps.to_string();

    let mut c = FfmpegCommand::base("error");
    c.stdin_piped(true);
    c.texts([
        "-f",
        "image2pipe",
        "-framerate",
        fps.as_str(),
        "-c:v",
        if kind.has_alpha() { "pam" } else { "ppm" },
        "-i",
        "-",
    ]);
    for audio in &spec.audio {
        c.input(&audio.path);
    }

    let mut graph = vec![format!(
        "[0:v]scale={w}:{h}:flags=lanczos,format={pix_fmt}[v]"
    )];
    if !spec.audio.is_empty() {
        let mut mix = String::new();
        for (i, audio) in spec.audio.iter().enumerate() {
            let input = i + 1;
            let delay_ms = (audio.start.max(0.0) * 1000.0).round() as u64;
            let mut trim = format!("atrim=start={:.3}", audio.media_start.max(0.0));
            if let Some(d) = audio.duration {
                trim.push_str(&format!(":duration={d:.3}"));
            }
            graph.push(format!(
                "[{input}:a]{trim},asetpts=PTS-STARTPTS,adelay={delay_ms}|{delay_ms},volume={:.3}[a{i}]",
                audio.volume
            ));
            mix.push_str(&format!("[a{i}]"));
        }
        graph.push(format!(
            "{mix}amix=inputs={}:normalize=0:duration=longest,atrim=duration={:.3},aresample=48000[aout]",
            spec.audio.len(),
            spec.duration
        ));
    }
    c.text("-filter_complex").text(graph.join(";"));
    c.text("-map").text("[v]");
    if !spec.audio.is_empty() {
        c.texts(["-map", "[aout]"]);
        if kind == EncoderKind::ProRes4444 {
            c.texts(["-c:a", "pcm_s16le"]);
        } else {
            c.texts(["-c:a", "aac", "-b:a", "192k"]);
        }
    }
    c.texts(["-c:v", spec.encoder.name]);
    match kind {
        EncoderKind::Mpeg4 => {
            c.texts([
                "-q:v".to_string(),
                spec.quality.mpeg4_qscale().to_string(),
                "-pix_fmt".into(),
                pix_fmt.into(),
            ]);
        }
        EncoderKind::H264 => {
            c.texts([
                "-b:v".to_string(),
                target_bitrate(w, h, spec.fps, spec.quality).to_string(),
                "-pix_fmt".into(),
                pix_fmt.into(),
            ]);
            if spec.encoder.name == "h264_videotoolbox" {
                c.texts(["-allow_sw", "1"]);
            }
        }
        // ProRes 4444 has no rate control here: the profile fixes the quality, so `Quality` is
        // ignored. `apl0` is the vendor tag Apple's decoders expect.
        EncoderKind::ProRes4444 => {
            c.texts(["-profile:v", "4444", "-pix_fmt", pix_fmt, "-vendor", "apl0"]);
        }
        // Constant-quality VP9 (`-b:v 0 -crf N`); `-auto-alt-ref 0` is required for the alpha
        // plane to survive.
        EncoderKind::Vp9Alpha => {
            c.texts([
                "-b:v".to_string(),
                "0".into(),
                "-crf".into(),
                spec.quality.vp9_crf().to_string(),
                "-pix_fmt".into(),
                pix_fmt.into(),
                "-auto-alt-ref".into(),
                "0".into(),
                "-row-mt".into(),
                "1".into(),
            ]);
        }
    }
    c.texts(["-r", fps.as_str()]);
    if matches!(kind, EncoderKind::H264 | EncoderKind::Mpeg4) {
        c.texts(["-movflags", "+faststart"]);
    }
    c.path(&spec.output);
    Ok(c)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    const LISTING: &str = "Encoders:
 V..... = Video
 A..... = Audio
 ------
 V....D h264_videotoolbox    VideoToolbox H.264 Encoder (codec h264)
 V....D h264_mf              H264 via MediaFoundation (codec h264)
 V....D mpeg4                MPEG-4 part 2 (codec mpeg4)
 V....D prores_ks            Apple ProRes (iCodec Pro) (codec prores)
 V....D libvpx-vp9           libvpx VP9 (codec vp9)
 A....D aac                  AAC (Advanced Audio Coding)
";

    fn inv(listing: &str) -> EncoderInventory {
        EncoderInventory::parse(listing)
    }

    fn spec(encoder: Encoder, audio: Vec<AudioInput>, out: &str) -> EncodeSpec {
        EncodeSpec {
            encoder,
            width: 1281,
            height: 721,
            fps: 30,
            duration: 2.0,
            quality: Quality::Draft,
            audio,
            output: PathBuf::from(out),
        }
    }

    fn joined(spec: &EncodeSpec) -> String {
        build_command(spec).unwrap().display_lossy()
    }

    fn clip() -> AudioInput {
        AudioInput {
            path: PathBuf::from("a.wav"),
            start: 0.5,
            duration: Some(1.0),
            media_start: 0.0,
            volume: 0.8,
        }
    }

    #[test]
    fn bits_per_pixel_maps_the_three_qualities() {
        assert_eq!(Quality::High.bits_per_pixel(), 0.25);
        assert_eq!(Quality::Standard.bits_per_pixel(), 0.12);
        assert_eq!(Quality::Draft.bits_per_pixel(), 0.06);
        assert_eq!(Quality::parse("standard"), Some(Quality::Standard));
        assert_eq!(Quality::parse("ultra"), None);
        // 1280 * 720 * 30 * 0.06 = 1_658_880
        assert_eq!(target_bitrate(1280, 720, 30, Quality::Draft), 1_658_880);
        assert_eq!(target_bitrate(1280, 720, 30, Quality::High), 6_912_000);
    }

    #[test]
    fn mpeg4_and_vp9_quality_maps() {
        assert_eq!(
            [Quality::High, Quality::Standard, Quality::Draft].map(Quality::mpeg4_qscale),
            [1, 3, 6]
        );
        assert_eq!(
            [Quality::High, Quality::Standard, Quality::Draft].map(Quality::vp9_crf),
            [24, 32, 40]
        );
    }

    #[test]
    fn h264_prefers_the_platform_hardware_encoder() {
        let mac = pick_encoder(&inv(LISTING), "macos", false, &mut |_| true).unwrap();
        assert_eq!(
            (mac.name, mac.kind, mac.fallback),
            ("h264_videotoolbox", EncoderKind::H264, false)
        );
        let win = pick_encoder(&inv(LISTING), "windows", false, &mut |_| true).unwrap();
        assert_eq!(win.name, "h264_mf");
    }

    #[test]
    fn a_refused_hardware_encoder_is_skipped_then_software_then_mpeg4_only_if_allowed() {
        let only_mac = inv(" V....D h264_videotoolbox x\n V....D mpeg4 y\n");
        // The hardware encoder refused to start: not usable, MPEG-4 not allowed -> unavailable.
        let refused = pick_encoder(&only_mac, "macos", false, &mut |name| {
            name != "h264_videotoolbox"
        });
        assert!(matches!(
            refused,
            Err(RenderError::EncoderUnavailable {
                kind: EncoderKind::H264,
                ..
            })
        ));
        // Allowed: the fallback is MPEG-4 and says so.
        let fallback = pick_encoder(&only_mac, "macos", true, &mut |name| {
            name != "h264_videotoolbox"
        })
        .unwrap();
        assert_eq!(
            (fallback.name, fallback.fallback, fallback.kind),
            ("mpeg4", true, EncoderKind::Mpeg4)
        );
        // A listed software encoder beats MPEG-4.
        let soft = inv(" V....D libopenh264 s\n V....D mpeg4 y\n");
        assert_eq!(
            pick_encoder(&soft, "macos", true, &mut |_| true)
                .unwrap()
                .name,
            "libopenh264"
        );
    }

    #[test]
    fn libx264_is_never_picked_and_mpeg4_needs_the_flag() {
        let gpl = inv(" V....D libx264 x\n V....D mpeg4 y\n");
        let error = pick_encoder(&gpl, "linux", false, &mut |_| true).unwrap_err();
        assert_eq!(error.code(), "h264_encoder_unavailable");
        assert_eq!(
            pick_encoder(&inv(" V....D h264_vaapi x\n"), "linux", true, &mut |_| true)
                .unwrap()
                .name,
            "h264_vaapi"
        );
        // MPEG-4 allowed but not listed -> still unavailable.
        assert!(pick_encoder(&inv(" V....D foo x\n"), "linux", true, &mut |_| true).is_err());
    }

    #[test]
    fn the_usable_check_only_runs_for_listed_hardware_encoders() {
        let mut asked = Vec::new();
        let _ = pick_encoder(&inv(LISTING), "windows", false, &mut |name| {
            asked.push(name.to_string());
            false
        });
        // h264_nvenc is not listed in LISTING, so only h264_mf is ever asked.
        assert_eq!(asked, ["h264_mf"]);
    }

    #[test]
    fn alpha_picks_follow_the_inventory_and_never_fall_back_to_opaque() {
        let prores = pick_alpha_encoder(&inv(LISTING), EncoderKind::ProRes4444).unwrap();
        assert_eq!(
            (prores.name, prores.kind),
            ("prores_ks", EncoderKind::ProRes4444)
        );
        let vp9 = pick_alpha_encoder(&inv(LISTING), EncoderKind::Vp9Alpha).unwrap();
        assert_eq!(vp9.name, "libvpx-vp9");
        let none = inv(" V....D mpeg4 y\n V....D h264_videotoolbox x\n");
        for kind in [EncoderKind::ProRes4444, EncoderKind::Vp9Alpha] {
            let error = pick_alpha_encoder(&none, kind).unwrap_err();
            assert_eq!(error.code(), "alpha_encoder_unavailable");
        }
        assert_eq!(
            pick_alpha_encoder(&inv(LISTING), EncoderKind::H264)
                .unwrap_err()
                .code(),
            "invalid_input"
        );
    }

    #[test]
    fn the_alpha_probe_reads_both_encoders() {
        assert_eq!(
            AlphaSupport::from_inventory(&inv(LISTING)),
            AlphaSupport {
                prores_4444: true,
                vp9_alpha: true
            }
        );
        assert_eq!(
            AlphaSupport::from_inventory(&inv(" V....D prores_ks x\n")),
            AlphaSupport {
                prores_4444: true,
                vp9_alpha: false
            }
        );
        assert_eq!(
            AlphaSupport::from_inventory(&inv("")),
            AlphaSupport::default()
        );
    }

    #[test]
    fn init_failures_are_recognised_from_ffmpegs_own_output() {
        assert!(is_init_failure(
            "[h264_mf @ 0x1] Error while opening encoder for output stream"
        ));
        assert!(is_init_failure("Unknown encoder 'libvpx-vp9'"));
        assert!(!is_init_failure("Conversion failed: disk full"));
    }

    #[test]
    fn h264_args_even_the_size_and_set_the_bitrate() {
        let s = spec(Encoder::h264("h264_videotoolbox"), vec![], "out.mp4");
        let a = joined(&s);
        assert!(
            a.contains("-f image2pipe -framerate 30 -c:v ppm -i -"),
            "{a}"
        );
        assert!(
            a.contains("scale=1280:720:flags=lanczos,format=yuv420p[v]"),
            "{a}"
        );
        assert!(
            a.contains("-c:v h264_videotoolbox -b:v 1658880 -pix_fmt yuv420p -allow_sw 1"),
            "{a}"
        );
        assert!(a.contains("-r 30 -movflags +faststart out.mp4"), "{a}");
        assert!(!a.contains("-c:a") && !a.contains("-nostdin"), "{a}");
        assert!(build_command(&s).unwrap().stdin_is_piped());
        // Not VideoToolbox: no -allow_sw.
        let mf = joined(&spec(Encoder::h264("h264_mf"), vec![], "out.mp4"));
        assert!(
            mf.contains("-c:v h264_mf -b:v 1658880") && !mf.contains("allow_sw"),
            "{mf}"
        );
    }

    #[test]
    fn mpeg4_args_use_the_quality_scale_and_mix_audio() {
        let a = joined(&spec(Encoder::mpeg4_fallback(), vec![clip()], "out.mp4"));
        assert!(a.contains("-c:v mpeg4 -q:v 6 -pix_fmt yuv420p"), "{a}");
        assert!(a.contains("atrim=start=0.000:duration=1.000,asetpts=PTS-STARTPTS,adelay=500|500,volume=0.800[a0]"), "{a}");
        assert!(a.contains("[a0]amix=inputs=1:normalize=0:duration=longest,atrim=duration=2.000,aresample=48000[aout]"), "{a}");
        assert!(a.contains("-map [v] -map [aout] -c:a aac -b:a 192k"), "{a}");
        assert!(a.contains("-i a.wav"), "{a}");
    }

    #[test]
    fn prores_4444_args_use_pam_alpha_and_a_mov_container() {
        let s = spec(Encoder::prores_4444(), vec![clip()], "out.mov");
        let a = joined(&s);
        assert!(a.contains("-c:v pam -i -"), "{a}");
        // ProRes keeps the exact size.
        assert!(
            a.contains("scale=1281:721:flags=lanczos,format=yuva444p10le[v]"),
            "{a}"
        );
        assert!(
            a.contains("-c:v prores_ks -profile:v 4444 -pix_fmt yuva444p10le -vendor apl0"),
            "{a}"
        );
        assert!(a.contains("-c:a pcm_s16le"), "{a}");
        assert!(!a.contains("faststart") && !a.contains("-b:v"), "{a}");
        assert!(a.ends_with("out.mov"), "{a}");
    }

    #[test]
    fn vp9_alpha_args_use_constant_quality_and_yuva420p() {
        let mut s = spec(Encoder::vp9_alpha(), vec![], "out.webm");
        s.quality = Quality::High;
        let a = joined(&s);
        assert!(a.contains("-c:v pam -i -"), "{a}");
        assert!(
            a.contains("scale=1280:720:flags=lanczos,format=yuva420p[v]"),
            "{a}"
        );
        assert!(
            a.contains(
                "-c:v libvpx-vp9 -b:v 0 -crf 24 -pix_fmt yuva420p -auto-alt-ref 0 -row-mt 1"
            ),
            "{a}"
        );
        assert!(!a.contains("faststart"), "{a}");
        assert!(a.ends_with("out.webm"), "{a}");
    }

    #[test]
    fn vp9_alpha_refuses_audio_and_zero_fps_is_invalid() {
        let with_audio = spec(Encoder::vp9_alpha(), vec![clip()], "out.webm");
        assert_eq!(
            build_command(&with_audio).unwrap_err().code(),
            "invalid_input"
        );
        let mut zero = spec(Encoder::mpeg4_fallback(), vec![], "o.mp4");
        zero.fps = 0;
        assert_eq!(build_command(&zero).unwrap_err().code(), "invalid_input");
    }

    #[test]
    fn the_output_path_stays_a_path_argument() {
        let c = build_command(&spec(
            Encoder::mpeg4_fallback(),
            vec![],
            "dir with space/out.mp4",
        ))
        .unwrap();
        let last = c.to_os_args().pop().unwrap();
        assert_eq!(Path::new(&last), Path::new("dir with space/out.mp4"));
    }

    #[test]
    fn extensions_follow_the_kind() {
        assert_eq!(
            [
                EncoderKind::H264,
                EncoderKind::Mpeg4,
                EncoderKind::ProRes4444,
                EncoderKind::Vp9Alpha
            ]
            .map(EncoderKind::extension),
            ["mp4", "mp4", "mov", "webm"]
        );
    }
}
