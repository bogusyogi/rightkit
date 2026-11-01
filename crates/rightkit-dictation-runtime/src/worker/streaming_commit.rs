//! Streaming insertion (Developer mode): decide which words are stable enough
//! to type into the target while recording continues.
//!
//! Policy (LocalAgreement-2 keyed on TDT timestamps): a word commits when two
//! consecutive decodes of the trailing window agree on it (same normalised
//! text, start within `AGREE_START_SEC`) and it ended at least
//! `RIGHT_CONTEXT_SEC` before the audio the decode saw. Committed words are
//! append-only; a later decode never revises them.

use rightkit_dictation_inference::TimedToken;

/// Decodes cover at most this much trailing audio (the static-15 encoder input).
pub(crate) const STREAM_WINDOW_SAMPLES: usize = 14 * 16_000;
/// Minimum spacing between streaming decodes.
pub(crate) const STREAM_INTERVAL: std::time::Duration = std::time::Duration::from_millis(400);
/// No decode until the recording holds this much audio.
pub(crate) const STREAM_MIN_SAMPLES: usize = 16_000 * 3 / 4;

const AGREE_START_SEC: f32 = 0.16;
const RIGHT_CONTEXT_SEC: f32 = 0.6;
/// Word starts jitter by a frame between windows; a word starting this close
/// before the committed edge is the last committed word decoded again.
const COMMITTED_EDGE_SLACK_SEC: f32 = 0.08;

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct StreamWord {
    pub(crate) text: String,
    pub(crate) start: f32,
    pub(crate) end: f32,
}

#[derive(Debug, Default)]
pub(crate) struct StreamCommitter {
    committed: Vec<StreamWord>,
    previous: Vec<StreamWord>,
    revision: u64,
}

