//! rightkit-motion (default features: beats and sheets, no media): parse a bracketed script, snap its cues to timed
//! words with lead and tail padding, refuse a mismatching transcript, and plan a storyboard sheet layout whose frame
//! numbers and tile positions are computed by hand here. The render and encode half needs the `encode` feature and an
//! ffmpeg binary, and is not part of this probe.
use rightkit_motion::{
    parse_script, plan_sheets, snap, BeatError, Selection, SheetError, SheetSpec, SnapConfig,
    SHEETS_SCHEMA,
};
use rightkit_transcript::TimedWord;
use serde_json::{json, Value};

use super::Ctx;

fn word(text: &str, start_ms: u64, end_ms: u64) -> TimedWord {
    TimedWord::new(text, start_ms, end_ms)
}

pub fn probe(_ctx: &Ctx) -> Result<Value, String> {
    // Parse: spans map to words, escapes are literal, malformed brackets are typed errors.
    let script =
        parse_script("We ship [three tools] and [one] rule").map_err(|e| format!("parse: {e}"))?;
    if script.text != "We ship three tools and one rule"
        || script.words.len() != 7
        || script.spans.len() != 2
    {
        return Err(format!("parsed script is wrong: {:?}", script));
    }
    let first = &script.spans[0];
    let second = &script.spans[1];
    if (first.word_start, first.word_end, first.text.as_str()) != (2, 4, "three tools")
        || (second.word_start, second.word_end, second.text.as_str()) != (5, 6, "one")
    {
        return Err(format!("span coverage is wrong: {first:?} {second:?}"));
    }
    let escaped = parse_script(r"a \[b\] [c]").map_err(|e| e.to_string())?;
    if escaped.text != "a [b] c" || escaped.spans.len() != 1 {
        return Err(format!("escapes are not literal: {escaped:?}"));
    }
    if parse_script("a [b") != Err(BeatError::UnclosedSpan(2))
        || parse_script("a [ ] b") != Err(BeatError::EmptySpan(2))
    {
        return Err("malformed brackets were not refused with the right error".into());
    }

    // Snap: beats land on the covered words' timings; padding clamps at zero and extends the tail.
    let words = vec![
        word("We", 0, 200),
        word("ship", 200, 500),
        word("three", 600, 900),
        word("tools,", 900, 1300),
        word("and", 1300, 1500),
        word("one", 1600, 1900),
        word("rule.", 1900, 2400),
    ];
    let beats = snap(&script, &words, SnapConfig::default()).map_err(|e| format!("snap: {e}"))?;
    let got: Vec<(usize, u64, u64, usize, usize)> = beats
        .iter()
        .map(|b| (b.span, b.start_ms, b.end_ms, b.word_start, b.word_end))
        .collect();
    if got != vec![(0, 600, 1300, 2, 4), (1, 1600, 1900, 5, 6)] {
        return Err(format!("beats are wrong: {got:?}"));
    }
    let padded = snap(
        &script,
        &words,
        SnapConfig {
            lead_ms: 100,
            tail_ms: 250,
        },
    )
    .map_err(|e| e.to_string())?;
    if (
        padded[0].start_ms,
        padded[0].end_ms,
        padded[1].start_ms,
        padded[1].end_ms,
    ) != (500, 1550, 1500, 2150)
    {
        return Err(format!("padded beats are wrong: {padded:?}"));
    }
    let early = parse_script("[Go] now").map_err(|e| e.to_string())?;
    let clamped = snap(
        &early,
        &[word("Go", 40, 300), word("now", 300, 600)],
        SnapConfig {
            lead_ms: 100,
            tail_ms: 0,
        },
    )
    .map_err(|e| e.to_string())?;
    if clamped[0].start_ms != 0 {
        return Err(format!(
            "lead padding went below zero: {}",
            clamped[0].start_ms
        ));
    }
    let mut wrong = words.clone();
    wrong[2] = word("four", 600, 900);
    match snap(&script, &wrong, SnapConfig::default()) {
        Err(BeatError::WordMismatch { index: 2, .. }) => {}
        other => {
            return Err(format!(
                "a mismatching word: expected WordMismatch at 2, got {other:?}"
            ))
        }
    }
    match snap(&script, &words[..6], SnapConfig::default()) {
        Err(BeatError::WordCountMismatch {
            script: 7,
            timed: 6,
        }) => {}
        other => {
            return Err(format!(
                "a missing word: expected WordCountMismatch, got {other:?}"
            ))
        }
    }

    // Sheets: eight uniform frames of a 10 s, 30 fps clip on the default 2x3 grid of 960x540 tiles.
    let plan = plan_sheets(SheetSpec::new(10_000, 30.0, Selection::Uniform(8)))
        .map_err(|e| format!("plan: {e}"))?;
    if plan.schema != SHEETS_SCHEMA
        || (plan.sheet_width, plan.sheet_height) != (1920, 1620)
        || plan.sheets.len() != 2
    {
        return Err(format!(
            "sheet geometry is wrong: {}x{}, {} sheets",
            plan.sheet_width,
            plan.sheet_height,
            plan.sheets.len()
        ));
    }
    if plan.sheets[0].tiles.len() != 6 || plan.sheets[1].tiles.len() != 2 {
        return Err("tiles are not filled six then two".into());
    }
    // Slice centres: (2i+1) * 10000 / 16 ms, frame = round(ms * 30 / 1000).
    let times: Vec<u64> = plan
        .sheets
        .iter()
        .flat_map(|s| s.tiles.iter().map(|t| t.time_ms))
        .collect();
    let frames: Vec<u64> = plan
        .sheets
        .iter()
        .flat_map(|s| s.tiles.iter().map(|t| t.frame))
        .collect();
    if times != vec![625, 1875, 3125, 4375, 5625, 6875, 8125, 9375]
        || frames != vec![19, 56, 94, 131, 169, 206, 244, 281]
    {
        return Err(format!("frame selection is wrong: {times:?} {frames:?}"));
    }
    let tile = &plan.sheets[0].tiles[3];
    if (tile.slot, tile.x, tile.y, tile.width, tile.height) != (3, 960, 540, 960, 540) {
        return Err(format!("tile 3 is at the wrong place: {tile:?}"));
    }
    let manifest: Value = serde_json::from_str(&plan.to_manifest_json())
        .map_err(|e| format!("manifest is not JSON: {e}"))?;
    if manifest["schema"] != SHEETS_SCHEMA || manifest["sheets"].as_array().map(Vec::len) != Some(2)
    {
        return Err("the manifest lost its schema or sheets".into());
    }
    // Beats drive the selection: the start of each beat is a tile time.
    let by_beats = plan_sheets(SheetSpec::new(2_400, 30.0, Selection::Beats(beats.clone())))
        .map_err(|e| e.to_string())?;
    let beat_times: Vec<u64> = by_beats.sheets[0].tiles.iter().map(|t| t.time_ms).collect();
    if beat_times != vec![600, 1600] {
        return Err(format!("beat selection is wrong: {beat_times:?}"));
    }
    match plan_sheets(SheetSpec::new(10_000, 30.0, Selection::Times(vec![10_001]))) {
        Err(SheetError::TimeOutOfRange {
            time_ms: 10_001,
            duration_ms: 10_000,
        }) => {}
        other => {
            return Err(format!(
                "a time past the end: expected TimeOutOfRange, got {other:?}"
            ))
        }
    }
    let mut narrow = SheetSpec::new(10_000, 30.0, Selection::Uniform(1));
    narrow.tile_width = 100;
    if !matches!(
        plan_sheets(narrow),
        Err(SheetError::TileTooNarrow { width: 100, .. })
    ) {
        return Err("a 100 px tile was not refused".into());
    }

    Ok(json!({
        "spans": script.spans.len(),
        "beats": got,
        "sheets": plan.sheets.len(),
        "frames": frames,
    }))
}
