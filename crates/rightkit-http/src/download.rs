//! Resumable download to a file, with optional SHA-256 and length verification.
//!
//! Bytes go to `<dest>.part`. On success the file is fsynced and renamed to `dest`.
//! Every request goes through the same policy layer as [`Client::send`]: timeouts,
//! retry/backoff, redirect and size settings.

use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use sha2::{Digest, Sha256};

use crate::cancel::is_set;
use crate::client::{
    header_of, map_ureq_error_in, read_limited, status_error, Client, Raw, Request,
};
use crate::error::{HttpError, Phase, MAX_ERROR_BODY_BYTES};

const CHUNK: usize = 64 * 1024;

/// Options for [`Client::download_to_path`].
///
/// `headers` may not contain `Range` or `If-Range`; the download manages them.
/// `Accept-Encoding: identity` is sent unless `headers` sets one, so `Content-Length`
/// and `Range` offsets refer to the bytes written to disk.
#[derive(Default)]
pub struct DownloadOptions<'a> {
    pub headers: Vec<(String, String)>,
    /// Expected lowercase or uppercase hex SHA-256 of the complete file.
    pub expected_sha256: Option<String>,
    /// Expected complete length in bytes.
    pub expected_bytes: Option<u64>,
    /// Keep an existing `.part` file and continue it with a `Range` request.
    pub resume: bool,
    /// Checked before every read and every retry sleep. Setting it keeps `.part`.
    pub cancel: Option<&'a AtomicBool>,
    /// Called after each body chunk is written.
    pub on_progress: Option<&'a mut dyn FnMut(&DownloadProgress)>,
    /// Mid-body failures to resume after before giving up (the `.part` is kept).
    pub max_reconnects: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DownloadProgress {
    /// Bytes present in the `.part` file, including any resumed prefix.
    pub downloaded: u64,
    /// Total object length, when the server reported it.
    pub total: Option<u64>,
    /// Bytes kept from an earlier call; 0 after a restart.
    pub resumed_from: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DownloadReport {
    pub path: PathBuf,
    pub bytes: u64,
    /// Lowercase hex SHA-256 of the file.
    pub sha256: String,
    /// Bytes kept from an earlier call; 0 for a fresh download.
    pub resumed_from: u64,
    /// HTTP requests issued, including retries and reconnects.
    pub attempts: u32,
}

#[derive(Debug)]
pub enum DownloadError {
    Http(HttpError),
    Io(String),
    /// Cancelled; the `.part` file is kept and holds `partial_bytes` bytes.
    Cancelled {
        partial_bytes: u64,
    },
    /// Hash mismatch; the `.part` file was deleted.
    Integrity {
        expected: String,
        actual: String,
    },
    /// Length mismatch; the `.part` file was deleted.
    LengthMismatch {
        expected: u64,
        actual: u64,
    },
}

impl fmt::Display for DownloadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DownloadError::Http(e) => write!(f, "download failed: {e}"),
            DownloadError::Io(m) => write!(f, "download I/O error: {m}"),
            DownloadError::Cancelled { partial_bytes } => {
                write!(f, "download cancelled after {partial_bytes} byte(s)")
            }
            DownloadError::Integrity { expected, actual } => {
                write!(f, "SHA-256 mismatch: expected {expected}, got {actual}")
            }
            DownloadError::LengthMismatch { expected, actual } => {
                write!(
                    f,
                    "length mismatch: expected {expected} bytes, got {actual}"
                )
            }
        }
    }
}

impl std::error::Error for DownloadError {}

impl From<HttpError> for DownloadError {
    fn from(e: HttpError) -> Self {
        DownloadError::Http(e)
    }
}

#[derive(Default)]
struct FetchState {
    written: u64,
    resumed_from: u64,
    total: Option<u64>,
    etag: Option<String>,
    attempts: u32,
    reconnects: u32,
    restarted: bool,
}

