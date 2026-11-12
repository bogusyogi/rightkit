fn read_pdf_summary(path: &Path, args: &Value) -> ToolResult {
    let bytes = match read_bounded_bytes(path, MAX_PDF_BYTES, "PDF") {
        Ok(bytes) => bytes,
        Err(error) => return ToolResult::error(error),
    };
    if !bytes.starts_with(b"%PDF-") {
        return ToolResult::error(format!("'{}' is not a valid PDF header", path.display()));
    }
    let text = String::from_utf8_lossy(&bytes);
    let page_count = count_pdf_pages(&text);
    if page_count > MAX_PDF_PAGES {
        return ToolResult::error(format!(
            "PDF has {page_count} pages; maximum supported count is {MAX_PDF_PAGES}"
        ));
    }
    if pdf_has_encryption_dictionary(&text) {
        return ToolResult::ok(format!(
            "PDF file: {}\nsize: {} bytes\npages: {}\n\nExtracted text:\n[encrypted PDF detected; built-in PDF fallback cannot decrypt password-protected documents]",
            path.display(),
            bytes.len(),
            page_count
        ));
    }
    let requested_page = args["page"].as_u64().map(|v| v as usize);
    let requested_page_limit = args["page_limit"].as_u64().map(|v| v as usize);
    if requested_page_limit.is_some_and(|limit| limit == 0 || limit > MAX_PDF_PAGE_WINDOW) {
        return ToolResult::error(format!(
            "PDF page_limit must be between 1 and {MAX_PDF_PAGE_WINDOW}"
        ));
    }
    if let Some(page) = requested_page {
        if page == 0 {
            return ToolResult::error("PDF page must be 1 or greater.");
        }
        if page > page_count {
            return ToolResult::error(format!(
                "PDF page {page} is out of range; document has {page_count} pages."
            ));
        }
    }

    let mut decode_budget = PdfDecodeBudget::default();
    let extracted = if requested_page.is_some() || requested_page_limit.is_some() {
        extract_requested_pdf_pages(
            &bytes,
            &text,
            requested_page.unwrap_or(1),
            requested_page_limit.unwrap_or(1),
            &mut decode_budget,
        )
    } else {
        extract_pdf_text(&bytes, &text, &mut decode_budget)
    };
    let extracted = if extracted.trim().is_empty() {
        "[no extractable text found with built-in PDF fallback]".to_string()
    } else {
        truncate_chars(extracted.trim(), 8000)
    };
    ToolResult::ok(format!(
        "PDF file: {}\nsize: {} bytes\npages: {}\n\nExtracted text:\n{}",
        path.display(),
        bytes.len(),
        page_count,
        extracted
    ))
}

fn count_pdf_pages(text: &str) -> usize {
    let count = text.matches("/Type /Page").count();
    let pages_objects = text.matches("/Type /Pages").count();
    count.saturating_sub(pages_objects).max(1)
}

fn pdf_has_encryption_dictionary(text: &str) -> bool {
    text.contains("/Encrypt")
}

fn extract_basic_pdf_text(text: &str) -> String {
    let mut out = Vec::new();
    let bytes = text.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        match bytes[i] {
            b'(' => {
                if let Some((s, next)) = parse_pdf_string(bytes, i + 1) {
                    let rest = &text[next..text.len().min(next + 32)];
                    if rest_has_text_operator(rest) {
                        out.push(s);
                    }
                    i = next;
                    continue;
                }
            }
            b'<' if bytes.get(i + 1) != Some(&b'<') => {
                if let Some((s, next)) = parse_pdf_hex_string(bytes, i + 1) {
                    let rest = &text[next..text.len().min(next + 32)];
                    if rest_has_text_operator(rest) {
                        out.push(s);
                    }
                    i = next;
                    continue;
                }
            }
            _ => {}
        }
        i += 1;
    }
    out.join("\n")
}

fn rest_has_text_operator(rest: &str) -> bool {
    rest.contains("Tj") || rest.contains("TJ") || rest.contains("'") || rest.contains("\"")
}

const MAX_DECOMPRESSED_PDF_TOTAL_BYTES: usize = 8 * 1024 * 1024;
const MAX_DECOMPRESSED_PDF_STREAM_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug)]
struct PdfDecodeBudget {
    remaining: usize,
    exhausted: bool,
}

