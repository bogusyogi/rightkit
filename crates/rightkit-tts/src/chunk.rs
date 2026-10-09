//! Long-text chunking. QTS generates one utterance per call; long scripts are
//! split at sentence boundaries (then clause, then word) and the audio is
//! joined with short pauses.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chunk {
    pub text: String,
    /// Silence to append after this chunk's audio.
    pub pause_ms: u32,
}

/// Default chunk size in characters (about 12-15 s of speech).
pub const DEFAULT_MAX_CHARS: usize = 220;

const SENT_END: &[char] = &[
    '.', '!', '?', '\u{3002}', '\u{ff01}', '\u{ff1f}', '\u{2026}',
];
const CLAUSE: &[char] = &[
    ',', ';', ':', '\u{ff0c}', '\u{3001}', '\u{ff1b}', '\u{ff1a}',
];

pub fn split_text(text: &str, max_chars: usize) -> Vec<Chunk> {
    let max_chars = max_chars.max(20);
    let mut out: Vec<Chunk> = Vec::new();
    for para in text.split("\n") {
        let para = para.trim();
        if para.is_empty() {
            continue;
        }
        let mut cur = String::new();
        for sent in sentences(para) {
            for piece in hard_split(&sent, max_chars) {
                if !cur.is_empty() && cur.chars().count() + 1 + piece.chars().count() > max_chars {
                    out.push(Chunk {
                        text: std::mem::take(&mut cur),
                        pause_ms: 200,
                    });
                }
                if !cur.is_empty() {
                    cur.push(' ');
                }
                cur.push_str(&piece);
            }
        }
        if !cur.is_empty() {
            out.push(Chunk {
                text: cur,
                pause_ms: 200,
            });
        }
        if let Some(last) = out.last_mut() {
            last.pause_ms = 280; // paragraph break; joins must stay at or under 300 ms
        }
    }
    if let Some(last) = out.last_mut() {
        last.pause_ms = 0;
    }
    out
}

fn sentences(para: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let chars: Vec<char> = para.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        cur.push(c);
        if SENT_END.contains(&c) {
            let next = chars.get(i + 1);
            // Break after the terminator when followed by space/end/CJK, not inside "3.5" or "e.g.x".
            if next.is_none_or(|n| n.is_whitespace() || !n.is_ascii()) {
                let s = cur.trim();
                if !s.is_empty() {
                    out.push(s.to_string());
                }
                cur.clear();
            }
        }
    }
    let s = cur.trim();
    if !s.is_empty() {
        out.push(s.to_string());
    }
    out
}

fn hard_split(s: &str, max: usize) -> Vec<String> {
    if s.chars().count() <= max {
        return vec![s.to_string()];
    }
    let mut out = Vec::new();
    let mut rest: Vec<char> = s.chars().collect();
    while rest.len() > max {
        let window = &rest[..max];
        let cut = window
            .iter()
            .rposition(|c| CLAUSE.contains(c))
            .or_else(|| window.iter().rposition(|c| c.is_whitespace()))
            .map(|i| i + 1)
            .filter(|&i| i >= max / 3)
            .unwrap_or(max);
        let head: String = rest[..cut].iter().collect();
        out.push(head.trim().to_string());
        rest = rest[cut..]
            .iter()
            .skip_while(|c| c.is_whitespace())
            .copied()
            .collect();
    }
    if !rest.is_empty() {
        out.push(rest.iter().collect());
    }
    out.retain(|p| !p.is_empty());
    out
}

/// Frame budget for one chunk (12.5 frames/s of audio; speech runs ~0.8
/// frames/char, so 3x + slack stops a runaway without clipping real speech).
pub fn frame_budget(chunk_chars: usize) -> usize {
    (chunk_chars * 3 + 50).clamp(64, 1500)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_text_is_one_chunk() {
        let c = split_text("Hello there. How are you?", 220);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].text, "Hello there. How are you?");
        assert_eq!(c[0].pause_ms, 0);
    }

    #[test]
    fn long_text_splits_at_sentences_within_limit() {
        let s = "This is a sentence of moderate length. ".repeat(30);
        let c = split_text(&s, 120);
        assert!(c.len() > 5);
        assert!(c.iter().all(|c| c.text.chars().count() <= 120), "{:?}", c);
        assert!(c.iter().all(|c| c.text.ends_with('.')));
    }

    #[test]
    fn overlong_sentence_splits_at_clauses_then_words() {
        let s = format!(
            "{}, {}",
            "alpha beta gamma ".repeat(10).trim(),
            "delta epsilon zeta ".repeat(10).trim()
        );
        let c = split_text(&s, 100);
        assert!(c.len() >= 3);
        assert!(c.iter().all(|c| c.text.chars().count() <= 100));
        let nowordcut: Vec<_> = c.iter().flat_map(|c| c.text.split_whitespace()).collect();
        assert!(
            nowordcut.iter().all(|w| [
                "alpha", "beta", "gamma,", "gamma", "delta", "epsilon", "zeta", "beta,"
            ]
            .contains(w)),
            "{nowordcut:?}"
        );
    }

    #[test]
    fn paragraphs_get_longer_pauses_and_decimals_hold() {
        let c = split_text("Costs 3.5 dollars.\nNext paragraph here.", 220);
        assert_eq!(c.len(), 2);
        assert_eq!(c[0].pause_ms, 280);
        assert_eq!(c[0].text, "Costs 3.5 dollars.");
        assert_eq!(c[1].pause_ms, 0);
    }

    #[test]
    fn empty_and_cjk() {
        assert!(split_text("  \n ", 100).is_empty());
        assert_eq!(split_text("你好。再见！", 100).len(), 1);
        assert!(frame_budget(10) >= 64 && frame_budget(100_000) == 1500);
    }
}
