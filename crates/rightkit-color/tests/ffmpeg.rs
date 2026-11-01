//! Real-FFmpeg checks against the editing build, located via `RIGHTKIT_FFMPEG` / `RIGHTKIT_FFPROBE`.
//! With no editing FFmpeg they print `SKIP <test>: ...` and return; under `RIGHTKIT_REQUIRE_ASSETS=1`
//! they fail instead (ci/asset-gated.json). Before/after PNGs are kept in `$RIGHTKIT_COLOR_ARTIFACTS`
//! or `<tmp>/rightkit-color-it-artifacts`.

use std::path::{Path, PathBuf};
use std::process::Command;

use rightkit_color::*;

/// The host's own H.264 encoder: the bundled LGPL ffmpeg has no libx264.
const H264: &str = if cfg!(target_os = "macos") {
    "h264_videotoolbox"
} else {
    "h264_mf"
};

fn tool(env: &str, name: &str) -> Option<PathBuf> {
    if let Ok(p) = std::env::var(env) {
        return Path::new(&p).is_file().then(|| PathBuf::from(p));
    }
    let _ = name;
    None
}

struct Env {
    ffmpeg: PathBuf,
    ffprobe: PathBuf,
    dir: PathBuf,
}

fn env(label: &str) -> Option<Env> {
    let (Some(ffmpeg), Some(ffprobe)) = (
        tool("RIGHTKIT_FFMPEG", "ffmpeg"),
        tool("RIGHTKIT_FFPROBE", "ffprobe"),
    ) else {
        let message = format!(
            "SKIP {label}: no editing FFmpeg (looked at RIGHTKIT_FFMPEG and RIGHTKIT_FFPROBE)"
        );
        if std::env::var("RIGHTKIT_REQUIRE_ASSETS").is_ok_and(|value| value == "1") {
            panic!("{message}");
        }
        eprintln!("{message}");
        return None;
    };
    let base = std::env::var("RIGHTKIT_COLOR_ARTIFACTS")
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir().join("rightkit-color-it-artifacts"));
    let dir = base.join(label);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    Some(Env {
        ffmpeg,
        ffprobe,
        dir,
    })
}

impl Env {
    fn run(&self, args: &[&str]) {
        let out = Command::new(&self.ffmpeg)
            .args(["-nostdin", "-hide_banner", "-v", "error", "-y"])
            .args(args)
            .current_dir(&self.dir)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "ffmpeg {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    fn p(&self, n: &str) -> PathBuf {
        self.dir.join(n)
    }
    fn stats(&self, f: &Path, pre: Option<&str>) -> FrameStats {
        sample_stats(
            &self.ffmpeg,
            f,
            ClipSpan {
                start_s: 0.0,
                duration_s: 1.0,
            },
            SampleSpec {
                frames: 4,
                width: 160,
            },
            pre,
        )
        .unwrap()
    }
    fn png(&self, f: &Path, name: &str) {
        self.run(&[
            "-i",
            f.to_str().unwrap(),
            "-frames:v",
            "1",
            self.p(name).to_str().unwrap(),
        ]);
    }
    /// Render `fragment` over `input` to an h264 mp4 with the fragment's tags.
    fn render(&self, input: &Path, frag: &ColorFragment, out: &str) -> PathBuf {
        let mut a: Vec<String> = vec![
            "-i".into(),
            input.to_str().unwrap().into(),
            "-vf".into(),
            frag.filter.clone(),
            "-c:v".into(),
            H264.into(),
            "-b:v".into(),
            "4M".into(),
        ];
        a.extend(frag.encoder_args.iter().cloned());
        a.push(out.into());
        let refs: Vec<&str> = a.iter().map(String::as_str).collect();
        self.run(&refs);
        self.p(out)
    }
}

fn plan_env(e: &Env) -> BuildEnv<'_> {
    BuildEnv::new(&e.dir)
}

#[test]
fn editing_ffmpeg_has_every_filter_the_fragments_need() {
    let Some(e) = env("caps") else {
        return;
    };
    let caps = FilterCapabilities::probe(&e.ffmpeg).unwrap();
    for f in [
        "zscale",
        "tonemap",
        "lut3d",
        "colortemperature",
        "colorchannelmixer",
        "lutrgb",
        "lutyuv",
        "setparams",
        "scale",
        "format",
    ] {
        assert!(caps.has(f), "missing {f}");
    }
    for kind in [
        ColorSpaceKind::Rec2020Pq,
        ColorSpaceKind::AppleLog,
        ColorSpaceKind::Rec709,
    ] {
        let frag = build_fragment(&ColorPlan::default(), kind, &plan_env(&e)).unwrap();
        caps.check(&frag).unwrap();
    }
}

