//! `view_image` tool — loads a local image into a model-visible content block.
//!
//! This is the agent tool half of FEATURE-MATRIX #67 (multimodal/image input).
//! The Tauri transcript renderer detects the `[image-block]` prefix in the
//! summary and renders the image inline instead of as a raw path string.

use crate::coding_tools::context::ToolContext;
use crate::coding_tools::tool_trait::{Tool, ToolResult};
use rightkit_agent_policy::{CapabilityClass, PermissionDecision, PermissionEngine};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// Resolve a file path relative to the workspace root, rejecting escapes and
/// absolute paths outside the workspace.
fn resolve_in_workspace(raw: &str, workspace_root: &Path) -> Result<PathBuf, String> {
    let candidate = PathBuf::from(raw);
    let absolute = if candidate.is_absolute() {
        candidate
    } else {
        workspace_root.join(&candidate)
    };
    // Canonicalize to resolve `..` and symlinks, then verify it's still under the workspace.
    let canonical = absolute
        .canonicalize()
        .map_err(|e| format!("Cannot resolve path '{}': {e}", raw))?;
    let canonical_root = workspace_root
        .canonicalize()
        .unwrap_or_else(|_| workspace_root.to_path_buf());
    // Normalize Windows `\\?\` prefixes for comparison.
    let canon_str = canonical.to_string_lossy();
    let root_str = canonical_root.to_string_lossy();
    let canon_norm = canon_str.strip_prefix(r"\\?\").unwrap_or(&canon_str);
    let root_norm = root_str.strip_prefix(r"\\?\").unwrap_or(&root_str);
    if !canon_norm.starts_with(root_norm) {
        return Err(format!(
            "Path '{}' escapes the workspace root '{}'",
            raw,
            workspace_root.display()
        ));
    }
    Ok(canonical)
}

/// Derive a policy target path (workspace-relative) for permission checks.
fn policy_target_path(absolute: &Path, workspace_root: &Path) -> String {
    absolute
        .strip_prefix(workspace_root)
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| absolute.to_string_lossy().to_string())
}

/// Supported image extensions and their MIME types.
const IMAGE_EXTENSIONS: &[(&str, &str)] = &[
    ("png", "image/png"),
    ("jpg", "image/jpeg"),
    ("jpeg", "image/jpeg"),
    ("gif", "image/gif"),
    ("webp", "image/webp"),
    ("bmp", "image/bmp"),
    ("svg", "image/svg+xml"),
    ("ico", "image/x-icon"),
    ("tiff", "image/tiff"),
    ("tif", "image/tiff"),
];

fn classify_image(ext: &str) -> Option<&'static str> {
    IMAGE_EXTENSIONS
        .iter()
        .find(|(e, _)| *e == ext)
        .map(|(_, mime)| *mime)
}

/// Cap the image size to avoid blowing up the context window (max ~20 MB base64).
const MAX_IMAGE_BYTES: u64 = 20 * 1024 * 1024;

pub struct ViewImageTool;

#[async_trait::async_trait]
impl Tool for ViewImageTool {
    fn name(&self) -> &str {
        "view_image"
    }

    fn is_read_only(&self) -> bool {
        true
    }