impl Default for PdfDecodeBudget {
    fn default() -> Self {
        Self {
            remaining: MAX_DECOMPRESSED_PDF_TOTAL_BYTES,
            exhausted: false,
        }
    }
}

fn extract_pdf_text(bytes: &[u8], text: &str, budget: &mut PdfDecodeBudget) -> String {
    let mut parts = Vec::new();
    let to_unicode = parse_pdf_tounicode_maps(bytes, budget);
    extract_pdf_text_with_map(bytes, text, &to_unicode, budget, &mut parts);
    dedupe_join(parts)
}

fn extract_pdf_text_with_map(
    bytes: &[u8],
    text: &str,
    to_unicode: &PdfToUnicodeMap,
    budget: &mut PdfDecodeBudget,
    parts: &mut Vec<String>,
) {
    if !to_unicode.is_empty() {
        let mapped = extract_cmap_pdf_text(text, to_unicode);
        if !mapped.trim().is_empty() {
            parts.push(mapped);
        }
    }
    let fallback = extract_basic_pdf_text(text);
    if !fallback.trim().is_empty() {
        parts.push(fallback);
    }
    let flate = extract_flate_pdf_text(bytes, to_unicode, budget);
    if !flate.trim().is_empty() {
        parts.push(flate);
    }
}

