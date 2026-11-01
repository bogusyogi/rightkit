//! Ingest against real synthetic media made by the editing ffmpeg. Needs
//! `RIGHTKIT_FFMPEG`/`RIGHTKIT_FFPROBE` or a local `tests/.ffmpeg-dev` marker (two lines:
//! ffmpeg then ffprobe); without them the tests print `SKIP <test>: <reason>` and return, or
//! fail under `RIGHTKIT_REQUIRE_ASSETS=1` (ci/asset-gated.json).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use rightkit_timeline::Fps;
use rightkit_video_project::ingest::{
    ingest, probe_deep, source_provenance, DeepProbe, HdrKind, IngestOptions, IngestOrder,
};
use rightkit_video_project::source::verify_record;
use rightkit_video_project::{Access, MediaTools, Project};

#[test]
fn still_job_publishes_a_clip() {
    let Some(rig) = Rig::new() else { return };
    let image = rig.path("still.ppm");
    let pixels = format!("P3\n320 240\n255\n{}", "255 0 0\n".repeat(320 * 240));
    fs::write(&image, pixels.as_bytes()).unwrap();
    let out = rig.path("still.mp4");
    rightkit_video_project::still::still_to_clip(
        &rig.tools,
        &image,
        &out,
        Fps { num: 30, den: 1 },
        30,
        std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
    )
    .unwrap();
    let info = rig.probe(&out);
    assert_eq!((info.display_width, info.display_height), (320, 240));
    assert_eq!(info.avg_frame_rate, Some((30, 1)));
    assert_eq!(fs::read(&image).unwrap(), pixels.as_bytes());
}

#[test]
fn ingest_job_encodes_a_cfr_derived_copy() {
    let Some(rig) = Rig::new() else { return };
    let clip = vfr_clip(&rig, "sweep-vfr.mp4", &[]);
    let original = fs::read(&clip).unwrap();
    let mut project = rig.project(Fps { num: 30, den: 1 });
    let reports = ingest(
        &mut project,
        &rig.tools,
        std::slice::from_ref(&clip),
        &opts(),
    );
    assert_eq!(reports[0].status, "registered", "{:?}", reports[0]);
    assert!(reports[0].derived.is_some(), "must exercise CFR encode");
    let record = project
        .source_record(reports[0].source_id.as_ref().unwrap())
        .unwrap();
    verify_record(&project, record).unwrap();
    assert!(!rig.probe(&project.source_path(record)).vfr);
    assert_eq!(fs::read(&clip).unwrap(), original);
}

/// A 10-bit source encoder and pixel format: H.264 from the host encoders is 8-bit
/// only. macOS writes HEVC Main 10 (the phone HDR case); elsewhere ffmpeg's own ProRes.
const TEN_BIT: (&str, &str) = if cfg!(target_os = "macos") {
    ("hevc_videotoolbox", "yuv420p10le")
} else {
    ("prores_ks", "yuv422p10le")
};

/// The host's HEVC encoder for the phone-style clip.
const HEVC: &str = if cfg!(target_os = "macos") {
    "hevc_videotoolbox"
} else {
    "hevc_mf"
};

/// The host's own H.264 encoder: the bundled LGPL ffmpeg has no libx264.
const H264: &str = if cfg!(target_os = "macos") {
    "h264_videotoolbox"
} else {
    "h264_mf"
};

fn tools() -> Option<MediaTools> {
    let from_env = |var: &str| std::env::var_os(var).map(PathBuf::from);
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let marker = |p: &Path| -> Vec<PathBuf> {
        fs::read_to_string(p)
            .unwrap_or_default()
            .lines()
            .map(|l| PathBuf::from(l.trim()))
            .collect()
    };
    let own = marker(&root.join("tests/.ffmpeg-dev"));
    let pick = |i: usize, var: &str| from_env(var).or_else(|| own.get(i).cloned());
    let (ffmpeg, ffprobe) = (pick(0, "RIGHTKIT_FFMPEG")?, pick(1, "RIGHTKIT_FFPROBE")?);
    (ffmpeg.is_file() && ffprobe.is_file()).then_some(MediaTools { ffmpeg, ffprobe })
}

struct Rig {
    tools: MediaTools,
    tmp: tempfile::TempDir,
}

