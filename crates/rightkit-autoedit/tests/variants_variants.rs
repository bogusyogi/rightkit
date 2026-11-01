use std::path::{Path, PathBuf};
use std::process::Command;

use rightkit_autoedit::cuts::{CutConfig, ManualRemoval};
use rightkit_autoedit::variants::*;
use rightkit_timeline::{
    Action, AudioClip, Encoder, Fps, MilliDb, OverlayClip, SourceId, SourceInfo, Ticks,
    TimelineStore, TitleTemplate, UnderAudio, VideoClip, VideoStreamInfo,
};
use rightkit_transcript::{EndSource, ModelRef, Span, Transcript, Word, LEGACY_SCHEMA as SCHEMA};

/// The host's own H.264 encoder: the bundled LGPL ffmpeg has no libx264.
const H264: &str = if cfg!(target_os = "macos") {
    "h264_videotoolbox"
} else {
    "h264_mf"
};

const HEX: &str = "abababababababababababababababababababababababababababababababab";

fn fps() -> Fps {
    Fps::new(30, 1).unwrap()
}

fn w(text: &str, s: u64, e: u64) -> Word {
    Word {
        text: text.into(),
        start_ms: s,
        end_ms: e,
        confidence: None,
        end_source: EndSource::Model,
    }
}

/// Pauses of 1500 / 400 ms and a filler: tight and natural must differ.
fn transcript() -> Transcript {
    let words = vec![
        w("Hello", 0, 400),
        w("there", 450, 800),
        w("friends.", 2300, 2900),
        w("um", 3300, 3500),
        w("we", 3900, 4100),
        w("go", 4150, 4500),
    ];
    let mut vad: Vec<Span> = Vec::new();
    for x in &words {
        match vad.last_mut() {
            Some(l) if x.start_ms - l.end_ms < 150 => l.end_ms = x.end_ms,
            _ => vad.push(Span {
                start_ms: x.start_ms,
                end_ms: x.end_ms,
            }),
        }
    }
    Transcript {
        schema: SCHEMA.into(),
        source_blake3: HEX.into(),
        language: "en".into(),
        duration_ms: 5000,
        words,
        vad,
        model: ModelRef {
            id: "m".into(),
            revision: "r".into(),
            backend: "synthetic".into(),
        },
    }
}

fn source() -> SourceInfo {
    SourceInfo {
        id: SourceId::new(HEX).unwrap(),
        duration: Ticks::from_millis(5000),
        video: Some(VideoStreamInfo {
            width: 1920,
            height: 1080,
            duration: None,
        }),
        has_audio: true,
    }
}

fn main_store() -> TimelineStore {
    let mut s = TimelineStore::new(fps());
    let src = source();
    s.apply(Action::RegisterSource {
        source: src.clone(),
    })
    .unwrap();
    s.apply(Action::InsertClip {
        track_id: "V1".into(),
        index: 0,
        clip: VideoClip {
            id: "base".into(),
            source: src.id.clone(),
            source_in: Ticks::ZERO,
            source_out: src.duration,
        },
    })
    .unwrap();
    s.apply(Action::AddAudioClip {
        track_id: "A1".into(),
        clip: AudioClip {
            id: "base-a".into(),
            source: src.id,
            trim_in: Ticks::ZERO,
            trim_out: src.duration,
            start: Ticks::ZERO,
            gain: MilliDb::ZERO,
            link: Some("base".into()),
            under: UnderAudio::Keep,
        },
    })
    .unwrap();
    s.apply(Action::AddTitle {
        track_id: "T1".into(),
        clip: OverlayClip {
            id: "t1".into(),
            start_frame: 3,
            frames: 30,
            template: TitleTemplate::Centered { text: "Hi".into() },
            fade_in_frames: 0,
            fade_out_frames: 0,
        },
    })
    .unwrap();
    s
}

fn two() -> (Variant, Variant) {
    let t = transcript();
    let v = create_preset_variants(&source(), &t, None, fps()).unwrap();
    let mut it = v.into_iter();
    (it.next().unwrap(), it.next().unwrap())
}

#[test]
fn custom_config_variant_is_marked_and_removes_words() {
    let t = transcript();
    let mut cfg = CutConfig::natural();
    cfg.manual_removals.push(ManualRemoval {
        start_ms: 450,
        end_ms: 800,
    });
    let v = create_variant("no-there", &source(), &t, None, cfg, fps()).unwrap();
    assert!(v.provenance.custom);
    assert!(!v.plan.kept_words.contains(&1));
    let (_, natural) = two();
    let (report, _) = compare_variants(&natural, &v, &t).unwrap();
    assert_eq!(report.only_a, vec![1]);
    assert!(report.only_b.is_empty());
    assert_eq!(report.words[1].status, WordStatus::OnlyA);
}

