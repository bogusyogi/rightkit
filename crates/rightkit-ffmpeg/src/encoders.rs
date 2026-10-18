//! H.264 encoder discovery and the one pick rule for an ffmpeg build.
//!
//! An app that writes H.264 must choose an encoder the build actually lists. The bundled
//! builds are LGPL with no software H.264 encoder, so the hardware encoder for the platform
//! is the normal choice (VideoToolbox on macOS, NVENC or Media Foundation on Windows). This
//! module lists the encoders with one `-encoders` run, keeps the listing in ffmpeg's own
//! order, and picks the first usable H.264 encoder in a fixed order. Consumers decide what
//! "usable" means (a runtime probe, or an encoder that already refused to start) through a
//! callback, and decide the fallback (for example MPEG-4) themselves.
//!
//! The same module also holds the app-facing selection policies. [`select_delivery`] keeps
//! platform-only selection: VideoToolbox on macOS; NVENC (qualified) then Media Foundation on
//! Windows. [`select_intermediate`] falls back to software ProRes HQ while preserving the
//! requested bit depth and chroma. [`select_qualified_delivery`] qualifies every candidate and
//! optionally falls back to listed `libopenh264`. No GPL encoder is selected implicitly.

use std::{
    collections::HashMap,
    fmt,
    path::PathBuf,
    sync::{LazyLock, Mutex},
    time::Duration,
};

use crate::{
    run_version_probe, CancellationToken, FfmpegBinary, FfmpegCommand, MediaError, RunError,
};

static INVENTORIES: LazyLock<Mutex<HashMap<PathBuf, EncoderInventory>>> =
    LazyLock::new(Mutex::default);

/// Forget process-lifetime encoder listings, for example after replacing a binary.
/// Already returned snapshots remain valid values but may describe an older build.
pub fn encoder_cache_clear() {
    if let Ok(mut cache) = INVENTORIES.lock() {
        cache.clear();
    }
}

/// Encoder selection or inventory failure.
#[derive(Debug)]
pub enum EncoderError {
    /// No listed encoder satisfies this request.
    Unavailable(String),
    /// Listing failed, timed out or was cancelled.
    Probe(RunError),
}

impl EncoderError {
    /// Stable code suitable for product error mapping.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Unavailable(_) => "encoder_unavailable",
            Self::Probe(_) => "encoder_probe_failed",
        }
    }
}

impl fmt::Display for EncoderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unavailable(message) => f.write_str(message),
            Self::Probe(error) => write!(f, "encoder inventory: {error}"),
        }
    }
}

impl std::error::Error for EncoderError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Probe(error) => Some(error),
            Self::Unavailable(_) => None,
        }
    }
}

/// Software H.264 encoders, tried after the hardware encoders and only when the build lists
/// them. The bundled builds never list one. `libx264` is absent on purpose: it is GPL, and
/// `--enable-libx264` is a forbidden configure flag in every catalog entry.
pub const SOFTWARE_H264_ENCODERS: &[&str] = &["libopenh264"];

/// Hardware H.264 encoders for `os` (the `std::env::consts::OS` names), in preference order.
pub fn hardware_h264_encoders(os: &str) -> &'static [&'static str] {
    match os {
        "macos" => &["h264_videotoolbox"],
        "windows" => &["h264_nvenc", "h264_mf"],
        "linux" => &["h264_vaapi"],
        _ => &[],
    }
}

/// Video encoder names in an `ffmpeg -encoders` listing, in listing order. A video row has a
/// six-character flag column starting with `V`. The legend rows (`V..... = Video`) have `=` in
/// the name column and are skipped, as are the header and the separator line.
pub fn video_encoder_names(listing: &str) -> Vec<String> {
    listing
        .lines()
        .filter_map(|line| {
            let mut columns = line.split_whitespace();
            let flags = columns.next()?;
            let name = columns.next()?;
            (flags.len() == 6 && flags.starts_with('V') && name != "=").then(|| name.to_owned())
        })
        .collect()
}

/// The video encoders one ffmpeg build lists, in the order the build lists them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EncoderInventory {
    names: Vec<String>,
}