impl Rig {
    fn new() -> Option<Rig> {
        let Some(tools) = tools() else {
            let test = std::thread::current()
                .name()
                .unwrap_or("unnamed")
                .to_owned();
            let message = format!("SKIP {test}: no editing ffmpeg (looked at RIGHTKIT_FFMPEG, RIGHTKIT_FFPROBE, tests/.ffmpeg-dev)");
            if std::env::var("RIGHTKIT_REQUIRE_ASSETS").is_ok_and(|value| value == "1") {
                panic!("{message}");
            }
            eprintln!("{message}");
            return None;
        };
        Some(Rig {
            tools,
            tmp: tempfile::tempdir().unwrap(),
        })
    }
    fn path(&self, name: &str) -> PathBuf {
        self.tmp.path().join(name)
    }
    /// Runs ffmpeg; returns false (with the error) when it fails.
    fn try_ffmpeg(&self, args: &[&str]) -> bool {
        let out = Command::new(&self.tools.ffmpeg)
            .args(["-hide_banner", "-nostdin", "-y", "-loglevel", "error"])
            .args(args)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        if !out.status.success() {
            eprintln!("ffmpeg {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        }
        out.status.success()
    }
    fn ffmpeg(&self, args: &[&str]) {
        assert!(self.try_ffmpeg(args), "ffmpeg failed: {args:?}");
    }
    fn project(&self, fps: Fps) -> Project {
        let dir = self.path("proj");
        Project::init(&dir, Some("t"), fps).unwrap();
        Project::open(&dir, Access::Write).unwrap()
    }
    fn probe(&self, p: &Path) -> DeepProbe {
        probe_deep(&self.tools, p).unwrap()
    }
}

const TONE: &str = "aevalsrc=sin(2*PI*1000*t)*between(t\\,1\\,1.2):s=48000:d=4";

/// CFR 30 fps clip with a tone burst at 1.0 s (an audible sync marker).
fn cfr_clip(rig: &Rig, name: &str, extra: &[&str], seconds: &str) -> PathBuf {
    let out = rig.path(name);
    let mut args = vec![
        "-f",
        "lavfi",
        "-i",
        "testsrc2=size=320x240:rate=30",
        "-f",
        "lavfi",
        "-i",
        TONE,
        "-t",
        seconds,
        "-c:v",
        H264,
        "-b:v",
        "4M",
        "-pix_fmt",
        "yuv420p",
        "-c:a",
        "aac",
    ];
    args.extend_from_slice(extra);
    args.push(out.to_str().unwrap());
    rig.ffmpeg(&args);
    out
}

/// VFR clip: 30 fps for 2 s then every other frame (15 fps), timestamps preserved.
fn vfr_clip(rig: &Rig, name: &str, extra: &[&str]) -> PathBuf {
    let out = rig.path(name);
    let mut args = vec![
        "-f",
        "lavfi",
        "-i",
        "testsrc2=size=320x240:rate=30",
        "-f",
        "lavfi",
        "-i",
        TONE,
        "-t",
        "4",
        "-vf",
        "select='lt(n,60)+not(mod(n,2))'",
        "-fps_mode",
        "passthrough",
        "-c:v",
        H264,
        "-b:v",
        "4M",
        "-pix_fmt",
        "yuv420p",
        "-c:a",
        "aac",
    ];
    args.extend_from_slice(extra);
    args.push(out.to_str().unwrap());
    rig.ffmpeg(&args);
    out
}

/// First time (s) the audio exceeds silence, via ffmpeg silencedetect.
fn first_sound(rig: &Rig, file: &Path) -> f64 {
    let out = Command::new(&rig.tools.ffmpeg)
        .args(["-hide_banner", "-nostdin", "-i"])
        .arg(file)
        .args([
            "-vn",
            "-af",
            "silencedetect=noise=-30dB:d=0.1",
            "-f",
            "null",
            "-",
        ])
        .output()
        .unwrap();
    let log = String::from_utf8_lossy(&out.stderr);
    // The first silence_end marks where the tone starts.
    let end = log
        .lines()
        .find_map(|l| l.split("silence_end: ").nth(1))
        .unwrap_or_else(|| panic!("no silence_end in:\n{log}"));
    end.split_whitespace().next().unwrap().parse().unwrap()
}

fn opts() -> IngestOptions {
    IngestOptions::default()
}

#[test]
fn cfr_clip_registers_the_original_without_proxy() {
    let Some(rig) = Rig::new() else { return };
    let clip = cfr_clip(&rig, "plain.mp4", &[], "4");
    let mut project = rig.project(Fps { num: 30, den: 1 });
    let r = ingest(
        &mut project,
        &rig.tools,
        std::slice::from_ref(&clip),
        &opts(),
    );
    assert_eq!(r.len(), 1);
    let f = &r[0];
    assert_eq!(f.status, "registered", "{f:?}");
    assert!(!f.needs_proxy && f.reasons.is_empty() && f.derived.is_none());
    assert_eq!((f.display_width, f.display_height), (320, 240));
    assert_eq!(f.fps, Some((30, 1)));
    assert!(f.has_audio);
    assert_eq!(f.video_codec, "h264");
    // Original registered as-is: id equals the file's BLAKE3.
    assert_eq!(f.source_id, f.original_blake3);
    assert_eq!(
        f.source_id.as_ref().unwrap().as_str(),
        rightkit_fs::digest::blake3_file(&clip).unwrap()
    );
    assert!(!project.dir.join("media/derived").exists());
}

#[test]
fn vfr_clip_gets_a_cfr_intermediate_in_sync_with_provenance() {
    let Some(rig) = Rig::new() else { return };
    let clip = vfr_clip(&rig, "vfr.mp4", &[]);
    let before = fs::read(&clip).unwrap();
    let probe = rig.probe(&clip);
    assert!(probe.vfr, "{probe:?}");
    assert!(probe.vfr_reasons.contains(&"timestamp_jitter"), "{probe:?}");

    let mut project = rig.project(Fps { num: 30, den: 1 });
    let r = ingest(
        &mut project,
        &rig.tools,
        std::slice::from_ref(&clip),
        &opts(),
    );
    let f = &r[0];
    assert_eq!(f.status, "registered", "{f:?}");
    assert!(f.needs_proxy && f.reasons.contains(&"timestamp_jitter".to_string()));
    assert_eq!(f.fps, Some((30, 1)));
    let derived = f.derived.as_ref().expect("derived info");
    assert!(!derived.reused);
    let original = f.original_blake3.clone().unwrap();
    let source = f.source_id.clone().unwrap();
    assert_ne!(source, original, "registered source is the intermediate");

    // Original untouched, and preserved inside the project.
    assert_eq!(fs::read(&clip).unwrap(), before);
    let owned = project.dir.join(format!("media/originals/{original}.mp4"));
    assert_eq!(fs::read(&owned).unwrap(), before);

    // Intermediate is content-addressed and CFR with audio, in sync with the original.
    let record = project.source_record(&source).unwrap().clone();
    assert_eq!(record.path, format!("media/derived/{source}.mov"));
    verify_record(&project, &record).unwrap();
    let path = project.source_path(&record);
    let after = rig.probe(&path);
    assert!(!after.vfr, "{after:?}");
    assert!(after.has_audio && after.audio_channels == probe.audio_channels);
    assert_eq!(after.audio_sample_rate, probe.audio_sample_rate);
    let (a, b) = (first_sound(&rig, &clip), first_sound(&rig, &path));
    assert!((a - b).abs() < 0.03, "tone onset moved {a} -> {b}");
    assert!(
        (a - 1.0).abs() < 0.1,
        "marker should sit near 1.0s, got {a}"
    );
    let dur = (after.duration.0 - probe.duration.0).abs() as f64
        / rightkit_timeline::TICKS_PER_SECOND as f64;
    assert!(dur < 0.1, "duration drift {dur}");

    // Manifest records the transform and points at the original's BLAKE3.
    let m = source_provenance(&project, &source).expect("manifest");
    assert_eq!(m.original.blake3, original);
    assert_eq!(m.derived.blake3, source);
    assert_eq!(m.transform.kind, "cfr");
    assert_eq!(m.transform.fps, Fps { num: 30, den: 1 });
    assert!(m.transform.ffmpeg_args.contains(&"<original>".to_string()));
    assert!(source_provenance(&project, &original).is_none());

    // Idempotent: nothing re-registered, nothing re-encoded, files unchanged.
    let manifest_file = project.dir.join(&derived.manifest);
    let m_before = fs::read(&manifest_file).unwrap();
    let again = ingest(&mut project, &rig.tools, &[clip], &opts());
    assert_eq!(again[0].status, "already_registered", "{:?}", again[0]);
    assert_eq!(again[0].source_id.as_ref(), Some(&source));
    assert!(again[0].derived.as_ref().unwrap().reused);
    assert_eq!(fs::read(&manifest_file).unwrap(), m_before);
    assert_eq!(project.meta.sources.len(), 1);
}

#[test]
fn rate_change_concat_is_vfr() {
    let Some(rig) = Rig::new() else { return };
    let a = rig.path("a.mp4");
    let b = rig.path("b.mp4");
    for (out, rate) in [(&a, "30"), (&b, "12")] {
        rig.ffmpeg(&[
            "-f",
            "lavfi",
            "-i",
            &format!("testsrc2=size=320x240:rate={rate}:duration=2"),
            "-c:v",
            H264,
            "-b:v",
            "4M",
            "-pix_fmt",
            "yuv420p",
            out.to_str().unwrap(),
        ]);
    }
    let list = rig.path("list.txt");
    fs::write(
        &list,
        format!("file '{}'\nfile '{}'\n", a.display(), b.display()),
    )
    .unwrap();
    let joined = rig.path("joined.mp4");
    rig.ffmpeg(&[
        "-f",
        "concat",
        "-safe",
        "0",
        "-i",
        list.to_str().unwrap(),
        "-c",
        "copy",
        joined.to_str().unwrap(),
    ]);
    let p = rig.probe(&joined);
    assert!(p.vfr, "{p:?}");
}

#[test]
fn rotated_mov_reports_display_orientation_and_render_honours_it() {
    let Some(rig) = Rig::new() else { return };
    let plain = cfr_clip(&rig, "land.mp4", &[], "2");
    let rotated = rig.path("portrait.mov");
    rig.ffmpeg(&[
        "-display_rotation",
        "90",
        "-i",
        plain.to_str().unwrap(),
        "-c",
        "copy",
        rotated.to_str().unwrap(),
    ]);
    let p = rig.probe(&rotated);
    assert_eq!((p.coded_width, p.coded_height), (320, 240));
    assert_eq!((p.display_width, p.display_height), (240, 320));
    assert!(p.rotation == 90 || p.rotation == 270, "{p:?}");

    let mut project = rig.project(Fps { num: 30, den: 1 });
    let r = ingest(
        &mut project,
        &rig.tools,
        std::slice::from_ref(&rotated),
        &opts(),
    );
    let f = &r[0];
    assert_eq!(f.status, "registered", "{f:?}");
    assert!(!f.needs_proxy, "rotation alone needs no proxy");
    assert_eq!((f.display_width, f.display_height), (240, 320));
    assert!(f.notes.iter().any(|n| n.starts_with("rotated_")));
    // The timeline sees display dimensions.
    let info = project
        .timeline()
        .source(f.source_id.as_ref().unwrap())
        .unwrap();
    let v = info.video.as_ref().unwrap();
    assert_eq!((v.width, v.height), (240, 320));

    // A plain ffmpeg render (default autorotate, as the render pipeline uses) comes out
    // in display orientation.
    let rendered = rig.path("rendered.mp4");
    rig.ffmpeg(&[
        "-i",
        rotated.to_str().unwrap(),
        "-vf",
        "scale=iw:ih",
        "-c:v",
        H264,
        "-b:v",
        "4M",
        "-an",
        rendered.to_str().unwrap(),
    ]);
    let rp = rig.probe(&rendered);
    assert_eq!((rp.coded_width, rp.coded_height), (240, 320));
    assert_eq!(rp.rotation, 0);
}

#[test]
fn rotated_vfr_proxy_bakes_the_rotation() {
    let Some(rig) = Rig::new() else { return };
    let vfr = vfr_clip(&rig, "v.mp4", &[]);
    let rotated = rig.path("vr.mov");
    rig.ffmpeg(&[
        "-display_rotation",
        "90",
        "-i",
        vfr.to_str().unwrap(),
        "-c",
        "copy",
        "-fps_mode",
        "passthrough",
        rotated.to_str().unwrap(),
    ]);
    let mut project = rig.project(Fps { num: 30, den: 1 });
    let r = ingest(&mut project, &rig.tools, &[rotated], &opts());
    let f = &r[0];
    assert_eq!(f.status, "registered", "{f:?}");
    assert!(f.needs_proxy);
    assert_eq!((f.display_width, f.display_height), (240, 320));
    assert_eq!(
        f.rotation, 0,
        "intermediate carries the rotation in its pixels"
    );
    let rec = project
        .source_record(f.source_id.as_ref().unwrap())
        .unwrap()
        .clone();
    let after = rig.probe(&project.source_path(&rec));
    assert_eq!((after.coded_width, after.coded_height), (240, 320));
    assert_eq!(after.rotation, 0);
}

/// Whether the ffmpeg this test drives lists `name` in `-encoders`.
fn has_encoder(rig: &Rig, name: &str) -> bool {
    let out = Command::new(&rig.tools.ffmpeg)
        .args(["-hide_banner", "-nostdin", "-encoders"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .any(|l| l.split_whitespace().nth(1) == Some(name))
}

#[test]
#[cfg_attr(
    windows,
    ignore = "needs an HEVC encoder: hevc_mf lists but has no usable HEVC MFT on the Dell; run where one exists"
)]
fn hevc_clip_is_probed_and_registered() {
    let Some(rig) = Rig::new() else { return };
    assert!(
        has_encoder(&rig, HEVC),
        "hevc test not run: ffmpeg build has no {HEVC} encoder on this host"
    );
    let out = rig.path("hevc.mov");
    rig.ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        "testsrc2=size=640x360:rate=30:duration=2",
        "-f",
        "lavfi",
        "-i",
        "sine=frequency=440:duration=2",
        "-c:v",
        HEVC,
        "-b:v",
        "4M",
        "-tag:v",
        "hvc1",
        "-pix_fmt",
        "yuv420p",
        "-c:a",
        "aac",
        out.to_str().unwrap(),
    ]);
    let mut project = rig.project(Fps { num: 30, den: 1 });
    let r = ingest(&mut project, &rig.tools, &[out], &opts());
    assert_eq!(r[0].status, "registered", "{:?}", r[0]);
    assert_eq!(r[0].video_codec, "hevc");
}

