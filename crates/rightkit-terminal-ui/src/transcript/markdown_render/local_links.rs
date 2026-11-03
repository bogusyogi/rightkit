//! Local file-link parsing, label comparison, and display for Markdown transcripts.
//!
//! Markdown rendering intentionally treats local file links differently from normal web links. For
//! local paths, transcripts always show the real file target (including normalized location
//! suffixes) and can shorten absolute paths relative to a known working directory. Descriptive
//! Markdown labels remain visible alongside that target, while path-like labels collapse to the
//! canonical target to avoid duplicate file references.
//!

// Adapted seams: `regex` (already a crate dep) stands in for `regex_lite`;
// `urlencoding::decode` and the two donor util-crate helpers are the small
// local functions at the bottom of this file (same donor revision,
// behavior preserved for the path shapes Codex emits).
use regex::Regex;
use std::path::Path;
use std::sync::LazyLock;
use url::Url;

static COLON_LOCATION_SUFFIX_RE: LazyLock<Regex> =
    LazyLock::new(
        || match Regex::new(r":\d+(?::\d+)?(?:[-–]\d+(?::\d+)?)?$") {
            Ok(regex) => regex,
            Err(error) => panic!("invalid location suffix regex: {error}"),
        },
    );

// Covered by load_location_suffix_regexes.
static HASH_LOCATION_SUFFIX_RE: LazyLock<Regex> =
    LazyLock::new(|| match Regex::new(r"^L\d+(?:C\d+)?(?:-L\d+(?:C\d+)?)?$") {
        Ok(regex) => regex,
        Err(error) => panic!("invalid hash location regex: {error}"),
    });

pub(super) fn is_local_path_like_link(dest_url: &str) -> bool {
    dest_url.starts_with("file://")
        || dest_url.starts_with('/')
        || dest_url.starts_with("~/")
        || dest_url.starts_with("./")
        || dest_url.starts_with("../")
        || dest_url.starts_with("\\\\")
        || matches!(
            dest_url.as_bytes(),
            [drive, b':', separator, ..]
                if drive.is_ascii_alphabetic() && matches!(separator, b'/' | b'\\')
        )
}

/// Decide whether a local-file link label adds meaning beyond its canonical target.
///
/// Matching path-like labels collapse to the target; prose labels remain visible.
pub(super) fn should_render_local_link_label(label: &str, destination: &str) -> bool {
    let label = label.trim();
    if label.is_empty() {
        return false;
    }
    let Some(parsed_label) = comparable_local_link_path(label) else {
        return true;
    };
    let Some(target) = comparable_local_link_path(destination) else {
        return true;
    };
    let target_path = trim_trailing_local_path_separator(target.trim_start_matches("./"));
    let has_boundary_suffix = |path: &str, suffix: &str| {
        !suffix.is_empty()
            && path
                .strip_suffix(suffix)
                .is_some_and(|prefix| prefix.is_empty() || prefix.ends_with('/'))
    };
    // Labels can spell a filename literally or URL-encode it. Compare both without decoding
    // the destination twice (for example, percent%2520.rs denotes percent%20.rs).
    let literal_label = normalize_local_link_path_text(label).to_lowercase();
    ![literal_label, parsed_label].iter().any(|label| {
        let label_path = trim_trailing_local_path_separator(label.trim_start_matches("./"));
        has_boundary_suffix(target_path, label_path)
            || (is_absolute_local_link_path(label_path)
                && has_boundary_suffix(label_path, target_path))
    })
}

/// Normalize original Markdown strings for comparison only, never already-rendered path text.
/// Case and URL spelling are intentionally forgiving; the visible target retains its spelling.
fn comparable_local_link_path(text: &str) -> Option<String> {
    let text = if text
        .get(..7)
        .is_some_and(|scheme| scheme.eq_ignore_ascii_case("file://"))
    {
        std::borrow::Cow::Owned(format!("file://{}", &text[7..]))
    } else {
        std::borrow::Cow::Borrowed(text)
    };
    let (path, _) = parse_local_link_target(&text)?;
    Some(normalize_local_link_path_text(&path).to_lowercase())
}

/// Parse a local link target into normalized path text plus an optional location suffix.
///
/// This accepts the path shapes Codex emits today: `file://` URLs, absolute and relative paths,
/// `~/...`, Windows paths, and `#L..C..` or `:line:col` suffixes.
pub(super) fn render_local_link_target(dest_url: &str, cwd: Option<&Path>) -> Option<String> {
    let (path_text, location_suffix) = parse_local_link_target(dest_url)?;
    let mut rendered = display_local_link_path(&path_text, cwd);
    if let Some(location_suffix) = location_suffix {
        rendered.push_str(&location_suffix);
    }
    Some(rendered)
}