#[test]
fn bad_names_and_mismatched_transcript_are_refused() {
    let t = transcript();
    for n in ["", "Tight", "a/b", "..", "-x", "a b"] {
        assert!(
            matches!(
                create_variant(n, &source(), &t, None, CutConfig::tight(), fps()),
                Err(VariantError::BadName(_))
            ),
            "{n}"
        );
    }
    let mut other = t.clone();
    other.source_blake3 = "cd".repeat(32);
    assert!(matches!(
        create_variant("x", &source(), &other, None, CutConfig::tight(), fps()),
        Err(VariantError::Mismatch(_))
    ));
    let (a, b) = two();
    let mut t2 = t;
    t2.language = "fr".into();
    assert!(matches!(
        compare_variants(&a, &b, &t2),
        Err(VariantError::Mismatch(_))
    ));
}

#[test]
fn save_load_list_delete_and_main_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let main_path = dir.path().join("timeline.json");
    let main = main_store();
    std::fs::write(&main_path, main.to_canonical_json()).unwrap();
    let before = std::fs::read(&main_path).unwrap();

    let (tight, natural) = two();
    let p = save_variant(dir.path(), &tight, false).unwrap();
    assert!(p.ends_with("variants/tight/timeline.json"));
    save_variant(dir.path(), &natural, false).unwrap();
    assert!(matches!(
        save_variant(dir.path(), &tight, false),
        Err(VariantError::Exists(_))
    ));
    save_variant(dir.path(), &tight, true).unwrap();
    assert_eq!(list_variants(dir.path()).unwrap(), ["natural", "tight"]);

    let back = load_variant(dir.path(), "tight").unwrap();
    assert_eq!(back.provenance, tight.provenance);
    assert_eq!(back.plan, tight.plan);
    assert_eq!(back.store.head_hash(), tight.store.head_hash());
    assert!(!back.is_edited());
    assert_eq!(std::fs::read(&main_path).unwrap(), before);

    // Tampering with the plan is detected.
    let plan_path = dir.path().join("variants/tight/plan.json");
    let mut bytes = std::fs::read(&plan_path).unwrap();
    bytes.push(b' ');
    std::fs::write(&plan_path, bytes).unwrap();
    assert!(matches!(
        load_variant(dir.path(), "tight"),
        Err(VariantError::Integrity { .. })
    ));
    delete_variant(dir.path(), "tight").unwrap();
    assert!(matches!(
        load_variant(dir.path(), "tight"),
        Err(VariantError::NotFound(_))
    ));
    assert_eq!(list_variants(dir.path()).unwrap(), ["natural"]);
}

#[test]
fn editing_a_variant_timeline_is_flagged() {
    let dir = tempfile::tempdir().unwrap();
    let (mut tight, _) = two();
    tight
        .store
        .apply(Action::AddTitle {
            track_id: "T1".into(),
            clip: OverlayClip {
                id: "tt".into(),
                start_frame: 0,
                frames: 10,
                template: TitleTemplate::Caption { text: "x".into() },
                fade_in_frames: 0,
                fade_out_frames: 0,
            },
        })
        .unwrap();
    assert!(tight.is_edited());
    save_variant(dir.path(), &tight, false).unwrap();
    assert!(load_variant(dir.path(), "tight").unwrap().is_edited());
}