impl Client {
    /// Download `url` to `dest`, resuming a `.part` file when `opts.resume` is set.
    ///
    /// Resume rules: a non-empty `.part` is requested with `Range: bytes=<len>-`, plus
    /// `If-Range` with the first ETag seen in this call. A `206` appends. A `200` truncates
    /// and restarts. A `416` whose `Content-Range` total equals the local length verifies
    /// the file without fetching bytes. Any other `416` restarts once from zero. A
    /// mid-body failure reconnects from the current length, up to `max_reconnects` times.
    ///
    /// Note: `If-Range` is only sent once this call has seen an ETag. Resuming a `.part`
    /// left by an earlier call is protected only by `expected_sha256`.
    pub fn download_to_path(
        &self,
        url: &str,
        dest: &Path,
        mut opts: DownloadOptions<'_>,
    ) -> Result<DownloadReport, DownloadError> {
        if opts
            .headers
            .iter()
            .any(|(k, _)| k.eq_ignore_ascii_case("range") || k.eq_ignore_ascii_case("if-range"))
        {
            return Err(HttpError::InvalidRequest(
                "Range and If-Range are managed by download_to_path".into(),
            )
            .into());
        }
        let part = part_path(dest);
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .read(true)
            .open(&part)
            .map_err(io_error("open partial file"))?;
        let mut st = FetchState::default();
        if opts.resume {
            st.written = file
                .metadata()
                .map_err(io_error("stat partial file"))?
                .len();
            st.resumed_from = st.written;
        } else {
            file.set_len(0).map_err(io_error("truncate partial file"))?;
        }

        let fetched = self.fetch_into(url, &mut file, &mut st, &mut opts);
        drop(file);
        fetched?;

        let mismatch = st
            .total
            .filter(|t| *t != st.written)
            .or(opts.expected_bytes.filter(|e| *e != st.written));
        if let Some(expected) = mismatch {
            remove_part(&part)?;
            return Err(DownloadError::LengthMismatch {
                expected,
                actual: st.written,
            });
        }
        let actual = sha256_file(&part).map_err(io_error("hash partial file"))?;
        if let Some(expected) = &opts.expected_sha256 {
            if !expected.eq_ignore_ascii_case(&actual) {
                remove_part(&part)?;
                return Err(DownloadError::Integrity {
                    expected: expected.clone(),
                    actual,
                });
            }
        }
        fs::rename(&part, dest).map_err(io_error("rename into place"))?;
        Ok(DownloadReport {
            path: dest.to_path_buf(),
            bytes: st.written,
            sha256: actual,
            resumed_from: st.resumed_from,
            attempts: st.attempts,
        })
    }

