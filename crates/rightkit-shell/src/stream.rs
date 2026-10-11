//! Ranged custom-protocol streaming of broker-granted files.
//!
//! Donors: ViewRight `binary_stream.rs` (id resolution, HEAD/GET/OPTIONS, single-range parsing,
//! heavy reads off the calling thread) and `stream_http.rs` (range parser, fallible response
//! builders that never panic). The shell registers one or more schemes with
//! `ShellBuilder::stream_protocol("myapp-stream", policy)`; URLs are `myapp-stream://localhost/<id>`
//! (`http://myapp-stream.localhost/<id>` on Windows). The only credential is the id minted by
//! [`crate::broker::PathBroker::grant_stream`]. Revoking the file revokes its ids for good.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use tauri::http::response::Builder;
use tauri::http::{header, Method, Request, Response, StatusCode};

use crate::broker::PathBroker;

/// Policy for what a stream scheme will serve.
#[derive(Debug, Clone)]
pub struct StreamPolicy {
    /// Largest file served (bytes) when no per-kind cap applies.
    pub max_bytes: u64,
    /// Lowercase extensions served; `None` serves any type the mime table knows.
    pub extensions: Option<Vec<String>>,
    /// Per-kind size caps (bytes). A key is an extension (`"mov"`, a leading dot is optional), a full
    /// MIME type (`"application/pdf"`) or a MIME family (`"video"` or `"video/*"`). Lookup order is
    /// extension, then full MIME, then family; a file matching no key uses `max_bytes`. Empty by
    /// default, so behaviour is unchanged until a cap is added with [`StreamPolicy::with_kind_cap`].
    pub max_bytes_by_kind: BTreeMap<String, u64>,
}

impl Default for StreamPolicy {
    fn default() -> Self {
        Self {
            max_bytes: 150 * 1024 * 1024,
            extensions: None,
            max_bytes_by_kind: BTreeMap::new(),
        }
    }
}

impl StreamPolicy {
    /// Add or replace a per-kind cap. `kind` follows the key rules of
    /// [`StreamPolicy::max_bytes_by_kind`].
    pub fn with_kind_cap(mut self, kind: &str, max_bytes: u64) -> Self {
        self.max_bytes_by_kind
            .insert(normalize_kind(kind), max_bytes);
        self
    }

    /// The size cap that applies to `path`: the most specific per-kind entry, else `max_bytes`.
    pub fn max_bytes_for(&self, path: &Path) -> u64 {
        if self.max_bytes_by_kind.is_empty() {
            return self.max_bytes;
        }
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase());
        if let Some(cap) = ext.as_deref().and_then(|e| self.max_bytes_by_kind.get(e)) {
            return *cap;
        }
        let Some(mime) = mime_for(path) else {
            return self.max_bytes;
        };
        let mime = mime.split(';').next().unwrap_or(mime).trim();
        if let Some(cap) = self.max_bytes_by_kind.get(mime) {
            return *cap;
        }
        let family = mime.split('/').next().unwrap_or(mime);
        self.max_bytes_by_kind
            .get(family)
            .copied()
            .unwrap_or(self.max_bytes)
    }
}

fn normalize_kind(kind: &str) -> String {
    kind.trim()
        .trim_start_matches('.')
        .trim_end_matches("/*")
        .trim_end_matches('/')
        .to_ascii_lowercase()
}

/// Content type by extension (media and document formats the suite streams).
pub fn mime_for(path: &Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "pdf" => "application/pdf",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "svg" => "image/svg+xml",
        "bmp" => "image/bmp",
        "ico" => "image/x-icon",
        "tif" | "tiff" => "image/tiff",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "m4a" => "audio/mp4",
        "flac" => "audio/flac",
        "ogg" | "oga" => "audio/ogg",
        "mp4" | "m4v" => "video/mp4",
        "mov" => "video/quicktime",
        "webm" => "video/webm",
        "txt" | "log" => "text/plain; charset=utf-8",
        "md" => "text/markdown; charset=utf-8",
        "json" => "application/json",
        "csv" => "text/csv; charset=utf-8",
        "bin" | "dat" => "application/octet-stream",
        _ => return None,
    })
}

/// Last-resort response when a builder call fails (a malformed header value). Built from
/// `Response::new`, which cannot fail, so a release build never panics here.
pub(crate) fn internal_error() -> Response<Vec<u8>> {
    let mut r = Response::new(Vec::new());
    *r.status_mut() = StatusCode::INTERNAL_SERVER_ERROR;
    r
}

/// Finish a builder, falling back to [`internal_error`] instead of panicking.
pub(crate) fn finish(b: Builder, body: Vec<u8>) -> Response<Vec<u8>> {
    b.body(body).unwrap_or_else(|_| internal_error())
}

/// CORS headers every stream and site response carries, errors included.
pub(crate) fn cors(status: StatusCode) -> Builder {
    Response::builder()
        .status(status)
        .header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")
        .header(header::ACCESS_CONTROL_ALLOW_METHODS, "GET, HEAD, OPTIONS")
        .header(header::ACCESS_CONTROL_ALLOW_HEADERS, "Range")
        .header(
            header::ACCESS_CONTROL_EXPOSE_HEADERS,
            "Accept-Ranges, Content-Length, Content-Range",
        )
}