#[test]
fn prores_10bit_hdr_is_flagged_not_proxied() {
    let Some(rig) = Rig::new() else { return };
    for (trc, kind, note) in [
        ("arib-std-b67", HdrKind::Hlg, "hdr_hlg"),
        ("smpte2084", HdrKind::Pq, "hdr_pq"),
    ] {
        let out = rig.path(&format!("hdr-{trc}.mov"));
        rig.ffmpeg(&[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=320x240:rate=30:duration=2",
            "-c:v",
            "prores_ks",
            "-profile:v",
            "3",
            "-pix_fmt",
            "yuv422p10le",
            "-vf",
            &format!("setparams=color_primaries=bt2020:color_trc={trc}:colorspace=bt2020nc"),
            "-movflags",
            "+write_colr",
            out.to_str().unwrap(),
        ]);
        let p = rig.probe(&out);
        assert_eq!(p.hdr, Some(kind), "{p:?}");
        assert_eq!(p.bit_depth, 10);
        assert_eq!(p.video_codec, "prores");
        let proj_dir = rig.path(&format!("p-{trc}"));
        Project::init(&proj_dir, Some("t"), Fps { num: 30, den: 1 }).unwrap();
        let mut project = Project::open(&proj_dir, Access::Write).unwrap();
        let r = ingest(&mut project, &rig.tools, &[out], &opts());
        let f = &r[0];
        assert_eq!(f.status, "registered", "{f:?}");
        assert!(!f.needs_proxy);
        assert_eq!(f.hdr, Some(kind));
        assert!(f.notes.contains(&note.to_string()), "{:?}", f.notes);
        assert_eq!(
            f.source_id, f.original_blake3,
            "original kept, not tone-mapped"
        );
    }
}

