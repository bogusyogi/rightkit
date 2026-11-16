pub struct ReadFileTool;

/// Default ceiling on lines returned when no explicit `limit` is given, so a
/// single read can't flood the context window with a huge file.
const DEFAULT_LINE_CAP: usize = 2000;
const MAX_TEXT_OUTPUT_BYTES: usize = 128 * 1024;
const MAX_TEXT_LINE_SCALARS: usize = 2_000;
const MAX_TEXT_LINE_BYTES: usize = MAX_TEXT_LINE_SCALARS * 4;
pub(crate) const MAX_IMAGE_BYTES: usize = 16 * 1024 * 1024;
pub(crate) const MAX_IMAGE_PIXELS: u64 = 40_000_000;
pub(crate) const MAX_NOTEBOOK_BYTES: usize = 16 * 1024 * 1024;
pub(crate) const MAX_NOTEBOOK_CELLS: usize = 10_000;
pub(crate) const MAX_PDF_BYTES: usize = 32 * 1024 * 1024;
const MAX_PDF_PAGES: usize = 2_000;
const MAX_PDF_PAGE_WINDOW: usize = 50;
/// Byte ceiling of one read that passes `allow_large: true` (host option `read_file_windows`); the
/// line cap is lifted for it.
const ALLOW_LARGE_OUTPUT_BYTES: usize = 1024 * 1024;
/// Windows at or above this keep the full caps; smaller ones scale down in proportion, so one read
/// cannot fill a small model's context (host option `read_file_windows`).
const FULL_CAP_WINDOW_TOKENS: u64 = 131_072;
/// Longest edge sent to the provider; larger images are downscaled first (host option
/// `images_downscaled`).
#[cfg(feature = "host")]
const PROVIDER_IMAGE_MAX_EDGE: u32 = 2000;
/// Most memory the image decoder may allocate for one downscale (host option `images_downscaled`).
#[cfg(feature = "host")]
const DOWNSCALE_MAX_ALLOC: u64 = 256 * 1024 * 1024;

/// (default line cap, byte cap) for a model window in tokens; `None` keeps the full caps.
fn read_caps(window_tokens: Option<u64>) -> (usize, usize) {
    match window_tokens {
        Some(window) if window > 0 && window < FULL_CAP_WINDOW_TOKENS => {
            let scale = |full: usize, floor: usize| {
                ((full as u128 * window as u128 / FULL_CAP_WINDOW_TOKENS as u128) as usize)
                    .max(floor)
            };
            (
                scale(DEFAULT_LINE_CAP, 100),
                scale(MAX_TEXT_OUTPUT_BYTES, 8 * 1024),
            )
        }
        _ => (DEFAULT_LINE_CAP, MAX_TEXT_OUTPUT_BYTES),
    }
}

pub(crate) fn read_bounded_bytes(
    path: &Path,
    max_bytes: usize,
    kind: &str,
) -> Result<Vec<u8>, String> {
    let file = std::fs::File::open(path)
        .map_err(|error| format!("Failed to open {kind} '{}': {error}", path.display()))?;
    let declared = file
        .metadata()
        .map_err(|error| format!("Failed to inspect {kind} '{}': {error}", path.display()))?
        .len();
    if declared > max_bytes as u64 {
        return Err(format!(
            "{kind} '{}' is {declared} bytes; maximum supported size is {max_bytes} bytes",
            path.display()
        ));
    }
    let mut bytes = Vec::with_capacity(declared.min(max_bytes as u64) as usize);
    file.take(max_bytes as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("Failed to read {kind} '{}': {error}", path.display()))?;
    if bytes.len() > max_bytes {
        return Err(format!(
            "{kind} '{}' exceeds the maximum supported size of {max_bytes} bytes",
            path.display()
        ));
    }
    Ok(bytes)
}

#[async_trait::async_trait]
impl Tool for ReadFileTool {
    fn name(&self) -> &str {
        "read_file"
    }

    fn is_read_only(&self) -> bool {
        true
    }