fn extract_requested_pdf_pages(
    bytes: &[u8],
    text: &str,
    start_page: usize,
    page_limit: usize,
    budget: &mut PdfDecodeBudget,
) -> String {
    let page_limit = page_limit.max(1);
    let page_texts = extract_pdf_page_texts(bytes, start_page, page_limit, budget);
    if page_texts.is_empty() {
        return extract_pdf_text(bytes, text, budget);
    }
    page_texts
        .iter()
        .enumerate()
        .map(|(offset, page_text)| {
            let body = if page_text.trim().is_empty() {
                "[no extractable text on this page]".to_string()
            } else {
                page_text.trim().to_string()
            };
            format!("page {}:\n{}", start_page + offset, body)
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn extract_pdf_page_texts(
    bytes: &[u8],
    start_page: usize,
    page_limit: usize,
    budget: &mut PdfDecodeBudget,
) -> Vec<String> {
    let objects = parse_pdf_objects(bytes);
    if objects.is_empty() {
        return Vec::new();
    }
    let to_unicode = parse_pdf_tounicode_maps(bytes, budget);
    let mut pages = Vec::new();
    let mut page_number = 0usize;
    for body in objects.values() {
        let body_text = String::from_utf8_lossy(body);
        if !body_text.contains("/Type /Page") || body_text.contains("/Type /Pages") {
            continue;
        }
        page_number += 1;
        if page_number < start_page {
            continue;
        }
        if pages.len() >= page_limit {
            break;
        }
        let refs = extract_pdf_contents_refs(&body_text);
        let mut page_text = Vec::new();
        if refs.is_empty() {
            let mut parts = Vec::new();
            extract_pdf_text_with_map(body, &body_text, &to_unicode, budget, &mut parts);
            page_text.push(dedupe_join(parts));
        } else {
            for id in refs {
                if let Some(content_body) = objects.get(&id) {
                    let content_text = String::from_utf8_lossy(content_body);
                    let mut parts = Vec::new();
                    extract_pdf_text_with_map(
                        content_body,
                        &content_text,
                        &to_unicode,
                        budget,
                        &mut parts,
                    );
                    let text = dedupe_join(parts);
                    if !text.trim().is_empty() {
                        page_text.push(text);
                    }
                }
            }
        }
        pages.push(page_text.join("\n"));
    }
    pages
}

fn parse_pdf_objects(bytes: &[u8]) -> BTreeMap<u32, Vec<u8>> {
    let mut objects = BTreeMap::new();
    for chunk in split_bytes(bytes, b"endobj") {
        let start = chunk
            .iter()
            .position(|b| !b.is_ascii_whitespace())
            .unwrap_or(chunk.len());
        let trimmed = &chunk[start..];
        let header_len = trimmed.len().min(96);
        let header = String::from_utf8_lossy(&trimmed[..header_len]);
        let mut header_tokens = header.split_whitespace();
        let Some(id_token) = header_tokens.next() else {
            continue;
        };
        let Some(_generation) = header_tokens.next() else {
            continue;
        };
        let Some(obj_token) = header_tokens.next() else {
            continue;
        };
        if obj_token != "obj" {
            continue;
        }
        let Some(id) = id_token.parse::<u32>().ok() else {
            continue;
        };
        if let Some(body_start) = find_bytes(trimmed, b"obj") {
            let body = &trimmed[body_start + 3..];
            objects.insert(id, trim_ascii_bytes(body).to_vec());
        }
    }
    objects
}

fn split_bytes<'a>(bytes: &'a [u8], delimiter: &[u8]) -> Vec<&'a [u8]> {
    let mut out = Vec::new();
    let mut start = 0usize;
    while let Some(pos) = find_bytes(&bytes[start..], delimiter) {
        out.push(&bytes[start..start + pos]);
        start += pos + delimiter.len();
    }
    if start < bytes.len() {
        out.push(&bytes[start..]);
    }
    out
}

fn trim_ascii_bytes(bytes: &[u8]) -> &[u8] {
    let start = bytes
        .iter()
        .position(|b| !b.is_ascii_whitespace())
        .unwrap_or(bytes.len());
    let end = bytes
        .iter()
        .rposition(|b| !b.is_ascii_whitespace())
        .map(|idx| idx + 1)
        .unwrap_or(start);
    &bytes[start..end]
}

fn extract_flate_pdf_text(
    bytes: &[u8],
    to_unicode: &PdfToUnicodeMap,
    budget: &mut PdfDecodeBudget,
) -> String {
    let objects = parse_pdf_objects(bytes);
    let mut parts = Vec::new();
    for body in objects.values() {
        if budget.exhausted {
            break;
        }
        let body_text = String::from_utf8_lossy(body);
        if !body_text.contains("/FlateDecode") {
            continue;
        }
        if let Some(stream) = extract_pdf_stream_bytes(body) {
            if let Some(decoded) = decode_flate_stream(stream, budget) {
                let decoded_text = String::from_utf8_lossy(&decoded);
                if !to_unicode.is_empty() {
                    let mapped = extract_cmap_pdf_text(&decoded_text, to_unicode);
                    if !mapped.trim().is_empty() {
                        parts.push(mapped);
                    }
                }
                let text = extract_basic_pdf_text(&decoded_text);
                if !text.trim().is_empty() {
                    parts.push(text);
                }
            }
        }
    }
    dedupe_join(parts)
}

fn extract_pdf_stream_bytes(body: &[u8]) -> Option<&[u8]> {
    let stream_marker = b"stream";
    let end_marker = b"endstream";
    let stream_start = find_bytes(body, stream_marker)? + stream_marker.len();
    let mut data_start = stream_start;
    if body.get(data_start) == Some(&b'\r') && body.get(data_start + 1) == Some(&b'\n') {
        data_start += 2;
    } else if matches!(body.get(data_start), Some(b'\n' | b'\r')) {
        data_start += 1;
    }
    let end = find_bytes(&body[data_start..], end_marker)? + data_start;
    let mut data_end = end;
    while data_end > data_start && matches!(body[data_end - 1], b'\r' | b'\n') {
        data_end -= 1;
    }
    Some(&body[data_start..data_end])
}

fn decode_flate_stream(stream: &[u8], budget: &mut PdfDecodeBudget) -> Option<Vec<u8>> {
    if budget.remaining == 0 {
        budget.exhausted = true;
        return None;
    }
    let allowed = budget.remaining.min(MAX_DECOMPRESSED_PDF_STREAM_BYTES);
    let decoder = flate2::read::ZlibDecoder::new(stream);
    let mut decoded = Vec::new();
    decoder
        .take(allowed as u64 + 1)
        .read_to_end(&mut decoded)
        .ok()?;
    if decoded.len() > allowed {
        budget.remaining = 0;
        budget.exhausted = true;
        return None;
    }
    budget.remaining -= decoded.len();
    Some(decoded)
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn dedupe_join(parts: Vec<String>) -> String {
    let mut out = Vec::new();
    for part in parts {
        let trimmed = part.trim();
        if trimmed.is_empty() || out.iter().any(|existing: &String| existing == trimmed) {
            continue;
        }
        out.push(trimmed.to_string());
    }
    out.join("\n")
}

type PdfToUnicodeMap = BTreeMap<Vec<u8>, String>;
