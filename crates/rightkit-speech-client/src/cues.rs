//! The host's cues as typed values. HeardRight renders `srt` and `vtt` from its own
//! cue grouping (`rightkit-transcript`: a cue closes at 12 words, 7 s, a gap over 1 s, or
//! sentence-final punctuation after 3 words, with an 80 ms floor), so these cues are the
//! service's segments. Nothing here regroups word timings on the client.

use crate::protocol::FileTranscript;

/// One subtitle cue from the host's SRT. Times are whole milliseconds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cue {
    pub start_ms: u64,
    pub end_ms: u64,
    /// The cue's text lines joined with `\n`, as the host wrote them. The host's SRT is
    /// literal text, so markup such as `<b>` and `&amp;` is kept as written.
    pub text: String,
}

impl FileTranscript {
    /// The host's cues, parsed from [`FileTranscript::srt`]. Empty when `srt` is empty.
    pub fn cues(&self) -> Vec<Cue> {
        parse_srt_cues(&self.srt)
    }
}

/// Parse SubRip text into cues, in order.
///
/// Blocks are separated by blank lines. A block is skipped when it has no `-->` timing
/// line, when either timestamp is unreadable, or when it has no text. Cue numbers are not
/// kept. Accepts LF or CRLF line endings, a leading byte-order mark, and `,` or `.` before
/// the milliseconds. Never fails: unreadable input yields fewer cues, not an error.
pub fn parse_srt_cues(srt: &str) -> Vec<Cue> {
    let text = srt
        .trim_start_matches('\u{feff}')
        .replace("\r\n", "\n")
        .replace('\r', "\n");
    text.split("\n\n")
        .filter_map(|block| {
            let lines: Vec<&str> = block.lines().collect();
            let at = lines.iter().position(|line| line.contains("-->"))?;
            let (start, end) = lines[at].split_once("-->")?;
            let start_ms = parse_stamp(start)?;
            let end_ms = parse_stamp(end.split_whitespace().next()?)?;
            let body: Vec<&str> = lines[at + 1..]
                .iter()
                .copied()
                .filter(|line| !line.trim().is_empty())
                .collect();
            if body.is_empty() {
                return None;
            }
            Some(Cue {
                start_ms,
                end_ms,
                text: body.join("\n"),
            })
        })
        .collect()
}

/// `HH:MM:SS,mmm`, `HH:MM:SS.mmm` or `MM:SS.mmm`. A fraction shorter than three digits is
/// padded with zeros and a longer one is cut to three.
fn parse_stamp(stamp: &str) -> Option<u64> {
    let (clock, fraction) = stamp.trim().split_once(|c: char| c == ',' || c == '.')?;
    let fields = clock
        .split(':')
        .map(|part| part.parse::<u64>().ok())
        .collect::<Option<Vec<u64>>>()?;
    let (hours, minutes, seconds) = match fields.as_slice() {
        [h, m, s] => (*h, *m, *s),
        [m, s] => (0, *m, *s),
        _ => return None,
    };
    if fraction.is_empty() || !fraction.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let mut digits: String = fraction.chars().take(3).collect();
    while digits.len() < 3 {
        digits.push('0');
    }
    let millis: u64 = digits.parse().ok()?;
    hours
        .checked_mul(60)?
        .checked_add(minutes)?
        .checked_mul(60)?
        .checked_add(seconds)?
        .checked_mul(1000)?
        .checked_add(millis)
}
