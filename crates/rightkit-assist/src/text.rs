//! Cleaning of text that came from outside. Anything shown or stored from a mail passes through here first.

/// True for characters that can hide or reorder text: bidirectional overrides and isolates, zero-width
/// characters and the byte-order mark.
fn is_hidden(ch: char) -> bool {
    matches!(
        ch,
        '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{2064}' | '\u{2066}'..='\u{2069}' | '\u{FEFF}'
    )
}

/// One line: control characters, whitespace runs and hidden characters collapse to single spaces, then the
/// result is cut to `max_chars` characters and trimmed.
pub fn clean(raw: &str, max_chars: usize) -> String {
    let mut out = String::new();
    let mut count = 0usize;
    let mut last_space = true;
    for ch in raw.chars() {
        if count >= max_chars {
            break;
        }
        let ch = if ch.is_control() || ch.is_whitespace() || is_hidden(ch) {
            ' '
        } else {
            ch
        };
        if ch == ' ' {
            if last_space {
                continue;
            }
            last_space = true;
        } else {
            last_space = false;
        }
        out.push(ch);
        count += 1;
    }
    out.trim().to_string()
}

/// Several lines: like [`clean`] but newlines survive (runs of blank lines collapse to one) and tabs become
/// spaces. Cut to `max_bytes` bytes on a character boundary.
pub fn clean_multiline(raw: &str, max_bytes: usize) -> String {
    let mut out = String::new();
    let mut newlines = 0usize;
    for ch in raw.chars() {
        if out.len() >= max_bytes {
            break;
        }
        if ch == '\n' || ch == '\r' {
            newlines += 1;
            if newlines <= 2 && ch == '\n' {
                out.push('\n');
            }
            continue;
        }
        newlines = 0;
        if ch == '\t' {
            out.push(' ');
        } else if ch.is_control() || is_hidden(ch) {
            continue;
        } else {
            out.push(ch);
        }
    }
    truncate_bytes(&out, max_bytes).trim().to_string()
}

/// The longest prefix of `s` of at most `max` bytes that ends on a character boundary.
pub fn truncate_bytes(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Escape `&`, `<` and `>` so outside text can never close or open a wrapper tag around it.
pub fn escape_angle(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            other => out.push(other),
        }
    }
    out
}
