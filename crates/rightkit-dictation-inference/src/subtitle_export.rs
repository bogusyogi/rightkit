//! SRT and WebVTT rendering for file transcription, shared by macOS (CoreML)
//! and Windows (ONNX). Cue grouping and export come from `rightkit-transcript`:
//! `GroupRules::default()` closes a cue at 12 words, 7 s span, a >1 s gap, or
//! sentence-final punctuation after 3 words, with an 80 ms cue floor.

use rightkit_transcript::{group_words, to_srt, to_vtt, GroupRules, Newline, TimedWord};

/// Renders `(text, start_seconds, end_seconds)` words to `(srt, vtt)`.
pub fn render_srt_vtt(words: impl IntoIterator<Item = (String, f32, f32)>) -> (String, String) {
    let timed: Vec<TimedWord> = words
        .into_iter()
        .map(|(text, start, end)| TimedWord::from_seconds(text, start, end))
        .collect();
    let cues = group_words(&timed, &GroupRules::default());
    (to_srt(&cues, Newline::Lf), to_vtt(&cues, Newline::Lf))
}

#[cfg(test)]
mod tests {
    use super::render_srt_vtt;

    fn w(text: &str, start: f32, end: f32) -> (String, f32, f32) {
        (text.to_string(), start, end)
    }

    #[test]
    fn empty_input_renders_empty_srt_and_bare_vtt_header() {
        let (srt, vtt) = render_srt_vtt(Vec::<(String, f32, f32)>::new());
        assert_eq!(srt, "");
        assert_eq!(vtt, "WEBVTT\n\n");
    }

    #[test]
    fn worked_example_renders_exact_srt_and_vtt() {
        let words = vec![
            w("The", 0.0, 0.4),
            w("quick", 0.4, 0.8),
            w("brown", 0.8, 1.2),
            w("fox", 1.2, 1.6),
            w("jumps.", 1.6, 2.0),
            w("over", 2.0, 2.9),
            w("the", 2.9, 3.8),
            w("lazy", 3.8, 4.7),
            w("dog", 4.7, 5.6),
            w("and", 5.6, 6.5),
            w("then", 6.5, 7.4),
            w("runs", 7.4, 8.3),
            w("far", 8.3, 9.2),
            w("away", 9.2, 10.1),
            w("again", 10.1, 11.0),
        ];
        let (srt, vtt) = render_srt_vtt(words);
        assert_eq!(
            srt,
            "1\n00:00:00,000 --> 00:00:02,000\nThe quick brown fox jumps.\n\n\
             2\n00:00:02,000 --> 00:00:09,200\nover the lazy dog and then runs far\n\n\
             3\n00:00:09,200 --> 00:00:11,000\naway again\n\n"
        );
        assert_eq!(
            vtt,
            "WEBVTT\n\n\
             00:00:00.000 --> 00:00:02.000\nThe quick brown fox jumps.\n\n\
             00:00:02.000 --> 00:00:09.200\nover the lazy dog and then runs far\n\n\
             00:00:09.200 --> 00:00:11.000\naway again\n\n"
        );
    }

    #[test]
    fn markup_is_escaped_in_vtt_and_literal_in_srt() {
        let (srt, vtt) = render_srt_vtt(vec![w("AT&T", 0.0, 0.5), w("<b>", 0.5, 1.0)]);
        assert_eq!(srt, "1\n00:00:00,000 --> 00:00:01,000\nAT&T <b>\n\n");
        assert_eq!(
            vtt,
            "WEBVTT\n\n00:00:00.000 --> 00:00:01.000\nAT&amp;T &lt;b&gt;\n\n"
        );
    }
}
