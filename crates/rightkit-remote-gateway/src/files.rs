//! `files.info`, `files.read` and `files.find`: read-only, inside the approved folders only (see
//! `confine`). Each takes the settings it is checked against, so the CLI and the engine answer
//! the same way.

use super::b64;
use super::confine::{self, Root};
use super::redact;
use super::settings::Settings;
use super::{Refusal, Result};
use serde_json::{Value, json};
use std::fs::Metadata;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, UNIX_EPOCH};

/// Deepest folder a search enters.
const FIND_MAX_DEPTH: usize = 32;
/// Most directory entries a search looks at, in total.
const FIND_MAX_VISITED: usize = 200_000;
/// Longest a search runs.
const FIND_MAX_TIME: Duration = Duration::from_secs(8);
/// Longest search text, in characters.
const QUERY_MAX_CHARS: usize = 256;
/// Content search: the largest file read, in bytes.
const CONTENT_FILE_MAX: u64 = 2 * 1024 * 1024;
/// Content search: the most bytes read in one search.
const CONTENT_TOTAL_MAX: u64 = 256 * 1024 * 1024;
/// Content search: most matching lines returned per file, and longest one in characters.
const MATCHES_PER_FILE: usize = 3;
const MATCH_CHARS: usize = 200;

fn mtime(meta: &Metadata) -> Option<u64> {
    meta.modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs())
}

/// Most lines `files.info` `tail` returns.
const TAIL_MAX_LINES: u64 = 1000;
/// Most bytes `files.info` `tail` looks back through, whatever the line count.
const TAIL_WINDOW: u64 = 256 * 1024;
/// Largest file `files.info` `sha256` hashes.
const HASH_MAX_BYTES: u64 = 1024 * 1024 * 1024;

/// `files.info`: kind, size, modification time and read-only flag, no contents; with `args`
/// `tail` (the last N lines of a text file, N from 1 to 1000, read from the end of the last
/// 256 KiB) and/or `sha256: true` (the SHA-256 of a file up to 1 GiB) it also returns those.
/// A secret-named file gets neither (they would return or confirm its content, `sensitive_path`)
/// on any door, and tail text passes `redact`; `_gateway` no longer changes either (#578 D5).
pub fn info(settings: &Settings, path: &str, args: &Value, _gateway: bool) -> Result<Value> {
    let tail = match &args["tail"] {
        Value::Null => None,
        value => match value.as_u64() {
            Some(n) if (1..=TAIL_MAX_LINES).contains(&n) => Some(n as usize),
            _ => {
                return Err(Refusal::new(
                    "invalid_request",
                    format!("tail is a number of lines from 1 to {TAIL_MAX_LINES}"),
                ));
            }
        },
    };
    let hash = match &args["sha256"] {
        Value::Null => false,
        Value::Bool(b) => *b,
        _ => {
            return Err(Refusal::new("invalid_request", "sha256 is true or false"));
        }
    };
    let roots = confine::approved_roots(settings)?;
    let resolved = confine::resolve(&roots, path)?;
    // The handle is opened and verified like a read's, and the facts come from it.
    let (mut handle, meta) = confine::open_any(&roots, &resolved)?;
    let folder = meta.is_dir();
    let mut extra = serde_json::Map::new();
    if tail.is_some() || hash {
        if folder {
            return Err(Refusal::new(
                "invalid_request",
                "tail and sha256 need a file, not a folder",
            ));
        }
        if secret_file(&resolved.path) || secret_file(Path::new(path)) {
            return Err(Refusal::new(
                "sensitive_path",
                "that file usually holds secrets and is not returned (the agent never sees a secret)",
            ));
        }
        if !meta.is_file() {
            return Err(Refusal::new(
                "special_file",
                "tail and sha256 need a regular file",
            ));
        }
    }
    if hash {
        if meta.len() > HASH_MAX_BYTES {
            return Err(Refusal::new(
                "too_large",
                format!("a hash is limited to files of {HASH_MAX_BYTES} bytes"),
            ));
        }
        let mut limited = (&mut handle).take(HASH_MAX_BYTES + 1);
        let digest = rightkit_fs::digest::sha256_reader(&mut limited)
            .map_err(|e| Refusal::new("read_failed", format!("could not read: {}", e.kind())))?;
        if limited.limit() == 0 {
            return Err(Refusal::new(
                "too_large",
                format!("the file grew past {HASH_MAX_BYTES} bytes while it was hashed"),
            ));
        }
        extra.insert("sha256".into(), json!(digest));
        extra.insert(
            "hashedBytes".into(),
            json!(HASH_MAX_BYTES + 1 - limited.limit()),
        );
    }
    if let Some(wanted) = tail {
        let size = meta.len();
        let start = size.saturating_sub(TAIL_WINDOW);
        handle
            .seek(SeekFrom::Start(start))
            .map_err(|e| Refusal::new("read_failed", format!("could not seek: {}", e.kind())))?;
        let mut bytes = Vec::new();
        (&mut handle)
            .take(TAIL_WINDOW)
            .read_to_end(&mut bytes)
            .map_err(|e| Refusal::new("read_failed", format!("could not read: {}", e.kind())))?;
        if bytes.contains(&0) {
            return Err(Refusal::new(
                "unsupported_format",
                "tail reads text files; this one holds binary data",
            ));
        }
        let mut text = String::from_utf8_lossy(&bytes).into_owned();
        // A window that starts inside the file starts inside a line: drop that partial line.
        let mut more_before = start > 0;
        if start > 0
            && let Some(newline) = text.find('\n')
        {
            text.drain(..=newline);
        }
        let lines: Vec<&str> = text.lines().collect();
        if lines.len() > wanted {
            more_before = true;
        }
        let kept = &lines[lines.len().saturating_sub(wanted)..];
        let joined = kept.join("\n");
        let (joined, redactions) = redact::redact(&joined);
        let mut block = json!({
            "lines": kept.len(),
            "text": joined,
            "moreBefore": more_before,
        });
        block["redactions"] = json!(redactions);
        extra.insert("tail".into(), block);
    }
    let mut answer = json!({
        "ok": true,
        "path": confine::display(&resolved.path),
        "root": roots[resolved.root].index,
        "relative": confine::display(&resolved.relative),
        "kind": if folder { "directory" } else { "file" },
        "size": if folder { Value::Null } else { json!(meta.len()) },
        "mtime": mtime(&meta),
        "readonly": meta.permissions().readonly(),
    });
    if let Some(fields) = answer.as_object_mut() {
        fields.extend(extra);
    }
    Ok(answer)
}