fn hdr_roundtrip(label: &str, trc: &str, kind: ColorSpaceKind, tonemap: Tonemap) {
    let Some(e) = env(label) else {
        return;
    };
    // This scenario is HDR to SDR conversion, which needs zscale and tonemap. Without them it
    // cannot run, so it fails with the reason rather than passing.
    if !FilterCapabilities::probe(&e.ffmpeg)
        .unwrap()
        .hdr_to_sdr_available()
    {
        eprintln!("SKIP: HDR to SDR conversion is not available on this computer (no zscale)");
        panic!(
            "{label} not run: HDR to SDR conversion is not available on this computer (no zscale)"
        );
    }
    // 10-bit testsrc2 tagged as HDR, stored losslessly with its tags (FFV1 in Matroska keeps the
    // frame tags; verified by the tag assertion below).
    let src = e.p("hdr.mkv");
    e.run(&["-f", "lavfi", "-i", "testsrc2=s=640x360:r=25:d=2", "-vf", &format!("format=yuv420p10le,setparams=color_primaries=bt2020:color_trc={trc}:colorspace=bt2020nc:range=tv"), "-c:v", "ffv1", src.to_str().unwrap()]);
    let tags = probe_color_tags(&e.ffprobe, &src).unwrap();
    assert_eq!(detect_color_space(&tags), kind, "{tags:?}");

    let plan = ColorPlan {
        sdr: SdrConversion {
            tonemap,
            ..Default::default()
        },
        ..Default::default()
    };
    let frag = build_fragment(&plan, kind, &plan_env(&e)).unwrap();
    FilterCapabilities::probe(&e.ffmpeg)
        .unwrap()
        .check(&frag)
        .unwrap();
    let out = e.render(&src, &frag, "sdr.mp4");
    verify_output_tags(&e.ffprobe, &out, &frag.expected).unwrap();

    // The tone curve really ran: the same clip merely relabelled as bt709 looks very different.
    let naive = e.p("naive.mp4");
    e.run(&[
        "-i",
        src.to_str().unwrap(),
        "-vf",
        "format=yuv420p,setparams=color_primaries=bt709:color_trc=bt709:colorspace=bt709",
        "-c:v",
        H264,
        "-b:v",
        "4M",
        naive.to_str().unwrap(),
    ]);
    let (s_out, s_naive) = (e.stats(&out, None), e.stats(&naive, None));
    assert!(
        (s_out.luma.mean - s_naive.luma.mean).abs() > 0.05,
        "tonemap had no effect: {} vs {}",
        s_out.luma.mean,
        s_naive.luma.mean
    );
    assert!(s_out.luma.p95 <= 1.0 && s_out.luma.mean > 0.1 && s_out.luma.mean < 0.9);
    let pixfmt = Command::new(&e.ffprobe)
        .args([
            "-v",
            "error",
            "-show_entries",
            "stream=pix_fmt,color_range",
            "-of",
            "csv=p=0",
        ])
        .arg(&out)
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&pixfmt.stdout).contains("yuv420p,tv"));
    e.png(&src, "before.png");
    e.png(&out, "after.png");
}

#[test]
fn pq_hdr_to_sdr_mobius() {
    hdr_roundtrip(
        "pq-mobius",
        "smpte2084",
        ColorSpaceKind::Rec2020Pq,
        Tonemap::Mobius,
    );
}

