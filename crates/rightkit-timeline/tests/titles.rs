mod common;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use common::*;
use rightkit_timeline::title::{escape_filter_path, wrap_text};
use rightkit_timeline::*;

// Kept unit tests: validation rejections, escaping/wrapping internals and
// the missing-assets error path — none reachable through the engine's serve
// boundary without internal hooks. Title-in-the-pixels delivery is covered
// by `cutright-engine/tests/editing_e2e.rs`.

fn lower_third() -> OverlayClip {
    OverlayClip {
        id: "t1".into(),
        start_frame: 15,
        frames: 60,
        template: TitleTemplate::LowerThird {
            title: "Ada Lovelace".into(),
            subtitle: Some("Analyst & 100% programmer: \"first\", it's 'true'".into()),
        },
        fade_in_frames: 6,
        fade_out_frames: 6,
    }
}

fn base() -> TimelineStore {
    store_with(vec![
        video_source("a", 10.0, false),
        insert("c", "a", 0, 0.0, 4.0),
    ])
}

fn add(id: &str) -> Action {
    let mut clip = lower_third();
    clip.id = id.into();
    Action::AddTitle {
        track_id: "T1".into(),
        clip,
    }
}

fn render(
    s: &TimelineStore,
    preset: Preset,
) -> (BuiltRender, std::collections::BTreeMap<SourceId, PathBuf>) {
    let plan = compile(s.timeline()).unwrap();
    let inputs: BTreeMap<_, _> = [(sid("a"), PathBuf::from("/m/a.mov"))].into();
    let built = build_ffmpeg_render(
        &RenderRequest {
            plan: &plan,
            preset,
            encoder: Encoder::host_system(),
            inputs: &inputs,
            output: Path::new("/o.mp4"),
        },
        Some(&TitleAssets {
            font: Path::new("/fonts/JetBrainsMono-Regular.ttf"),
            text_dir: Path::new("/tmp/titles"),
        }),
    )
    .unwrap();
    (built, inputs)
}

#[test]
fn invalid_titles_are_rejected_without_a_trace() {
    let mut s = base();
    s.apply(add("t1")).unwrap();
    let bad = |t: TitleTemplate| Action::EditTitle {
        clip_id: "t1".into(),
        template: t,
        fade_in_frames: 0,
        fade_out_frames: 0,
    };
    let head = s.head_hash();
    for a in [
        bad(TitleTemplate::Caption { text: "  ".into() }),
        bad(TitleTemplate::Caption {
            text: "a\tb".into(),
        }),
        bad(TitleTemplate::Caption {
            text: "x".repeat(MAX_TITLE_CHARS + 1),
        }),
        Action::TrimTitle {
            clip_id: "t1".into(),
            start_frame: 0,
            frames: 10, // fades 6+6 > 10
        },
        Action::MoveTitle {
            clip_id: "t1".into(),
            start_frame: -1,
        },
        Action::RemoveTitle {
            clip_id: "nope".into(),
        },
        add("t1"), // duplicate id
    ] {
        assert!(s.apply(a).is_err());
    }
    assert_eq!(s.head_hash(), head);
}

#[test]
fn title_json_shape_is_typed() {
    let json = serde_json::to_string(&lower_third()).unwrap();
    assert!(json.contains(r#""template":"lower_third""#), "{json}");
    assert_eq!(
        serde_json::from_str::<OverlayClip>(&json).unwrap(),
        lower_third()
    );
}

#[test]
fn builder_requires_assets_for_titles() {
    let mut s = base();
    s.apply(add("t1")).unwrap();
    let plan = compile(s.timeline()).unwrap();
    let inputs: BTreeMap<_, _> = [(sid("a"), PathBuf::from("/m/a.mov"))].into();
    let req = RenderRequest {
        plan: &plan,
        preset: Preset::YouTube,
        encoder: Encoder::host_system(),
        inputs: &inputs,
        output: Path::new("/o.mp4"),
    };
    assert!(matches!(
        build_ffmpeg_command(&req),
        Err(TimelineError::Unsupported(_))
    ));
    assert!(matches!(
        build_ffmpeg_render(&req, None),
        Err(TimelineError::Unsupported(_))
    ));
}

#[test]
fn text_files_carry_exact_wrapped_text_and_graph_has_no_user_text() {
    let mut s = base();
    s.apply(add("t1")).unwrap();
    let (b, _) = render(&s, Preset::YouTube);
    let graph = b.command.display_lossy();
    assert!(!graph.contains("Lovelace") && !graph.contains("100%"));
    assert_eq!(b.text_files.len(), 2);
    assert!(b.text_files.iter().any(|t| t.content == "Ada Lovelace"));
    assert!(b
        .text_files
        .iter()
        .any(|t| t.content.contains("Analyst & 100% programmer:")));
    // frames 15..=74, single rounding rule, applied after the base chain
    assert!(graph.contains("enable='between(n,15,74)'"));
    assert!(graph.contains("[vbase]drawtext="));
    assert!(graph.contains("[vout];anullsrc"));
}

#[test]
fn title_past_the_end_is_dropped_and_no_titles_keep_vout() {
    let mut s = base();
    let mut late = lower_third();
    late.start_frame = 10_000;
    s.apply(Action::AddTitle {
        track_id: "T1".into(),
        clip: late,
    })
    .unwrap();
    let (b, _) = render(&s, Preset::Reels);
    assert!(b.text_files.is_empty());
    assert!(b.command.display_lossy().contains("[vbase]null[vout]"));
}

#[test]
fn wrapping_and_escaping() {
    assert_eq!(wrap_text("aaa bbb ccc", 7), "aaa bbb\nccc");
    assert_eq!(wrap_text("abcdefghij", 4), "abcd\nefgh\nij");
    assert_eq!(wrap_text("one\ntwo three", 20), "one\ntwo three");
    assert_eq!(wrap_text("héllo wörld", 5), "héllo\nwörld");
    if !cfg!(windows) {
        let e = escape_filter_path(Path::new("/a b/c:d/it's,[x];y.ttf")).unwrap();
        assert_eq!(e, r#"/a\ b/c\\:d/it\\\'s\,\[x\]\;y.ttf"#);
    }
}
