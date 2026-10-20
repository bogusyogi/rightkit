//! End-to-end renders against a real ffmpeg process. All tests are `#[ignore]`d: run the
//! `encode_e2e` test target of `rightkit-motion` with feature `encode` and `--ignored`.
//!
//! FFmpeg is found the way `rightkit-ffmpeg`'s own e2e suites find it
//! (`dev_resolve::resolve_for_development`: `RIGHTKIT_FFMPEG`, then the vendored catalog build, then
//! host locations). A host without it prints one `SKIP <test>: ...` line and passes; with
//! `RIGHTKIT_REQUIRE_ASSETS=1` it panics instead, so a host that is meant to have ffmpeg cannot pass
//! by skipping. An encoder the build does not list (ProRes, libvpx-vp9) skips the same way.

use std::path::Path;
use std::time::Duration;

use rightkit_ffmpeg::dev_resolve::{development_candidates, resolve_for_development, DevResolved};
use rightkit_ffmpeg::{run_capture, CancellationToken, FfmpegBinary, FfmpegCommand};
use rightkit_motion::{
    pick_alpha_encoder, pick_encoder, probe_inventory, render_with_fallback, AlphaSupport,
    EncoderKind, FrameError, FrameSource, Quality, RenderJob,
};

const T: Duration = Duration::from_secs(120);

fn skip_missing(what: &str, looked_at: &str) {
    let test = std::thread::current()
        .name()
        .unwrap_or("unnamed")
        .to_owned();
    let message = format!("SKIP {test}: {what} (looked at {looked_at})");
    if std::env::var("RIGHTKIT_REQUIRE_ASSETS").is_ok_and(|value| value == "1") {
        panic!("{message}");
    }
    eprintln!("{message}");
}

fn ffmpeg() -> Option<DevResolved> {
    let found = resolve_for_development().map(|(bin, _kind)| bin);
    if found.is_none() {
        let looked: Vec<String> = development_candidates()
            .iter()
            .map(|p| p.display().to_string())
            .collect();
        skip_missing(
            "FFmpeg",
            &format!("RIGHTKIT_FFMPEG or {}", looked.join(", ")),
        );
    }
    found
}

/// A moving RGBA gradient with a transparent corner, so alpha has something to carry.
struct Gradient {
    size: (u32, u32),
    t: f64,
}

impl FrameSource for Gradient {
    fn seek(&mut self, time_s: f64) -> Result<(), FrameError> {
        self.t = time_s;
        Ok(())
    }

    fn capture(&mut self) -> Result<Vec<u8>, FrameError> {
        let (w, h) = self.size;
        let shift = (self.t * 100.0) as u32;
        let img = image::RgbaImage::from_fn(w, h, |x, y| {
            let alpha = if x < w / 4 && y < h / 4 { 0 } else { 255 };
            image::Rgba([((x + shift) % 256) as u8, (y % 256) as u8, 128, alpha])
        });
        let mut png = Vec::new();
        image::DynamicImage::from(img)
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .map_err(|e| FrameError(e.to_string()))?;
        Ok(png)
    }
}

fn job(out: &Path, encoder: rightkit_motion::Encoder) -> RenderJob {
    RenderJob {
        width: 64,
        height: 48,
        fps: 10,
        duration_s: 0.5,
        quality: Quality::Draft,
        encoder,
        audio: vec![],
        output: out.to_path_buf(),
        poster: Some(out.with_extension("png")),
    }
}

/// ffmpeg's `-i` banner for `file` (it exits non-zero without an output, so only stderr is read).
fn describe(bin: &dyn FfmpegBinary, file: &Path) -> String {
    let mut c = FfmpegCommand::new();
    c.texts(["-hide_banner"]).input(file);
    run_capture(bin, &c, T, &CancellationToken::new())
        .unwrap()
        .stderr
}

fn decodes(bin: &dyn FfmpegBinary, file: &Path) -> bool {
    let mut c = FfmpegCommand::base("error");
    c.input(file).texts(["-f", "null", "-"]);
    run_capture(bin, &c, T, &CancellationToken::new())
        .unwrap()
        .status
        .success()
}