/// `files.read`: up to `limit` bytes of a file from `offset`, as UTF-8 text or, for a binary
/// window, base64. `limit` defaults to `remoteReadMaxBytes`; a larger one is refused
/// (`too_large`). `truncated` says there is more after this window.
///
/// On every door (local CLI, engine, gateway; #578 D5, B7/R12: the agent never sees the secret)
/// files that usually hold secrets (`secret_file`) are refused (`sensitive_path`) on top of the
/// credential folders, and a text window passes `redact`. `_gateway` no longer changes either. `bytes`, `size` and `nextOffset` stay in source bytes, so a
/// redacted window can be shorter than they say. A binary (base64) window cannot be redacted.
pub fn read(
    settings: &Settings,
    path: &str,
    offset: Option<u64>,
    limit: Option<u64>,
    _gateway: bool,
) -> Result<Value> {
    // The switch and the folders come first: a refused caller learns nothing else.
    let roots = confine::approved_roots(settings)?;
    let cap = settings.read_max_bytes;
    let limit = match limit {
        Some(0) => return Err(Refusal::new("invalid_request", "--limit must be 1 or more")),
        Some(n) if n > cap => {
            return Err(Refusal::new(
                "too_large",
                format!("a read is limited to {cap} bytes (remoteReadMaxBytes); ask for less"),
            ));
        }
        Some(n) => n,
        None => cap,
    };
    let offset = offset.unwrap_or(0);
    let resolved = confine::resolve(&roots, path)?;
    // The name as asked and the name the path resolves to (a link can point at a secret file).
    if secret_file(&resolved.path) || secret_file(Path::new(path)) {
        return Err(Refusal::new(
            "sensitive_path",
            "that file usually holds secrets and is not returned (the agent never sees a secret)",
        ));
    }
    let (mut file, meta) = confine::open_regular(&roots, &resolved)?;
    let size = meta.len();
    if offset > 0 {
        file.seek(SeekFrom::Start(offset))
            .map_err(|e| Refusal::new("read_failed", format!("could not seek: {}", e.kind())))?;
    }
    // One byte more than the window tells whether the file goes on.
    let mut bytes = Vec::new();
    file.take(limit.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|e| Refusal::new("read_failed", format!("could not read: {}", e.kind())))?;
    let more = bytes.len() as u64 > limit;
    if more {
        bytes.truncate(limit as usize);
    }
    let (encoding, content, used) = encode(&bytes, more);
    let used = used as u64;
    let truncated = more || used < bytes.len() as u64;
    let (content, redactions) = if encoding == "utf-8" {
        redact::redact(&content)
    } else {
        (content, 0)
    };
    let mut answer = json!({
        "ok": true,
        "path": confine::display(&resolved.path),
        "root": roots[resolved.root].index,
        "size": size,
        "offset": offset,
        "bytes": used,
        "encoding": encoding,
        "content": content,
        "truncated": truncated,
        "nextOffset": if truncated { json!(offset + used) } else { Value::Null },
    });
    answer["redactions"] = json!(redactions);
    Ok(answer)
}