/// Split a local-link destination into `(normalized_path_text, location_suffix)`.
///
/// The returned path text never includes a trailing `#L..` or `:line[:col]` suffix. Path
/// normalization preserves `~/...` and rewrites path separators into display-stable forward
/// slashes. The suffix, when present, is returned separately in normalized markdown form.
///
/// Returns `None` only when the destination looks like a `file://` URL but cannot be parsed into a
/// local path. Plain path-like inputs always return `Some(...)` even if they are relative.
fn parse_local_link_target(dest_url: &str) -> Option<(String, Option<String>)> {
    if dest_url.starts_with("file://") {
        let url = Url::parse(dest_url).ok()?;
        // Infer the path's OS from the URI, not the host running the TUI.
        let path_text = normalize_local_link_path_text(&file_url_native_path(&url)?);
        let location_suffix = url
            .fragment()
            .and_then(normalize_hash_location_suffix_fragment);
        return Some((path_text, location_suffix));
    }

    let mut path_text = dest_url;
    let mut location_suffix = None;
    // Prefer `#L..` style fragments when both forms are present so URLs like `path#L10` do not
    // get misparsed as a plain path ending in `:10`.
    if let Some((candidate_path, fragment)) = dest_url.rsplit_once('#')
        && let Some(normalized) = normalize_hash_location_suffix_fragment(fragment)
    {
        path_text = candidate_path;
        location_suffix = Some(normalized);
    }
    if location_suffix.is_none()
        && let Some(suffix) = extract_colon_location_suffix(path_text)
    {
        let path_len = path_text.len().saturating_sub(suffix.len());
        path_text = &path_text[..path_len];
        location_suffix = Some(suffix);
    }

    let decoded_path_text =
        percent_decode(path_text).unwrap_or(std::borrow::Cow::Borrowed(path_text));
    Some((
        normalize_local_link_path_text(&decoded_path_text),
        location_suffix,
    ))
}

/// Normalize a hash fragment like `L12` or `L12C3-L14C9` into the display suffix we render.
///
/// Returns `None` for fragments that are not location references. This deliberately ignores other
/// `#...` fragments so non-location hashes stay part of the path text.
fn normalize_hash_location_suffix_fragment(fragment: &str) -> Option<String> {
    HASH_LOCATION_SUFFIX_RE
        .is_match(fragment)
        .then(|| format!("#{fragment}"))
        .and_then(|suffix| normalize_markdown_hash_location_suffix(&suffix))
}

/// Extract a trailing `:line`, `:line:col`, or range suffix from a plain path-like string.
///
/// The suffix must occur at the end of the input; embedded colons elsewhere in the path are left
/// alone. This is what keeps Windows drive letters like `C:/...` from being misread as locations.
pub(super) fn extract_colon_location_suffix(path_text: &str) -> Option<String> {
    COLON_LOCATION_SUFFIX_RE
        .find(path_text)
        .filter(|matched| matched.end() == path_text.len())
        .map(|matched| matched.as_str().to_string())
}

/// Normalize local-path text into the transcript display form.
///
/// Display normalization is intentionally lexical: it does not touch the filesystem, resolve
/// symlinks, or collapse `.` / `..`. It only converts separators to forward slashes and rewrites
/// UNC-style `\\\\server\\share` inputs into `//server/share` so later prefix checks operate on a
/// stable representation.
fn normalize_local_link_path_text(path_text: &str) -> String {
    // Render all local link paths with forward slashes so display and prefix stripping are stable
    // across mixed Windows and Unix-style inputs.
    if let Some(rest) = path_text.strip_prefix("\\\\") {
        format!("//{}", rest.replace('\\', "/").trim_start_matches('/'))
    } else {
        path_text.replace('\\', "/")
    }
}

fn is_absolute_local_link_path(path_text: &str) -> bool {
    path_text.starts_with('/')
        || path_text.starts_with("//")
        || matches!(
            path_text.as_bytes(),
            [drive, b':', b'/', ..] if drive.is_ascii_alphabetic()
        )
}

/// Remove trailing separators from a local path without destroying root semantics.
///
/// Roots like `/`, `//`, and `C:/` stay intact so callers can still distinguish "the root itself"
/// from "a path under the root".
fn trim_trailing_local_path_separator(path_text: &str) -> &str {
    if path_text == "/" || path_text == "//" {
        return path_text;
    }
    if matches!(path_text.as_bytes(), [drive, b':', b'/'] if drive.is_ascii_alphabetic()) {
        return path_text;
    }
    path_text.trim_end_matches('/')
}