#[test]
fn hdr_vfr_proxy_keeps_10bit_and_colour_tags() {
    let Some(rig) = Rig::new() else { return };
    let out = rig.path("hdrvfr.mov");
    rig.ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        "testsrc2=size=320x240:rate=30",
        "-t",
        "4",
        "-vf",
        "select='lt(n,60)+not(mod(n,2))',setparams=color_primaries=bt2020:color_trc=arib-std-b67:colorspace=bt2020nc",
        "-fps_mode",
        "passthrough",
        "-c:v",
        TEN_BIT.0,
        "-b:v",
        "4M",
        "-pix_fmt",
        TEN_BIT.1,
        "-movflags",
        "+write_colr",
        out.to_str().unwrap(),
    ]);
    let mut project = rig.project(Fps { num: 30, den: 1 });
    let r = ingest(&mut project, &rig.tools, &[out], &opts());
    let f = &r[0];
    assert_eq!(f.status, "registered", "{f:?}");
    assert!(f.needs_proxy && !f.has_audio);
    let rec = project
        .source_record(f.source_id.as_ref().unwrap())
        .unwrap()
        .clone();
    let after = rig.probe(&project.source_path(&rec));
    assert_eq!(after.bit_depth, 10);
    assert_eq!(after.hdr, Some(HdrKind::Hlg), "{after:?}");
}