/// Most paths one batch read takes.
const BATCH_MAX_PATHS: usize = 16;
/// Most bytes of a file a line-range read walks through to reach its first line.
const LINES_SCAN_MAX: u64 = 64 * 1024 * 1024;

/// `files.read` with `args`: `paths` (a batch of up to 16 whole-file reads) or `startLine` and
/// `endLine` (1-based, inclusive, on one `path`). Everything else is `read`.
///
/// A batch shares one budget: `limit` (default and cap `remoteReadMaxBytes`) bytes in all, each
/// file taking what it needs from what is left, from the start of the file (`offset` is not for a
/// batch). A file that fails is a row with `error` and `code` and the rest still read; a file that
/// finds the budget spent is `too_large`. A line range walks the file (at most 64 MiB) and
/// returns the lines from `startLine` through `endLine` that fit `limit` bytes (default and cap
/// `remoteReadMaxBytes`): `truncated` and `nextLine` say where to continue.
pub fn read_more(
    settings: &Settings,
    path: Option<&str>,
    args: &Value,
    offset: Option<u64>,
    limit: Option<u64>,
    gateway: bool,
) -> Result<Value> {
    let bad = |why: &str| Refusal::new("invalid_request", why.to_string());
    let number = |key: &str| -> Result<Option<u64>> {
        match &args[key] {
            Value::Null => Ok(None),
            value => match value.as_u64() {
                Some(n) if n >= 1 => Ok(Some(n)),
                _ => Err(bad(&format!("{key} is a line number from 1"))),
            },
        }
    };
    let (start, end) = (number("startLine")?, number("endLine")?);
    if !args["paths"].is_null() {
        if path.is_some() || start.is_some() || end.is_some() || offset.is_some() {
            return Err(bad(
                "paths is a batch of whole files: give it without path, offset, startLine or endLine",
            ));
        }
        let Some(list) = args["paths"].as_array() else {
            return Err(bad("paths must be a list"));
        };
        if list.is_empty() || list.len() > BATCH_MAX_PATHS {
            return Err(bad(&format!("paths holds 1 to {BATCH_MAX_PATHS} items")));
        }
        let mut names = Vec::with_capacity(list.len());
        for item in list {
            match item.as_str().filter(|p| !p.is_empty()) {
                Some(p) => names.push(p),
                None => return Err(bad("each path is a non-empty string")),
            }
        }
        let cap = settings.read_max_bytes;
        let total = match limit {
            Some(0) => return Err(bad("limit must be 1 or more")),
            Some(n) if n > cap => {
                return Err(Refusal::new(
                    "too_large",
                    format!("a read is limited to {cap} bytes (remoteReadMaxBytes); ask for less"),
                ));
            }
            Some(n) => n,
            None => cap,
        };
        let mut left = total;
        let mut rows = Vec::with_capacity(names.len());
        for name in names {
            if left == 0 {
                let mut row = Refusal::new(
                    "too_large",
                    "the total limit of this call was used up by earlier files; read this one again",
                )
                .to_json();
                row["path"] = json!(name);
                rows.push(row);
                continue;
            }
            match read(settings, name, None, Some(left), gateway) {
                Ok(row) => {
                    left = left.saturating_sub(row["bytes"].as_u64().unwrap_or(0));
                    rows.push(row);
                }
                Err(refusal) => {
                    let mut row = refusal.to_json();
                    row["path"] = json!(name);
                    rows.push(row);
                }
            }
        }
        return Ok(json!({
            "ok": true,
            "count": rows.len(),
            "totalBytes": total - left,
            "files": rows,
        }));
    }
    let Some(path) = path else {
        return Err(bad("a path is needed"));
    };
    let first = start.unwrap_or(1);
    if let Some(last) = end
        && last < first
    {
        return Err(bad("endLine is before startLine"));
    }
    if offset.is_some() {
        return Err(bad(
            "offset is for byte reads; startLine and endLine are for lines",
        ));
    }
    let roots = confine::approved_roots(settings)?;
    let cap = settings.read_max_bytes;
    let limit = match limit {
        Some(0) => return Err(bad("limit must be 1 or more")),
        Some(n) if n > cap => {
            return Err(Refusal::new(
                "too_large",
                format!("a read is limited to {cap} bytes (remoteReadMaxBytes); ask for less"),
            ));
        }
        Some(n) => n,
        None => cap,
    };
    let resolved = confine::resolve(&roots, path)?;
    if secret_file(&resolved.path) || secret_file(Path::new(path)) {
        return Err(Refusal::new(
            "sensitive_path",
            "that file usually holds secrets and is not returned (the agent never sees a secret)",
        ));
    }
    let (file, meta) = confine::open_regular(&roots, &resolved)?;
    let read_fail =
        |e: std::io::Error| Refusal::new("read_failed", format!("could not read: {}", e.kind()));
    let mut reader = std::io::BufReader::new(file.take(LINES_SCAN_MAX));
    let mut line = Vec::new();
    let mut number_now = 0u64;
    let mut out = String::new();
    let mut last_returned = 0u64;
    let mut next_line: Option<u64> = None;
    let mut ended = false;
    loop {
        line.clear();
        let got = std::io::BufRead::read_until(&mut reader, b'\n', &mut line).map_err(read_fail)?;
        if got == 0 {
            ended = true;
            break;
        }
        number_now += 1;
        if line[..line.len().min(8192)].contains(&0) {
            return Err(Refusal::new(
                "unsupported_format",
                "a line range reads text files; this one holds binary data",
            ));
        }
        if number_now < first {
            continue;
        }
        if end.is_some_and(|last| number_now > last) {
            break;
        }
        let text = String::from_utf8_lossy(&line);
        if !out.is_empty() && (out.len() + text.len()) as u64 > limit {
            next_line = Some(number_now);
            break;
        }
        // A first line longer than the limit is cut at a character boundary so progress is made.
        if text.len() as u64 > limit {
            let mut cut = limit as usize;
            while !text.is_char_boundary(cut) {
                cut -= 1;
            }
            out.push_str(&text[..cut]);
            last_returned = number_now;
            next_line = Some(number_now + 1);
            break;
        }
        out.push_str(&text);
        last_returned = number_now;
    }
    if last_returned == 0 && ended && first > number_now {
        return Err(bad(&format!(
            "the file has {number_now} lines; startLine is after the last one"
        )));
    }
    let (content, redactions) = redact::redact(&out);
    let mut answer = json!({
        "ok": true,
        "path": confine::display(&resolved.path),
        "root": roots[resolved.root].index,
        "size": meta.len(),
        "startLine": first,
        "endLine": last_returned,
        "encoding": "utf-8",
        "content": content,
        "truncated": next_line.is_some(),
        "nextLine": next_line,
        "bytes": content.len(),
    });
    answer["redactions"] = json!(redactions);
    Ok(answer)
}

