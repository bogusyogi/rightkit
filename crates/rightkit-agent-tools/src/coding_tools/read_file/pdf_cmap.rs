fn parse_pdf_tounicode_maps(bytes: &[u8], budget: &mut PdfDecodeBudget) -> PdfToUnicodeMap {
    let objects = parse_pdf_objects(bytes);
    let mut map = PdfToUnicodeMap::new();
    for body in objects.values() {
        if budget.exhausted {
            break;
        }
        let body_text = String::from_utf8_lossy(body);
        if !body_text.contains("beginbfchar") && !body_text.contains("beginbfrange") {
            continue;
        }
        let cmap_bytes = if body_text.contains("/FlateDecode") {
            extract_pdf_stream_bytes(body)
                .and_then(|stream| decode_flate_stream(stream, budget))
                .unwrap_or_else(|| body.to_vec())
        } else {
            extract_pdf_stream_bytes(body)
                .map(|stream| stream.to_vec())
                .unwrap_or_else(|| body.to_vec())
        };
        parse_tounicode_cmap_text(&String::from_utf8_lossy(&cmap_bytes), &mut map);
    }
    map
}

fn parse_tounicode_cmap_text(cmap: &str, map: &mut PdfToUnicodeMap) {
    let mut in_bfchar = false;
    let mut in_bfrange = false;
    for line in cmap.lines() {
        let trimmed = line.trim();
        if trimmed.contains("beginbfchar") {
            in_bfchar = true;
            continue;
        }
        if trimmed.contains("endbfchar") {
            in_bfchar = false;
            continue;
        }
        if trimmed.contains("beginbfrange") {
            in_bfrange = true;
            continue;
        }
        if trimmed.contains("endbfrange") {
            in_bfrange = false;
            continue;
        }
        if in_bfchar {
            parse_tounicode_bfchar_line(trimmed, map);
        } else if in_bfrange {
            parse_tounicode_bfrange_line(trimmed, map);
        }
    }
}

fn parse_tounicode_bfchar_line(line: &str, map: &mut PdfToUnicodeMap) {
    let tokens = pdf_hex_tokens(line);
    if tokens.len() < 2 {
        return;
    }
    if let (Some(src), Some(dst)) = (
        pdf_hex_to_bytes(&tokens[0]),
        decode_tounicode_destination(&tokens[1]),
    ) {
        map.insert(src, dst);
    }
}

fn parse_tounicode_bfrange_line(line: &str, map: &mut PdfToUnicodeMap) {
    let tokens = pdf_hex_tokens(line);
    if tokens.len() < 3 {
        return;
    }
    let Some(start) = pdf_hex_to_bytes(&tokens[0]) else {
        return;
    };
    let Some(end) = pdf_hex_to_bytes(&tokens[1]) else {
        return;
    };
    let Some(start_value) = pdf_code_bytes_to_u32(&start) else {
        return;
    };
    let Some(end_value) = pdf_code_bytes_to_u32(&end) else {
        return;
    };
    if end_value < start_value || end_value - start_value > 1024 {
        return;
    }
    let width = start.len().max(end.len());
    let destinations = &tokens[2..];
    if line.contains('[') {
        for (offset, dest_token) in destinations.iter().enumerate() {
            let code = start_value + offset as u32;
            if code > end_value {
                break;
            }
            if let Some(dst) = decode_tounicode_destination(dest_token) {
                map.insert(u32_to_pdf_code_bytes(code, width), dst);
            }
        }
    } else if let Some(base_dest) = pdf_hex_to_bytes(&tokens[2]) {
        let Some(base_value) = pdf_code_bytes_to_u32(&base_dest) else {
            return;
        };
        for code in start_value..=end_value {
            let dest_value = base_value + (code - start_value);
            let dest = decode_utf16be_units(&u32_to_pdf_code_bytes(dest_value, base_dest.len()));
            if !dest.is_empty() {
                map.insert(u32_to_pdf_code_bytes(code, width), dest);
            }
        }
    }
}