    fn description(&self) -> &str {
        if crate::tool_options::read_file_windows() {
            return "Read file content with hashline prefixes (e.g., '     1:a3f: fn main() {'). Each call returns at most 2000 lines or 128 KB; for a larger file page with offset/limit (limit up to 2000), starting at line 1 and continuing from the offset the result names, or pass allow_large=true to read up to 1 MiB in one call. Reading every range from line 1 to the end counts as reading the whole file. A path must resolve inside the workspace root: an absolute path inside it works, one outside it, or one that climbs with ../ past the root, fails.";
        }
        "Read file content with hashline prefixes (e.g., '     1:a3f: fn main() {'). Use offset/limit to page through large files (default cap: 2000 lines). Fail: passes absolute path outside workspace root, or path climbs with ../ past root."
    }

    fn input_schema(&self) -> Value {
        if crate::tool_options::read_file_windows() {
            return json!({
                "type": "object",
                "required": ["path"],
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the file to read (relative to the workspace root, or absolute inside it)"
                    },
                    "offset": {
                        "type": "integer",
                        "description": "Line number to start reading from (1-based, inclusive)"
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Maximum number of lines to read"
                    },
                    "allow_large": {
                        "type": "boolean",
                        "description": "Lift the 2000-line cap and raise the byte cap from 128 KB to 1 MiB for this read (a bigger file still pages with offset)"
                    },
                    "page": {
                        "type": "integer",
                        "description": "For PDFs, 1-based page number to extract"
                    },
                    "page_limit": {
                        "type": "integer",
                        "description": "For PDFs, maximum number of pages to extract starting at page"
                    }
                }
            });
        }
        json!({
            "type": "object",
            "required": ["path"],
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file to read (relative to workspace root or absolute)"
                },
                "offset": {
                    "type": "integer",
                    "description": "Line number to start reading from (1-based, inclusive)"
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of lines to read"
                },
                "page": {
                    "type": "integer",
                    "description": "For PDFs, 1-based page number to extract"
                },
                "page_limit": {
                    "type": "integer",
                    "description": "For PDFs, maximum number of pages to extract starting at page"
                }
            }
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolResult {
        let path_str = match args["path"].as_str() {
            Some(p) => p,
            None => return ToolResult::error("Missing required argument: path"),
        };

        // Narrow read-only allowance: the host app's own shell-output recovery
        // copies live in the session store, outside the workspace. Only the
        // calling session's own copies qualify.
        let recovery_candidate = std::path::Path::new(path_str);
        // Shell (and its recovery copies) exists only in host builds, not iOS.
        #[cfg(feature = "host")]
        if let Some(reason) =
            crate::shell::shell_output_recovery_unavailable(recovery_candidate, &ctx.session_id)
        {
            return ToolResult::error(format!(
                "[shell output recovery unavailable: {}]",
                reason.label()
            ));
        }
        let is_recovery = recovery_candidate.is_absolute()
            && crate::output_compaction::is_shell_output_recovery_path(
                recovery_candidate,
                &ctx.session_id,
            );
        let path = if is_recovery {
            recovery_candidate.to_path_buf()
        } else {
            match resolve_in_workspace(path_str, &ctx.workspace_root) {
                Ok(p) => p,
                Err(e) => return ToolResult::error(e),
            }
        };
        if !is_recovery {
            if let Err(error) = enforce_path_deny(ctx, &path, PathAccess::Read) {
                return ToolResult::error(error);
            }
        }
        let policy_path = policy_target_path(&path, &ctx.workspace_root);

        // Permission check (recovery copies are the host app-owned, read-only).
        let engine = PermissionEngine::new();
        let verdict = engine.evaluate(
            CapabilityClass::ReadFile,
            ctx.permission_mode,
            ctx.workspace_trust,
            Some(&policy_path),
            false,
        );
        if verdict.decision == PermissionDecision::Deny && !is_recovery {
            return ToolResult::error(format!(
                "Permission denied: {} (policy: {})",
                verdict.reason, verdict.policy_source
            ));
        }

        if path.is_dir() {
            return ToolResult::error(format!(
                "'{}' is a directory, not a file. Use list_files to see its contents.",
                path.display()
            ));
        }

        // PDF/notebook/office readers run in a worker process. Without one,
        // fall through to the plain text read below.
        #[cfg(feature = "host")]
        if let Some(result) =
            crate::rich_parser_worker::read_rich_file(&path, &args, &ctx.workspace_root).await
        {
            return result;
        }

        let windows = crate::tool_options::read_file_windows();
        // A NUL byte in the first 8 KiB means binary: lossy text with hashlines would be noise and
        // would wrongly count as a read, so answer with the size and record no receipt. (Latin-1
        // text has no NUL.)
        if windows {
            if let Ok(mut probe) = std::fs::File::open(&path) {
                let mut head = [0_u8; 8 * 1024];
                if let Ok(read) = probe.read(&mut head) {
                    if head[..read].contains(&0) {
                        let bytes = probe.metadata().map(|m| m.len()).unwrap_or(read as u64);
                        return ToolResult::ok(format!(
                            "'{}' is a binary file ({bytes} bytes), not shown as text. Inspect it with a shell command (xxd, file).",
                            path.display()
                        ));
                    }
                }
            }
        }

        let offset = args["offset"].as_u64().map(|v| v as usize);
        let limit = args["limit"].as_u64().map(|v| v as usize);
        let allow_large = windows && args["allow_large"].as_bool().unwrap_or(false);

        match read_text_file_window(
            &path,
            offset,
            limit,
            windows.then_some((ctx.model_window_tokens, allow_large)),
        ) {
            Ok(output) => {
                let recorded = if crate::tool_options::edit_read_discipline() {
                    ctx.file_service.record_read_window(
                        &path,
                        output.start,
                        output.shown_end,
                        output.reached_eof,
                    )
                } else {
                    ctx.file_service.record_read(&path, output.complete)
                };
                if let Err(error) = recorded {
                    return ToolResult::error(error);
                }
                ToolResult::ok(output.rendered)
            }
            Err(e) => ToolResult::error(format!("Failed to read file '{}': {e}", path.display())),
        }
    }
}