impl EncoderInventory {
    /// Parse the text of one `-encoders` run.
    pub fn parse(listing: &str) -> Self {
        Self {
            names: video_encoder_names(listing),
        }
    }

    /// Run `-hide_banner -encoders` once against `binary`, bounded by `timeout`. A failed run
    /// is [`MediaError::BuildProbeFailed`]: it is a probe failure, not an encoder finding.
    pub fn probe(binary: &dyn FfmpegBinary, timeout: Duration) -> Result<Self, MediaError> {
        let (ok, output) = run_version_probe(
            binary.ffmpeg_path(),
            &["-hide_banner", "-encoders"],
            timeout,
        )?;
        if !ok {
            return Err(MediaError::BuildProbeFailed);
        }
        Ok(Self::parse(&output))
    }

    /// Construct an inventory from known encoder names, keeping the given order.
    pub fn from_names<I: IntoIterator<Item = S>, S: Into<String>>(names: I) -> Self {
        Self {
            names: names.into_iter().map(Into::into).collect(),
        }
    }

    /// Cached, cancellable form of [`Self::probe`] for [`EncoderError`] callers. Lists
    /// encoders once per binary path for this process, matching bundled-tool lifetime. Call
    /// [`encoder_cache_clear`] after replacing binaries, or use [`Self::probe_uncached`] for
    /// mutable tool paths. Failures are never cached. A cancelled token is refused even on
    /// cache hits. [`Self::probe`] itself never reads or updates the cache.
    pub fn probe_cached(
        binary: &dyn FfmpegBinary,
        timeout: Duration,
        cancel: &CancellationToken,
    ) -> Result<Self, EncoderError> {
        if cancel.is_cancelled() {
            return Err(EncoderError::Probe(RunError::Cancelled));
        }
        let path = binary.ffmpeg_path().to_owned();
        if let Some(hit) = INVENTORIES
            .lock()
            .ok()
            .and_then(|cache| cache.get(&path).cloned())
        {
            return Ok(hit);
        }
        let inventory = Self::probe_uncached(binary, timeout, cancel)?;
        if let Ok(mut cache) = INVENTORIES.lock() {
            cache.insert(path, inventory.clone());
        }
        Ok(inventory)
    }

    /// Run one bounded, cancellable listing without reading or updating the process-wide
    /// cache. A non-zero exit is [`RunError::Failed`]; cancellation, timeout and spawn
    /// failure stay typed runner errors.
    pub fn probe_uncached(
        binary: &dyn FfmpegBinary,
        timeout: Duration,
        cancel: &CancellationToken,
    ) -> Result<Self, EncoderError> {
        let mut command = FfmpegCommand::base("error");
        command.text("-encoders");
        let output =
            crate::run_checked(binary, &command, timeout, cancel).map_err(EncoderError::Probe)?;
        Ok(Self::parse(&String::from_utf8_lossy(&output.stdout)))
    }

    /// Video encoder names in listing order.
    pub fn names(&self) -> &[String] {
        &self.names
    }

    /// Whether the build lists `name`.
    pub fn has(&self, name: &str) -> bool {
        self.names.iter().any(|listed| listed == name)
    }
}

/// The H.264 encoder to use on `os`, or `None` when the build offers none.
///
/// The first hardware encoder from [`hardware_h264_encoders`] that the build lists and that
/// `usable` accepts wins. `usable` is called only for a listed hardware encoder, so it can run
/// a runtime probe or refuse an encoder that already failed to start; pass `&mut |_| true` when
/// no check is needed. If no hardware encoder is picked, the first listed entry of
/// [`SOFTWARE_H264_ENCODERS`] is returned.
pub fn pick_h264(
    inventory: &EncoderInventory,
    os: &str,
    usable: &mut dyn FnMut(&str) -> bool,
) -> Option<&'static str> {
    for name in hardware_h264_encoders(os).iter().copied() {
        if inventory.has(name) && usable(name) {
            return Some(name);
        }
    }
    SOFTWARE_H264_ENCODERS
        .iter()
        .copied()
        .find(|name| inventory.has(name))
}

