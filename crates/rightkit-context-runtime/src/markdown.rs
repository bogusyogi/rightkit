//! `[[wikilink]]` extraction for memory content (the link graph).

/// Extract `[[wikilink]]` targets from the body (the OKF link graph), in order.
pub fn wikilinks(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    let b = body.as_bytes();
    let mut i = 0;
    while i + 1 < b.len() {
        if b[i] == b'[' && b[i + 1] == b'[' {
            if let Some(close) = body[i + 2..].find("]]") {
                let inner = &body[i + 2..i + 2 + close];
                if !inner.is_empty() {
                    out.push(inner.to_string());
                }
                i = i + 2 + close + 2;
                continue;
            }
        }
        i += 1;
    }
    out
}