struct TextWindow {
    rendered: String,
    complete: bool,
    /// 0-based first line of the window and 0-based end (exclusive) of the real lines shown;
    /// truncation marker rows are not counted.
    start: usize,
    shown_end: usize,
    reached_eof: bool,
}

/// Read one window of `path`. `windows` is `Some((model window tokens, allow_large))` under host
/// option `read_file_windows` (scaled caps, exact line counts and the corrected resume offset),
/// `None` for the fixed caps.
fn read_text_file_window(
    path: &Path,
    offset: Option<usize>,
    limit: Option<usize>,
    windows: Option<(Option<u64>, bool)>,
) -> std::io::Result<TextWindow> {
    let file = std::fs::File::open(path)?;
    let mut reader = BufReader::new(file);
    let start = offset.map(|o| o.saturating_sub(1)).unwrap_or(0);
    let newer = windows.is_some();
    let (line_cap, byte_cap) = match windows {
        Some((_, true)) => (usize::MAX, ALLOW_LARGE_OUTPUT_BYTES),
        Some((window_tokens, false)) => read_caps(window_tokens),
        None => (DEFAULT_LINE_CAP, MAX_TEXT_OUTPUT_BYTES),
    };
    let requested_limit = limit.unwrap_or(line_cap);
    let mut line_number = 0usize;
    let mut selected = Vec::new();
    let mut output_bytes = 0usize;
    let mut shown_lines = 0usize;
    let mut reached_eof = true;

    while let Some((line, line_was_truncated)) = read_text_line(&mut reader)? {
        let line_index = line_number;
        line_number += 1;
        if line_index < start {
            continue;
        }
        // The newer count leaves the truncation marker rows out of the limit.
        if (if newer { shown_lines } else { selected.len() }) >= requested_limit {
            reached_eof = false;
            break;
        }
        let rendered = format_hashline(line_index + 1, &line);
        if output_bytes.saturating_add(rendered.len() + 1) > byte_cap {
            reached_eof = false;
            break;
        }
        output_bytes += rendered.len() + 1;
        shown_lines += 1;
        selected.push(rendered);
        if line_was_truncated {
            selected.push("[line truncated at 2,000 Unicode scalars]".to_string());
        }
    }

    let mut output = selected.join("\n");
    let explicit_window = offset.is_some() || limit.is_some();
    let shown_from = start.saturating_add(1);
    let shown_to = start + if newer { shown_lines } else { selected.len() };
    // Older rule: a window that ends at end of file is complete wherever it began. Newer rule: only
    // a read that began at line 1 (or continued one) counts, which the file service decides from
    // the recorded window.
    let complete = line_number == shown_to;
    let window_was_capped = if newer { !reached_eof } else { !complete };
    let resume_at = if newer { shown_to + 1 } else { shown_to };

    if window_was_capped {
        if !output.is_empty() {
            output.push_str("\n\n");
        }
        if explicit_window {
            if limit.is_none() {
                output.push_str(&format!(
                    "[read is bounded; showing lines {shown_from}-{shown_to}. Use offset/limit to read more; resume at offset {resume_at}.]"
                ));
            } else {
                output.push_str(&format!(
                    "[read is bounded; showing lines {shown_from}-{shown_to}. Resume at offset {resume_at}.]"
                ));
            }
        } else {
            output.push_str(&format!(
                "[read is bounded; showing first {} lines. Resume at offset {resume_at}.]",
                if newer {
                    if line_cap == usize::MAX {
                        shown_lines
                    } else {
                        line_cap
                    }
                } else {
                    DEFAULT_LINE_CAP
                }
            ));
        }
    } else if output.is_empty() && start >= line_number {
        output = format!(
            "[file has {line_number} lines; offset {shown_from} is beyond the end of the file.]"
        );
    }

    Ok(TextWindow {
        rendered: output,
        complete,
        start,
        shown_end: start + shown_lines,
        reached_eof,
    })
}