#[test]
#[ignore = "needs ffmpeg (RIGHTKIT_FFMPEG or a catalog build); run with --ignored"]
fn the_inventory_probe_lists_encoders() {
    let Some(bin) = ffmpeg() else { return };
    let inventory = probe_inventory(&bin, Duration::from_secs(30)).unwrap();
    assert!(
        inventory.has("mpeg4"),
        "every ffmpeg build lists the native MPEG-4 encoder"
    );
    eprintln!(
        "alpha support: {:?}",
        AlphaSupport::from_inventory(&inventory)
    );
}

#[test]
#[ignore = "needs ffmpeg (RIGHTKIT_FFMPEG or a catalog build); run with --ignored"]
fn an_opaque_render_produces_a_decodable_mp4_and_a_poster() {
    let Some(bin) = ffmpeg() else { return };
    let dir = tempfile::tempdir().unwrap();
    let inventory = probe_inventory(&bin, Duration::from_secs(30)).unwrap();
    let encoder = pick_encoder(&inventory, std::env::consts::OS, true, &mut |_| true).unwrap();
    eprintln!("encoder: {} (fallback: {})", encoder.name, encoder.fallback);
    let out = dir.path().join("out.mp4");
    let j = job(&out, encoder);
    let mut source = Gradient {
        size: (64, 48),
        t: 0.0,
    };
    let outcome = render_with_fallback(
        &bin,
        &mut source,
        &j,
        true,
        &CancellationToken::new(),
        &mut |_| {},
    )
    .unwrap();
    assert_eq!(outcome.frames, 5);
    assert!(std::fs::metadata(&out).unwrap().len() > 0);
    assert!(decodes(&bin, &out), "ffmpeg must decode its own output");
    assert!(outcome.poster.is_some_and(|p| p.is_file()));
}

#[test]
#[ignore = "needs ffmpeg with prores_ks; run with --ignored"]
fn prores_4444_keeps_the_alpha_plane() {
    let Some(bin) = ffmpeg() else { return };
    let dir = tempfile::tempdir().unwrap();
    let inventory = probe_inventory(&bin, Duration::from_secs(30)).unwrap();
    let Ok(encoder) = pick_alpha_encoder(&inventory, EncoderKind::ProRes4444) else {
        skip_missing("prores_ks in `ffmpeg -encoders`", "the resolved ffmpeg");
        return;
    };
    let out = dir.path().join("out.mov");
    let mut source = Gradient {
        size: (64, 48),
        t: 0.0,
    };
    render_with_fallback(
        &bin,
        &mut source,
        &job(&out, encoder),
        false,
        &CancellationToken::new(),
        &mut |_| {},
    )
    .unwrap();
    assert!(decodes(&bin, &out));
    let info = describe(&bin, &out);
    assert!(
        info.contains("prores") && info.contains("yuva"),
        "no alpha pixel format in: {info}"
    );
}

#[test]
#[ignore = "needs ffmpeg with libvpx-vp9 (not in the LGPL catalog builds); run with --ignored"]
fn vp9_alpha_marks_the_stream_as_having_alpha() {
    let Some(bin) = ffmpeg() else { return };
    let dir = tempfile::tempdir().unwrap();
    let inventory = probe_inventory(&bin, Duration::from_secs(30)).unwrap();
    let Ok(encoder) = pick_alpha_encoder(&inventory, EncoderKind::Vp9Alpha) else {
        skip_missing("libvpx-vp9 in `ffmpeg -encoders`", "the resolved ffmpeg");
        return;
    };
    let out = dir.path().join("out.webm");
    let mut source = Gradient {
        size: (64, 48),
        t: 0.0,
    };
    render_with_fallback(
        &bin,
        &mut source,
        &job(&out, encoder),
        false,
        &CancellationToken::new(),
        &mut |_| {},
    )
    .unwrap();
    assert!(std::fs::metadata(&out).unwrap().len() > 0);
    // The webm alpha flag is the `alpha_mode` stream tag.
    let info = describe(&bin, &out);
    assert!(info.contains("alpha_mode"), "no alpha_mode tag in: {info}");
}