fn normalise(word: &str) -> String {
    word.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// Group subword tokens into words with absolute times. A token starting with
/// whitespace opens a word; punctuation and continuations attach to it.
pub(crate) fn words_from_tokens(tokens: &[TimedToken], offset_sec: f32) -> Vec<StreamWord> {
    let mut words: Vec<StreamWord> = Vec::new();
    for token in tokens {
        if !token.start.is_finite() || !token.end.is_finite() || token.end < token.start {
            return Vec::new();
        }
        if token.text.starts_with(char::is_whitespace) || words.is_empty() {
            words.push(StreamWord {
                text: token.text.trim().to_string(),
                start: offset_sec + token.start,
                end: offset_sec + token.end,
            });
        } else if let Some(word) = words.last_mut() {
            word.text.push_str(token.text.trim());
            word.end = offset_sec + token.end;
        }
    }
    words.retain(|word| !word.text.is_empty());
    words
}

impl StreamCommitter {
    fn committed_edge(&self) -> f32 {
        self.committed.last().map_or(0.0, |word| word.end)
    }

    fn uncommitted<'a>(&self, words: &'a [StreamWord]) -> &'a [StreamWord] {
        let edge = self.committed_edge();
        let first = words
            .iter()
            .position(|word| word.start >= edge - COMMITTED_EDGE_SLACK_SEC)
            .unwrap_or(words.len());
        let mut tail = &words[first..];
        if let (Some(last), Some(head)) = (self.committed.last(), tail.first()) {
            if normalise(&last.text) == normalise(&head.text)
                && (head.start - last.start).abs() <= AGREE_START_SEC
            {
                tail = &tail[1..];
            }
        }
        tail
    }

    /// Feed one decode of audio `[offset_sec, audio_end_sec]`. Returns true
    /// when new words committed.
    pub(crate) fn observe(
        &mut self,
        tokens: &[TimedToken],
        offset_sec: f32,
        audio_end_sec: f32,
    ) -> bool {
        let words = words_from_tokens(tokens, offset_sec);
        let current = self.uncommitted(&words).to_vec();
        let previous = self.uncommitted(&self.previous).to_vec();
        let agreed = current
            .iter()
            .zip(previous.iter())
            .take_while(|(now, before)| {
                normalise(&now.text) == normalise(&before.text)
                    && !normalise(&now.text).is_empty()
                    && (now.start - before.start).abs() <= AGREE_START_SEC
                    && now.end <= audio_end_sec - RIGHT_CONTEXT_SEC
            })
            .count();
        self.committed.extend(current.iter().take(agreed).cloned());
        self.previous = words;
        if agreed > 0 {
            self.revision = self.revision.wrapping_add(1);
        }
        agreed > 0
    }

    pub(crate) fn text(&self) -> String {
        self.committed
            .iter()
            .map(|word| word.text.as_str())
            .collect::<Vec<_>>()
            .join(" ")
    }

    pub(crate) fn revision(&self) -> u64 {
        self.revision
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tokens(words: &[(&str, f32, f32)]) -> Vec<TimedToken> {
        words
            .iter()
            .map(|(text, start, end)| TimedToken {
                text: format!(" {text}"),
                start: *start,
                end: *end,
            })
            .collect()
    }

    #[test]
    fn commits_only_words_two_decodes_agree_on_with_right_context() {
        let mut c = StreamCommitter::default();
        assert!(!c.observe(
            &tokens(&[("hello", 0.2, 0.5), ("world", 0.6, 0.9)]),
            0.0,
            1.0
        ));
        assert_eq!(c.text(), "");
        // "hello" agrees and ended 0.6 s before the audio edge; "world" is too recent.
        assert!(c.observe(
            &tokens(&[
                ("hello", 0.24, 0.5),
                ("world", 0.6, 0.95),
                ("there", 1.0, 1.2)
            ]),
            0.0,
            1.3
        ));
        assert_eq!(c.text(), "hello");
        assert!(c.observe(
            &tokens(&[
                ("hello", 0.2, 0.5),
                ("world", 0.6, 0.95),
                ("there", 1.0, 1.2)
            ]),
            0.0,
            1.9
        ));
        assert_eq!(c.text(), "hello world there");
    }

    #[test]
    fn disagreement_stops_the_commit_at_the_first_changed_word() {
        let mut c = StreamCommitter::default();
        c.observe(
            &tokens(&[("I", 0.0, 0.1), ("scream", 0.2, 0.5), ("now", 0.6, 0.8)]),
            0.0,
            2.0,
        );
        c.observe(
            &tokens(&[("I", 0.0, 0.1), ("screen", 0.2, 0.5), ("now", 0.6, 0.8)]),
            0.0,
            2.4,
        );
        assert_eq!(c.text(), "I");
    }

    #[test]
    fn committed_words_are_never_revised_or_repeated() {
        let mut c = StreamCommitter::default();
        let first = tokens(&[("check", 0.0, 0.3), ("this", 0.4, 0.6)]);
        c.observe(&first, 0.0, 2.0);
        c.observe(&first, 0.0, 2.0);
        assert_eq!(c.text(), "check this");
        // A later window starts mid-recording and re-decodes "this" a frame early.
        let later = tokens(&[("this", 0.33, 0.6), ("out", 0.7, 0.9)]);
        c.observe(&later, 0.0, 3.0);
        c.observe(&later, 0.0, 3.0);
        assert_eq!(c.text(), "check this out");
    }

    #[test]
    fn window_offset_maps_tokens_to_recording_time() {
        let mut c = StreamCommitter::default();
        let window = tokens(&[("late", 0.1, 0.4)]);
        c.observe(&window, 20.0, 21.5);
        c.observe(&window, 20.0, 21.5);
        assert_eq!(c.text(), "late");
        assert_eq!(c.committed_edge(), 20.4);
    }

    #[test]
    fn subword_tokens_and_punctuation_join_into_words() {
        let raw = vec![
            TimedToken {
                text: " Ye".into(),
                start: 0.0,
                end: 0.1,
            },
            TimedToken {
                text: "ah".into(),
                start: 0.1,
                end: 0.2,
            },
            TimedToken {
                text: ",".into(),
                start: 0.2,
                end: 0.25,
            },
            TimedToken {
                text: " man".into(),
                start: 0.3,
                end: 0.5,
            },
        ];
        let words = words_from_tokens(&raw, 0.0);
        assert_eq!(
            words.iter().map(|w| w.text.as_str()).collect::<Vec<_>>(),
            ["Yeah,", "man"]
        );
    }
}
