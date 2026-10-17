//! H.264 encoder discovery and the one pick rule for an ffmpeg build.
//!
//! An app that writes H.264 must choose an encoder the build actually lists. The bundled
//! builds are LGPL with no software H.264 encoder, so the hardware encoder for the platform
//! is the normal choice (VideoToolbox on macOS, NVENC or Media Foundation on Windows). This
//! module lists the encoders with one `-encoders` run, keeps the listing in ffmpeg's own
//! order, and picks the first usable H.264 encoder in a fixed order. Consumers decide what
//! "usable" means (a runtime probe, or an encoder that already refused to start) through a
//! callback, and decide the fallback (for example MPEG-4) themselves.

use std::time::Duration;

use crate::{run_version_probe, FfmpegBinary, MediaError};

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
}
