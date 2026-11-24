//! Preflight for offline file transcription and the guarded save of a transcript export.
//!
//! The host forwards a user-picked path; the sidecar owns media decode and ASR. Before the path
//! reaches the sidecar this module checks, in this order: the path is a file, its extension is on
//! the host's allowlist, the file is not empty, and THIS process can read it (so the OS raises its
//! own file-access prompt for the app; a child process's denied read never does). The export side
//! accepts only an absolute, `..`-free path with an allowed extension whose parent exists, then
//! writes atomically.
//!
//! The host owns the extension lists, the app name shown in permission text and the command
//! wrappers (`spawn_blocking`, the runtime call); nothing here names a product.

use std::path::{Component, Path, PathBuf};

/// Media extensions the bundled decoder reliably handles for ASR. A neutral default for the
/// `allowed_extensions` a host passes to [`preflight_media_file`].
pub const DEFAULT_MEDIA_EXTENSIONS: &[&str] = &[
    "mp3", "wav", "m4a", "flac", "ogg", "opus", "aac", "mp4", "m4b", "mov", "mkv", "webm",
];

/// Transcript and subtitle export extensions: a neutral default for [`write_text_export`].
pub const DEFAULT_EXPORT_EXTENSIONS: &[&str] = &["txt", "srt", "vtt"];

/// The macOS Settings pane URL that holds the grant a permission error names: Removable Volumes
/// when `removable`, otherwise Full Disk Access. The host opens it (macOS only).
pub fn files_settings_url(removable: bool) -> String {
    let pane = if removable {
        "Privacy_RemovableVolumes"
    } else {
        "Privacy_AllFiles"
    };
    format!("x-apple.systempreferences:com.apple.preference.security?{pane}")
}

/// Open and read one byte of `input` in THIS process. A denial becomes the typed
/// `E_PERMISSION: ...` message naming `app_name` (the name shown in the OS privacy settings) and,
/// on macOS, the pane to grant; any other failure is `could not read <path>: <error>`.
pub fn host_read_check(input: &Path, app_name: &str) -> Result<(), String> {
    use std::io::Read;
    match std::fs::File::open(input).and_then(|mut file| file.read(&mut [0u8; 1])) {
        Ok(_) => Ok(()),
        Err(e)
            if e.kind() == std::io::ErrorKind::PermissionDenied || e.raw_os_error() == Some(1) =>
        {
            #[cfg(target_os = "macos")]
            {
                let grant = if input.starts_with("/Volumes") {
                    format!("System Settings \u{2192} Privacy & Security \u{2192} Files & Folders \u{2192} {app_name} \u{2192} Removable Volumes (or Full Disk Access)")
                } else {
                    format!("System Settings \u{2192} Privacy & Security \u{2192} Full Disk Access \u{2192} {app_name}")
                };
                Err(format!(
                    "E_PERMISSION: {app_name} is not allowed to read {}. Grant {grant}, then try again.",
                    input.display()
                ))
            }
            #[cfg(not(target_os = "macos"))]
            {
                let _ = app_name;
                Err("E_PERMISSION: Windows denied access to this file. Check its permissions or copy it somewhere you can open, then try again.".to_string())
            }
        }
        Err(e) => Err(format!("could not read {}: {e}", input.display())),
    }
}

/// Check a picked media path before it is handed to the sidecar. Returns the path to transcribe.
/// Order: is a file, extension in `allowed_extensions` (case-insensitive), not empty, readable by
/// this process ([`host_read_check`] with `app_name`). The host runs this and the runtime call on
/// a blocking thread; a transcription can run for minutes.
pub fn preflight_media_file(
    path: &str,
    allowed_extensions: &[&str],
    app_name: &str,
) -> Result<PathBuf, String> {
    let input = PathBuf::from(path);
    if !input.is_file() {
        return Err(format!("file not found: {}", input.display()));
    }
    let ext_ok = input
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| {
            let lower = e.to_lowercase();
            allowed_extensions.contains(&lower.as_str())
        })
        .unwrap_or(false);
    if !ext_ok {
        return Err(format!(
            "unsupported media extension '{}'; allowed: {}",
            input
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("<none>"),
            allowed_extensions.join(", ")
        ));
    }
    let metadata = std::fs::metadata(&input).map_err(|e| format!("stat: {e}"))?;
    if metadata.len() == 0 {
        return Err(format!("file is empty: {}", input.display()));
    }
    host_read_check(&input, app_name)?;
    Ok(input)
}

fn join_or(extensions: &[&str]) -> String {
    let dotted: Vec<String> = extensions.iter().map(|e| format!(".{e}")).collect();
    match dotted.as_slice() {
        [] => String::new(),
        [one] => one.clone(),
        [init @ .., last] => format!("{} or {last}", init.join(", ")),
    }
}

/// Validate a save path for a text export. Order: extension in `allowed_extensions`
/// (case-insensitive), absolute, no `..` component, has a parent that exists. It never creates a
/// directory. Returns the path to write.
pub fn validate_export_path(path: &str, allowed_extensions: &[&str]) -> Result<PathBuf, String> {
    let p = PathBuf::from(path);
    let ok_ext = p
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| allowed_extensions.contains(&e.to_lowercase().as_str()))
        .unwrap_or(false);
    if !ok_ext {
        return Err(format!(
            "only {} files can be saved",
            join_or(allowed_extensions)
        ));
    }
    if !p.is_absolute() {
        return Err("save path must be absolute (use the OS save dialog)".into());
    }
    if p.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err("save path must not contain '..' components".into());
    }
    let parent = p
        .parent()
        .ok_or_else(|| "save path has no parent directory".to_string())?;
    if !parent.is_dir() {
        return Err(format!(
            "save path parent does not exist: {}",
            parent.display()
        ));
    }
    Ok(p)
}

/// [`validate_export_path`], then an atomic write of `contents`.
pub fn write_text_export(
    path: &str,
    contents: &str,
    allowed_extensions: &[&str],
) -> Result<(), String> {
    let p = validate_export_path(path, allowed_extensions)?;
    rightkit_fs::atomic::atomic_write(&p, contents.as_bytes()).map_err(|e| e.to_string())
}