#[test]
fn alignment_is_word_locked_and_matches_the_time_maps() {
    let t = transcript();
    let (tight, natural) = two();
    let (report, seek) = compare_variants(&tight, &natural, &t).unwrap();
    assert_eq!(report.schema, COMPARE_SCHEMA);
    assert_eq!(report.words.len(), t.words.len());
    assert_eq!(report.a.name, "tight");
    assert_eq!(report.b.name, "natural");
    assert_eq!(
        report.duration_delta_ms,
        report.b.output_ms - report.a.output_ms
    );
    assert!(report.duration_delta_ms > 0);
    assert_eq!(report.cut_in_both, 1, "the filler");
    assert_eq!(report.words[3].status, WordStatus::Neither);
    assert_eq!(report.shared_words, 5);

    let (ta, tb) = (tight.time_map(), natural.time_map());
    for (i, word) in t.words.iter().enumerate() {
        if i == 3 {
            assert!(seek.entries[i].a_ms.is_none() && seek.entries[i].b_ms.is_none());
            continue;
        }
        // Independent check against the source->output map (word starts sit
        // inside kept ranges here).
        let a = ta.source_ms_to_output_ms(word.start_ms as i64).unwrap();
        let b = tb.source_ms_to_output_ms(word.start_ms as i64).unwrap();
        let e = seek.entries[i];
        assert_eq!((e.a_ms, e.b_ms), (Some(a), Some(b)), "word {i}");
        assert_eq!(report.words[i].delta_ms, Some(b - a));
        assert_eq!(seek.seek(i, Side::A), Some(a));
        // The map inverts back to the same source word.
        assert_eq!(ta.output_ms_to_source_ms(a), Some(word.start_ms as i64));
        assert_eq!(tb.output_ms_to_source_ms(b), Some(word.start_ms as i64));
    }
    // Words after a longer pause are later in natural than in tight.
    assert!(report.words[2].delta_ms.unwrap() > 0);
    assert_eq!(report.words[0].delta_ms, Some(0));
    assert_eq!(
        report.max_abs_delta_ms,
        report.words[5].delta_ms.unwrap().abs()
    );

    // Cut word snaps to the next kept word; past-the-end snaps to duration.
    assert_eq!(seek.seek(3, Side::A), seek.entries[4].a_ms);
    assert_eq!(seek.seek(3, Side::B), seek.entries[4].b_ms);
    assert_eq!(seek.next_shared(3), Some(4));
    assert_eq!(seek.a_duration_ms, report.a.output_ms);

    // Player sync: the position of a word in A maps to the same word in B,
    // and to the right offset inside it.
    let a2 = seek.entries[2].a_ms.unwrap();
    let b2 = seek.entries[2].b_ms.unwrap();
    assert_eq!(seek.sync(Side::A, a2), Some(b2));
    assert_eq!(seek.sync(Side::A, a2 + 100), Some(b2 + 100));
    assert_eq!(seek.sync(Side::B, b2), Some(a2));

    // Report round-trips through JSON for the UI.
    let json = serde_json::to_string(&report).unwrap();
    assert_eq!(
        serde_json::from_str::<CompareReport>(&json).unwrap(),
        report
    );
}

#[test]
fn identical_variants_compare_as_equal() {
    let t = transcript();
    let (tight, _) = two();
    let (r, _) = compare_variants(&tight, &tight, &t).unwrap();
    assert_eq!(r.duration_delta_ms, 0);
    assert_eq!(r.max_abs_delta_ms, 0);
    assert!(r.only_a.is_empty() && r.only_b.is_empty());
}

#[test]
fn promote_is_undoable_redoable_and_idempotent() {
    let (tight, natural) = two();
    let mut main = main_store();
    let original = main.head_hash();
    let original_cursor = main.cursor();

    let p = promote_variant(&mut main, &tight).unwrap();
    assert_eq!(p.removed_video_clips, 1);
    assert_eq!(p.removed_audio_clips, 1);
    assert_eq!(p.titles_kept, 1);
    assert!(p.applied.revisions > 0);
    let (mtl, vtl) = (main.timeline(), tight.store.timeline());
    assert_eq!(mtl.video_tracks, vtl.video_tracks);
    assert_eq!(mtl.audio_tracks, vtl.audio_tracks);
    assert_eq!(mtl.overlay_tracks[0].clips[0].id, "t1");
    mtl.validate().unwrap();
    let promoted = main.head_hash();

    // Re-promoting the same variant records nothing.
    let again = promote_variant(&mut main, &tight).unwrap();
    assert_eq!(again.applied.revisions, 0);
    assert_eq!(main.head_hash(), promoted);
    undo_promotion(&mut main, &again).unwrap(); // no-op group

    // Switching to the other variant is its own group; undo returns to tight.
    let q = promote_variant(&mut main, &natural).unwrap();
    assert_eq!(
        main.timeline().video_tracks,
        natural.store.timeline().video_tracks
    );
    undo_promotion(&mut main, &q).unwrap();
    assert_eq!(main.head_hash(), promoted);

    // Undo the first promotion: main is exactly as before.
    undo_promotion(&mut main, &p).unwrap();
    assert_eq!(main.head_hash(), original);
    assert_eq!(main.cursor(), original_cursor);
    // Redo replays the whole group.
    for _ in 0..p.applied.revisions {
        main.redo().unwrap();
    }
    assert_eq!(main.head_hash(), promoted);

    // The main document still verifies after promote + undo + redo.
    let reopened = TimelineStore::from_canonical_json(&main.to_canonical_json()).unwrap();
    assert_eq!(reopened.head_hash(), promoted);
}