/// The most source bytes `files.image` returns whatever `remoteImageMaxBytes` says. The answer
/// carries the file as base64 inside one JSON frame, which must stay under the engine contract's
/// 16 MiB and the gateway's 8 MiB result limit: 5 MiB becomes about 6.7 MiB.
const IMAGE_REPLY_MAX: u64 = 5 * 1024 * 1024;
/// The most pixels an image may claim in its header (a decompression bomb for whoever decodes it).
const IMAGE_MAX_PIXELS: u64 = 100_000_000;

/// What an image's own header says.
struct ImageHeader {
    mime: &'static str,
    width: u64,
    height: u64,
}

/// `files.image`: one PNG, JPEG or GIF inside an approved folder, returned as it is on disk
/// (base64 in `content`) with its type and size from its header. the host app has no image decoder, so
/// nothing is re-encoded or shrunk: `max_pixel` (the longest side the caller can use) refuses a
/// larger image (`too_large`) instead of scaling it. The type comes from the file's bytes, never
/// its name; any other format, including SVG (which can carry script), is `unsupported_format`.
/// A file over `min(remoteImageMaxBytes, 5 MiB)` is `too_large`.
///
/// Files that usually hold secrets are refused (`sensitive_path`) on every door, as for
/// `files.read`. The pixels themselves are not redacted.
pub fn image(
    settings: &Settings,
    path: &str,
    max_pixel: Option<u64>,
    _gateway: bool,
) -> Result<Value> {
    // The switch and the folders come first: a refused caller learns nothing else.
    let roots = confine::approved_roots(settings)?;
    if max_pixel == Some(0) {
        return Err(Refusal::new(
            "invalid_request",
            "--max-pixel must be 1 or more",
        ));
    }
    let cap = settings.image_max_bytes.min(IMAGE_REPLY_MAX);
    let resolved = confine::resolve(&roots, path)?;
    if secret_file(&resolved.path) || secret_file(Path::new(path)) {
        return Err(Refusal::new(
            "sensitive_path",
            "that file usually holds secrets and is not returned (the agent never sees a secret)",
        ));
    }
    let (file, meta) = confine::open_regular(&roots, &resolved)?;
    let too_large = || {
        Refusal::new(
            "too_large",
            format!(
                "an image is limited to {cap} bytes (remoteImageMaxBytes, at most {IMAGE_REPLY_MAX})"
            ),
        )
    };
    if meta.len() > cap {
        return Err(too_large());
    }
    let mut bytes = Vec::with_capacity(meta.len() as usize);
    // One byte more than the cap tells a file that grew after the check.
    file.take(cap.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|e| Refusal::new("read_failed", format!("could not read: {}", e.kind())))?;
    if bytes.len() as u64 > cap {
        return Err(too_large());
    }
    let Some(header) = image_header(&bytes) else {
        return Err(Refusal::new(
            "unsupported_format",
            "files.image returns PNG, JPEG and GIF files (detected from the file's content)",
        ));
    };
    if header.width == 0 || header.height == 0 {
        return Err(Refusal::new("unreadable", "the image reports no size"));
    }
    if header.width.saturating_mul(header.height) > IMAGE_MAX_PIXELS {
        return Err(Refusal::new(
            "too_large",
            format!("the image claims more than {IMAGE_MAX_PIXELS} pixels"),
        ));
    }
    if let Some(side) = max_pixel
        && header.width.max(header.height) > side
    {
        return Err(Refusal::new(
            "too_large",
            "the image is larger than --max-pixel; the app cannot shrink an image, so it is not returned",
        ));
    }
    let size = bytes.len() as u64;
    Ok(json!({
        "ok": true,
        "path": confine::display(&resolved.path),
        "root": roots[resolved.root].index,
        "mime": header.mime,
        "width": header.width,
        "height": header.height,
        "size": size,
        "encoding": "base64",
        "content": b64::encode(&bytes),
        "bytes": size,
    }))
}

