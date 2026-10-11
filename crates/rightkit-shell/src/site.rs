//! Site-root serving: a second custom scheme (`<app>-site://`) that serves allowlisted
//! subresources beneath a granted site root.
//!
//! Donor: ViewRight `site_root.rs` (root inference, MIME allowlist, containment checked after
//! canonicalization, opaque per-grant tokens) and `stream_http.rs`.
//!
//! Flow: the app grants a document it already holds through the broker (the
//! `rk_shell_site_grant` command, or [`grant_document_site`]). The shell infers a site root (the
//! nearest enclosing project marker, never a filesystem root or the home directory, falling back to
//! the document's own folder), mints an opaque token and returns `<scheme>://localhost/<token>/`.
//! A document whose `<base href>` points there resolves `src`, CSS `url()`, fonts and scripts
//! natively.
//!
//! Security model:
//! * Tokens are opaque and per grant. [`PathBroker::revoke_site_root`] ends one permanently.
//! * A request is percent-decoded, refused when it carries `..`, a root or a prefix component,
//!   canonicalized, and re-checked against the canonical root, so a symlink that leaves the tree
//!   fails after resolution.
//! * Denied file names (by default the dotenv family and the npm and netrc credential files;
//!   configurable on [`SitePolicy`]) are refused as requested and again after symlink resolution.
//! * Only the MIME allowlist ([`site_mime`]) is served, with `nosniff` on every body. Page script
//!   gets no `fetch()` access to this scheme (the app's CSP decides that).
//! * A site root authorises this protocol only. It is not a file grant: the document commands
//!   still refuse any file the person did not hand to the app.

use std::path::{Component, Path, PathBuf};

use tauri::http::{header, Method, Request, Response, StatusCode};

use crate::broker::{path_to_string, PathBroker};
use crate::error::ShellError;
use crate::stream::{cors, empty, finish, method_gate_status, parse_range, ranged, read_range};

/// Directory names that mark a plausible project root when walking up from a document.
pub const ROOT_MARKERS: [&str; 3] = [".git", ".hg", ".svn"];

/// File names that mark a plausible project root when walking up from a document.
pub const ROOT_MARKER_FILES: [&str; 5] = [
    "package.json",
    "pnpm-workspace.yaml",
    "Cargo.toml",
    "go.mod",
    "pyproject.toml",
];

/// How many directory levels above the document are considered. Bounds both the stat cost of
/// inference and how far a single grant can reach.
pub const MAX_ROOT_DEPTH: usize = 12;

/// Policy for site serving: which names are refused and which extra markers mark a root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SitePolicy {
    /// File names refused regardless of extension. Each entry is an exact name, or a prefix ending
    /// in `*`. Matching is case-insensitive against the final path component.
    pub denied_names: Vec<String>,
    /// Extra file or directory names (beside the built-in project markers) that mark a site root.
    pub root_markers: Vec<String>,
}

impl Default for SitePolicy {
    fn default() -> Self {
        // Credential and environment files a page must never be able to read back.
        let denied = [
            ".env".to_owned(),
            ".env.*".to_owned(),
            ".npmrc".to_owned(),
            ".netrc".to_owned(),
        ];
        Self {
            denied_names: denied.to_vec(),
            root_markers: Vec::new(),
        }
    }
}

impl SitePolicy {
    /// Refuse one more file name (exact, or a prefix ending in `*`).
    pub fn deny(mut self, name: &str) -> Self {
        self.denied_names.push(name.to_owned());
        self
    }

    /// Treat one more name as a root marker.
    pub fn marker(mut self, name: &str) -> Self {
        self.root_markers.push(name.to_owned());
        self
    }

    /// True when `name` is refused by the denied-name list.
    pub fn is_denied(&self, name: &str) -> bool {
        let lower = name.to_ascii_lowercase();
        self.denied_names.iter().any(|pattern| {
            let pattern = pattern.to_ascii_lowercase();
            match pattern.strip_suffix('*') {
                Some(prefix) => lower.starts_with(prefix),
                None => lower == pattern,
            }
        })
    }
}