/// Consume one line without allowing an unterminated multi-gigabyte line to
/// allocate proportionally. The reader retains at most enough UTF-8 bytes for
/// 2,000 Unicode scalars, discarding the rest until its delimiter.
fn read_text_line(reader: &mut impl BufRead) -> std::io::Result<Option<(String, bool)>> {
    let mut kept = Vec::with_capacity(MAX_TEXT_LINE_BYTES);
    let mut saw_bytes = false;
    let mut truncated = false;
    loop {
        let buffer = reader.fill_buf()?;
        if buffer.is_empty() {
            if !saw_bytes {
                return Ok(None);
            }
            break;
        }
        saw_bytes = true;
        let take = buffer
            .iter()
            .position(|byte| *byte == b'\n')
            .map(|index| index + 1)
            .unwrap_or(buffer.len());
        let available = MAX_TEXT_LINE_BYTES.saturating_sub(kept.len());
        let copy = take.min(available);
        kept.extend_from_slice(&buffer[..copy]);
        truncated |= copy < take;
        let completed = buffer[take - 1] == b'\n';
        reader.consume(take);
        if completed {
            break;
        }
    }
    if kept.ends_with(b"\n") {
        kept.pop();
    }
    if kept.ends_with(b"\r") {
        kept.pop();
    }
    let text = String::from_utf8_lossy(&kept);
    let scalar_count = text.chars().count();
    let content = text.chars().take(MAX_TEXT_LINE_SCALARS).collect::<String>();
    Ok(Some((
        content,
        truncated || scalar_count > MAX_TEXT_LINE_SCALARS,
    )))
}

pub(crate) fn read_rich_file_in_process(path: &Path, args: &Value) -> Option<ToolResult> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    match ext.as_str() {
        "png" | "jpg" | "jpeg" | "gif" | "webp" => Some(read_image_file(path)),
        "ipynb" => Some(read_notebook_summary(path)),
        "pdf" => Some(read_pdf_summary(path, args)),
        _ => None,
    }
}

fn read_image_file(path: &Path) -> ToolResult {
    let bytes = match read_bounded_bytes(path, MAX_IMAGE_BYTES, "image") {
        Ok(bytes) => bytes,
        Err(error) => return ToolResult::error(error),
    };
    let expected = match path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "jpg" | "jpeg" => ImageKind::Jpeg,
        "gif" => ImageKind::Gif,
        "webp" => ImageKind::Webp,
        _ => ImageKind::Png,
    };
    let Some(actual) = image_kind(&bytes) else {
        return ToolResult::error(format!(
            "'{}' does not have a supported PNG, JPEG, GIF, or WebP signature",
            path.display()
        ));
    };
    if actual != expected {
        return ToolResult::error(format!(
            "'{}' has {} content but a {} extension",
            path.display(),
            actual.mime(),
            expected.extension()
        ));
    }
    let Some((width, height)) = image_dimensions(actual, &bytes) else {
        return ToolResult::error(format!(
            "'{}' has an invalid or unsupported {} header",
            path.display(),
            actual.mime()
        ));
    };
    let pixels = u64::from(width).saturating_mul(u64::from(height));
    if pixels == 0 || pixels > MAX_IMAGE_PIXELS {
        return ToolResult::error(format!(
            "Image '{}' declares {width}x{height} ({pixels} pixels); maximum supported size is {MAX_IMAGE_PIXELS} pixels",
            path.display()
        ));
    }
    let mime = actual.mime();
    let (bytes, width, height) =
        downscale_for_provider(&bytes, mime, width, height).unwrap_or((bytes, width, height));
    let encoded = rightkit_crypto::base64_encode(&bytes);
    ToolResult::ok(format!(
        "Image file: {}\nsize: {} bytes\nmime: {}\ndimensions: {width}x{height}\ndata_url: data:{};base64,{}",
        path.display(),
        bytes.len(),
        mime,
        mime,
        encoded
    ))
}