/// The type and size a PNG, JPEG or GIF header declares; `None` for anything else or a header cut
/// short. Only the header is read, nothing is decoded.
fn image_header(bytes: &[u8]) -> Option<ImageHeader> {
    const PNG: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    if bytes.len() >= 24 && bytes[..8] == PNG && &bytes[12..16] == b"IHDR" {
        let side = |at: usize| {
            u64::from(u32::from_be_bytes([
                bytes[at],
                bytes[at + 1],
                bytes[at + 2],
                bytes[at + 3],
            ]))
        };
        return Some(ImageHeader {
            mime: "image/png",
            width: side(16),
            height: side(20),
        });
    }
    if bytes.len() >= 10 && (bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a")) {
        return Some(ImageHeader {
            mime: "image/gif",
            width: u64::from(u16::from_le_bytes([bytes[6], bytes[7]])),
            height: u64::from(u16::from_le_bytes([bytes[8], bytes[9]])),
        });
    }
    if bytes.starts_with(&[0xFF, 0xD8]) {
        return jpeg_header(bytes);
    }
    None
}

/// A JPEG's size from its first start-of-frame segment, walking the segment lengths.
fn jpeg_header(bytes: &[u8]) -> Option<ImageHeader> {
    let mut at = 2usize;
    while at + 4 <= bytes.len() {
        if bytes[at] != 0xFF {
            return None;
        }
        let marker = bytes[at + 1];
        match marker {
            // Fill byte before a marker.
            0xFF => {
                at += 1;
                continue;
            }
            // Markers with no length: restart, SOI, TEM.
            0x01 | 0xD0..=0xD7 | 0xD8 => {
                at += 2;
                continue;
            }
            // End of image or a stuffed zero before any frame header.
            0x00 | 0xD9 => return None,
            _ => {}
        }
        let length = usize::from(u16::from_be_bytes([bytes[at + 2], bytes[at + 3]]));
        if length < 2 {
            return None;
        }
        // SOF0 to SOF15 except DHT (C4), JPG (C8) and DAC (CC).
        if matches!(marker, 0xC0..=0xCF) && !matches!(marker, 0xC4 | 0xC8 | 0xCC) {
            if at + 9 > bytes.len() {
                return None;
            }
            return Some(ImageHeader {
                mime: "image/jpeg",
                height: u64::from(u16::from_be_bytes([bytes[at + 5], bytes[at + 6]])),
                width: u64::from(u16::from_be_bytes([bytes[at + 7], bytes[at + 8]])),
            });
        }
        at += 2 + length;
    }
    None
}

