// rightkit-motion 0.2.0 consumed by exact registry pin, outside the workspace.
//
// Proves the planning and encoding surface GenRight would adopt (genright-engine
// motion_service.rs:219 pending-adoption comment; motion_encode.rs:157 pick_encoder and :231
// build_command): a bracketed script parsed and one span snapped to timed words; a storyboard
// sheet planned from that beat; the bits-per-pixel bitrate map; an H.264 software encoder picked
// from a listed ffmpeg inventory (no ffmpeg process is started); refusal when no H.264 encoder is
// listed; the ffmpeg argument vector for the picked encoder.
// Does not prove: the render driver (it runs ffmpeg), png_to_ppm and png_to_pam (PNG decode),
// stills and posters, the assemble feature, or any real encode. Those stay under the crate's
// asset-gated encode_e2e entry in ci/asset-gated.json.
// State: no files, no processes, no network.
use std::path::PathBuf;

use rightkit_ffmpeg::{EncoderInventory, FfmpegArg};
use rightkit_motion::{
    build_command, parse_script, pick_encoder, plan_sheets, snap, target_bitrate, EncodeSpec,
    Quality, Selection, SheetSpec, SnapConfig,
};
use rightkit_transcript::TimedWord;

fn word(text: &str, start_ms: u64, end_ms: u64) -> TimedWord {
    TimedWord {
        text: text.to_owned(),
        start_ms,
        end_ms,
        source_ms: None,
    }
}

fn main() {
    // Script and timing: four spoken words, with one bracketed span on "important".
    let script = parse_script("This is [important] stuff").expect("script parses");
    assert_eq!(script.words.len(), 4, "words: {:?}", script.words);
    assert_eq!(script.spans.len(), 1);
    let words = vec![
        word("This", 0, 300),
        word("is", 300, 500),
        word("important", 500, 1100),
        word("stuff", 1100, 1500),
    ];
    let beats = snap(&script, &words, SnapConfig::default()).expect("snap");
    assert_eq!(beats.len(), 1);
    assert_eq!(
        (beats[0].span, beats[0].start_ms, beats[0].end_ms),
        (0, 500, 1100),
        "the span snaps to the timed words of 'important'"
    );

    // Storyboard inspect sheet planned from the beat: one tile at the beat start, one sheet.
    let plan = plan_sheets(SheetSpec::new(1500, 30.0, Selection::Beats(beats.clone())))
        .expect("sheet plan");
    assert_eq!(plan.sheets.len(), 1);
    assert_eq!(plan.sheets[0].tiles.len(), 1);
    assert_eq!(plan.sheets[0].tiles[0].time_ms, 500);

    // Bitrate map at Standard quality: 1920 x 1080 x 30 x 12 hundredths, over 100.
    assert_eq!(target_bitrate(1920, 1080, 30, Quality::Standard), 7_464_960);

    // Encoder pick from a listed ffmpeg inventory. No hardware H.264 is listed, so the listed
    // software encoder wins and MPEG-4 is not used because it was not allowed.
    let listing = " V....D libopenh264          OpenH264 H.264\n\
                   \x20V....D mpeg4                 MPEG-4 part 2\n\
                   \x20A....D aac                   AAC\n";
    let inventory = EncoderInventory::parse(listing);
    let encoder = pick_encoder(&inventory, "windows", false, &mut |_| true)
        .expect("software H.264 from the listing");
    assert_eq!(encoder.name, "libopenh264");
    assert!(!encoder.fallback, "not an MPEG-4 fallback");

    // No H.264 encoder listed at all: refused, never silently opaque MPEG-4.
    let without_h264 = EncoderInventory::parse(" V....D mpeg4                 MPEG-4 part 2\n");
    assert!(
        pick_encoder(&without_h264, "windows", false, &mut |_| true).is_err(),
        "no H.264 encoder listed must refuse"
    );

    // The ffmpeg argument vector for the picked encoder, built but never run.
    let spec = EncodeSpec {
        encoder,
        width: 1920,
        height: 1080,
        fps: 30,
        duration: 1.5,
        quality: Quality::Standard,
        audio: Vec::new(),
        output: PathBuf::from("out.mp4"),
    };
    let command = build_command(&spec).expect("command for the picked encoder");
    assert!(
        command
            .args()
            .iter()
            .any(|arg| matches!(arg, FfmpegArg::Text(text) if text.as_str() == "libopenh264")),
        "the picked encoder name is an ffmpeg argument"
    );

    println!("OK rightkit-motion 0.2.0 (registry consumer, encode feature, no ffmpeg run)");
}