    fn fetch_into(
        &self,
        url: &str,
        file: &mut File,
        st: &mut FetchState,
        opts: &mut DownloadOptions<'_>,
    ) -> Result<(), DownloadError> {
        loop {
            if is_set(opts.cancel) {
                return Err(DownloadError::Cancelled {
                    partial_bytes: st.written,
                });
            }
            let offset = st.written;
            let mut req = Request::get(url);
            for (k, v) in &opts.headers {
                req = req.header(k.clone(), v.clone());
            }
            if !opts
                .headers
                .iter()
                .any(|(k, _)| k.eq_ignore_ascii_case("accept-encoding"))
            {
                req = req.set_header("Accept-Encoding", "identity");
            }
            if offset > 0 {
                req = req.set_header("Range", format!("bytes={offset}-"));
                if let Some(tag) = &st.etag {
                    req = req.set_header("If-Range", tag.clone());
                }
            }
            let raw: Raw = self.execute(&req, self.config.stream_timeout, opts.cancel)?;
            let Raw {
                status,
                headers,
                body,
                attempts,
                ..
            } = raw;
            st.attempts += attempts;
            if let Some(encoding) = header_of(&headers, "content-encoding") {
                if !encoding.trim().eq_ignore_ascii_case("identity") {
                    return Err(stream_error(format!(
                        "unexpected Content-Encoding {encoding} for a download"
                    )));
                }
            }
            match status {
                206 if offset > 0 => {
                    let range = header_of(&headers, "content-range")
                        .and_then(parse_content_range)
                        .ok_or_else(|| stream_error("206 without a valid Content-Range".into()))?;
                    if range.start != Some(offset) {
                        return Err(stream_error(
                            "Content-Range does not start at the requested offset".into(),
                        ));
                    }
                    st.total = range.total;
                    if st.etag.is_none() {
                        st.etag = header_of(&headers, "etag").map(str::to_string);
                    }
                }
                200 => {
                    if offset > 0 {
                        file.set_len(0).map_err(io_error("truncate partial file"))?;
                        st.written = 0;
                        st.resumed_from = 0;
                    }
                    st.total = header_of(&headers, "content-length")
                        .and_then(|v| v.trim().parse::<u64>().ok());
                    st.etag = header_of(&headers, "etag").map(str::to_string);
                }
                416 => {
                    let range_total = header_of(&headers, "content-range")
                        .and_then(parse_content_range)
                        .and_then(|r| r.total);
                    if offset > 0 && range_total == Some(offset) {
                        // The local file is the whole object: verify only.
                        st.total = Some(offset);
                        return Ok(());
                    }
                    if offset > 0 && !st.restarted {
                        st.restarted = true;
                        file.set_len(0).map_err(io_error("truncate partial file"))?;
                        st.written = 0;
                        st.resumed_from = 0;
                        continue;
                    }
                    let bytes = read_error_body(body);
                    return Err(status_error(416, &headers, bytes, st.attempts).into());
                }
                s if (200..300).contains(&s) => {
                    return Err(stream_error(format!("unexpected HTTP {s} for a download")));
                }
                s => {
                    let bytes = read_error_body(body);
                    return Err(status_error(s, &headers, bytes, st.attempts).into());
                }
            }

            let mut reader = body.into_reader();
            let mut buf = vec![0u8; CHUNK];
            loop {
                if is_set(opts.cancel) {
                    return Err(DownloadError::Cancelled {
                        partial_bytes: st.written,
                    });
                }
                match reader.read(&mut buf) {
                    Ok(0) => return Ok(()),
                    Ok(n) => {
                        file.write_all(&buf[..n])
                            .map_err(io_error("write partial file"))?;
                        st.written += n as u64;
                        if let Some(cb) = opts.on_progress.as_deref_mut() {
                            cb(&DownloadProgress {
                                downloaded: st.written,
                                total: st.total,
                                resumed_from: st.resumed_from,
                            });
                        }
                    }
                    Err(e) => {
                        let err = if e.kind() == io::ErrorKind::TimedOut {
                            HttpError::timeout(e.to_string(), Phase::Body)
                        } else {
                            map_ureq_error_in(e.into(), Phase::Body)
                        };
                        if st.reconnects < opts.max_reconnects {
                            st.reconnects += 1;
                            break;
                        }
                        return Err(err.into());
                    }
                }
            }
        }
    }
}

fn read_error_body(body: ureq::Body) -> Vec<u8> {
    let mut reader = body.into_reader();
    read_limited(&mut reader, MAX_ERROR_BODY_BYTES).unwrap_or_default()
}

fn stream_error(message: String) -> DownloadError {
    DownloadError::Http(HttpError::Stream(message))
}

fn io_error(what: &'static str) -> impl Fn(io::Error) -> DownloadError {
    move |e| DownloadError::Io(format!("{what}: {e}"))
}

fn remove_part(part: &Path) -> Result<(), DownloadError> {
    fs::remove_file(part).map_err(io_error("remove partial file"))
}

fn part_path(dest: &Path) -> PathBuf {
    let mut name = dest.as_os_str().to_owned();
    name.push(".part");
    PathBuf::from(name)
}

struct ContentRange {
    start: Option<u64>,
    total: Option<u64>,
}

/// Parse `bytes <start>-<end>/<total>` or `bytes */<total>`; `*` means unknown.
fn parse_content_range(value: &str) -> Option<ContentRange> {
    let rest = value.trim().strip_prefix("bytes ")?;
    let (range, total) = rest.split_once('/')?;
    let total = match total.trim() {
        "*" => None,
        t => Some(t.parse::<u64>().ok()?),
    };
    let start = match range.trim() {
        "*" => None,
        r => Some(r.split_once('-')?.0.trim().parse::<u64>().ok()?),
    };
    Some(ContentRange { start, total })
}

fn sha256_file(path: &Path) -> io::Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; CHUNK];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}
