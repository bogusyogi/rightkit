//! Minimal JSON helpers for the WebDriver wire (flat string extraction only).

pub fn esc(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o
}

/// First string value for `"key": "..."` anywhere in `body`.
pub fn str_field(body: &str, key: &str) -> Option<String> {
    let pat = format!("\"{key}\"");
    let mut from = 0;
    while let Some(i) = body[from..].find(&pat) {
        let mut rest = body[from + i + pat.len()..].trim_start();
        if let Some(r) = rest.strip_prefix(':') {
            rest = r.trim_start();
            if let Some(r) = rest.strip_prefix('"') {
                let mut out = String::new();
                let mut it = r.chars();
                while let Some(c) = it.next() {
                    match c {
                        '"' => return Some(out),
                        '\\' => match it.next()? {
                            'n' => out.push('\n'),
                            't' => out.push('\t'),
                            'r' => out.push('\r'),
                            'u' => {
                                let h: String = it.by_ref().take(4).collect();
                                out.push(char::from_u32(u32::from_str_radix(&h, 16).ok()?)?);
                            }
                            o => out.push(o),
                        },
                        c => out.push(c),
                    }
                }
                return None;
            }
        }
        from += i + pat.len();
    }
    None
}

/// First numeric value for `"key": 12.5` anywhere in `body`.
pub fn num_field(body: &str, key: &str) -> Option<f64> {
    let pat = format!("\"{key}\"");
    let i = body.find(&pat)?;
    let rest = body[i + pat.len()..].trim_start().strip_prefix(':')?.trim_start();
    let end = rest.find(|c: char| !(c.is_ascii_digit() || "+-.eE".contains(c))).unwrap_or(rest.len());
    rest[..end].parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn extracts_and_escapes() {
        let b = r#"{"capabilities":{},"using":"link text","value":"a\"bA"}"#;
        assert_eq!(str_field(b, "using").as_deref(), Some("link text"));
        assert_eq!(str_field(b, "value").as_deref(), Some("a\"bA"));
        assert_eq!(esc("a\"\n"), "a\\\"\\n");
        assert_eq!(str_field(b, "missing"), None);
        assert_eq!(num_field(r#"{"x": 12.5,"y":-3}"#, "y"), Some(-3.0));
        assert_eq!(num_field(r#"{"x": 12.5}"#, "x"), Some(12.5));
    }
}