/// Strip `cwd_text` from the start of `path_text` when `path_text` is strictly underneath it.
///
/// Returns the relative remainder without a leading slash. If the path equals the cwd exactly, this
/// returns `None` so callers can keep rendering the full path instead of collapsing it to an empty
/// string.
fn strip_local_path_prefix<'a>(path_text: &'a str, cwd_text: &str) -> Option<&'a str> {
    let path_text = trim_trailing_local_path_separator(path_text);
    let cwd_text = trim_trailing_local_path_separator(cwd_text);
    if path_text == cwd_text {
        return None;
    }

    // Treat filesystem roots specially so `/tmp/x` under `/` becomes `tmp/x` instead of being
    // left unchanged by the generic prefix-stripping branch.
    if cwd_text == "/" || cwd_text == "//" {
        return path_text.strip_prefix('/');
    }

    path_text
        .strip_prefix(cwd_text)
        .and_then(|rest| rest.strip_prefix('/'))
}

/// Choose the visible path text for a local link after normalization.
///
/// Relative paths (including `~/...`) stay relative. Absolute paths prefer cwd-relative display
/// and otherwise stay absolute: the frontend home may differ from the execution host's home.
/// This is display logic only, not filesystem canonicalization.
fn display_local_link_path(path_text: &str, cwd: Option<&Path>) -> String {
    let path_text = normalize_local_link_path_text(path_text);
    if !is_absolute_local_link_path(&path_text) {
        return path_text;
    }

    if let Some(cwd) = cwd {
        // Only the session cwd is known to refer to the execution host.
        let cwd_text = normalize_local_link_path_text(&cwd.to_string_lossy());
        if let Some(stripped) = strip_local_path_prefix(&path_text, &cwd_text) {
            return stripped.to_string();
        }
    }

    path_text
}

// ── adapted seams (donor util crates, same revision) ─────────────────────

/// Donor `codex_utils_path_uri::PathUri::parse(...).inferred_native_path_string()`
/// reduced to what this module needs: turn a `file://` URL's authority+path
/// into display path text, inferring the path's OS from the URI shape rather
/// than the host.
/// - `file://server/share/x` → `//server/share/x` (UNC)
/// - `file:///C:/x` or `file:///C|/x` → `C:/x` (Windows drive)
/// - `file:///home/x` → `/home/x` (POSIX)
fn file_url_native_path(url: &Url) -> Option<String> {
    if url.scheme() != "file" {
        return None;
    }
    let decoded = percent_decode(url.path())?.into_owned();
    if let Some(host) = url.host_str()
        && !host.is_empty()
    {
        return Some(format!("//{host}{decoded}"));
    }
    match decoded.as_bytes() {
        // `/C:/x` → `C:/x` (Windows path inside a file URI).
        [b'/', drive, b':', b'/', ..] if drive.is_ascii_alphabetic() => {
            Some(decoded[1..].to_string())
        }
        // `/C|/x` legacy form → `C:/x`.
        [b'/', drive, b'|', b'/', ..] if drive.is_ascii_alphabetic() => {
            Some(format!("{}:/{}", *drive as char, &decoded[4..]))
        }
        _ => Some(decoded),
    }
}

/// Donor `urlencoding::decode`: percent-decode UTF-8 path text.
fn percent_decode(text: &str) -> Option<std::borrow::Cow<'_, str>> {
    if !text.contains('%') {
        return Some(std::borrow::Cow::Borrowed(text));
    }
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = bytes.get(i + 1..i + 3)?;
            let hi = (hex[0] as char).to_digit(16)?;
            let lo = (hex[1] as char).to_digit(16)?;
            out.push((hi * 16 + lo) as u8);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    Some(std::borrow::Cow::Owned(String::from_utf8(out).ok()?))
}

/// Donor `codex_utils_string::normalize_markdown_hash_location_suffix`,
/// verbatim: `#L74C3` → `:74:3`, `#L74C3-L76C9` → `:74:3-76:9`.
fn normalize_markdown_hash_location_suffix(suffix: &str) -> Option<String> {
    fn parse_point(point: &str) -> Option<(&str, Option<&str>)> {
        let point = point.strip_prefix('L')?;
        match point.split_once('C') {
            Some((line, column)) => Some((line, Some(column))),
            None => Some((point, None)),
        }
    }
    let fragment = suffix.strip_prefix('#')?;
    let (start, end) = match fragment.split_once('-') {
        Some((start, end)) => (start, Some(end)),
        None => (fragment, None),
    };
    let (start_line, start_column) = parse_point(start)?;
    let mut normalized = String::from(":");
    normalized.push_str(start_line);
    if let Some(column) = start_column {
        normalized.push(':');
        normalized.push_str(column);
    }
    if let Some(end) = end {
        let (end_line, end_column) = parse_point(end)?;
        normalized.push('-');
        normalized.push_str(end_line);
        if let Some(column) = end_column {
            normalized.push(':');
            normalized.push_str(column);
        }
    }
    Some(normalized)
}
