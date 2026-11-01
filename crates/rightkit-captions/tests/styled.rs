//! Styled captions: JSON catalogues, safe-zone placement, karaoke timing, and
//! real `drawtext` renders (frames written for visual review).

use std::path::{Path, PathBuf};

use rightkit_captions::*;
use rightkit_timeline::{Fps, TitleAssets};

fn words() -> Vec<TimedWord> {
    let spec: &[(&str, u64, u64)] = &[
        ("Welcome", 500, 900),
        ("back", 950, 1200),
        ("to", 1250, 1350),
        ("the", 1400, 1500),
        ("channel.", 1550, 2100),
        ("Today", 2300, 2700),
        ("we", 2750, 2850),
        ("are", 2900, 3000),
        ("building", 3050, 3500),
        ("a", 3550, 3600),
        ("video", 3650, 4000),
        ("editor,", 4050, 4500),
        ("and", 4550, 4700),
        ("it", 4750, 4850),
        ("runs", 4900, 5200),
        ("entirely", 5250, 5800),
        ("on", 5850, 5950),
        ("your", 6000, 6250),
        ("own", 6300, 6600),
        ("computer", 6650, 7300),
        ("for", 7350, 7500),
        ("100%", 7550, 8200),
        ("free.", 8250, 8800),
    ];
    spec.iter()
        .map(|&(t, s, e)| TimedWord::new(t, s, e))
        .collect()
}

const PLATFORMS: [&str; 5] = [
    "youtube",
    "youtube-shorts",
    "instagram-reels",
    "tiktok",
    "generic-square",
];

fn emphasis() -> Emphasis {
    Emphasis {
        words: vec!["editor".into(), "computer".into()],
        numbers: true,
    }
}

#[test]
fn catalogues_load_and_cover_the_required_looks() {
    let styles = StyleCatalog::builtin();
    for id in [
        "classic-box",
        "clean-outline",
        "shorts-pop",
        "karaoke-pop",
        "karaoke-fill",
        "card-classic",
        "card-emphasis",
        "minimal-lower",
    ] {
        assert!(styles.get(id).is_some(), "{id}");
    }
    assert!(styles.get("karaoke-pop").unwrap().karaoke.is_some());
    assert!(
        styles
            .get("card-classic")
            .unwrap()
            .boxed
            .as_ref()
            .unwrap()
            .radius_pct
            > 0
    );
    let zones = SafeZoneCatalog::builtin();
    for p in PLATFORMS {
        assert!(zones.get(p).is_some(), "{p}");
    }
    // Round-trips through JSON (data stays data).
    let again: Vec<CaptionStyle> =
        serde_json::from_str(&serde_json::to_string(&styles.styles).unwrap()).unwrap();
    assert_eq!(again, styles.styles);
    assert!(StyleCatalog::from_json(
        &serde_json::to_string(&serde_json::json!({
            "schema": "x", "segmentation": {}, "styles": []
        }))
        .unwrap()
    )
    .is_err());
    // Unknown fields and bad colours are rejected.
    let mut bad = serde_json::to_value(&styles.styles[0]).unwrap();
    bad["text_color"] = "red".into();
    assert!(serde_json::from_value::<CaptionStyle>(bad).is_err());
}

#[test]
fn every_style_on_every_platform_stays_inside_the_safe_zone() {
    let styles = StyleCatalog::builtin();
    let zones = SafeZoneCatalog::builtin();
    let w = words();
    for z in &zones.platforms {
        let (fw, fh) = (z.width as i64, z.height as i64);
        for s in &styles.styles {
            let cues = build_styled_cues(&w, s, &styles, z, fw, fh);
            assert!(!cues.is_empty(), "{} {}", s.id, z.id);
            let cfg = style_config(s, &styles, z, fw, fh);
            for c in &cues {
                assert!(c.lines.len() <= cfg.max_lines.max(1) || c.lines.len() == 1);
                let r = cue_rect(s, z, fw, fh, &c.lines);
                let v = z.violations(&r, fw, fh);
                assert!(
                    v.is_empty(),
                    "{} on {}: {:?} {:?} {:?}",
                    s.id,
                    z.id,
                    c.lines,
                    r,
                    v
                );
            }
        }
    }
}

#[test]
fn tiktok_rail_narrows_the_box_and_youtube_does_not() {
    let styles = StyleCatalog::builtin();
    let zones = SafeZoneCatalog::builtin();
    let s = styles.get("classic-box").unwrap();
    let tt = max_box_width(s, zones.get("tiktok").unwrap(), 1080, 1920);
    let yt = max_box_width(s, zones.get("youtube").unwrap(), 1920, 1080);
    assert!(tt < 1080 * 880 / 1000, "{tt}");
    assert_eq!(yt, 1920 * 800 / 1000);
}

#[test]
fn karaoke_layers_follow_word_frames() {
    let styles = StyleCatalog::builtin();
    let zones = SafeZoneCatalog::builtin();
    let (z, s) = (
        zones.get("tiktok").unwrap(),
        styles.get("karaoke-pop").unwrap(),
    );
    let w = words();
    let cues = build_styled_cues(&w, s, &styles, z, 1080, 1920);
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("cap-karaoke-unit");
    let font = PathBuf::from("/x/font.ttf");
    let assets = TitleAssets {
        font: &font,
        text_dir: &dir,
    };
    let t = CaptionTarget {
        width: 1080,
        height: 1920,
        fps: Fps::new(30, 1).unwrap(),
        total_frames: 30 * 12,
    };
    let (f, files) =
        styled_caption_filters(&w, &cues, s, z, &Emphasis::default(), &t, &assets).unwrap();
    // "back" is spoken 950..1250 ms -> frames 28..=36 (next word starts at 1250 ms = frame 38).
    let back = f
        .iter()
        .find(|x| x.contains("fontcolor=0xFFE600") && x.contains("between(n,28,"))
        .expect("highlight for 'back'");
    assert!(back.contains("between(n,28,37)"), "{back}");
    assert!(files.iter().any(|t| t.content == "BACK"));
    // Deterministic.
    let again = styled_caption_filters(&w, &cues, s, z, &Emphasis::default(), &t, &assets).unwrap();
    assert_eq!((f, files), again);
}

#[test]
fn emphasis_matches_words_and_numbers() {
    let e = emphasis();
    assert!(e.matches("editor,"));
    assert!(e.matches("Computer"));
    assert!(e.matches("100%"));
    assert!(!e.matches("channel."));
}

#[test]
fn rounded_card_boxes_are_disjoint_bands() {
    let styles = StyleCatalog::builtin();
    let zones = SafeZoneCatalog::builtin();
    let (z, s) = (
        zones.get("youtube").unwrap(),
        styles.get("card-classic").unwrap(),
    );
    let w = words();
    let cues = build_styled_cues(&w, s, &styles, z, 1920, 1080);
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("cap-card-unit");
    let font = PathBuf::from("/x/font.ttf");
    let assets = TitleAssets {
        font: &font,
        text_dir: &dir,
    };
    let t = CaptionTarget {
        width: 1920,
        height: 1080,
        fps: Fps::new(30, 1).unwrap(),
        total_frames: 360,
    };
    let (f, _) =
        styled_caption_filters(&w, &cues, s, z, &Emphasis::default(), &t, &assets).unwrap();
    let first_cue_boxes: Vec<&String> = f.iter().take_while(|x| x.starts_with("drawbox")).collect();
    assert_eq!(first_cue_boxes.len(), 9, "1 middle + 4 bands x 2");
}
