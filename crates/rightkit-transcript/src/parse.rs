//! SRT / WebVTT / timing-JSON import. Cues carry no word timings, so
//! [`words_from_cues`] spreads each cue's words across its span by character
//! count (flagged `EndSource::Interpolated` when put into a `Transcript`).

use crate::words::TimedWord;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ParseError {
    #[error("bad timestamp {0:?}")]
    Timestamp(String),
    #[error("invalid timing JSON: {0}")]
    Json(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedCue {
    pub start_ms: u64,
    pub end_ms: u64,
    /// Cue text lines with markup tags removed and entities unescaped.
    pub lines: Vec<String>,
}

fn parse_stamp(s: &str) -> Result<u64, ParseError> {
    let bad = || ParseError::Timestamp(s.to_string());
    let s = s.trim().replace(',', ".");
    let (hms, frac) = s.split_once('.').ok_or_else(bad)?;
    let parts: Vec<&str> = hms.split(':').collect();
    let nums: Result<Vec<u64>, _> = parts.iter().map(|p| p.parse::<u64>()).collect();
    let nums = nums.map_err(|_| bad())?;
    let (h, m, sec) = match nums.as_slice() {
        [h, m, s] => (*h, *m, *s),
        [m, s] => (0, *m, *s),
        _ => return Err(bad()),
    };
    let mut f = frac.to_string();
    f.truncate(3);
    while f.len() < 3 {
        f.push('0');
    }
    let ms: u64 = f.parse().map_err(|_| bad())?;
    Ok(((h * 60 + m) * 60 + sec) * 1000 + ms)
}

fn timing_line(line: &str) -> Option<Result<(u64, u64), ParseError>> {
    let (a, rest) = line.split_once("-->")?;
    // VTT allows cue settings after the end stamp.
    let b = rest.split_whitespace().next().unwrap_or("");
    Some(parse_stamp(a).and_then(|s| parse_stamp(b).map(|e| (s, e))))
}

fn clean_line(l: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for c in l.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.replace("&lt;", "<").replace("&gt;", ">").replace("&nbsp;", " ").replace("&amp;", "&")
}

fn parse_blocks(text: &str, vtt: bool) -> Result<Vec<ParsedCue>, ParseError> {
    let text = text.trim_start_matches('\u{feff}').replace("\r\n", "\n").replace('\r', "\n");
    let mut cues = Vec::new();
    for block in text.split("\n\n") {
        let lines: Vec<&str> = block.lines().collect();
        let Some(ti) = lines.iter().position(|l| l.contains("-->")) else { continue };
        if vtt && lines.first().is_some_and(|l| l.starts_with("NOTE") || l.starts_with("STYLE") || l.starts_with("REGION")) {
            continue;
        }
        let (start_ms, end_ms) = timing_line(lines[ti]).expect("line contains -->")?;
        let body: Vec<String> = lines[ti + 1..].iter().map(|l| clean_line(l)).filter(|l| !l.trim().is_empty()).collect();
        if body.is_empty() {
            continue;
        }
        cues.push(ParsedCue { start_ms, end_ms, lines: body });
    }
    Ok(cues)
}

/// Parse SubRip (CRLF/LF, BOM, optional numbering, `,` or `.` decimals).
pub fn parse_srt(text: &str) -> Result<Vec<ParsedCue>, ParseError> {
    parse_blocks(text, false)
}

/// Parse WebVTT (header, NOTE/STYLE blocks, cue ids/settings, `MM:SS.mmm` stamps).
pub fn parse_vtt(text: &str) -> Result<Vec<ParsedCue>, ParseError> {
    parse_blocks(text, true)
}

/// Spread each cue's whitespace-separated words across `[start, end)` in
/// proportion to character count. Words never overlap and tile the cue.
pub fn words_from_cues(cues: &[ParsedCue]) -> Vec<TimedWord> {
    let mut out = Vec::new();
    for c in cues {
        let toks: Vec<&str> = c.lines.iter().flat_map(|l| l.split_whitespace()).collect();
        let total: usize = toks.iter().map(|t| t.chars().count()).sum();
        if toks.is_empty() || c.end_ms <= c.start_ms || total == 0 {
            continue;
        }
        let span = c.end_ms - c.start_ms;
        let mut acc = 0usize;
        for t in toks {
            let s = c.start_ms + span * acc as u64 / total as u64;
            acc += t.chars().count();
            let e = c.start_ms + span * acc as u64 / total as u64;
            if e > s {
                out.push(TimedWord::new(t, s, e));
            }
        }
    }
    out
}

/// Parse the shared timing JSON (`{"segments":[..],"words":[..]}`, seconds).
/// Returns the word timings.
pub fn parse_timing_json(s: &str) -> Result<Vec<TimedWord>, ParseError> {
    let v: serde_json::Value = serde_json::from_str(s).map_err(|e| ParseError::Json(e.to_string()))?;
    let arr = v.get("words").and_then(|w| w.as_array()).ok_or_else(|| ParseError::Json("missing words array".into()))?;
    let ms = |x: &serde_json::Value| x.as_f64().map(|f| (f.max(0.0) * 1000.0).round() as u64);
    let mut out = Vec::with_capacity(arr.len());
    for w in arr {
        let (Some(t), Some(s), Some(e)) = (w.get("text").and_then(|t| t.as_str()), w.get("start").and_then(ms), w.get("end").and_then(ms)) else {
            return Err(ParseError::Json("word needs text/start/end".into()));
        };
        out.push(TimedWord::new(t, s, e));
    }
    Ok(out)
}