/// Body response headers: mime, length, range support, no caching, CORS.
pub(crate) fn ranged(status: StatusCode, mime: &str, len: u64) -> Builder {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, mime)
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::CONTENT_LENGTH, len.to_string())
        .header(header::CACHE_CONTROL, "no-store")
        .header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")
        .header(
            header::ACCESS_CONTROL_EXPOSE_HEADERS,
            "Accept-Ranges, Content-Length, Content-Range",
        )
}

/// A bodiless response with CORS headers.
pub(crate) fn empty(status: StatusCode) -> Response<Vec<u8>> {
    finish(cors(status), Vec::new())
}

/// The method gate shared by every custom scheme: OPTIONS answers 204, anything but GET/HEAD is
/// refused. `None` means keep handling the request.
pub(crate) fn method_gate_status(method: &Method) -> Option<StatusCode> {
    if method == Method::OPTIONS {
        return Some(StatusCode::NO_CONTENT);
    }
    if method != Method::GET && method != Method::HEAD {
        return Some(StatusCode::METHOD_NOT_ALLOWED);
    }
    None
}

/// Parse one `bytes=a-b` / `bytes=a-` / `bytes=-n` range against a known length into an inclusive
/// `(start, end)`. Multi-range requests, empty bodies and unsatisfiable ranges are refused.
pub fn parse_range(value: &str, len: u64) -> Option<(u64, u64)> {
    let spec = value.strip_prefix("bytes=")?;
    if spec.contains(',') || len == 0 {
        return None;
    }
    let (a, b) = spec.split_once('-')?;
    if a.is_empty() {
        let suffix = b.parse::<u64>().ok()?;
        if suffix == 0 {
            return None;
        }
        return Some((len.saturating_sub(suffix), len - 1));
    }
    let start = a.parse::<u64>().ok()?;
    if start >= len {
        return None;
    }
    let end = if b.is_empty() {
        len - 1
    } else {
        b.parse::<u64>().ok()?.min(len - 1)
    };
    (start <= end).then_some((start, end))
}

/// Read the inclusive `[start, end]` slice of `path`.
pub(crate) fn read_range(path: &Path, start: u64, end: u64) -> std::io::Result<Vec<u8>> {
    let mut f = File::open(path)?;
    f.seek(SeekFrom::Start(start))?;
    let mut buf = vec![0; (end - start + 1) as usize];
    f.read_exact(&mut buf)?;
    Ok(buf)
}

/// Serve one request. Validation (id, mime, stat, range) is synchronous and cheap; only the body
/// read runs on a dedicated thread, then `respond` is called from there.
pub fn serve<F>(broker: &PathBroker, policy: &StreamPolicy, request: &Request<Vec<u8>>, respond: F)
where
    F: FnOnce(Response<Vec<u8>>) + Send + 'static,
{
    if let Some(status) = method_gate_status(request.method()) {
        return respond(empty(status));
    }
    let id = request.uri().path().trim_start_matches('/').trim();
    if id.is_empty() {
        return respond(empty(StatusCode::BAD_REQUEST));
    }
    let Ok(target) = broker.resolve_stream(id) else {
        return respond(empty(StatusCode::FORBIDDEN));
    };
    let path = target.path;
    let allowed_ext = policy.extensions.as_ref().is_none_or(|exts| {
        path.extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| exts.iter().any(|x| x.eq_ignore_ascii_case(e)))
    });
    let mime = match mime_for(&path) {
        Some(m) if allowed_ext => m,
        _ => return respond(empty(StatusCode::UNSUPPORTED_MEDIA_TYPE)),
    };
    let Ok(meta) = std::fs::metadata(&path) else {
        return respond(empty(StatusCode::NOT_FOUND));
    };
    if !meta.is_file() || meta.len() > policy.max_bytes_for(&path) {
        return respond(empty(StatusCode::PAYLOAD_TOO_LARGE));
    }
    let total = meta.len();
    let head = method_is_head(request);

    if let Some(range) = request.headers().get(header::RANGE) {
        let parsed = range.to_str().ok().and_then(|r| parse_range(r, total));
        let Some((start, end)) = parsed else {
            return respond(finish(
                cors(StatusCode::RANGE_NOT_SATISFIABLE)
                    .header(header::CONTENT_RANGE, format!("bytes */{total}")),
                Vec::new(),
            ));
        };
        let builder = ranged(StatusCode::PARTIAL_CONTENT, mime, end - start + 1).header(
            header::CONTENT_RANGE,
            format!("bytes {start}-{end}/{total}"),
        );
        if head {
            return respond(finish(builder, Vec::new()));
        }
        std::thread::spawn(move || {
            respond(match read_range(&path, start, end) {
                Ok(bytes) => finish(builder, bytes),
                Err(_) => empty(StatusCode::NOT_FOUND),
            })
        });
        return;
    }
    if head {
        return respond(finish(ranged(StatusCode::OK, mime, total), Vec::new()));
    }
    std::thread::spawn(move || {
        respond(match std::fs::read(&path) {
            Ok(bytes) => finish(ranged(StatusCode::OK, mime, bytes.len() as u64), bytes),
            Err(_) => empty(StatusCode::NOT_FOUND),
        })
    });
}

fn method_is_head<B>(request: &Request<B>) -> bool {
    request.method() == Method::HEAD
}
