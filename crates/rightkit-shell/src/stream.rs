//! Ranged custom-protocol streaming of broker-granted files.
//!
//! Donors: ViewRight `binary_stream.rs` (id resolution, HEAD/GET/OPTIONS, single-range parsing,
//! heavy reads off the calling thread) and `stream_http.rs` (range parser, fallible response
//! builders that never panic). The shell registers the scheme with
//! `ShellBuilder::stream_protocol("myapp-stream")`; URLs are `myapp-stream://localhost/<id>`
//! (`http://myapp-stream.localhost/<id>` on Windows). The only credential is the id minted by
//! [`crate::broker::PathBroker::grant_stream`].

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use tauri::http::response::Builder;
use tauri::http::{header, Method, Request, Response, StatusCode};

use crate::broker::PathBroker;

/// Policy for what a stream scheme will serve.
#[derive(Debug, Clone)]
pub struct StreamPolicy {
    /// Largest file served (bytes).
    pub max_bytes: u64,
    /// Lowercase extensions served; `None` serves any type the mime table knows.
    pub extensions: Option<Vec<String>>,
}

impl Default for StreamPolicy {
    fn default() -> Self {
        Self {
            max_bytes: 150 * 1024 * 1024,
            extensions: None,
        }
    }
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

fn internal_error() -> Response<Vec<u8>> {
    let mut r = Response::new(Vec::new());
    *r.status_mut() = StatusCode::INTERNAL_SERVER_ERROR;
    r
}

fn finish(b: Builder, body: Vec<u8>) -> Response<Vec<u8>> {
    b.body(body).unwrap_or_else(|_| internal_error())
}

fn cors(status: StatusCode) -> Builder {
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

fn ranged(status: StatusCode, mime: &str, len: u64) -> Builder {
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

fn empty(status: StatusCode) -> Response<Vec<u8>> {
    finish(cors(status), Vec::new())
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

fn read_range(path: &Path, start: u64, end: u64) -> std::io::Result<Vec<u8>> {
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
    let method = request.method();
    if method == Method::OPTIONS {
        return respond(empty(StatusCode::NO_CONTENT));
    }
    if method != Method::GET && method != Method::HEAD {
        return respond(empty(StatusCode::METHOD_NOT_ALLOWED));
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
    if !meta.is_file() || meta.len() > policy.max_bytes {
        return respond(empty(StatusCode::PAYLOAD_TOO_LARGE));
    }
    let total = meta.len();
    let head = method == Method::HEAD;

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