#[test]
fn multi_file_is_ordered_by_creation_time_or_given_order_and_idempotent() {
    let Some(rig) = Rig::new() else { return };
    let mk = |name: &str, stamp: &str| {
        cfr_clip(
            &rig,
            name,
            &["-metadata", &format!("creation_time={stamp}")],
            "2",
        )
    };
    // Created late, early, middle: the file names are in the wrong order on purpose.
    let late = mk("a_late.mp4", "2026-10-06T12:00:00Z");
    let early = mk("b_early.mp4", "2026-10-06T08:00:00Z");
    let mid = mk("c_mid.mp4", "2026-10-06T10:00:00Z");
    let given = vec![late.clone(), early.clone(), mid.clone()];
    let mut project = rig.project(Fps { num: 30, den: 1 });
    let r = ingest(&mut project, &rig.tools, &given, &opts());
    let names: Vec<_> = r.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(names, ["b_early.mp4", "c_mid.mp4", "a_late.mp4"]);
    assert!(r.iter().enumerate().all(|(i, f)| f.index == i));
    assert_eq!(r[0].created.as_deref(), Some("2026-10-06T08:00:00Z"));
    assert_eq!(r[0].created_from, "container_tag");
    assert!(r.iter().all(|f| f.status == "registered"));
    assert_eq!(project.meta.sources.len(), 3);

    // Re-ingest in given order: idempotent, nothing new registered.
    let again = ingest(
        &mut project,
        &rig.tools,
        &given,
        &IngestOptions {
            order: IngestOrder::Given,
            ..opts()
        },
    );
    let names: Vec<_> = again.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(names, ["a_late.mp4", "b_early.mp4", "c_mid.mp4"]);
    assert!(again.iter().all(|f| f.status == "already_registered"));
    assert_eq!(project.meta.sources.len(), 3);
}