    fn description(&self) -> &str {
        "Load a local image file and render it as an inline image in the transcript. \
         Takes a path (relative to workspace or absolute within workspace). \
         Supports PNG, JPEG, GIF, WebP, BMP, SVG, TIFF, ICO. \
         The image must be under the workspace root; read-deny glob rules apply."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["path"],
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the image file (relative to workspace root or absolute within workspace)"
                }
            }
        })
    }

    fn capability_class(&self) -> Option<CapabilityClass> {
        // view_image reads files like read_file — classify as ReadFile so it
        // inherits the same permission gate.
        Some(CapabilityClass::ReadFile)
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolResult {
        let path_str = match args["path"].as_str() {
            Some(p) => p,
            None => return ToolResult::error("Missing required argument: path"),
        };

        let path = match resolve_in_workspace(path_str, &ctx.workspace_root) {
            Ok(p) => p,
            Err(e) => return ToolResult::error(e),
        };

        if let Err(error) = crate::coding_tools::read_file::enforce_path_deny(
            ctx,
            &path,
            crate::coding_tools::read_file::PathAccess::Read,
        ) {
            return ToolResult::error(error);
        }
        let policy_path = policy_target_path(&path, &ctx.workspace_root);

        // Permission check — reuses the same ReadFile gate as read_file.
        let engine = PermissionEngine::new();
        let verdict = engine.evaluate(
            CapabilityClass::ReadFile,
            ctx.permission_mode,
            ctx.workspace_trust,
            Some(&policy_path),
            false,
        );
        if verdict.decision == PermissionDecision::Deny {
            return ToolResult::error(format!(
                "Permission denied: {} (policy: {})",
                verdict.reason, verdict.policy_source
            ));
        }

        // Validate it's a file, not a directory.
        if !path.is_file() {
            if path.is_dir() {
                return ToolResult::error(format!(
                    "'{}' is a directory, not an image file.",
                    path.display()
                ));
            }
            return ToolResult::error(format!("File not found: '{}'", path.display()));
        }

        // Validate extension is a supported image type.
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();

        let mime = match classify_image(&ext) {
            Some(m) => m,
            None => {
                return ToolResult::error(format!(
                    "'{}' does not appear to be a supported image file. \
                     Extension '{}' is not a recognized image format. \
                     Supported: PNG, JPEG, GIF, WebP, BMP, SVG, TIFF, ICO.",
                    path.display(),
                    ext,
                ));
            }
        };

        // Size check before reading.
        let metadata = match std::fs::metadata(&path) {
            Ok(m) => m,
            Err(e) => {
                return ToolResult::error(format!(
                    "Failed to read metadata for '{}': {e}",
                    path.display()
                ))
            }
        };
        if metadata.len() > MAX_IMAGE_BYTES {
            return ToolResult::error(format!(
                "Image '{}' is too large ({} bytes). Maximum size is {MAX_IMAGE_BYTES} bytes (20 MB).",
                path.display(),
                metadata.len(),
            ));
        }

        // Read and encode.
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) => {
                return ToolResult::error(format!(
                    "Failed to read image file '{}': {e}",
                    path.display()
                ))
            }
        };

        // ENG-19: cross-check the extension-declared type against the file's
        // actual magic bytes for the formats we can sniff cheaply. A
        // mismatch (e.g. a renamed/mislabeled binary) is presented as an
        // explicit unsupported-preview error instead of silently decoding
        // and displaying whatever bytes happen to be there.
        if let Some(reason) = magic_bytes_mismatch(&ext, &bytes) {
            return ToolResult::error(format!(
                "'{}' is unsupported for image preview: {reason}. \
                 Use read_file to inspect its raw bytes instead.",
                path.display()
            ));
        }

        let size = bytes.len();
        let encoded = rightkit_crypto::base64_encode(&bytes);
        let dimensions = image_dimensions(&path, &bytes)
            .map(|(w, h)| format!("{w}x{h}"))
            .unwrap_or_else(|| "unknown".to_string());

        // Return a structured image content block. The [image-block] prefix is a
        // convention the Tauri transcript renderer detects to render inline <img>
        // instead of showing the raw data URL.
        ToolResult::ok(format!(
            "[image-block]\npath: {}\nsize: {size} bytes\nmime: {mime}\ndimensions: {dimensions}\ndata_url: data:{mime};base64,{encoded}",
            path.display(),
        ))
    }
}

/// Check the raw bytes' magic signature against what the file's extension
/// declares, for the formats with a cheap, unambiguous signature (PNG,
/// GIF, JPEG). Returns `Some(reason)` on a confirmed mismatch, `None` when
/// the signature matches or the format isn't one this function sniffs
/// (SVG/WebP/BMP/ICO/TIFF pass through unchecked here).
fn magic_bytes_mismatch(ext: &str, bytes: &[u8]) -> Option<String> {
    let looks_like =
        |signature: &[u8]| bytes.len() >= signature.len() && &bytes[..signature.len()] == signature;
    let is_png = looks_like(b"\x89PNG\r\n\x1a\n");
    let is_gif = looks_like(b"GIF87a") || looks_like(b"GIF89a");
    let is_jpeg = bytes.len() >= 2 && bytes[0] == 0xff && bytes[1] == 0xd8;

    match ext {
        "png" if !is_png => {
            Some("extension is .png but the file does not start with a PNG signature".to_string())
        }
        "gif" if !is_gif => {
            Some("extension is .gif but the file does not start with a GIF signature".to_string())
        }
        "jpg" | "jpeg" if !is_jpeg => Some(
            "extension is .jpg/.jpeg but the file does not start with a JPEG signature".to_string(),
        ),
        _ => None,
    }
}

/// Detect image dimensions from the raw bytes (same logic as `read_image_file`).
fn image_dimensions(path: &Path, bytes: &[u8]) -> Option<(u32, u32)> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    match ext.as_str() {
        "png" if bytes.len() >= 24 && bytes.starts_with(b"\x89PNG\r\n\x1a\n") => Some((
            u32::from_be_bytes(bytes[16..20].try_into().ok()?),
            u32::from_be_bytes(bytes[20..24].try_into().ok()?),
        )),
        "gif"
            if bytes.len() >= 10
                && (bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a")) =>
        {
            Some((
                u16::from_le_bytes(bytes[6..8].try_into().ok()?) as u32,
                u16::from_le_bytes(bytes[8..10].try_into().ok()?) as u32,
            ))
        }
        "jpg" | "jpeg" => jpeg_dimensions(bytes),
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
