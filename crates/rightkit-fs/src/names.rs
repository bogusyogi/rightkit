//! File names that come from somewhere else: sanitising a download name and picking a free one.
//!
//! A name taken from a URL, a `Content-Disposition` header or a mail attachment is attacker
//! controlled text. [`sanitize_file_name`] turns it into one safe path component that is valid on
//! Windows, macOS and Linux alike (the rules are the strictest of the three, so a file saved on one
//! platform can be copied to another): no directory part, no control or bidirectional-override
//! characters, no reserved Windows device names, no trailing dot or space, no leading dot, bounded
//! length with the extension kept. It never returns an empty name.
//!
//! [`numbered_candidates`] lists `name.ext`, `name (1).ext`, `name (2).ext`, ...; with feature `fs`,
//! [`create_unique_file`] creates the first free one exclusively (`create_new`, so two processes
//! never get the same file) and [`create_download_file`] sanitises first.
//!
//! The pure functions here need no feature and behave the same on every platform, so a name
//! sanitised on macOS is identical to the one Windows would produce.

/// How [`sanitize_file_name_with`] shapes a name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NameOptions {
    /// Longest result in UTF-8 bytes, clamped to `8..=240`. The default, 200, leaves room for a
    /// ` (9999)` collision suffix inside the 255-byte component limit of the common filesystems.
    pub max_bytes: usize,
    /// Returned when nothing usable is left. Must itself be a safe name; it is sanitised too, and
    /// `"download"` stands in if it sanitises to nothing.
    pub fallback: &'static str,
}

impl Default for NameOptions {
    fn default() -> Self {
        Self {
            max_bytes: 200,
            fallback: "download",
        }
    }
}

/// True for a name Windows treats as a device, whatever its extension or letter case: `CON`, `PRN`,
/// `AUX`, `NUL`, `COM1`-`COM9`, `LPT1`-`LPT9` (also with the superscript digits `¹ ² ³` that
/// Windows maps to the same devices), `CONIN$` and `CONOUT$`. The part before the first `.` decides:
/// `nul.txt` and `NUL.tar.gz` are reserved, and so is `con ` (trailing spaces are dropped).
pub fn is_windows_reserved_name(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or(name).trim_end_matches(' ');
    let upper = stem.to_uppercase();
    matches!(
        upper.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) || device_with_number(&upper, "COM")
        || device_with_number(&upper, "LPT")
}

fn device_with_number(upper: &str, prefix: &str) -> bool {
    let Some(rest) = upper.strip_prefix(prefix) else {
        return false;
    };
    let mut chars = rest.chars();
    matches!(
        (chars.next(), chars.next()),
        (Some('1'..='9' | '\u{b9}' | '\u{b2}' | '\u{b3}'), None)
    )
}

/// True when `name` cannot be used as one path component on Windows: empty, `.` or `..`, a reserved
/// device name ([`is_windows_reserved_name`]), a trailing dot or space (Windows drops it, so the name
/// would address a different entry), or any of `\ / : * ? " < > |` or a control character (a `:`
/// names an NTFS alternate data stream). Used by the containment checks for names on Windows.
pub fn is_unsafe_windows_component(name: &str) -> bool {
    name.is_empty()
        || name == "."
        || name == ".."
        || name.ends_with(['.', ' '])
        || name.chars().any(|c| {
            c < ' '
                || c == '\u{7f}'
                || matches!(c, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|')
        })
        || is_windows_reserved_name(name)
}

/// Characters removed outright: C0 and C1 controls, the Unicode line and paragraph separators,
/// zero-width and invisible formatting characters, and every bidirectional override, embedding and
/// isolate (the classic `invoice\u{202e}fdp.exe` spoof).
fn is_stripped(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{ad}'
                | '\u{61c}'
                | '\u{180e}'
                | '\u{200b}'..='\u{200f}'
                | '\u{2028}'..='\u{202e}'
                | '\u{2060}'..='\u{2064}'
                | '\u{2066}'..='\u{206f}'
                | '\u{feff}'
                | '\u{fff9}'..='\u{fffb}'
        )
}