#[test]
fn undoing_a_stale_promotion_is_refused() {
    let (tight, _) = two();
    let mut main = main_store();
    let p = promote_variant(&mut main, &tight).unwrap();
    main.apply(Action::AddTitle {
        track_id: "T1".into(),
        clip: OverlayClip {
            id: "t2".into(),
            start_frame: 0,
            frames: 5,
            template: TitleTemplate::Caption {
                text: "later".into(),
            },
            fade_in_frames: 0,
            fade_out_frames: 0,
        },
    })
    .unwrap();
    assert!(undo_promotion(&mut main, &p).is_err());
}

#[test]
fn promote_into_a_main_without_the_source_registers_it() {
    let (tight, _) = two();
    let mut main = TimelineStore::new(fps());
    let p = promote_variant(&mut main, &tight).unwrap();
    assert_eq!(p.removed_video_clips, 0);
    assert_eq!(main.timeline().sources.len(), 1);
    assert_eq!(
        main.timeline().video_tracks,
        tight.store.timeline().video_tracks
    );
    undo_promotion(&mut main, &p).unwrap();
    assert!(main.timeline().sources.is_empty());
}

// ---- compare render ------------------------------------------------------

fn ffmpeg_bin() -> Option<PathBuf> {
    std::env::var_os("RIGHTKIT_FFMPEG")
        .map(PathBuf::from)
        .filter(|p| p.is_file())
}

fn font() -> PathBuf {
    std::env::var_os("RIGHTKIT_FONT")
        .map(PathBuf::from)
        .unwrap_or_default()
}

fn request<'a>(
    a: &'a Path,
    b: &'a Path,
    dir: &'a Path,
    out: &'a Path,
    font: &'a Path,
    word: usize,
    layout: CompareLayout,
) -> CompareRenderRequest<'a> {
    CompareRenderRequest {
        a_video: a,
        b_video: b,
        a_label: "tight",
        b_label: "natural",
        layout,
        anchor_word: word,
        lead_ms: 800,
        max_duration_ms: None,
        panel_width: 320,
        panel_height: 180,
        fps: 30,
        audio: CompareAudio::A,
        encoder: Encoder::host_system(),
        font,
        text_dir: dir,
        output: out,
    }
}

#[test]
fn compare_render_command_locks_both_sides_on_the_word() {
    let t = transcript();
    let (tight, natural) = two();
    let (report, seek) = compare_variants(&tight, &natural, &t).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let (a, b, out, f) = (
        dir.path().join("a.mp4"),
        dir.path().join("b.mp4"),
        dir.path().join("o.mp4"),
        font(),
    );
    let req = request(&a, &b, dir.path(), &out, &f, 4, CompareLayout::SideBySide);
    let built = build_compare_render(&seek, &report, &req).unwrap();
    // Word 4 starts at its own output time in each; both clips start 800 ms earlier.
    assert_eq!(
        built.start_a_ms + built.anchor_at_ms,
        seek.entries[4].a_ms.unwrap()
    );
    assert_eq!(
        built.start_b_ms + built.anchor_at_ms,
        seek.entries[4].b_ms.unwrap()
    );
    assert_eq!(built.anchor_at_ms, 800);
    assert_ne!(built.start_a_ms, built.start_b_ms);
    let args: Vec<String> = built
        .command
        .to_os_args()
        .iter()
        .map(|s| s.to_string_lossy().into_owned())
        .collect();
    let graph = args.iter().find(|a| a.contains("hstack")).unwrap();
    assert!(graph.contains("drawtext") && graph.contains("[p0][p1]hstack=inputs=2[v]"));
    assert_eq!(built.text_files.len(), 4);
    assert!(built.text_files.iter().any(|f| f.content == "A: tight"));
    assert!(built.text_files.iter().any(|f| f.content == "we"));

    let stacked = request(&a, &b, dir.path(), &out, &f, 4, CompareLayout::Stacked);
    let s = build_compare_render(&seek, &report, &stacked).unwrap();
    assert!(s
        .command
        .to_os_args()
        .iter()
        .any(|a| a.to_string_lossy().contains("vstack")));

    // Lead is clamped so the lock holds even for the first word.
    let first = request(&a, &b, dir.path(), &out, &f, 0, CompareLayout::Stacked);
    let fb = build_compare_render(&seek, &report, &first).unwrap();
    assert_eq!((fb.start_a_ms, fb.start_b_ms, fb.anchor_at_ms), (0, 0, 0));

    // A word cut in either variant cannot anchor the render.
    let cut = request(&a, &b, dir.path(), &out, &f, 3, CompareLayout::Stacked);
    assert!(matches!(
        build_compare_render(&seek, &report, &cut),
        Err(VariantError::WordNotShared { index: 3, .. })
    ));
    let none = request(&a, &b, dir.path(), &out, &f, 99, CompareLayout::Stacked);
    assert!(matches!(
        build_compare_render(&seek, &report, &none),
        Err(VariantError::NoSuchWord(99))
    ));
}

