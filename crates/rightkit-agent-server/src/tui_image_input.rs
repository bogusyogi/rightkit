//! Image input for the TUI composer and `--image` (issue #227): `@path`
//! mentions, clipboard capture for paste, and byte-level type/size validation.
//!
//! Images reach the provider through the existing attachment channel
//! (`api::Attachment`, data-URL parts in the user turn); the provider layer
//! maps them to Anthropic base64 image blocks or OpenAI `image_url` /
//! `input_image` parts.

use std::path::{Path, PathBuf};

/// Hard ceiling for one attached image.
pub const MAX_IMAGE_BYTES: u64 = 20 * 1024 * 1024;

/// Sniff the MIME type from file magic. Extensions are never trusted: a
/// renamed non-image must not reach the provider as an image.
pub fn sniff_image_mime(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        Some("image/png")
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

/// Validate image bytes: non-empty, within the size cap, recognized type.
pub fn validate_image_bytes(bytes: &[u8]) -> Result<&'static str, String> {
    if bytes.is_empty() {
        return Err("image is empty".into());
    }
    if bytes.len() as u64 > MAX_IMAGE_BYTES {
        return Err("image exceeds 20 MiB".into());
    }
    sniff_image_mime(bytes).ok_or_else(|| "unsupported image type (png, jpeg, gif, webp)".into())
}

fn has_image_extension(path: &Path) -> bool {
    path.extension()
        .and_then(std::ffi::OsStr::to_str)
        .map(str::to_ascii_lowercase)
        .is_some_and(|ext| matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "gif" | "webp"))
}

/// Existing image files a prompt names: `@path` / `@"path with spaces"`
/// mentions (absolute, or relative to `workspace`) and bare absolute paths
/// (a path pasted or dragged into the composer). Non-image and missing
/// tokens stay plain text. Order follows first appearance, de-duplicated.
pub fn image_paths_in_prompt(prompt: &str, workspace: Option<&Path>) -> Vec<String> {
    let mut candidates: Vec<(String, bool)> = Vec::new();
    let mut rest = prompt;
    while let Some(at) = rest.find('@') {
        let before_ok = at == 0
            || rest[..at]
                .chars()
                .next_back()
                .is_some_and(char::is_whitespace);
        let after = &rest[at + 1..];
        let (token, consumed) = if let Some(quoted) = after.strip_prefix('"') {
            match quoted.find('"') {
                Some(end) => (quoted[..end].to_string(), end + 2),
                None => (String::new(), 0),
            }
        } else {
            let end = after.find(char::is_whitespace).unwrap_or(after.len());
            (after[..end].to_string(), end)
        };
        if before_ok && !token.is_empty() {
            candidates.push((token, true));
        }
        rest = &after[consumed.min(after.len())..];
    }
    for raw in prompt.split_whitespace() {
        candidates.push((
            raw.trim_matches(|c: char| c == '"' || c == '\'')
                .to_string(),
            false,
        ));
    }
    candidates.push((
        prompt
            .trim()
            .trim_matches(|c: char| c == '"' || c == '\'')
            .to_string(),
        false,
    ));

    let mut found: Vec<String> = Vec::new();
    for (token, mention) in candidates {
        let path = Path::new(&token);
        let resolved: PathBuf = if path.is_absolute() {
            path.to_path_buf()
        } else if mention {
            match workspace {
                Some(root) => root.join(path),
                None => continue,
            }
        } else {
            continue;
        };
        if has_image_extension(&resolved) && resolved.is_file() {
            let text = resolved.to_string_lossy().into_owned();
            if !found.contains(&text) {
                found.push(text);
            }
        }
    }
    found
}

/// The first-message prompt for an interactive launch with `--image` files:
/// the prompt text followed by one `@path` mention per image (made absolute
/// so the submit path resolves them regardless of workspace).
pub fn prompt_with_images(prompt: Option<&str>, images: &[String]) -> String {
    let mut text = prompt.unwrap_or_default().trim_end().to_string();
    for image in images {
        let absolute = std::path::absolute(image).unwrap_or_else(|_| PathBuf::from(image));
        if !text.is_empty() {
            text.push(' ');
        }
        text.push_str(mention_for(&absolute).trim_end());
    }
    text
}