/// A PNG or JPEG whose longest edge is over the provider limit, scaled to fit it and re-encoded in
/// the same format, with its new dimensions. `None` when the host option `images_downscaled` is
/// off, no scaling is needed, the format is not PNG or JPEG, the declared size is over
/// `MAX_IMAGE_PIXELS` (decoding it could need gigabytes), the image cannot be decoded within the
/// decoder's memory limit, or this build has no decoder (no `host` feature): the caller then sends
/// the original bytes.
pub(crate) fn downscale_for_provider(
    bytes: &[u8],
    mime: &str,
    width: u32,
    height: u32,
) -> Option<(Vec<u8>, u32, u32)> {
    if !crate::tool_options::images_downscaled() {
        return None;
    }
    #[cfg(feature = "host")]
    {
        if width.max(height) <= PROVIDER_IMAGE_MAX_EDGE {
            return None;
        }
        if u64::from(width).saturating_mul(u64::from(height)) > MAX_IMAGE_PIXELS {
            return None;
        }
        let format = match mime {
            "image/png" => image::ImageFormat::Png,
            "image/jpeg" => image::ImageFormat::Jpeg,
            _ => return None,
        };
        let mut reader = image::ImageReader::with_format(std::io::Cursor::new(bytes), format);
        let mut limits = image::Limits::default();
        limits.max_alloc = Some(DOWNSCALE_MAX_ALLOC);
        reader.limits(limits);
        let decoded = reader.decode().ok()?;
        let scaled = decoded.resize(
            PROVIDER_IMAGE_MAX_EDGE,
            PROVIDER_IMAGE_MAX_EDGE,
            image::imageops::FilterType::Triangle,
        );
        let (w, h) = (scaled.width(), scaled.height());
        let mut out = std::io::Cursor::new(Vec::new());
        scaled.write_to(&mut out, format).ok()?;
        Some((out.into_inner(), w, h))
    }
    #[cfg(not(feature = "host"))]
    {
        let _ = (bytes, mime, width, height);
        None
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ImageKind {
    Png,
    Jpeg,
    Gif,
    Webp,
}

impl ImageKind {
    fn mime(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Jpeg => "image/jpeg",
            Self::Gif => "image/gif",
            Self::Webp => "image/webp",
        }
    }

    fn extension(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpeg => "jpg/jpeg",
            Self::Gif => "gif",
            Self::Webp => "webp",
        }
    }
}

fn image_kind(bytes: &[u8]) -> Option<ImageKind> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some(ImageKind::Png)
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        Some(ImageKind::Jpeg)
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some(ImageKind::Gif)
    } else if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        Some(ImageKind::Webp)
    } else {
        None
    }
}

fn image_dimensions(kind: ImageKind, bytes: &[u8]) -> Option<(u32, u32)> {
    match kind {
        ImageKind::Png if bytes.len() >= 24 => Some((
            u32::from_be_bytes(bytes[16..20].try_into().ok()?),
            u32::from_be_bytes(bytes[20..24].try_into().ok()?),
        )),
        ImageKind::Gif if bytes.len() >= 10 => Some((
            u16::from_le_bytes(bytes[6..8].try_into().ok()?) as u32,
            u16::from_le_bytes(bytes[8..10].try_into().ok()?) as u32,
        )),
        ImageKind::Jpeg => jpeg_dimensions(bytes),
        ImageKind::Webp => webp_dimensions(bytes),
        _ => None,
    }
}