/// Renders two synthetic "variant" videos whose frames show their own output
/// time, runs the compare render, and checks the anchor word lands at the same
/// instant in both panels (frame PNG left in `target/` for eyeballing).
#[test]
fn compare_render_runs_with_real_ffmpeg() {
    let Some(ffmpeg) = ffmpeg_bin() else {
        let message = "SKIP compare_render: no editing ffmpeg (looked at RIGHTKIT_FFMPEG)";
        if std::env::var("RIGHTKIT_REQUIRE_ASSETS").is_ok_and(|value| value == "1") {
            panic!("{message}");
        }
        eprintln!("{message}");
        return;
    };
    let t = transcript();
    let (tight, natural) = two();
    let (report, seek) = compare_variants(&tight, &natural, &t).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let f = font();
    let mk = |name: &str, ms: i64, color: &str| -> PathBuf {
        let p = dir.path().join(name);
        let st = Command::new(&ffmpeg)
            .args(["-hide_banner", "-y", "-f", "lavfi", "-i"])
            .arg(format!(
                "color=c={color}:s=640x360:r=30:d={}",
                ms as f64 / 1000.0
            ))
            .args(["-f", "lavfi", "-i"])
            .arg(format!(
                "sine=frequency=440:duration={}",
                ms as f64 / 1000.0
            ))
            .args([
                "-c:v",
                H264,
                "-b:v",
                "4M",
                "-pix_fmt",
                "yuv420p",
                "-c:a",
                "aac",
                "-shortest",
            ])
            .arg(&p)
            .output()
            .unwrap();
        assert!(
            st.status.success(),
            "{}",
            String::from_utf8_lossy(&st.stderr)
        );
        p
    };
    let a = mk("a.mp4", seek.a_duration_ms, "red");
    let b = mk("b.mp4", seek.b_duration_ms, "blue");
    for layout in [CompareLayout::SideBySide, CompareLayout::Stacked] {
        let out = dir.path().join(format!("o-{layout:?}.mp4"));
        let req = request(&a, &b, dir.path(), &out, &f, 4, layout);
        let built = build_compare_render(&seek, &report, &req).unwrap();
        execute_compare(&ffmpeg, &built).unwrap();
        let probe = Command::new(ffmpeg.with_file_name("ffprobe"))
            .args([
                "-v",
                "error",
                "-show_entries",
                "stream=codec_type,width,height:format=duration",
                "-of",
                "json",
            ])
            .arg(&out)
            .output()
            .unwrap();
        let j: serde_json::Value = serde_json::from_slice(&probe.stdout).unwrap();
        let v = j["streams"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["codec_type"] == "video")
            .unwrap();
        let (ew, eh) = match layout {
            CompareLayout::SideBySide => (640, 180),
            CompareLayout::Stacked => (320, 360),
        };
        assert_eq!(
            (v["width"].as_i64(), v["height"].as_i64()),
            (Some(ew), Some(eh))
        );
        assert!(j["streams"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["codec_type"] == "audio"));
        let dur: f64 = j["format"]["duration"].as_str().unwrap().parse().unwrap();
        assert!(
            (dur * 1000.0 - built.duration_ms as f64).abs() < 100.0,
            "{dur}"
        );
        // Keep one frame per layout (just after the anchor word) for review.
        let keep = std::env::temp_dir().join("cutright-variants-compare");
        std::fs::create_dir_all(&keep).unwrap();
        let png = keep.join(format!("compare-{layout:?}.png"));
        let st = Command::new(&ffmpeg)
            .args(["-hide_banner", "-y", "-ss", "0.9", "-i"])
            .arg(&out)
            .args(["-frames:v", "1"])
            .arg(&png)
            .output()
            .unwrap();
        assert!(st.status.success());
    }
}