#[test]
fn apple_log_grey_card_lands_on_rec709_grey() {
    let Some(e) = env("apple-log") else {
        return;
    };
    // Grey card at 18% scene reflectance, encoded as Apple Log, 10-bit limited bt2020nc, no transfer tag.
    let p = apple_log::encode(0.18);
    let v = (p * 255.0).round() as u8;
    let src = e.p("alog.mkv");
    e.run(&["-f", "lavfi", "-i", &format!("color=c=0x{v:02x}{v:02x}{v:02x}:s=320x180:r=25:d=1"), "-vf", "format=gbrp,scale=out_color_matrix=bt2020:out_range=tv,format=yuv420p10le,setparams=color_primaries=bt2020:colorspace=bt2020nc:range=tv", "-c:v", "ffv1", src.to_str().unwrap()]);
    let tags = probe_color_tags(&e.ffprobe, &src).unwrap();
    assert_eq!(
        detect_color_space(&tags),
        ColorSpaceKind::AppleLog,
        "{tags:?}"
    );

    let frag = build_fragment(
        &ColorPlan::default(),
        ColorSpaceKind::AppleLog,
        &plan_env(&e),
    )
    .unwrap();
    assert_eq!(frag.files.len(), 1);
    let out = e.render(&src, &frag, "sdr.mp4");
    verify_output_tags(&e.ffprobe, &out, &frag.expected).unwrap();
    let s = e.stats(&out, None);
    let want = apple_log::to_rec709([p; 3], 0.0)[0];
    assert!(
        (s.luma.mean - want).abs() < 0.02,
        "luma {} want {}",
        s.luma.mean,
        want
    );
    assert!((s.r.mean - s.b.mean).abs() < 0.02, "neutral stays neutral");
    // And a real picture, for eyes.
    let pic = e.p("alog-pic.mkv");
    e.run(&["-f", "lavfi", "-i", "testsrc2=s=640x360:r=25:d=1", "-vf", "format=gbrp,lutrgb=r='val*0.45+0.1*maxval':g='val*0.45+0.1*maxval':b='val*0.45+0.1*maxval',scale=out_color_matrix=bt2020:out_range=tv,format=yuv420p10le,setparams=color_primaries=bt2020:colorspace=bt2020nc:range=tv", "-c:v", "ffv1", pic.to_str().unwrap()]);
    let out2 = e.render(&pic, &frag, "pic-sdr.mp4");
    e.png(&pic, "before.png");
    e.png(&out2, "after.png");
}

fn dark_warm_clip(e: &Env) -> PathBuf {
    let src = e.p("src.mkv");
    // Dark, warm, low-contrast skin-ish gradient plus detail.
    e.run(&["-f", "lavfi", "-i", "testsrc2=s=640x360:r=25:d=1", "-vf", "format=gbrp,lutrgb=r='val*0.45+0.04*maxval':g='val*0.32+0.03*maxval':b='val*0.20+0.02*maxval',format=yuv420p,setparams=color_primaries=bt709:color_trc=bt709:colorspace=bt709:range=tv", "-c:v", "ffv1", src.to_str().unwrap()]);
    src
}

#[test]
fn auto_exposure_and_white_balance_improve_a_dark_warm_clip_within_bounds() {
    let Some(e) = env("auto") else {
        return;
    };
    let src = dark_warm_clip(&e);
    let before = e.stats(&src, None);
    let limits = CorrectionLimits::default();
    let mut plan = preset("talking-head-auto").unwrap().plan;
    plan.resolve_auto(&before);
    let c = plan.correction.unwrap();
    assert!(
        c.exposure_ev > 0.0 && c.exposure_ev <= limits.max_exposure_ev,
        "{c:?}"
    );
    assert!(
        c.temperature_k >= limits.min_temperature_k
            && c.temperature_k <= limits.max_temperature_k
            && c.temperature_k > 6500.0,
        "{c:?}"
    );
    let frag = build_fragment(&plan, ColorSpaceKind::Rec709, &plan_env(&e)).unwrap();
    let out = e.render(&src, &frag, "graded.mp4");
    verify_output_tags(&e.ffprobe, &out, &frag.expected).unwrap();
    let after = e.stats(&out, None);
    let target = limits.target_median_luma;
    assert!(
        (after.luma.p50 - target).abs() < (before.luma.p50 - target).abs(),
        "median {} -> {}",
        before.luma.p50,
        after.luma.p50
    );
    let cast = |s: &FrameStats| (s.midtone_mean[0] / s.midtone_mean[2]).ln().abs();
    assert!(
        cast(&after) < cast(&before),
        "cast {} -> {}",
        cast(&before),
        cast(&after)
    );
    e.png(&src, "before.png");
    e.png(&out, "after.png");
}