/// A Ctrl+V key press or a bracketed paste carrying no text: the terminal
/// has nothing to paste, so the clipboard may hold an image.
pub fn is_image_paste_gesture(event: &rightkit_terminal_ui::component::Event) -> bool {
    use rightkit_terminal_ui::component::Event;
    use rightkit_terminal_ui::crossterm::event::{KeyCode, KeyEventKind, KeyModifiers};
    match event {
        Event::Paste(text) => text.trim().is_empty(),
        Event::Key(key) => {
            key.kind == KeyEventKind::Press
                && key.code == KeyCode::Char('v')
                && key.modifiers.contains(KeyModifiers::CONTROL)
                && !key.modifiers.contains(KeyModifiers::ALT)
        }
        _ => false,
    }
}

/// The composer text that attaches `path` (`@path`, quoted when it has spaces).
pub fn mention_for(path: &Path) -> String {
    let text = path.to_string_lossy();
    if text.chars().any(char::is_whitespace) {
        format!("@\"{text}\" ")
    } else {
        format!("@{text} ")
    }
}

/// Save the clipboard image (if any) to a PNG under the temp dir and return
/// its path. `None` when the clipboard holds no image or the platform helper
/// is unavailable.
pub fn capture_clipboard_image() -> Option<PathBuf> {
    let dir = std::env::temp_dir().join("coderight-clipboard");
    std::fs::create_dir_all(&dir).ok()?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    let path = dir.join(format!("paste-{stamp}.png"));
    let saved = write_clipboard_png(&path);
    let valid = saved
        && std::fs::read(&path)
            .ok()
            .is_some_and(|bytes| validate_image_bytes(&bytes).is_ok());
    if valid {
        Some(path)
    } else {
        let _ = std::fs::remove_file(&path);
        None
    }
}

/// Runs a clipboard helper through the shared RightKit runner (hidden console, null stdin, output
/// drained and discarded) with no deadline, as the previous direct `status()` call had.
#[cfg(any(windows, target_os = "macos"))]
fn run_quiet(program: &str, args: &[&str]) -> bool {
    let mut owned = rightkit_child_process::OwnedCommand::new(program);
    owned.command_mut().args(args);
    let options = rightkit_child_process::StreamOptions::new()
        .stdout_tail_cap(0)
        .stderr_tail_cap(0);
    rightkit_child_process::run_streaming(owned, &options, &|| false, &mut |_| {}, &mut |_| {})
        .is_ok_and(|output| output.status.success())
}

#[cfg(windows)]
fn write_clipboard_png(path: &Path) -> bool {
    let target = path.to_string_lossy().replace('\'', "''");
    let script = format!(
        "Add-Type -AssemblyName System.Windows.Forms,System.Drawing; \
         $i=[System.Windows.Forms.Clipboard]::GetImage(); if($null -eq $i){{exit 3}}; \
         $i.Save('{target}',[System.Drawing.Imaging.ImageFormat]::Png)"
    );
    run_quiet(
        "powershell",
        &["-NoProfile", "-NonInteractive", "-STA", "-Command", &script],
    )
}

#[cfg(target_os = "macos")]
fn write_clipboard_png(path: &Path) -> bool {
    let target = path
        .to_string_lossy()
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    run_quiet(
        "osascript",
        &[
            "-e",
            &format!("set f to open for access POSIX file \"{target}\" with write permission"),
            "-e",
            "write (the clipboard as «class PNGf») to f",
            "-e",
            "close access f",
        ],
    )
}

#[cfg(not(any(windows, target_os = "macos")))]
fn write_clipboard_png(path: &Path) -> bool {
    let attempts: [(&str, &[&str]); 2] = [
        ("wl-paste", &["--type", "image/png"]),
        (
            "xclip",
            &["-selection", "clipboard", "-t", "image/png", "-o"],
        ),
    ];
    for (program, args) in attempts {
        let mut owned = rightkit_child_process::OwnedCommand::new(program);
        owned.command_mut().args(args);
        let options = rightkit_child_process::StreamOptions::new()
            .stdout_tail_cap(0)
            .stderr_tail_cap(0);
        let mut stdout = Vec::new();
        let Ok(output) = rightkit_child_process::run_streaming(
            owned,
            &options,
            &|| false,
            &mut |chunk: &[u8]| stdout.extend_from_slice(chunk),
            &mut |_| {},
        ) else {
            continue;
        };
        if output.status.success() && !stdout.is_empty() && std::fs::write(path, &stdout).is_ok() {
            return true;
        }
    }
    false
}
