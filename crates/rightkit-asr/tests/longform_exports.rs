//! Golden: `timed_exports` / `group_cues` / `render_subtitles` output for a fixed
//! piece list, byte-identical to what the pre-`rightkit-transcript` exporter
//! produced (captured from that code before it was replaced).

use rightkit_asr::longform::*;
use rightkit_asr::TimedToken;

const OLD_SRT: &str = "1\n00:00:00,000 --> 00:00:01,520\nGood morning everyone.\n\n2\n00:00:01,600 --> 00:00:06,080\nToday we are going to talk about the quarterly results and plans.\n\n3\n00:00:07,600 --> 00:00:09,040\nThen a long pause\n\n4\n00:00:11,200 --> 00:00:12,600\nbefore this one Hello?\n\n5\n00:00:12,640 --> 00:00:14,000\nYes it is fine!\n\n6\n00:00:14,080 --> 00:00:15,280\ntail words end ing\n\n";
const OLD_VTT: &str = "WEBVTT\n\n00:00:00.000 --> 00:00:01.520\nGood morning everyone.\n\n00:00:01.600 --> 00:00:06.080\nToday we are going to talk about the quarterly results and plans.\n\n00:00:07.600 --> 00:00:09.040\nThen a long pause\n\n00:00:11.200 --> 00:00:12.600\nbefore this one Hello?\n\n00:00:12.640 --> 00:00:14.000\nYes it is fine!\n\n00:00:14.080 --> 00:00:15.280\ntail words end ing\n\n";

pub fn fixture() -> Vec<TimedToken> {
    let t = [
        (" Good", 0.00f32, 0.32f32), (" morning", 0.40, 0.88), (" everyone.", 0.96, 1.52),
        (" Today", 1.60, 1.92), (" we", 2.00, 2.16), (" are", 2.24, 2.40), (" going", 2.48, 2.80),
        (" to", 2.88, 3.04), (" talk", 3.12, 3.44), (" about", 3.52, 3.84), (" the", 3.92, 4.08),
        (" quarterly", 4.16, 4.72), (" results", 4.80, 5.28), (" and", 5.36, 5.52), (" plans.", 5.60, 6.08),
        (" Then", 7.60, 7.92), (" a", 8.00, 8.08), (" long", 8.16, 8.48), (" pause", 8.56, 9.04),
        (" before", 11.20, 11.52), (" this", 11.60, 11.84), (" one", 11.92, 12.16), (" Hello", 12.24, 12.56), ("?", 12.56, 12.60),
        (" Yes", 12.64, 12.96), (" it", 13.04, 13.20), (" is", 13.28, 13.44), (" fine!", 13.52, 14.00),
        (" tail", 14.08, 14.40), (" words", 14.48, 14.96), (" end", 15.04, 15.12), (" ", 15.12, 15.20), ("ing", 15.20, 15.20),
    ];
    t.iter().map(|(s, a, b)| TimedToken { text: s.to_string(), start: *a, end: *b }).collect()
}

#[test]
fn longform_exports_match_pre_transcript_output() {
    let toks = fixture();
    let ex = timed_exports(&toks);
    assert_eq!(ex.srt.as_deref(), Some(OLD_SRT));
    assert_eq!(ex.vtt.as_deref(), Some(OLD_VTT));

    let words = timed_pieces_to_words(&toks);
    assert_eq!(words.len(), 31);
    assert_eq!((words[0].start_ms, words[0].end_ms), (0, 320));
    assert_eq!((words[30].start_ms, words[30].end_ms), (15_200, 15_280));
    let cues = group_cues(&words);
    assert_eq!(cues.len(), 6);
    let (srt, vtt) = render_subtitles(&cues);
    assert_eq!((srt.as_str(), vtt.as_str()), (OLD_SRT, OLD_VTT));

    let json: serde_json::Value = serde_json::from_str(&ex.json.unwrap()).unwrap();
    assert_eq!(json["segments"].as_array().unwrap().len(), 6);
    assert_eq!(json["words"].as_array().unwrap().len(), 31);
    assert_eq!(json["segments"][0]["text"], "Good morning everyone.");
    assert_eq!(json["segments"][0]["end"], 1.52);
    assert_eq!(json["words"][1]["start"], 0.4);
}

#[test]
fn longform_exports_empty_input_is_none() {
    assert_eq!(timed_exports(&[]), TimedExports::default());
}
