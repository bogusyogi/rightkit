fn parse_pdf_string(bytes: &[u8], mut i: usize) -> Option<(String, usize)> {
    let mut out = String::new();
    let mut depth = 1usize;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' if i + 1 < bytes.len() => {
                i += 1;
                if bytes[i].is_ascii_digit() && bytes[i] < b'8' {
                    let mut value = (bytes[i] - b'0') as u32;
                    let mut consumed = 0usize;
                    while consumed < 2
                        && i + 1 < bytes.len()
                        && bytes[i + 1].is_ascii_digit()
                        && bytes[i + 1] < b'8'
                    {
                        i += 1;
                        consumed += 1;
                        value = value * 8 + (bytes[i] - b'0') as u32;
                    }
                    if let Some(c) = char::from_u32(value) {
                        out.push(c);
                    }
                    i += 1;
                    continue;
                }
                let c = match bytes[i] {
                    b'n' => '\n',
                    b'r' => '\r',
                    b't' => '\t',
                    b'b' => '\u{0008}',
                    b'f' => '\u{000c}',
                    b'(' => '(',
                    b')' => ')',
                    b'\\' => '\\',
                    b'\n' => {
                        i += 1;
                        continue;
                    }
                    b'\r' => {
                        if bytes.get(i + 1) == Some(&b'\n') {
                            i += 1;
                        }
                        i += 1;
                        continue;
                    }
                    other => other as char,
                };
                out.push(c);
            }
            b'(' => {
                depth += 1;
                out.push('(');
            }
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Some((out, i + 1));
                }
                out.push(')');
            }
            b => out.push(b as char),
        }
        i += 1;
    }
    None
}

fn parse_pdf_hex_string(bytes: &[u8], mut i: usize) -> Option<(String, usize)> {
    let mut hex = String::new();
    while i < bytes.len() {
        match bytes[i] {
            b'>' => return Some((decode_pdf_hex_text(&hex), i + 1)),
            b if b.is_ascii_hexdigit() => hex.push(b as char),
            b if b.is_ascii_whitespace() => {}
            _ => return None,
        }
        i += 1;
    }
    None
}

fn parse_pdf_hex_bytes(bytes: &[u8], mut i: usize) -> Option<(Vec<u8>, usize)> {
    let mut hex = String::new();
    while i < bytes.len() {
        match bytes[i] {
            b'>' => return Some((pdf_hex_to_bytes(&hex)?, i + 1)),
            b if b.is_ascii_hexdigit() => hex.push(b as char),
            b if b.is_ascii_whitespace() => {}
            _ => return None,
        }
        i += 1;
    }
    None
}

fn decode_pdf_hex_text(hex: &str) -> String {
    let mut normalized = hex.to_string();
    if normalized.len() % 2 == 1 {
        normalized.push('0');
    }
    let mut bytes = Vec::new();
    let chars = normalized.as_bytes();
    for pair in chars.chunks(2) {
        if pair.len() != 2 {
            continue;
        }
        let Ok(s) = std::str::from_utf8(pair) else {
            continue;
        };
        if let Ok(value) = u8::from_str_radix(s, 16) {
            bytes.push(value);
        }
    }
    if bytes.starts_with(&[0xfe, 0xff]) && bytes.len() >= 4 {
        let units = bytes[2..]
            .chunks(2)
            .filter_map(|chunk| {
                if chunk.len() == 2 {
                    Some(u16::from_be_bytes([chunk[0], chunk[1]]))
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        String::from_utf16_lossy(&units)
    } else if bytes.starts_with(&[0xff, 0xfe]) && bytes.len() >= 4 {
        let units = bytes[2..]
            .chunks(2)
            .filter_map(|chunk| {
                if chunk.len() == 2 {
                    Some(u16::from_le_bytes([chunk[0], chunk[1]]))
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        String::from_utf16_lossy(&units)
    } else {
        String::from_utf8_lossy(&bytes).to_string()
    }
}

fn truncate_chars(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max_chars).collect();
    out.push_str("\n[truncated]");
    out
}