/// H.264 delivery encoder. OpenH264 is available only through explicit software fallback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoder {
    VideoToolbox,
    Nvenc,
    MediaFoundation,
    OpenH264,
}

/// FFmpeg encoder name.
pub fn codec_name(encoder: Encoder) -> &'static str {
    match encoder {
        Encoder::VideoToolbox => "h264_videotoolbox",
        Encoder::Nvenc => "h264_nvenc",
        Encoder::MediaFoundation => "h264_mf",
        Encoder::OpenH264 => "libopenh264",
    }
}

fn unavailable(what: &str, os: &str, wanted: &[&str]) -> EncoderError {
    EncoderError::Unavailable(format!(
        "no {what} encoder for {os}: this ffmpeg build lists none of {}",
        wanted.join(", ")
    ))
}

/// Platform-only delivery selection. Only listed NVENC is qualified by `hardware_ok`;
/// platform system encoders are accepted when listed, matching CutRight's policy.
pub fn select_delivery(
    inventory: &EncoderInventory,
    os: &str,
    hardware_ok: &mut dyn FnMut(&str) -> bool,
) -> Result<Encoder, EncoderError> {
    match os {
        "macos" if inventory.has("h264_videotoolbox") => Ok(Encoder::VideoToolbox),
        "macos" => Err(unavailable("H.264", os, &["h264_videotoolbox"])),
        "windows" => {
            if inventory.has("h264_nvenc") && hardware_ok("h264_nvenc") {
                Ok(Encoder::Nvenc)
            } else if inventory.has("h264_mf") {
                Ok(Encoder::MediaFoundation)
            } else {
                Err(unavailable("H.264", os, &["h264_nvenc", "h264_mf"]))
            }
        }
        other => Err(unavailable("H.264", other, &[])),
    }
}

/// Require an explicitly chosen encoder to be listed, without executing it.
pub fn require_listed(inventory: &EncoderInventory, encoder: Encoder) -> Result<(), EncoderError> {
    let name = codec_name(encoder);
    if inventory.has(name) {
        Ok(())
    } else {
        Err(EncoderError::Unavailable(format!(
            "this ffmpeg build does not list {name}"
        )))
    }
}

/// Qualify every listed candidate in platform order, optionally trying software
/// OpenH264 last (including on other OSes). Callback runs only for listed candidates.
/// Media Foundation may itself choose hardware or software at runtime.
pub fn select_qualified_delivery(
    inventory: &EncoderInventory,
    os: &str,
    allow_software: bool,
    encoder_ok: &mut dyn FnMut(&str) -> bool,
) -> Result<Encoder, EncoderError> {
    let platform: &[Encoder] = match os {
        "macos" => &[Encoder::VideoToolbox],
        "windows" => &[Encoder::Nvenc, Encoder::MediaFoundation],
        _ => &[],
    };
    let software = allow_software.then_some(Encoder::OpenH264);
    let candidates = platform.iter().copied().chain(software);
    let mut wanted = Vec::new();
    for encoder in candidates {
        let name = codec_name(encoder);
        wanted.push(name);
        if inventory.has(name) && encoder_ok(name) {
            return Ok(encoder);
        }
    }
    Err(EncoderError::Unavailable(format!(
        "no usable H.264 encoder for {os}: tried {}",
        wanted.join(", ")
    )))
}

/// Run CutRight's short black-frame encode to qualify an encoder on this machine.
/// Spawn, timeout, cancellation & nonzero exit remain typed runner errors.
pub fn qualify_encoder(
    binary: &dyn FfmpegBinary,
    name: &str,
    timeout: Duration,
    cancel: &CancellationToken,
) -> Result<(), RunError> {
    let mut command = FfmpegCommand::base("error");
    command.texts([
        "-f",
        "lavfi",
        "-i",
        "color=c=black:s=256x256:r=30:d=0.2",
        "-c:v",
        name,
        "-f",
        "null",
        "-",
    ]);
    crate::run_checked(binary, &command, timeout, cancel).map(|_| ())
}