fn webp_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    let chunk = bytes.get(12..16)?;
    let payload = bytes.get(20..)?;
    match chunk {
        b"VP8X" if payload.len() >= 10 => {
            let width = 1 + u32::from_le_bytes([payload[4], payload[5], payload[6], 0]);
            let height = 1 + u32::from_le_bytes([payload[7], payload[8], payload[9], 0]);
            Some((width, height))
        }
        b"VP8 " if payload.len() >= 10 && payload[3..6] == [0x9d, 0x01, 0x2a] => Some((
            u16::from_le_bytes(payload[6..8].try_into().ok()?) as u32 & 0x3fff,
            u16::from_le_bytes(payload[8..10].try_into().ok()?) as u32 & 0x3fff,
        )),
        b"VP8L" if payload.len() >= 5 && payload[0] == 0x2f => {
            let bits = u32::from_le_bytes(payload[1..5].try_into().ok()?);
            Some(((bits & 0x3fff) + 1, ((bits >> 14) & 0x3fff) + 1))
        }
        _ => None,
    }
}

fn jpeg_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() < 4 || bytes[0] != 0xff || bytes[1] != 0xd8 {
        return None;
    }
    let mut i = 2usize;
    while i + 8 < bytes.len() {
        if bytes[i] != 0xff {
            i += 1;
            continue;
        }
        let marker = bytes[i + 1];
        i += 2;
        if marker == 0xd9 || marker == 0xda {
            break;
        }
        if i + 2 > bytes.len() {
            break;
        }
        let len = u16::from_be_bytes(bytes[i..i + 2].try_into().ok()?) as usize;
        if len < 2 || i + len > bytes.len() {
            break;
        }
        if matches!(
            marker,
            0xc0 | 0xc1
                | 0xc2
                | 0xc3
                | 0xc5
                | 0xc6
                | 0xc7
                | 0xc9
                | 0xca
                | 0xcb
                | 0xcd
                | 0xce
                | 0xcf
        ) && len >= 7
        {
            let h = u16::from_be_bytes(bytes[i + 3..i + 5].try_into().ok()?) as u32;
            let w = u16::from_be_bytes(bytes[i + 5..i + 7].try_into().ok()?) as u32;
            return Some((w, h));
        }
        i += len;
    }
    None
}

fn read_notebook_summary(path: &Path) -> ToolResult {
    let bytes = match read_bounded_bytes(path, MAX_NOTEBOOK_BYTES, "notebook") {
        Ok(bytes) => bytes,
        Err(error) => return ToolResult::error(error),
    };
    if bytes.iter().find(|byte| !byte.is_ascii_whitespace()) != Some(&b'{') {
        return ToolResult::error("Invalid notebook JSON: expected a top-level object");
    }
    let content = match String::from_utf8(bytes) {
        Ok(content) => content,
        Err(error) => return ToolResult::error(format!("Invalid notebook UTF-8: {error}")),
    };
    let notebook: Value = match serde_json::from_str(&content) {
        Ok(notebook) => notebook,
        Err(e) => return ToolResult::error(format!("Invalid notebook JSON: {e}")),
    };
    let nbformat = notebook
        .get("nbformat")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let minor = notebook
        .get("nbformat_minor")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let cells = match notebook.get("cells").and_then(Value::as_array) {
        Some(cells) => cells,
        None => return ToolResult::error("Invalid notebook JSON: missing cells array"),
    };
    if cells.len() > MAX_NOTEBOOK_CELLS {
        return ToolResult::error(format!(
            "Notebook has {} cells; maximum supported count is {MAX_NOTEBOOK_CELLS}",
            cells.len()
        ));
    }
    let mut output = format!(
        "Notebook: {}\nnbformat: {}.{}\ncells: {}",
        path.display(),
        nbformat,
        minor,
        cells.len()
    );
    for (index, cell) in cells.iter().enumerate().take(20) {
        let kind = cell
            .get("cell_type")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let source = notebook_source_text(cell.get("source"));
        output.push_str(&format!(
            "\n\ncell {index} [{kind}]\n{}",
            truncate_chars(&source, 1200)
        ));
    }
    if cells.len() > 20 {
        output.push_str(&format!(
            "\n\n[notebook has {} cells; showing first 20. Use notebook with cell_id for Claude-compatible edits or path/operation for legacy edits.]",
            cells.len()
        ));
    } else {
        output.push_str("\n\n[Use the notebook tool for cell-aware edits.]");
    }
    ToolResult::ok(output)
}

fn notebook_source_text(source: Option<&Value>) -> String {
    match source {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join(""),
        _ => String::new(),
    }
}