/// MIME allowlist for site subresources. Deliberately narrow: an extension absent from this table
/// is refused (415) rather than served as `application/octet-stream`.
pub fn site_mime(ext: &str) -> Option<&'static str> {
    Some(match ext.to_ascii_lowercase().as_str() {
        "css" => "text/css",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "svg" => "image/svg+xml",
        "ico" => "image/x-icon",
        "bmp" => "image/bmp",
        "apng" => "image/apng",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "eot" => "application/vnd.ms-fontobject",
        "wav" => "audio/wav",
        "mp3" => "audio/mpeg",
        "m4a" => "audio/mp4",
        "aac" => "audio/aac",
        "flac" => "audio/flac",
        "ogg" | "oga" => "audio/ogg",
        "webm" => "video/webm",
        "mp4" | "m4v" => "video/mp4",
        "mov" => "video/quicktime",
        "ogv" => "video/ogg",
        "vtt" => "text/vtt",
        "srt" => "application/x-subrip",
        "js" | "mjs" | "cjs" => "text/javascript",
        "json" => "application/json",
        "map" => "application/json",
        "wasm" => "application/wasm",
        _ => return None,
    })
}

/// The user's home directory, used only to refuse it as a site root.
fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
}

/// True when `dir` is too broad to ever be a site root: a filesystem root, a bare volume root, or
/// the user's home directory.
fn is_too_broad(dir: &Path) -> bool {
    if dir.parent().is_none() {
        return true;
    }
    let depth = dir
        .components()
        .filter(|c| matches!(c, Component::Normal(_)))
        .count();
    if depth == 0 {
        return true;
    }
    home_dir().is_some_and(|home| {
        std::fs::canonicalize(&home).is_ok_and(|home| home == dir) || home == dir
    })
}

fn has_root_marker(dir: &Path, policy: &SitePolicy) -> bool {
    ROOT_MARKERS.iter().any(|m| dir.join(m).exists())
        || policy.root_markers.iter().any(|m| dir.join(m).exists())
        || ROOT_MARKER_FILES.iter().any(|m| dir.join(m).is_file())
}

/// Infer the site root for a document: the nearest enclosing directory with a project marker,
/// searching upward at most [`MAX_ROOT_DEPTH`] levels and never returning a directory
/// [`is_too_broad`] rejects. Falls back to the document's own directory (the pre-existing
/// behaviour), so a loose HTML file grants no more than before.
pub fn infer_site_root(doc_path: &Path, policy: &SitePolicy) -> Option<PathBuf> {
    let doc_dir = std::fs::canonicalize(doc_path.parent()?).ok()?;
    let mut candidate = doc_dir.as_path();
    for _ in 0..MAX_ROOT_DEPTH {
        if is_too_broad(candidate) {
            break;
        }
        if has_root_marker(candidate, policy) {
            return Some(candidate.to_path_buf());
        }
        match candidate.parent() {
            Some(parent) => candidate = parent,
            None => break,
        }
    }
    Some(doc_dir)
}

/// Split a request path `/<token>/<relative path>` into `(token, percent-decoded relative path)`.
pub fn split_site_request(uri_path: &str) -> Option<(String, String)> {
    let trimmed = uri_path.trim_start_matches('/');
    let (token, rest) = trimmed.split_once('/')?;
    if token.is_empty() || rest.is_empty() {
        return None;
    }
    Some((token.to_string(), percent_decode(rest)))
}

/// Minimal percent-decoder for URL paths. An invalid escape passes through literally, so a file
/// whose name contains a bare `%` still resolves.
pub fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
            if let Some(byte) = hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Resolve `rel` beneath `root`. Refuses `..`, root and prefix components and denied names up
/// front, then canonicalizes and re-checks containment and the denied name on the real target, so
/// a symlink pointing outside the tree (or at a denied file) fails.
pub fn resolve_in_root(root: &Path, rel: &str, policy: &SitePolicy) -> Result<PathBuf, ShellError> {
    let rel = rel.trim_start_matches('/');
    if rel.is_empty() {
        return Err(ShellError::FileNotFound("empty asset path".into()));
    }
    if rel.contains('\0') {
        return Err(ShellError::Invalid("path contains a NUL byte".into()));
    }
    let requested = Path::new(rel);
    let escapes = requested.components().any(|c| {
        matches!(
            c,
            Component::ParentDir | Component::Prefix(_) | Component::RootDir
        )
    });
    if escapes {
        return Err(ShellError::BrokerDenied(
            "asset is outside the granted site root".into(),
        ));
    }
    if requested
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| policy.is_denied(n))
    {
        return Err(ShellError::BrokerDenied("asset type is not allowed".into()));
    }
    let target = std::fs::canonicalize(root.join(requested))?;
    let root = std::fs::canonicalize(root)?;
    if !target.starts_with(&root) {
        return Err(ShellError::BrokerDenied(
            "asset is outside the granted site root".into(),
        ));
    }
    if target
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| policy.is_denied(n))
    {
        return Err(ShellError::BrokerDenied("asset type is not allowed".into()));
    }
    Ok(target)
}

/// A site grant: the opaque token and the directory it serves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SiteGrant {
    pub token: String,
    pub root: PathBuf,
}