/// Boolean qualification for selection callbacks. Use [`qualify_encoder`] when
/// cancellation or failure details must propagate to callers.
pub fn hardware_usable(
    binary: &dyn FfmpegBinary,
    name: &str,
    timeout: Duration,
    cancel: &CancellationToken,
) -> bool {
    qualify_encoder(binary, name, timeout, cancel).is_ok()
}

/// Pixel layout an intermediate must keep.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntermediateFormat {
    /// 8-bit 4:2:0 (`yuv420p`).
    Yuv420p8,
    /// 10-bit 4:2:0 (`yuv420p10le`).
    Yuv420p10,
    /// 10-bit 4:2:2 (`yuv422p10le`).
    Yuv422p10,
}

impl IntermediateFormat {
    pub fn pix_fmt(self) -> &'static str {
        match self {
            IntermediateFormat::Yuv420p8 => "yuv420p",
            IntermediateFormat::Yuv420p10 => "yuv420p10le",
            IntermediateFormat::Yuv422p10 => "yuv422p10le",
        }
    }
}

/// A near-lossless intermediate encode: the codec name as ffprobe reports it, and the
/// encoder arguments (`-c:v ...`), without `-pix_fmt`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntermediateEncode {
    pub codec: &'static str,
    pub args: Vec<&'static str>,
}

fn intermediate(codec: &'static str, args: &[&'static str]) -> IntermediateEncode {
    IntermediateEncode {
        codec,
        args: args.to_vec(),
    }
}

/// ProRes 422 HQ from ffmpeg's own encoder: present on every platform, keeps 10-bit 4:2:2.
const PRORES_HQ: &[&str] = &["-c:v", "prores_ks", "-profile:v", "3", "-vendor", "apl0"];

/// The encoder for an editing intermediate (constant-frame-rate mezzanine, still clip)
/// that must keep `format` at visually lossless quality. Order per platform:
///
/// - 8-bit: H.264 from the platform encoder (macOS VideoToolbox; Windows NVENC, then
///   Media Foundation).
/// - 10-bit 4:2:0: HEVC Main 10 from the platform encoder (macOS VideoToolbox; Windows
///   NVENC), then ProRes.
/// - 10-bit 4:2:2: ProRes (the hardware HEVC encoders drop it to 4:2:0).
pub fn select_intermediate(
    inventory: &EncoderInventory,
    os: &str,
    format: IntermediateFormat,
    hardware_ok: &mut dyn FnMut(&str) -> bool,
) -> Result<IntermediateEncode, EncoderError> {
    let prores = |what: &str| {
        if inventory.has("prores_ks") {
            Ok(intermediate("prores", PRORES_HQ))
        } else {
            Err(unavailable(what, os, &["prores_ks"]))
        }
    };
    match (format, os) {
        (IntermediateFormat::Yuv422p10, _) => prores("10-bit 4:2:2"),
        (IntermediateFormat::Yuv420p8, "macos") if inventory.has("h264_videotoolbox") => {
            Ok(intermediate(
                "h264",
                &[
                    "-c:v",
                    "h264_videotoolbox",
                    "-profile:v",
                    "high",
                    "-q:v",
                    "80",
                ],
            ))
        }
        (IntermediateFormat::Yuv420p10, "macos") if inventory.has("hevc_videotoolbox") => {
            Ok(intermediate(
                "hevc",
                &[
                    "-c:v",
                    "hevc_videotoolbox",
                    "-profile:v",
                    "main10",
                    "-q:v",
                    "80",
                    "-tag:v",
                    "hvc1",
                ],
            ))
        }
        (IntermediateFormat::Yuv420p8, "windows")
            if inventory.has("h264_nvenc") && hardware_ok("h264_nvenc") =>
        {
            Ok(intermediate(
                "h264",
                &[
                    "-c:v",
                    "h264_nvenc",
                    "-preset",
                    "p5",
                    "-rc",
                    "vbr",
                    "-cq",
                    "14",
                    "-b:v",
                    "0",
                    "-profile:v",
                    "high",
                ],
            ))
        }
        (IntermediateFormat::Yuv420p8, "windows") if inventory.has("h264_mf") => Ok(intermediate(
            "h264",
            &[
                "-c:v",
                "h264_mf",
                "-rate_control",
                "quality",
                "-quality",
                "95",
            ],
        )),
        (IntermediateFormat::Yuv420p10, "windows")
            if inventory.has("hevc_nvenc") && hardware_ok("hevc_nvenc") =>
        {
            Ok(intermediate(
                "hevc",
                &[
                    "-c:v",
                    "hevc_nvenc",
                    "-preset",
                    "p5",
                    "-rc",
                    "vbr",
                    "-cq",
                    "14",
                    "-b:v",
                    "0",
                    "-profile:v",
                    "main10",
                    "-tag:v",
                    "hvc1",
                ],
            ))
        }
        (IntermediateFormat::Yuv420p8, _) => prores("8-bit"),
        (IntermediateFormat::Yuv420p10, _) => prores("10-bit 4:2:0"),
    }
}