/// Files that usually hold secrets, by name (letter case ignored): `.env`, `.env.*`, `*.pem`,
/// `*.key`, `*.p12`, `*.pfx`, `*.keystore`, `id_rsa*`, `id_ed25519*`, `credentials*.json` and
/// `.netrc`. The credential folders are `confine::sensitive`'s; this is the file-name list every
/// door adds.
pub(super) fn secret_file(path: &Path) -> bool {
    let Some(name) = path
        .file_name()
        .map(|n| n.to_string_lossy().to_ascii_lowercase())
    else {
        return false;
    };
    name == ".env"
        || name.starts_with(".env.")
        || name == ".netrc"
        || name.starts_with("id_rsa")
        || name.starts_with("id_ed25519")
        || [".pem", ".key", ".p12", ".pfx", ".keystore"]
            .iter()
            .any(|ext| name.ends_with(*ext))
        || (name.starts_with("credentials") && name.ends_with(".json"))
}

/// The window as text or base64, and how many of its bytes that covers. A window cut in the
/// middle of a UTF-8 character ends before it (the next window starts there); when that leaves
/// nothing (a window smaller than one character) the window is base64, so `nextOffset` always
/// moves forward.
fn encode(bytes: &[u8], cut_here: bool) -> (&'static str, String, usize) {
    match std::str::from_utf8(bytes) {
        Ok(text) if !text.contains('\0') => ("utf-8", text.to_string(), bytes.len()),
        Err(error)
            if cut_here
                && error.error_len().is_none()
                && error.valid_up_to() > 0
                && !bytes[..error.valid_up_to()].contains(&0) =>
        {
            let valid = error.valid_up_to();
            let text = std::str::from_utf8(&bytes[..valid])
                .unwrap_or_default()
                .to_string();
            ("utf-8", text, valid)
        }
        _ => ("base64", b64::encode(bytes), bytes.len()),
    }
}

