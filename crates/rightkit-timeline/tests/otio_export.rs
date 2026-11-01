use std::collections::BTreeMap;
use std::path::PathBuf;

use rightkit_timeline::otio::{export_otio, to_otio, validate_otio, ExportOptions};
use rightkit_timeline::*;
use serde_json::Value;

const FPS30: Fps = Fps { num: 30, den: 1 };

fn sid(name: &str) -> SourceId {
    SourceId::new(blake3::hash(name.as_bytes()).to_hex().to_string()).unwrap()
}

fn secs(s: f64) -> Ticks {
    Ticks((s * TICKS_PER_SECOND as f64).round() as i64)
}

/// An absolute media path on the host: `/media/...` on Unix, `C:\media\...` on Windows,
/// where a POSIX-style path is not absolute and the exporter rejects it.
fn media_path(path: &str) -> PathBuf {
    if cfg!(windows) {
        PathBuf::from(format!("C:{}", path.replace('/', r"\")))
    } else {
        PathBuf::from(path)
    }
}

fn register(id: &str, seconds: f64, video: bool, audio: bool) -> Action {
    Action::RegisterSource {
        source: SourceInfo {
            id: sid(id),
            duration: secs(seconds),
            video: video.then_some(VideoStreamInfo {
                width: 1920,
                height: 1080,
                duration: None,
            }),
            has_audio: audio,
        },
    }
}

fn clip(id: &str, src: &str, index: usize, from: f64, to: f64) -> Action {
    Action::InsertClip {
        track_id: "V1".into(),
        index,
        clip: VideoClip {
            id: id.into(),
            source: sid(src),
            source_in: secs(from),
            source_out: secs(to),
        },
    }
}

#[allow(clippy::too_many_arguments)]
fn audio(
    track: &str,
    id: &str,
    src: &str,
    start: f64,
    from: f64,
    to: f64,
    link: Option<&str>,
    under: UnderAudio,
) -> Action {
    Action::AddAudioClip {
        track_id: track.into(),
        clip: AudioClip {
            id: id.into(),
            source: sid(src),
            trim_in: secs(from),
            trim_out: secs(to),
            start: secs(start),
            gain: MilliDb::from_db(-6.0),
            link: link.map(Into::into),
            under,
        },
    }
}

fn sample() -> (Timeline, BTreeMap<SourceId, PathBuf>) {
    let mut s = TimelineStore::new(FPS30);
    for a in [
        register("talk", 20.0, true, true),
        register("broll", 10.0, true, false),
        register("vo", 6.0, false, true),
        register("music", 30.0, false, true),
        clip("c1", "talk", 0, 1.0, 4.0),
        clip("c2", "broll", 1, 0.0, 2.0),
        clip("c3", "talk", 2, 8.0, 10.5),
        Action::AddAudioTrack {
            track_id: "A2".into(),
            role: AudioRole::Music,
        },
        audio(
            "A1",
            "n1",
            "vo",
            3.0,
            0.0,
            2.0,
            Some("c2"),
            UnderAudio::Mute,
        ),
        audio(
            "A2",
            "m1",
            "music",
            0.0,
            5.0,
            12.0,
            None,
            UnderAudio::Duck {
                amount: MilliDb(12_000),
            },
        ),
        Action::AddTitle {
            track_id: "T1".into(),
            clip: OverlayClip {
                id: "t1".into(),
                start_frame: 6,
                frames: 60,
                template: TitleTemplate::LowerThird {
                    title: "Jane Doe".into(),
                    subtitle: Some("Host".into()),
                },
                fade_in_frames: 6,
                fade_out_frames: 6,
            },
        },
        Action::AddTitle {
            track_id: "T1".into(),
            clip: OverlayClip {
                id: "t2".into(),
                start_frame: 90,
                frames: 30,
                template: TitleTemplate::Caption {
                    text: "Hello there".into(),
                },
                fade_in_frames: 0,
                fade_out_frames: 0,
            },
        },
    ] {
        s.apply(a).unwrap();
    }
    let sources = [
        ("talk", "/media/take 1.mov"),
        ("broll", "/media/broll.mp4"),
        ("vo", "/media/vo.wav"),
        ("music", "/media/music.wav"),
    ]
    .into_iter()
    .map(|(n, p)| (sid(n), media_path(p)))
    .collect();
    (s.timeline().clone(), sources)
}

#[test]
fn ntsc_rate_is_exact_rational() {
    let fps = Fps {
        num: 30000,
        den: 1001,
    };
    let mut s = TimelineStore::new(fps);
    s.apply(register("talk", 20.0, true, true)).unwrap();
    s.apply(clip("c1", "talk", 0, 0.0, 2.0)).unwrap();
    let sources = [(sid("talk"), media_path("/m/a.mov"))]
        .into_iter()
        .collect();
    let (doc, _) = to_otio(s.timeline(), &sources, &ExportOptions::new("n")).unwrap();
    let d = &doc["tracks"]["children"][0]["children"][0]["source_range"]["duration"];
    assert_eq!(d["rate"].as_f64().unwrap(), 30000.0 / 1001.0);
    assert_eq!(d["value"], 60.0);
}

#[test]
fn writes_file_and_rejects_bad_input() {
    let (tl, sources) = sample();
    let dir = tempfile::tempdir().unwrap();
    let report = export_otio(&tl, &sources, dir.path(), &ExportOptions::new("talk")).unwrap();
    let path = report.otio.unwrap();
    assert_eq!(path.file_name().unwrap(), "talk.otio");
    let back: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    validate_otio(&back).unwrap();

    assert!(export_otio(&tl, &sources, dir.path(), &ExportOptions::new("../x")).is_err());
    let mut missing = sources.clone();
    missing.remove(&sid("vo"));
    assert!(to_otio(&tl, &missing, &ExportOptions::new("x")).is_err());
    let rel: BTreeMap<_, _> = sources
        .keys()
        .map(|k| (k.clone(), PathBuf::from("rel.mov")))
        .collect();
    assert!(to_otio(&tl, &rel, &ExportOptions::new("x")).is_err());
    let empty = Timeline::new(FPS30);
    assert!(to_otio(&empty, &BTreeMap::new(), &ExportOptions::new("x")).is_err());
}

#[test]
fn validator_rejects_broken_documents() {
    let (tl, sources) = sample();
    let (doc, _) = to_otio(&tl, &sources, &ExportOptions::new("talk")).unwrap();
    let mut bad = doc.clone();
    bad["tracks"]["children"][0]["children"][0]["active_media_reference_key"] = "NOPE".into();
    assert!(validate_otio(&bad).is_err());
    let mut bad = doc.clone();
    bad["tracks"]["children"][0]["kind"] = "Subtitle".into();
    assert!(validate_otio(&bad).is_err());
    let mut bad = doc.clone();
    bad["tracks"]["children"][0]["children"][0]["source_range"]["start_time"]["rate"] = 0.0.into();
    assert!(validate_otio(&bad).is_err());
    let mut bad = doc;
    bad["OTIO_SCHEMA"] = "Timeline.2".into();
    assert!(validate_otio(&bad).is_err());
}