/// Probe & choose an intermediate on this host using platform selection policy.
/// Each listing or NVENC qualification is bounded by `timeout`.
pub fn intermediate_for(
    binary: &dyn FfmpegBinary,
    format: IntermediateFormat,
    timeout: Duration,
    cancel: &CancellationToken,
) -> Result<IntermediateEncode, EncoderError> {
    let inventory = EncoderInventory::probe_cached(binary, timeout, cancel)?;
    select_intermediate(&inventory, std::env::consts::OS, format, &mut |name| {
        hardware_usable(binary, name, timeout, cancel)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const LISTING: &str = "Encoders:
 V..... = Video
 A..... = Audio
 S..... = Subtitle
 .F.... = Frame-level multithreading
 ..S... = Slice-level multithreading
 ------
 V....D h264_videotoolbox    VideoToolbox H.264 Encoder (codec h264)
 V....D hevc_videotoolbox    VideoToolbox H.265 Encoder (codec hevc)
 V....D h264_nvenc           NVIDIA NVENC H.264 encoder (codec h264)
 V....D h264_mf              H264 Encoder MFT (codec h264)
 A....D aac                  AAC (Advanced Audio Coding)
 V....D libopenh264          OpenH264 H.264 encoder (codec h264)
";

    #[test]
    fn listing_keeps_video_encoders_in_listing_order() {
        assert_eq!(
            video_encoder_names(LISTING),
            [
                "h264_videotoolbox",
                "hevc_videotoolbox",
                "h264_nvenc",
                "h264_mf",
                "libopenh264"
            ]
        );
        let inventory = EncoderInventory::parse(LISTING);
        assert_eq!(
            inventory.names().first().map(String::as_str),
            Some("h264_videotoolbox")
        );
        assert!(inventory.has("h264_mf"));
        assert!(!inventory.has("aac"));
        assert!(!inventory.has("libx264"));
    }

    #[test]
    fn macos_picks_videotoolbox_when_it_is_usable() {
        let inventory = EncoderInventory::parse(LISTING);
        let mut asked = Vec::new();
        let pick = pick_h264(&inventory, "macos", &mut |name| {
            asked.push(name.to_owned());
            true
        });
        assert_eq!(pick, Some("h264_videotoolbox"));
        assert_eq!(asked, ["h264_videotoolbox"]);
    }

    #[test]
    fn windows_skips_a_refused_nvenc_and_takes_media_foundation() {
        let inventory = EncoderInventory::parse(LISTING);
        let mut asked = Vec::new();
        let pick = pick_h264(&inventory, "windows", &mut |name| {
            asked.push(name.to_owned());
            name != "h264_nvenc"
        });
        assert_eq!(pick, Some("h264_mf"));
        assert_eq!(asked, ["h264_nvenc", "h264_mf"]);
    }

    #[test]
    fn software_is_used_only_when_no_listed_hardware_encoder_is_usable() {
        let inventory = EncoderInventory::parse(LISTING);
        let pick = pick_h264(&inventory, "windows", &mut |_| false);
        assert_eq!(pick, Some("libopenh264"));

        // libx264 is GPL and never a candidate, even when a development build lists it.
        let gpl_only = EncoderInventory::parse(" V....D libx264  x264 (codec h264)\n");
        assert_eq!(pick_h264(&gpl_only, "macos", &mut |_| true), None);
    }

    #[test]
    fn a_build_without_h264_yields_none() {
        let inventory = EncoderInventory::parse(" V....D mpeg4  MPEG-4 part 2\n");
        assert_eq!(pick_h264(&inventory, "macos", &mut |_| true), None);
        assert_eq!(pick_h264(&inventory, "plan9", &mut |_| true), None);
    }

    // Selection-policy tests (issue #353).

    // Unit tests: the choice is a pure function, and its failure cases (an ffmpeg build
    // without the platform encoder, a GPU that refuses NVENC) cannot be provoked from an
    // end-to-end run on the bundled ffmpeg.

    const SELECTION_LISTING: &str = "Encoders:\n V..... = Video\n ------\n V....D h264_videotoolbox     VideoToolbox H.264 Encoder (codec h264)\n V....D hevc_videotoolbox     x (codec hevc)\n VFS... prores_ks            Apple ProRes (iCodec Pro)\n A....D aac                  AAC\n";

    #[test]
    fn listing_keeps_video_encoders_only() {
        let inv = EncoderInventory::parse(SELECTION_LISTING);
        assert!(inv.has("h264_videotoolbox") && inv.has("prores_ks"));
        assert!(!inv.has("aac") && !inv.has("="));
    }

    #[test]
    fn delivery_order_is_fixed_per_platform() {
        let mac = EncoderInventory::parse(SELECTION_LISTING);
        let mut yes = |_: &str| true;
        let mut no = |_: &str| false;
        assert_eq!(
            select_delivery(&mac, "macos", &mut yes).unwrap(),
            Encoder::VideoToolbox
        );
        let win = EncoderInventory::from_names(["h264_nvenc", "h264_mf"]);
        assert_eq!(
            select_delivery(&win, "windows", &mut yes).unwrap(),
            Encoder::Nvenc
        );
        assert_eq!(
            select_delivery(&win, "windows", &mut no).unwrap(),
            Encoder::MediaFoundation
        );
        for (inv, os) in [
            (EncoderInventory::default(), "macos"),
            (EncoderInventory::from_names(["h264_nvenc"]), "windows"),
            (mac.clone(), "linux"),
        ] {
            assert_eq!(
                select_delivery(&inv, os, &mut no).unwrap_err().code(),
                "encoder_unavailable"
            );
        }
        assert_eq!(
            require_listed(&mac, Encoder::Nvenc).unwrap_err().code(),
            "encoder_unavailable"
        );
    }

    #[test]
    fn intermediate_keeps_bit_depth_and_chroma() {
        let mac = EncoderInventory::parse(SELECTION_LISTING);
        let mut yes = |_: &str| true;
        let mut no = |_: &str| false;
        let pick = |inv: &EncoderInventory, os: &str, f, ok: &mut dyn FnMut(&str) -> bool| {
            select_intermediate(inv, os, f, ok).map(|e| (e.codec, e.args[1]))
        };
        assert_eq!(
            pick(&mac, "macos", IntermediateFormat::Yuv420p8, &mut yes).unwrap(),
            ("h264", "h264_videotoolbox")
        );
        assert_eq!(
            pick(&mac, "macos", IntermediateFormat::Yuv420p10, &mut yes).unwrap(),
            ("hevc", "hevc_videotoolbox")
        );
        assert_eq!(
            pick(&mac, "macos", IntermediateFormat::Yuv422p10, &mut yes).unwrap(),
            ("prores", "prores_ks")
        );
        let win =
            EncoderInventory::from_names(["h264_nvenc", "hevc_nvenc", "h264_mf", "prores_ks"]);
        assert_eq!(
            pick(&win, "windows", IntermediateFormat::Yuv420p8, &mut no).unwrap(),
            ("h264", "h264_mf")
        );
        assert_eq!(
            pick(&win, "windows", IntermediateFormat::Yuv420p10, &mut no).unwrap(),
            ("prores", "prores_ks")
        );
        assert_eq!(
            pick(&win, "windows", IntermediateFormat::Yuv420p10, &mut yes).unwrap(),
            ("hevc", "hevc_nvenc")
        );
        assert_eq!(
            pick(
                &EncoderInventory::default(),
                "windows",
                IntermediateFormat::Yuv420p8,
                &mut no
            )
            .unwrap_err()
            .code(),
            "encoder_unavailable"
        );
    }

    #[test]
    fn qualified_delivery_checks_system_encoders_then_software() {
        let inventory = EncoderInventory::from_names([
            "h264_nvenc",
            "h264_mf",
            "h264_videotoolbox",
            "libopenh264",
            "libx264",
        ]);
        let mut calls = Vec::new();
        let selected = select_qualified_delivery(&inventory, "windows", true, &mut |name| {
            calls.push(name.to_owned());
            name == "libopenh264"
        })
        .unwrap();
        assert_eq!(selected, Encoder::OpenH264);
        assert_eq!(calls, ["h264_nvenc", "h264_mf", "libopenh264"]);
        assert_eq!(codec_name(selected), "libopenh264");
        assert_eq!(
            select_qualified_delivery(&inventory, "macos", true, &mut |name| name == "libopenh264")
                .unwrap(),
            Encoder::OpenH264
        );
        assert!(
            select_qualified_delivery(&inventory, "linux", false, &mut |_| panic!(
                "no platform candidates"
            ))
            .is_err()
        );
        assert!(select_qualified_delivery(
            &EncoderInventory::from_names(["libx264"]),
            "linux",
            true,
            &mut |_| panic!("GPL candidate must not be selected")
        )
        .is_err());
        assert_eq!(
            select_qualified_delivery(&inventory, "windows", false, &mut |name| name == "h264_mf")
                .unwrap(),
            Encoder::MediaFoundation
        );
        assert_eq!(
            select_qualified_delivery(&inventory, "macos", false, &mut |_| true).unwrap(),
            Encoder::VideoToolbox
        );
        assert_eq!(
            select_qualified_delivery(&inventory, "linux", true, &mut |_| true).unwrap(),
            Encoder::OpenH264
        );
    }

    #[test]
    fn strict_callbacks_only_run_for_listed_nvenc() {
        let inventory =
            EncoderInventory::from_names(["h264_mf", "h264_videotoolbox", "libopenh264"]);
        let mut never = |_: &str| panic!("only NVENC needs qualification in strict policy");
        assert_eq!(
            select_delivery(&inventory, "windows", &mut never).unwrap(),
            Encoder::MediaFoundation
        );
        assert_eq!(
            select_delivery(&inventory, "macos", &mut never).unwrap(),
            Encoder::VideoToolbox
        );
        assert!(select_delivery(&inventory, "linux", &mut never).is_err());
    }

    #[test]
    fn software_intermediate_arguments_and_missing_encoder_errors() {
        let inventory = EncoderInventory::from_names(["prores_ks"]);
        for os in ["macos", "windows", "linux"] {
            for format in [
                IntermediateFormat::Yuv420p8,
                IntermediateFormat::Yuv420p10,
                IntermediateFormat::Yuv422p10,
            ] {
                let result = select_intermediate(&inventory, os, format, &mut |_| {
                    panic!("unlisted encoder cannot be qualified")
                })
                .unwrap();
                assert_eq!(result.codec, "prores");
                assert_eq!(result.args, PRORES_HQ);
            }
        }
        assert_eq!(IntermediateFormat::Yuv420p8.pix_fmt(), "yuv420p");
        assert_eq!(IntermediateFormat::Yuv420p10.pix_fmt(), "yuv420p10le");
        assert_eq!(IntermediateFormat::Yuv422p10.pix_fmt(), "yuv422p10le");
        require_listed(&inventory, Encoder::VideoToolbox).unwrap_err();
        require_listed(
            &EncoderInventory::from_names(["h264_mf"]),
            Encoder::MediaFoundation,
        )
        .unwrap();
    }
}