/// `files.find`: names that contain `query` (case-insensitive), in the approved folders only.
/// `root` narrows it to one approved folder (its number in `remoteRoots`) or to a folder inside
/// one (a path). `ext` keeps files with one of those extensions. `limit` defaults to
/// `remoteFindMaxResults`; a larger one is refused (`too_large`).
///
/// It is a bounded walk, not the whole-disk index, which is not limited to the approved folders.
/// The walk never enters or lists a link, skips credential folders, and stops at the result
/// limit, `FIND_MAX_VISITED` entries, `FIND_MAX_DEPTH` levels or `FIND_MAX_TIME`; `complete` says
/// whether it looked at everything.
///
/// `args["mode"]` is `name` (the default) or `content`: then `query` is looked for inside the
/// text files (not secret-named, at most `CONTENT_FILE_MAX` each, UTF-8 without NUL; at most
/// `CONTENT_TOTAL_MAX` bytes read in all), case-insensitively, and each result carries up to
/// `MATCHES_PER_FILE` matching lines (`line`, `text`; the text passes `redact`).
/// Files skipped for size are counted in `skipped`. Same approved folders, same stop limits.
pub fn find(
    settings: &Settings,
    query: &str,
    root: Option<&str>,
    ext: &[String],
    limit: Option<u64>,
    args: &Value,
    _gateway: bool,
) -> Result<Value> {
    let content = match args["mode"].as_str() {
        None if args["mode"].is_null() => false,
        Some("name") => false,
        Some("content") => true,
        _ => {
            return Err(Refusal::new("invalid_request", "mode is name or content"));
        }
    };
    let roots = confine::approved_roots(settings)?;
    let cap = settings.find_max_results;
    let limit = match limit {
        Some(0) => return Err(Refusal::new("invalid_request", "--limit must be 1 or more")),
        Some(n) if n > cap => {
            return Err(Refusal::new(
                "too_large",
                format!("a search returns at most {cap} results (remoteFindMaxResults)"),
            ));
        }
        Some(n) => n as usize,
        None => cap as usize,
    };
    let query = query.trim();
    if query.is_empty() || query.chars().count() > QUERY_MAX_CHARS {
        return Err(Refusal::new(
            "invalid_request",
            format!("a search needs text of 1 to {QUERY_MAX_CHARS} characters"),
        ));
    }
    let needle = query.to_lowercase();
    let wanted: Vec<String> = ext
        .iter()
        .map(|e| e.trim().trim_start_matches('.').to_ascii_lowercase())
        .filter(|e| !e.is_empty())
        .collect();
    let starts = starts(&roots, root)?;

    let mut walk = Walk {
        roots: &roots,
        needle: &needle,
        wanted: &wanted,
        limit,
        started: Instant::now(),
        visited: 0,
        results: Vec::new(),
        complete: true,
        content,
        bytes_read: 0,
        skipped: 0,
    };
    for (position, start) in starts {
        walk.run(position, start);
        if !walk.complete {
            break;
        }
    }
    let returned = walk.results.len();
    let mut answer = json!({
        "ok": true,
        "query": query,
        "results": walk.results,
        "returned": returned,
        "complete": walk.complete,
    });
    if content {
        answer["mode"] = json!("content");
        answer["skipped"] = json!(walk.skipped);
    }
    Ok(answer)
}

/// Where a search starts: every approved folder, or the one `root` names.
fn starts(roots: &[Root], root: Option<&str>) -> Result<Vec<(usize, PathBuf)>> {
    let Some(root) = root.map(str::trim).filter(|r| !r.is_empty()) else {
        return Ok(roots
            .iter()
            .enumerate()
            .map(|(position, r)| (position, r.path.clone()))
            .collect());
    };
    if let Ok(number) = root.parse::<usize>() {
        return match roots.iter().position(|r| r.index == number) {
            Some(position) => Ok(vec![(position, roots[position].path.clone())]),
            None => Err(Refusal::new(
                "outside_roots",
                "no approved folder has that number",
            )),
        };
    }
    let resolved = confine::resolve(roots, root)?;
    let (_handle, meta) = confine::open_any(roots, &resolved)?;
    if !meta.is_dir() {
        return Err(Refusal::new("invalid_request", "--root must be a folder"));
    }
    Ok(vec![(resolved.root, resolved.path)])
}

struct Walk<'a> {
    roots: &'a [Root],
    needle: &'a str,
    wanted: &'a [String],
    limit: usize,
    started: Instant,
    visited: usize,
    results: Vec<Value>,
    complete: bool,
    /// Content search: `needle` is looked for inside files, not in names.
    content: bool,
    /// Content search: bytes read so far, and files left unread for size.
    bytes_read: u64,
    skipped: usize,
}

