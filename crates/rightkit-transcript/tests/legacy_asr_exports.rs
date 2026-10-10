//! Golden: the SRT/VTT the pre-0.2 `rightkit-asr` long-form exporter produced
//! for this word list (captured from that code before it was replaced by
//! `group_words` + `to_srt`/`to_vtt`), so the migration is byte-exact.

use rightkit_transcript::{group_words, to_srt, to_vtt, GroupRules, Newline, TimedWord};

const WORDS: &[(&str, u64, u64)] = &[
    ("Good", 0, 320),
    ("morning", 400, 880),
    ("everyone.", 960, 1520),
    ("Today", 1600, 1920),
    ("we", 2000, 2160),
    ("are", 2240, 2400),
    ("going", 2480, 2800),
    ("to", 2880, 3040),
    ("talk", 3120, 3440),
    ("about", 3520, 3840),
    ("the", 3920, 4080),
    ("quarterly", 4160, 4720),
    ("results", 4800, 5280),
    ("and", 5360, 5520),
    ("plans.", 5600, 6080),
    ("Then", 7600, 7920),
    ("a", 8000, 8080),
    ("long", 8160, 8480),
    ("pause", 8560, 9040),
    ("before", 11200, 11520),
    ("this", 11600, 11840),
    ("one", 11920, 12160),
    ("Hello?", 12240, 12600),
    ("Yes", 12640, 12960),
    ("it", 13040, 13200),
    ("is", 13280, 13440),
    ("fine!", 13520, 14000),
    ("tail", 14080, 14400),
    ("words", 14480, 14960),
    ("end", 15040, 15120),
    ("ing", 15200, 15280),
];

const OLD_SRT: &str = "1
00:00:00,000 --> 00:00:01,520
Good morning everyone.

2
00:00:01,600 --> 00:00:06,080
Today we are going to talk about the quarterly results and plans.

3
00:00:07,600 --> 00:00:09,040
Then a long pause

4
00:00:11,200 --> 00:00:12,600
before this one Hello?

5
00:00:12,640 --> 00:00:14,000
Yes it is fine!

6
00:00:14,080 --> 00:00:15,280
tail words end ing

";
const OLD_VTT: &str = "WEBVTT

00:00:00.000 --> 00:00:01.520
Good morning everyone.

00:00:01.600 --> 00:00:06.080
Today we are going to talk about the quarterly results and plans.

00:00:07.600 --> 00:00:09.040
Then a long pause

00:00:11.200 --> 00:00:12.600
before this one Hello?

00:00:12.640 --> 00:00:14.000
Yes it is fine!

00:00:14.080 --> 00:00:15.280
tail words end ing

";

fn words() -> Vec<TimedWord> {
    WORDS.iter().copied().map(TimedWord::from).collect()
}

#[test]
fn group_words_srt_matches_legacy_asr_export() {
    let cues = group_words(&words(), &GroupRules::default());
    assert_eq!(to_srt(&cues, Newline::Lf), OLD_SRT);
}

#[test]
fn group_words_vtt_matches_legacy_asr_export() {
    let cues = group_words(&words(), &GroupRules::default());
    assert_eq!(to_vtt(&cues, Newline::Lf), OLD_VTT);
}

#[test]
fn group_words_cue_indices_cover_every_word_once() {
    let w = words();
    let cues = group_words(&w, &GroupRules::default());
    assert_eq!(cues.first().map(|c| c.first_word), Some(0));
    assert_eq!(cues.last().map(|c| c.last_word), Some(w.len() - 1));
    for pair in cues.windows(2) {
        assert_eq!(pair[0].last_word + 1, pair[1].first_word);
    }
}

#[test]
fn timed_word_conversions() {
    assert_eq!(
        TimedWord::from_seconds("a", 0.4, 0.88),
        TimedWord::new("a", 400, 880)
    );
    assert_eq!(
        TimedWord::from_seconds("a", -1.0, f32::NAN),
        TimedWord::new("a", 0, 0)
    );
    assert_eq!(TimedWord::from(("a", 1500u64, 2500u64)).start_secs(), 1.5);
    assert_eq!(TimedWord::new("a", 1500, 2500).end_secs(), 2.5);
}