#[test]
fn shot_match_pulls_clip_b_towards_reference_a() {
    let Some(e) = env("shot-match") else {
        return;
    };
    let a = e.p("a.mkv");
    let b = e.p("b.mkv");
    let tag =
        "format=yuv420p,setparams=color_primaries=bt709:color_trc=bt709:colorspace=bt709:range=tv";
    e.run(&["-f", "lavfi", "-i", "testsrc2=s=640x360:r=25:d=1", "-vf", &format!("format=gbrp,lutrgb=r='val*0.9+0.08*maxval':g='val*0.85+0.06*maxval':b='val*0.7+0.04*maxval',{tag}"), "-c:v", "ffv1", a.to_str().unwrap()]);
    e.run(&["-f", "lavfi", "-i", "testsrc2=s=640x360:r=25:d=1", "-vf", &format!("format=gbrp,lutrgb=r='val*0.6+0.02*maxval':g='val*0.65+0.05*maxval':b='val*0.8+0.1*maxval',{tag}"), "-c:v", "ffv1", b.to_str().unwrap()]);
    let (sa, sb) = (e.stats(&a, None), e.stats(&b, None));
    let m = match_clip_to_reference(&sa, &sb, &ShotMatchLimits::default());
    let plan = ColorPlan {
        shot_match: Some(m),
        ..Default::default()
    };
    let frag = build_fragment(&plan, ColorSpaceKind::Rec709, &plan_env(&e)).unwrap();
    assert!(frag.filter.contains("lutrgb"), "{}", frag.filter);
    let out = e.render(&b, &frag, "b-matched.mp4");
    let sm = e.stats(&out, None);
    let dist = |x: &FrameStats| {
        ((x.r.mean - sa.r.mean).abs() + (x.g.mean - sa.g.mean).abs() + (x.b.mean - sa.b.mean).abs())
            / 3.0
    };
    assert!(
        dist(&sm) < dist(&sb) * 0.6,
        "mean distance {} -> {}",
        dist(&sb),
        dist(&sm)
    );
    e.png(&a, "ref-a.png");
    e.png(&b, "before.png");
    e.png(&out, "after.png");
}

#[test]
fn user_lut_applies_at_bounded_intensity_and_malformed_luts_are_rejected() {
    let Some(e) = env("lut") else {
        return;
    };
    // Black/white invert LUT.
    let mut cube = String::from("LUT_3D_SIZE 2\n");
    for b in 0..2 {
        for g in 0..2 {
            for r in 0..2 {
                cube.push_str(&format!("{} {} {}\n", 1 - r, 1 - g, 1 - b));
            }
        }
    }
    let lut = e.p("invert.cube");
    std::fs::write(&lut, &cube).unwrap();
    let src = e.p("grey.mkv");
    e.run(&[
        "-f",
        "lavfi",
        "-i",
        "color=c=0x404040:s=160x90:r=25:d=1",
        "-vf",
        "format=yuv420p,setparams=color_primaries=bt709:color_trc=bt709:colorspace=bt709:range=tv",
        "-c:v",
        "ffv1",
        src.to_str().unwrap(),
    ]);
    let base = e.stats(&src, None).luma.mean;
    let mut seen = Vec::new();
    // Work dir with a space and a colon exercises filter-path escaping (colon is Windows-illegal, so mac only).
    let dir = if cfg!(windows) {
        e.dir.join("lut dir")
    } else {
        e.dir.join("lut dir:x")
    };
    for intensity in [0.0, 0.5, 1.0, 7.0] {
        let plan = ColorPlan {
            lut: Some(LutRef {
                path: lut.clone(),
                intensity,
            }),
            ..Default::default()
        };
        let frag = build_fragment(&plan, ColorSpaceKind::Rec709, &BuildEnv::new(&dir)).unwrap();
        let out = e.render(&src, &frag, &format!("lut-{intensity}.mp4"));
        seen.push(e.stats(&out, None).luma.mean);
    }
    assert!(
        (seen[0] - base).abs() < 0.02,
        "intensity 0 is a no-op: {seen:?}"
    );
    assert!(
        (seen[2] - (1.0 - base)).abs() < 0.04,
        "intensity 1 inverts: {seen:?}"
    );
    assert!(
        (seen[1] - 0.5).abs() < 0.04,
        "half intensity of an inverting LUT flattens to mid grey: {seen:?}"
    );
    assert!(
        (seen[3] - seen[2]).abs() < 0.01,
        "intensity above 1 is clamped: {seen:?}"
    );
    for bad in [
        "LUT_3D_SIZE 66\n",
        "LUT_3D_SIZE 2\n0 0 NaN\n",
        &cube.replace("1 1 1", "1 1"),
    ] {
        std::fs::write(e.p("bad.cube"), bad).unwrap();
        let plan = ColorPlan {
            lut: Some(LutRef {
                path: e.p("bad.cube"),
                intensity: 1.0,
            }),
            ..Default::default()
        };
        assert!(build_fragment(&plan, ColorSpaceKind::Rec709, &plan_env(&e)).is_err());
    }
}