/// Grant the site root of a document the person already opened. The document must be a granted
/// file; its root is inferred with `policy`, and the grant covers only this protocol.
pub fn grant_document_site(
    broker: &PathBroker,
    document: &str,
    policy: &SitePolicy,
) -> Result<SiteGrant, ShellError> {
    let granted = broker.require_file(document)?;
    let root = infer_site_root(Path::new(&granted), policy)
        .ok_or_else(|| ShellError::Invalid("document has no parent directory".into()))?;
    let token = broker.grant_site_root(&root)?;
    Ok(SiteGrant {
        token,
        root: PathBuf::from(path_to_string(&root)),
    })
}

/// The base URL a document uses for a site token (`<base href>`), per platform convention.
pub fn site_base_url(scheme: &str, token: &str) -> String {
    if cfg!(windows) {
        format!("http://{scheme}.localhost/{token}/")
    } else {
        format!("{scheme}://localhost/{token}/")
    }
}

fn base_builder(status: StatusCode, mime: &str, len: u64) -> tauri::http::response::Builder {
    ranged(status, mime, len).header(header::X_CONTENT_TYPE_OPTIONS, "nosniff")
}

/// Serve one `<scheme>://localhost/<token>/<relative path>` request. Validation (token, containment,
/// denied names, MIME, stat, range) is synchronous; only the body read runs on a worker thread.
/// `respond` runs exactly once.
pub fn serve_site_request<F>(
    broker: &PathBroker,
    policy: &SitePolicy,
    request: &Request<Vec<u8>>,
    respond: F,
) where
    F: FnOnce(Response<Vec<u8>>) + Send + 'static,
{
    if let Some(status) = method_gate_status(request.method()) {
        return respond(empty(status));
    }
    let Some((token, rel)) = split_site_request(request.uri().path()) else {
        return respond(empty(StatusCode::BAD_REQUEST));
    };
    let Ok(root) = broker.resolve_site_root(&token) else {
        return respond(empty(StatusCode::FORBIDDEN));
    };
    let target = match resolve_in_root(&root, &rel, policy) {
        Ok(target) => target,
        // A policy refusal (403) stays distinct from a missing file (404) so the UI never reports
        // a containment failure as "not found".
        Err(ShellError::BrokerDenied(_)) => return respond(empty(StatusCode::FORBIDDEN)),
        Err(ShellError::Invalid(_)) => return respond(empty(StatusCode::BAD_REQUEST)),
        Err(_) => return respond(empty(StatusCode::NOT_FOUND)),
    };
    let Some(mime) = target
        .extension()
        .and_then(|e| e.to_str())
        .and_then(site_mime)
    else {
        return respond(empty(StatusCode::UNSUPPORTED_MEDIA_TYPE));
    };
    let Ok(meta) = std::fs::metadata(&target) else {
        return respond(empty(StatusCode::NOT_FOUND));
    };
    if !meta.is_file() {
        return respond(empty(StatusCode::NOT_FOUND));
    }
    let total = meta.len();
    let is_head = request.method() == Method::HEAD;

    if let Some(range_header) = request.headers().get(header::RANGE) {
        let Some((start, end)) = range_header
            .to_str()
            .ok()
            .and_then(|text| parse_range(text, total))
        else {
            return respond(finish(
                cors(StatusCode::RANGE_NOT_SATISFIABLE)
                    .header(header::CONTENT_RANGE, format!("bytes */{total}")),
                Vec::new(),
            ));
        };
        let content_range = format!("bytes {start}-{end}/{total}");
        if is_head {
            return respond(finish(
                base_builder(StatusCode::PARTIAL_CONTENT, mime, end - start + 1)
                    .header(header::CONTENT_RANGE, content_range),
                Vec::new(),
            ));
        }
        std::thread::spawn(move || {
            let response = match read_range(&target, start, end) {
                Ok(bytes) => finish(
                    base_builder(StatusCode::PARTIAL_CONTENT, mime, end - start + 1)
                        .header(header::CONTENT_RANGE, content_range),
                    bytes,
                ),
                Err(_) => empty(StatusCode::NOT_FOUND),
            };
            respond(response);
        });
        return;
    }

    if is_head {
        return respond(finish(
            base_builder(StatusCode::OK, mime, total),
            Vec::new(),
        ));
    }
    std::thread::spawn(move || {
        let response = match std::fs::read(&target) {
            Ok(bytes) => finish(base_builder(StatusCode::OK, mime, total), bytes),
            Err(_) => empty(StatusCode::NOT_FOUND),
        };
        respond(response);
    });
}