fn pdf_hex_tokens(line: &str) -> Vec<String> {
    let bytes = line.as_bytes();
    let mut tokens = Vec::new();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'<' && bytes.get(i + 1) != Some(&b'<') {
            if let Some(end) = bytes[i + 1..].iter().position(|b| *b == b'>') {
                let raw = &line[i + 1..i + 1 + end];
                if raw
                    .chars()
                    .all(|c| c.is_ascii_hexdigit() || c.is_whitespace())
                {
                    tokens.push(raw.to_string());
                }
                i += end + 2;
                continue;
            }
        }
        i += 1;
    }
    tokens
}

fn pdf_hex_to_bytes(hex: &str) -> Option<Vec<u8>> {
    let mut normalized = hex
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect::<String>();
    if normalized.is_empty() {
        return None;
    }
    if normalized.len() % 2 == 1 {
        normalized.push('0');
    }
    let mut out = Vec::new();
    for pair in normalized.as_bytes().chunks(2) {
        let s = std::str::from_utf8(pair).ok()?;
        out.push(u8::from_str_radix(s, 16).ok()?);
    }
    Some(out)
}

fn decode_tounicode_destination(hex: &str) -> Option<String> {
    let bytes = pdf_hex_to_bytes(hex)?;
    let decoded = decode_utf16be_units(&bytes);
    if decoded.is_empty() {
        None
    } else {
        Some(decoded)
    }
}

fn decode_utf16be_units(bytes: &[u8]) -> String {
    let units = bytes
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
}

fn pdf_code_bytes_to_u32(bytes: &[u8]) -> Option<u32> {
    if bytes.is_empty() || bytes.len() > 4 {
        return None;
    }
    let mut value = 0u32;
    for byte in bytes {
        value = (value << 8) | (*byte as u32);
    }
    Some(value)
}

fn u32_to_pdf_code_bytes(value: u32, width: usize) -> Vec<u8> {
    (0..width)
        .rev()
        .map(|shift| ((value >> (shift * 8)) & 0xff) as u8)
        .collect()
}

fn extract_cmap_pdf_text(text: &str, map: &PdfToUnicodeMap) -> String {
    let mut out = Vec::new();
    let bytes = text.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'<' && bytes.get(i + 1) != Some(&b'<') {
            if let Some((raw_bytes, next)) = parse_pdf_hex_bytes(bytes, i + 1) {
                let rest = &text[next..text.len().min(next + 32)];
                if rest_has_text_operator(rest) {
                    let decoded = decode_pdf_bytes_with_cmap(&raw_bytes, map);
                    if !decoded.trim().is_empty() {
                        out.push(decoded);
                    }
                }
                i = next;
                continue;
            }
        }
        i += 1;
    }
    out.join("\n")
}

fn decode_pdf_bytes_with_cmap(bytes: &[u8], map: &PdfToUnicodeMap) -> String {
    let max_key_len = map.keys().map(Vec::len).max().unwrap_or(1).min(4);
    let mut out = String::new();
    let mut i = 0usize;
    while i < bytes.len() {
        let remaining = bytes.len() - i;
        let mut matched = None;
        for len in (1..=max_key_len.min(remaining)).rev() {
            if let Some(value) = map.get(&bytes[i..i + len]) {
                matched = Some((len, value));
                break;
            }
        }
        if let Some((len, value)) = matched {
            out.push_str(value);
            i += len;
        } else {
            out.push(bytes[i] as char);
            i += 1;
        }
    }
    out
}

fn extract_pdf_contents_refs(page_body: &str) -> Vec<u32> {
    let Some(contents_index) = page_body.find("/Contents") else {
        return Vec::new();
    };
    let after = &page_body[contents_index..];
    let end = after
        .find(">>")
        .map(|idx| idx.min(after.len()))
        .unwrap_or(after.len());
    let tokens = after[..end]
        .split_whitespace()
        .map(|token| token.trim_matches(|c: char| matches!(c, '[' | ']' | '<' | '>' | '(' | ')')))
        .collect::<Vec<_>>();
    let mut refs = Vec::new();
    for window in tokens.windows(3) {
        if window[1] == "0" && window[2] == "R" {
            if let Ok(id) = window[0].parse::<u32>() {
                refs.push(id);
            }
        }
    }
    refs
}
