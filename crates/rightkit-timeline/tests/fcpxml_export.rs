use std::collections::BTreeMap;
use std::path::PathBuf;

use quick_xml::events::Event;
use quick_xml::Reader;
use rightkit_timeline::fcpxml::{export_fcpxml, CaptionCue, ExportOptions};
use rightkit_timeline::*;

const FPS: Fps = Fps {
    num: 30000,
    den: 1001,
};

fn sid(name: &str) -> SourceId {
    SourceId::new(blake3::hash(name.as_bytes()).to_hex().to_string()).unwrap()
}

fn secs(s: f64) -> Ticks {
    Ticks((s * TICKS_PER_SECOND as f64).round() as i64)
}

fn register(name: &str, seconds: f64, video: bool, audio: bool) -> Action {
    Action::RegisterSource {
        source: SourceInfo {
            id: sid(name),
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

fn timeline() -> Timeline {
    let mut s = TimelineStore::new(FPS);
    let mut a = |act: Action| {
        s.apply(act).unwrap();
    };
    a(register("silent", 5.0, true, false));
    a(register("scene", 10.0, true, true));
    a(register("vo", 8.0, false, true));
    a(register("bed", 20.0, false, true));
    for (id, src, i, from, to) in [("v1", "silent", 0, 0.0, 3.0), ("v2", "scene", 1, 2.0, 5.5)] {
        a(Action::InsertClip {
            track_id: "V1".into(),
            index: i,
            clip: VideoClip {
                id: id.into(),
                source: sid(src),
                source_in: secs(from),
                source_out: secs(to),
            },
        });
    }
    a(Action::AddAudioTrack {
        track_id: "A2".into(),
        role: AudioRole::Music,
    });
    let start = FPS.frame_to_ticks(60);
    a(Action::AddAudioClip {
        track_id: "A1".into(),
        clip: AudioClip {
            id: "n1".into(),
            source: sid("vo"),
            trim_in: secs(1.0),
            trim_out: secs(7.0),
            start,
            gain: MilliDb::from_db(-3.0),
            link: Some("v2".into()),
            under: UnderAudio::Duck {
                amount: MilliDb::from_db(12.0),
            },
        },
    });
    a(Action::AddAudioClip {
        track_id: "A2".into(),
        clip: AudioClip {
            id: "m1".into(),
            source: sid("bed"),
            trim_in: secs(0.0),
            trim_out: secs(4.0),
            start: Ticks::ZERO,
            gain: MilliDb::ZERO,
            link: None,
            under: UnderAudio::Keep,
        },
    });
    for (id, frames_at, tpl) in [
        (
            "c1",
            10,
            TitleTemplate::Caption {
                text: "Hello & <welcome>".into(),
            },
        ),
        (
            "t1",
            30,
            TitleTemplate::Centered {
                text: "Chapter".into(),
            },
        ),
    ] {
        a(Action::AddTitle {
            track_id: "T1".into(),
            clip: OverlayClip {
                id: id.into(),
                start_frame: frames_at,
                frames: 30,
                template: tpl,
                fade_in_frames: 0,
                fade_out_frames: 0,
            },
        });
    }
    s.timeline().clone()
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

fn media() -> BTreeMap<SourceId, PathBuf> {
    [
        ("silent", "/media/My Clips/silent.mov"),
        ("scene", "/media/scene.mov"),
        ("vo", "/media/vo.wav"),
        ("bed", "/media/bed.wav"),
    ]
    .into_iter()
    .map(|(n, p)| (sid(n), media_path(p)))
    .collect()
}

#[derive(Debug)]
struct Node {
    name: String,
    attrs: BTreeMap<String, String>,
    children: Vec<Node>,
}

fn parse(xml: &str) -> Node {
    let mut r = Reader::from_str(xml);
    let mut stack: Vec<Node> = vec![Node {
        name: "#root".into(),
        attrs: BTreeMap::new(),
        children: vec![],
    }];
    let mk = |e: &quick_xml::events::BytesStart| Node {
        name: String::from_utf8_lossy(e.name().as_ref()).into_owned(),
        attrs: e
            .attributes()
            .map(|a| {
                let a = a.unwrap();
                (
                    String::from_utf8_lossy(a.key.as_ref()).into_owned(),
                    String::from_utf8_lossy(&a.value)
                        .replace("&quot;", "\"")
                        .replace("&lt;", "<")
                        .replace("&gt;", ">")
                        .replace("&apos;", "'")
                        .replace("&amp;", "&"),
                )
            })
            .collect(),
        children: vec![],
    };
    loop {
        match r.read_event().expect("well-formed XML") {
            Event::Start(e) => stack.push(mk(&e)),
            Event::Empty(e) => {
                let n = mk(&e);
                stack.last_mut().unwrap().children.push(n);
            }
            Event::End(_) => {
                let n = stack.pop().unwrap();
                stack.last_mut().expect("balanced tags").children.push(n);
            }
            Event::Eof => break,
            _ => {}
        }
    }
    assert_eq!(stack.len(), 1, "unclosed element");
    let mut root = stack.pop().unwrap();
    assert_eq!(root.children.len(), 1, "exactly one root element");
    root.children.remove(0)
}

impl Node {
    fn find(&self, name: &str) -> &Node {
        self.children
            .iter()
            .find(|c| c.name == name)
            .unwrap_or_else(|| panic!("no <{name}> in <{}>", self.name))
    }
}

/// `N/Ds` or `Ns` as an exact (num, den).

#[test]
fn unsupported_items_and_captions_are_reported() {
    let tl = timeline();
    let dir = tempfile::tempdir().unwrap();
    let mut opts = ExportOptions::new("demo");
    opts.extra_cues = vec![CaptionCue {
        start_ms: 0,
        end_ms: 500,
        text: "first".into(),
    }];
    let rep = export_fcpxml(&tl, &media(), dir.path(), &opts).unwrap();
    let kinds: Vec<_> = rep
        .unsupported
        .iter()
        .map(|u| (u.kind, u.id.as_str()))
        .collect();
    assert!(kinds.contains(&("title", "t1")), "{kinds:?}");
    assert!(kinds.contains(&("ducking", "n1")), "{kinds:?}");
    assert!(
        !kinds.iter().any(|k| k.1 == "c1"),
        "caption titles go to the SRT"
    );
    let srt = std::fs::read_to_string(rep.srt.unwrap()).unwrap();
    assert_eq!(
        srt,
        "1\n00:00:00,000 --> 00:00:00,500\nfirst\n\n2\n00:00:00,334 --> 00:00:01,335\nHello & <welcome>\n\n"
    );
    assert_eq!(
        (rep.video_clips, rep.audio_clips, rep.caption_cues),
        (2, 2, 2)
    );
    // Escaping in the XML keeps it well formed.
    parse(&std::fs::read_to_string(rep.fcpxml).unwrap());
}

#[test]
fn collecting_media_copies_files_next_to_the_xml() {
    let tl = timeline();
    let src_dir = tempfile::tempdir().unwrap();
    let mut map = BTreeMap::new();
    for (n, file) in [
        ("silent", "silent.mov"),
        ("scene", "scene.mov"),
        ("vo", "vo.wav"),
        ("bed", "bed.wav"),
    ] {
        let p = src_dir.path().join(file);
        std::fs::write(&p, n.as_bytes()).unwrap();
        map.insert(sid(n), p);
    }
    let out = tempfile::tempdir().unwrap();
    let mut opts = ExportOptions::new("collected");
    opts.collect_media = true;
    let rep = export_fcpxml(&tl, &map, out.path(), &opts).unwrap();
    assert_eq!(rep.media.len(), 4);
    for m in &rep.media {
        assert!(m.collected && m.path.starts_with(out.path().join("Media")));
        assert!(m.path.is_file());
        assert!(m.url.starts_with("file://"));
    }
    let xml = std::fs::read_to_string(&rep.fcpxml).unwrap();
    assert!(xml.contains("/Media/"));
    assert!(!xml.contains(src_dir.path().to_str().unwrap()));
}

#[test]
fn collecting_changed_media_refuses_without_replacing_destination_or_source() {
    let tl = timeline();
    let source = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let mut map = BTreeMap::new();
    for name in ["silent", "scene", "vo", "bed"] {
        let path = source.path().join(format!("{name}.mov"));
        std::fs::write(&path, name.as_bytes()).unwrap();
        map.insert(sid(name), path);
    }
    let changed = map.get(&sid("scene")).unwrap();
    std::fs::write(changed, b"changed source bytes").unwrap();
    let media_dir = out.path().join("Media");
    std::fs::create_dir(&media_dir).unwrap();
    let destination = media_dir.join(format!("{}-scene.mov", &sid("scene").as_str()[..8]));
    std::fs::write(&destination, b"previous export").unwrap();
    let mut options = ExportOptions::new("verified");
    options.collect_media = true;
    assert!(export_fcpxml(&tl, &map, out.path(), &options).is_err());
    assert_eq!(std::fs::read(&destination).unwrap(), b"previous export");
    assert_eq!(std::fs::read(changed).unwrap(), b"changed source bytes");
}

#[test]
fn bad_inputs_are_refused() {
    let tl = timeline();
    let dir = tempfile::tempdir().unwrap();
    let mut rel = media();
    rel.insert(sid("scene"), PathBuf::from("relative/scene.mov"));
    assert!(export_fcpxml(&tl, &rel, dir.path(), &ExportOptions::new("x")).is_err());
    let mut missing = media();
    missing.remove(&sid("vo"));
    assert!(export_fcpxml(&tl, &missing, dir.path(), &ExportOptions::new("x")).is_err());
    assert!(export_fcpxml(&tl, &media(), dir.path(), &ExportOptions::new("../x")).is_err());
    let empty = Timeline::new(FPS);
    assert!(export_fcpxml(&empty, &media(), dir.path(), &ExportOptions::new("x")).is_err());
}

#[test]
fn audio_only_timeline_uses_a_gap_spine() {
    let mut s = TimelineStore::new(Fps { num: 25, den: 1 });
    s.apply(register("vo", 4.0, false, true)).unwrap();
    s.apply(Action::AddAudioClip {
        track_id: "A1".into(),
        clip: AudioClip {
            id: "n1".into(),
            source: sid("vo"),
            trim_in: Ticks::ZERO,
            trim_out: secs(4.0),
            start: Ticks::ZERO,
            gain: MilliDb::ZERO,
            link: None,
            under: UnderAudio::Keep,
        },
    })
    .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let mut m = BTreeMap::new();
    m.insert(sid("vo"), media_path("/media/vo.wav"));
    let rep = export_fcpxml(s.timeline(), &m, dir.path(), &ExportOptions::new("a")).unwrap();
    let root = parse(&std::fs::read_to_string(rep.fcpxml).unwrap());
    let spine = root
        .find("library")
        .find("event")
        .find("project")
        .find("sequence")
        .find("spine");
    assert_eq!(spine.children.len(), 1);
    assert_eq!(spine.children[0].name, "gap");
    assert_eq!(spine.children[0].attrs["duration"], "4s");
    assert_eq!(spine.children[0].children[0].attrs["lane"], "-1");
}