/// Characters that cannot appear in a Windows file name, plus the path separators. Replaced by `_`.
fn is_replaced(c: char) -> bool {
    matches!(c, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|')
}

/// [`sanitize_file_name_with`] with [`NameOptions::default`].
pub fn sanitize_file_name(raw: &str) -> String {
    sanitize_file_name_with(raw, &NameOptions::default())
}

/// Turn `raw` into one safe, portable file name.
///
/// In order: only the last non-empty segment between `/` and `\` is kept (a name that is only
/// separators has none); control, invisible and bidirectional-control characters are removed; `: * ? " < > |` become
/// `_`; leading dots and whitespace are dropped (no hidden file, no `.` or `..`); trailing dots and
/// whitespace are dropped (Windows would drop them silently); a reserved Windows device name
/// ([`is_windows_reserved_name`]) gets a `_` prefix; the result is cut to `options.max_bytes` at a
/// character boundary, keeping the extension (up to 24 bytes of it) when there is one; and an empty
/// result is the fallback name. The result is never empty, never `.` or `..`, and contains no
/// separator, so joining it to a directory cannot leave that directory.
pub fn sanitize_file_name_with(raw: &str, options: &NameOptions) -> String {
    let max = options.max_bytes.clamp(8, 240);
    match shape(raw, max) {
        Some(name) => name,
        None => shape(options.fallback, max).unwrap_or_else(|| "download".to_string()),
    }
}

fn shape(raw: &str, max: usize) -> Option<String> {
    let last = raw
        .rsplit(['/', '\\'])
        .find(|s| !s.is_empty())
        .unwrap_or("");
    let cleaned: String = last
        .chars()
        .filter(|c| !is_stripped(*c))
        .map(|c| if is_replaced(c) { '_' } else { c })
        .collect();
    let trimmed = cleaned
        .trim_start_matches(|c: char| c == '.' || c.is_whitespace())
        .trim_end_matches(|c: char| c == '.' || c.is_whitespace());
    if trimmed.is_empty() {
        return None;
    }
    let settle = |name: String| -> String {
        // A cut can expose a trailing dot or space.
        name.trim_end_matches(|c: char| c == '.' || c.is_whitespace())
            .to_string()
    };
    let mut name = settle(truncate_keeping_extension(trimmed, max));
    if name.is_empty() {
        return None;
    }
    // After the cut, so a cut that leaves exactly `COM1.txt` is still caught. The prefix makes the
    // stem start with `_`, which no reserved name does, and a second cut keeps that prefix.
    if is_windows_reserved_name(&name) {
        name.insert(0, '_');
        name = settle(truncate_keeping_extension(&name, max));
    }
    Some(name)
}

/// Cut `name` to at most `max` bytes at a character boundary. When it has an extension of at most 24
/// bytes (the part after the last `.`, with the dot), the stem is shortened and the extension kept.
fn truncate_keeping_extension(name: &str, max: usize) -> String {
    if name.len() <= max {
        return name.to_string();
    }
    // An extension is kept only when it is short and leaves at least half the budget to the stem.
    let split = name
        .rfind('.')
        .filter(|i| *i > 0 && name.len() - *i <= 24usize.min(max / 2));
    let (stem, ext) = match split {
        Some(i) => name.split_at(i),
        None => (name, ""),
    };
    let budget = max - ext.len();
    let mut end = stem.len().min(budget);
    while !stem.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{}", &stem[..end], ext)
}

/// `name`, then `stem (1).ext`, `stem (2).ext`, ... up to `count` names in all (the first is
/// `name` itself). The number goes before the last extension; a name with no extension, or a
/// leading-dot name, takes it at the end. `name` should already be safe ([`sanitize_file_name`]).
/// A `count` of zero gives an empty list.
pub fn numbered_candidates(name: &str, count: usize) -> Vec<String> {
    let split = name.rfind('.').filter(|i| *i > 0);
    let (stem, ext) = match split {
        Some(i) => name.split_at(i),
        None => (name, ""),
    };
    (0..count)
        .map(|n| {
            if n == 0 {
                name.to_string()
            } else {
                format!("{stem} ({n}){ext}")
            }
        })
        .collect()
}

/// The most names [`create_unique_file`] tries before giving up with `AlreadyExists`.
#[cfg(feature = "fs")]
pub const MAX_UNIQUE_TRIES: usize = 10_000;

/// Exclusively create the first free name of [`numbered_candidates`]`(name, MAX_UNIQUE_TRIES)` in
/// `dir` and return its path and the open file (write-only).
///
/// `create_new` is the atomic claim, so two processes or threads asking for the same name get
/// different files, and a name that exists as a file, directory or symlink (a link is never
/// followed) is skipped. The file is created with the platform's default permissions (for a download
/// the user will open; use [`crate::create_private_new`] for a secret). `name` must be a single safe
/// component: anything with a separator, `..`, NUL or an unsafe Windows name is refused as
/// [`crate::FsError::BadName`], so a caller that skipped [`sanitize_file_name`] cannot write outside
/// `dir`. Every other error ends the search. When every candidate exists the result is
/// [`crate::FsError::AlreadyExists`].
#[cfg(feature = "fs")]
pub fn create_unique_file(
    dir: &std::path::Path,
    name: &str,
) -> crate::error::Result<(std::path::PathBuf, std::fs::File)> {
    use crate::error::FsError;
    if name.contains('\0') || name.contains(['/', '\\']) || is_unsafe_windows_component(name) {
        return Err(FsError::BadName(name.to_string()));
    }
    crate::atomic::reject_reserved_name(name)?;
    let mut last = dir.join(name);
    for candidate in numbered_candidates(name, MAX_UNIQUE_TRIES) {
        let path = dir.join(&candidate);
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(file) => return Ok((path, file)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => last = path,
            // Windows refuses `create_new` on an existing directory with access denied, not
            // "already exists". A name that is taken by anything (a directory, a link) is skipped.
            Err(_) if std::fs::symlink_metadata(&path).is_ok() => last = path,
            Err(e) => return Err(FsError::io(&path, e)),
        }
    }
    Err(FsError::AlreadyExists(last))
}

/// [`sanitize_file_name`] then [`create_unique_file`]: the one call for "save this download under
/// the name it asked for, or the next free one".
#[cfg(feature = "fs")]
pub fn create_download_file(
    dir: &std::path::Path,
    raw_name: &str,
) -> crate::error::Result<(std::path::PathBuf, std::fs::File)> {
    create_unique_file(dir, &sanitize_file_name(raw_name))
}
