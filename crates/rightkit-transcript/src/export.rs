//! SRT, WebVTT, plain-text and timing-JSON export. UTF-8, exact millisecond timestamps.

use crate::segment::Cue;

/// Line terminator for exported files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Newline {
    Lf,
    Crlf,
}

impl Newline {
    fn as_str(self) -> &'static str {
        match self {
            Newline::Lf => "\n",
            Newline::Crlf => "\r\n",
        }
    }
}

fn stamp(ms: u64, sep: char) -> String {
    format!(
        "{:02}:{:02}:{:02}{sep}{:03}",
        ms / 3_600_000,
        ms / 60_000 % 60,
        ms / 1000 % 60,
        ms % 1000
    )
}

/// Cue text lines made safe for either format: no CR, no blank line (which
/// would end the cue) and no `-->` (which would read as a timing line).
fn safe_lines(cue: &Cue) -> Vec<String> {
    cue.lines
        .iter()
        .map(|l| l.replace('\r', "").replace("-->", "->"))
        .filter(|l| !l.trim().is_empty())
        .collect()
}

/// SubRip: numbered cues, `HH:MM:SS,mmm --> HH:MM:SS,mmm`, blank line after each.
pub fn to_srt(cues: &[Cue], nl: Newline) -> String {
    let nl = nl.as_str();
    let mut out = String::new();
    let mut n = 0;
    for c in cues {
        let lines = safe_lines(c);
        if lines.is_empty() {
            continue;
        }
        n += 1;
        out.push_str(&format!(
            "{n}{nl}{} --> {}{nl}",
            stamp(c.start_ms, ','),
            stamp(c.end_ms, ',')
        ));
        for l in lines {
            out.push_str(&l);
            out.push_str(nl);
        }
        out.push_str(nl);
    }
    out
}

fn vtt_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// WebVTT: `WEBVTT` header, `HH:MM:SS.mmm` timestamps, `&`/`<`/`>` escaped.
pub fn to_vtt(cues: &[Cue], nl: Newline) -> String {
    let nl = nl.as_str();
    let mut out = format!("WEBVTT{nl}{nl}");
    for c in cues {
        let lines = safe_lines(c);
        if lines.is_empty() {
            continue;
        }
        out.push_str(&format!(
            "{} --> {}{nl}",
            stamp(c.start_ms, '.'),
            stamp(c.end_ms, '.')
        ));
        for l in lines {
            out.push_str(&vtt_escape(&l));
            out.push_str(nl);
        }
        out.push_str(nl);
    }
    out
}

/// Plain text: one line per cue.
pub fn to_plain_text(cues: &[Cue]) -> String {
    cues.iter().map(|c| c.lines.join(" ")).collect::<Vec<_>>().join("\n")
}

/// Timing JSON shared by every consumer (same shape `rightkit-asr` emits):
/// `segments` are the cues, `words` the word timings, times in seconds.
pub fn to_timing_json(cues: &[Cue], words: &[crate::words::TimedWord]) -> String {
    let secs = |ms: u64| ms as f64 / 1000.0;
    let seg: Vec<serde_json::Value> = cues
        .iter()
        .map(|c| serde_json::json!({ "text": c.lines.join(" "), "start": secs(c.start_ms), "end": secs(c.end_ms) }))
        .collect();
    let w: Vec<serde_json::Value> = words
        .iter()
        .map(|w| serde_json::json!({ "text": w.text, "start": secs(w.start_ms), "end": secs(w.end_ms) }))
        .collect();
    serde_json::to_string(&serde_json::json!({ "segments": seg, "words": w })).expect("json serialises")
}