#[test]
fn bad_files_fail_individually_with_typed_errors() {
    let Some(rig) = Rig::new() else { return };
    let good = cfr_clip(&rig, "good.mp4", &[], "2");
    let missing = rig.path("nope.mp4");
    let garbage = rig.path("garbage.mov");
    fs::write(&garbage, b"definitely not media").unwrap();
    let audio_only = rig.path("voice.m4a");
    rig.ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        "sine=frequency=440:duration=1",
        audio_only.to_str().unwrap(),
    ]);
    let vp9 = rig.path("clip.mkv");
    let have_vp9 = rig.try_ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        "testsrc2=size=64x64:rate=30:duration=1",
        "-c:v",
        "mpeg2video",
        vp9.to_str().unwrap(),
    ]);
    let mut files = vec![missing, garbage, audio_only, good];
    if have_vp9 {
        files.push(vp9);
    }
    let mut project = rig.project(Fps { num: 30, den: 1 });
    let r = ingest(
        &mut project,
        &rig.tools,
        &files,
        &IngestOptions {
            order: IngestOrder::Given,
            ..opts()
        },
    );
    let code = |i: usize| r[i].error.as_ref().map(|e| e.code.as_str());
    assert_eq!(code(0), Some("source_not_found"));
    assert_eq!(code(1), Some("probe_failed"));
    assert_eq!(code(2), Some("no_video_stream"));
    assert_eq!(r[3].status, "registered", "{:?}", r[3]);
    if have_vp9 {
        assert_eq!(code(4), Some("unsupported_codec"), "{:?}", r[4]);
    }
    assert_eq!(project.meta.sources.len(), 1);
}

#[test]
fn dry_run_changes_nothing() {
    let Some(rig) = Rig::new() else { return };
    let vfr = vfr_clip(&rig, "d.mp4", &[]);
    let dir = rig.path("proj");
    Project::init(&dir, Some("t"), Fps { num: 30, den: 1 }).unwrap();
    let mut project = Project::open(&dir, Access::Read).unwrap();
    let r = ingest(
        &mut project,
        &rig.tools,
        &[vfr],
        &IngestOptions {
            dry_run: true,
            ..opts()
        },
    );
    assert_eq!(r[0].status, "would_register", "{:?}", r[0]);
    assert!(r[0].needs_proxy);
    assert!(!dir.join("media/derived").exists());
    assert!(!dir.join("media/originals").exists());
    assert!(project.meta.sources.is_empty());
}