impl Walk<'_> {
    fn stop(&mut self) {
        self.complete = false;
    }

    fn run(&mut self, position: usize, start: PathBuf) {
        // Breadth first, each folder's names in order, so the same tree answers the same way.
        let mut queue = std::collections::VecDeque::from([(start, 0usize)]);
        while let Some((dir, depth)) = queue.pop_front() {
            if self.started.elapsed() > FIND_MAX_TIME || self.visited >= FIND_MAX_VISITED {
                return self.stop();
            }
            // The folder is opened and verified like a read's file (not a link, inside the
            // approved folder), then read, then checked to be the folder that was opened.
            let roots = self.roots;
            let Ok(handle) = confine::open_dir(&roots[position], &dir) else {
                continue;
            };
            let Ok(read) = std::fs::read_dir(&dir) else {
                continue;
            };
            // A folder of millions of names is read only as far as the visit limit allows.
            let room = FIND_MAX_VISITED.saturating_sub(self.visited) + 1;
            let mut entries: Vec<_> = read.flatten().take(room).collect();
            if !confine::still_here(&handle, &dir) {
                continue;
            }
            drop(handle);
            entries.sort_by_key(|e| e.file_name());
            for entry in entries {
                self.visited += 1;
                if self.visited >= FIND_MAX_VISITED {
                    return self.stop();
                }
                // `DirEntry::file_type` does not follow a link, so a link is neither listed
                // nor entered.
                let Ok(kind) = entry.file_type() else {
                    continue;
                };
                let name = entry.file_name().to_string_lossy().into_owned();
                let is_dir = kind.is_dir();
                if !is_dir && !kind.is_file() {
                    continue;
                }
                if confine::sensitive(Path::new(&name))
                    || (is_dir && confine::protected(&dir.join(&name)))
                {
                    continue;
                }
                if self.content {
                    if !is_dir && self.keeps(&name, false) {
                        if self.started.elapsed() > FIND_MAX_TIME
                            || self.bytes_read >= CONTENT_TOTAL_MAX
                        {
                            return self.stop();
                        }
                        if let Some(matches) = self.scan(&dir.join(&name)) {
                            if self.results.len() >= self.limit {
                                return self.stop();
                            }
                            self.push(position, &dir, &entry, false, Some(matches));
                        }
                    }
                } else if name.to_lowercase().contains(self.needle) && self.keeps(&name, is_dir) {
                    if self.results.len() >= self.limit {
                        return self.stop();
                    }
                    self.push(position, &dir, &entry, is_dir, None);
                }
                if is_dir {
                    if depth + 1 > FIND_MAX_DEPTH {
                        self.complete = false;
                    } else {
                        queue.push_back((dir.join(&name), depth + 1));
                    }
                }
            }
        }
    }

    /// An extension filter keeps files with one of the extensions and drops folders.
    fn keeps(&self, name: &str, is_dir: bool) -> bool {
        if self.wanted.is_empty() {
            return true;
        }
        if is_dir {
            return false;
        }
        let ext = Path::new(name)
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase)
            .unwrap_or_default();
        self.wanted.contains(&ext)
    }

    /// The matching lines of one file for a content search: `None` when the file is not searched
    /// (secret-named, binary, not UTF-8, over `CONTENT_FILE_MAX`, unreadable) or has no match.
    /// The file is opened through the same confinement as a read.
    fn scan(&mut self, path: &Path) -> Option<Vec<Value>> {
        if secret_file(path) {
            return None;
        }
        let resolved = confine::resolve(self.roots, &path.to_string_lossy()).ok()?;
        if secret_file(&resolved.path) {
            return None;
        }
        let (file, meta) = confine::open_regular(self.roots, &resolved).ok()?;
        if meta.len() > CONTENT_FILE_MAX {
            self.skipped += 1;
            return None;
        }
        let mut bytes = Vec::new();
        file.take(CONTENT_FILE_MAX + 1)
            .read_to_end(&mut bytes)
            .ok()?;
        self.bytes_read += bytes.len() as u64;
        if bytes.len() as u64 > CONTENT_FILE_MAX {
            self.skipped += 1;
            return None;
        }
        if bytes[..bytes.len().min(8192)].contains(&0) {
            return None;
        }
        let text = std::str::from_utf8(&bytes).ok()?;
        let mut matches = Vec::new();
        for (number, line) in text.lines().enumerate() {
            if !line.to_lowercase().contains(self.needle) {
                continue;
            }
            let shown: String = line.trim().chars().take(MATCH_CHARS).collect();
            let shown = redact::redact(&shown).0;
            matches.push(json!({"line": number + 1, "text": shown}));
            if matches.len() >= MATCHES_PER_FILE {
                break;
            }
        }
        (!matches.is_empty()).then_some(matches)
    }

    fn push(
        &mut self,
        position: usize,
        dir: &Path,
        entry: &std::fs::DirEntry,
        is_dir: bool,
        matches: Option<Vec<Value>>,
    ) {
        let path = dir.join(entry.file_name());
        let roots = self.roots;
        let root = &roots[position];
        let relative = path.strip_prefix(&root.path).unwrap_or(path.as_path());
        let meta = entry.metadata().ok();
        let mut row = json!({
            "path": confine::display(&path),
            "root": root.index,
            "relative": confine::display(relative),
            "kind": if is_dir { "directory" } else { "file" },
            "size": match (&meta, is_dir) {
                (Some(m), false) => json!(m.len()),
                _ => Value::Null,
            },
            "mtime": meta.as_ref().and_then(mtime),
        });
        if let Some(matches) = matches {
            row["matches"] = json!(matches);
        }
        self.results.push(row);
    }
}
