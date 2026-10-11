//! Resumable download to a file, with optional SHA-256 and length verification.
//!
//! Bytes go to `<dest>.part`, created without following links. On success the file is
//! fsynced, hashed through the same open handle and renamed to `dest`. The URL policy runs
//! before the filesystem is touched, and one download at a time holds `dest` (an exclusive
//! OS lock on `<dest>.lock`) from before `.part` is opened until the rename or cleanup.
//! Every request goes through the same policy layer as [`Client::send`]: timeouts,
//! retry/backoff, redirect and size settings.

use std::fmt;
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use sha2::{Digest, Sha256};

use crate::cancel::{is_set, CheckedReader};
use crate::client::{
    header_of, map_ureq_error_in, read_limited, redact_str, redaction_values_for, scrub_error,
    status_error, Client, Raw, Request,
};
use crate::error::{HttpError, Phase, MAX_ERROR_BODY_BYTES};
use crate::urlredact::redact_urls_in_text;
use crate::wire::WireHead;

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
    /// Hard cap on the bytes this response may deliver: the end of a `206` range, or the
    /// `Content-Length` of a `200`.
    limit: Option<u64>,
    etag: Option<String>,
    /// Resource (final URL without query) that supplied the bytes on disk and the ETag.
    source: Option<String>,
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
    /// The saved prefix and its ETag belong to the resource (final URL without its query)
    /// that supplied them: a reconnect that lands on another resource restarts from zero.
    ///
    /// Safety rules: the URL is checked against the address policy before any file is
    /// opened. The destination is then locked: an exclusive OS lock on `<dest>.lock` (created
    /// beside `dest`, never deleted) is taken before `.part` is opened and held until the
    /// download has been verified, cleaned up or renamed, so two calls (threads or processes)
    /// can never append to one `.part`. A call that finds the lock held fails at once with
    /// [`DownloadError::Io`] and sends no request. `.part` is never opened through a symbolic
    /// link: an existing link is removed (never followed) and the file is created
    /// exclusively, or on resume opened with `O_NOFOLLOW` (Unix) or
    /// `FILE_FLAG_OPEN_REPARSE_POINT` plus a reparse-point check on the opened handle
    /// (Windows), so a link swapped in after the check is refused before anything is written.
    /// The bytes are hashed through the same handle that wrote them, after an fsync, and the
    /// path is compared with the handle (device and inode, or volume and file index) just
    /// before the rename. Completion is checked against the real length of the file, not only
    /// the count of bytes this call received. A body that runs past `expected_bytes` (or the
    /// declared range or length) stops the download and deletes `.part`. A response whose
    /// framing shows transparent content decoding is refused, because byte offsets would then
    /// describe decoded data; so is one whose head was not observed on the wire, because then
    /// an encoding cannot be ruled out.
    ///
    /// Note: `If-Range` is only sent once this call has seen an ETag. Resuming a `.part`
    /// left by an earlier call is protected only by `expected_sha256`.
    pub fn download_to_path(
        &self,
        url: &str,
        dest: &Path,
        mut opts: DownloadOptions<'_>,
    ) -> Result<DownloadReport, DownloadError> {
        let values = redaction_values_for(&self.config, &Request::get(url));
        self.download_inner(url, dest, &mut opts)
            .map_err(|e| scrub_download_error(e, &values))
    }

    fn download_inner(
        &self,
        url: &str,
        dest: &Path,
        opts: &mut DownloadOptions<'_>,
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
        // Policy first: a denied URL must not touch the filesystem.
        self.preflight_url(url)?;
        // One writer per destination, from before `.part` is opened until after the rename.
        // Declared before `file`, so it is released after the handle is closed.
        let _dest_lock = DestLock::acquire(dest)?;
        let part = part_path(dest);
        let mut file = open_part(&part, opts.resume)?;
        let mut st = FetchState::default();
        if opts.resume {
            st.written = file
                .metadata()
                .map_err(io_error("stat partial file"))?
                .len();
            st.resumed_from = st.written;
        }

        match self.fetch_into(url, &mut file, &mut st, opts) {
            Err(e @ DownloadError::LengthMismatch { .. }) => {
                drop(file);
                let _ = fs::remove_file(&part);
                return Err(e);
            }
            other => other?,
        }

        // The count above is what this call received; the file is what gets published. They
        // differ when something else wrote through its own handle (an older crate version
        // that took no lock, another tool), and then neither length nor hash can be trusted.
        let on_disk = file
            .metadata()
            .map_err(io_error("stat partial file"))?
            .len();
        if on_disk != st.written {
            drop(file);
            remove_part(&part)?;
            return Err(DownloadError::LengthMismatch {
                expected: st.written,
                actual: on_disk,
            });
        }

        // Whole-resource completion: the advertised total when there is one. `limit` is only
        // where this response's range ends, which may fall short of the resource.
        let expected_len = st.total.or(st.limit);
        let mismatch = expected_len
            .filter(|t| *t != st.written)
            .or(opts.expected_bytes.filter(|e| *e != st.written));
        if let Some(expected) = mismatch {
            drop(file);
            remove_part(&part)?;
            return Err(DownloadError::LengthMismatch {
                expected,
                actual: st.written,
            });
        }
        file.sync_all().map_err(io_error("fsync partial file"))?;
        let actual = sha256_handle(&mut file).map_err(io_error("hash partial file"))?;
        if let Some(expected) = &opts.expected_sha256 {
            if !expected.eq_ignore_ascii_case(&actual) {
                drop(file);
                remove_part(&part)?;
                return Err(DownloadError::Integrity {
                    expected: expected.clone(),
                    actual,
                });
            }
        }
        ensure_same_file(&file, &part).map_err(io_error("check partial file"))?;
        fs::rename(&part, dest).map_err(io_error("rename into place"))?;
        drop(file);
        sync_parent(dest);
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
                final_url,
                wire_head,
                ..
            } = raw;
            st.attempts += attempts;
            let source = resource_key(&final_url);
            check_wire_encoding(&wire_head, &headers)?;
            if matches!(status, 200 | 206) && frames_like_decoding(&headers, &body) {
                return Err(stream_error(
                    "response is neither length-delimited nor chunked, which is how a transparently \
                     decoded or unframed body looks; refusing it for a download"
                        .into(),
                ));
            }
            match status {
                206 if offset > 0 => {
                    let range = header_of(&headers, "content-range")
                        .and_then(parse_content_range)
                        .ok_or_else(|| stream_error("206 without a valid Content-Range".into()))?;
                    let (Some(start), Some(end)) = (range.start, range.end) else {
                        return Err(stream_error("206 Content-Range has no byte range".into()));
                    };
                    if start != offset {
                        return Err(stream_error(
                            "Content-Range does not start at the requested offset".into(),
                        ));
                    }
                    // `parse_content_range` guarantees `start <= end < u64::MAX`, so neither
                    // expression below can overflow.
                    let span = end - start + 1;
                    let end_exclusive = end + 1;
                    if let Some(length) = header_of(&headers, "content-length")
                        .and_then(|v| v.trim().parse::<u64>().ok())
                    {
                        if length != span {
                            return Err(stream_error(
                                "Content-Length disagrees with Content-Range".into(),
                            ));
                        }
                    }
                    if st.source.as_deref().is_some_and(|s| s != source) {
                        // The saved prefix came from another resource: start over.
                        file.set_len(0).map_err(io_error("truncate partial file"))?;
                        st.written = 0;
                        st.resumed_from = 0;
                        st.etag = None;
                        st.source = None;
                        st.total = None;
                        st.limit = None;
                        continue;
                    }
                    st.total = range.total;
                    st.limit = Some(end_exclusive);
                    if st.etag.is_none() {
                        st.etag = header_of(&headers, "etag").map(str::to_string);
                    }
                    st.source.get_or_insert(source);
                }
                200 => {
                    if offset > 0 {
                        file.set_len(0).map_err(io_error("truncate partial file"))?;
                        st.written = 0;
                        st.resumed_from = 0;
                    }
                    st.total = header_of(&headers, "content-length")
                        .and_then(|v| v.trim().parse::<u64>().ok());
                    st.limit = st.total;
                    st.etag = header_of(&headers, "etag").map(str::to_string);
                    st.source = Some(source);
                }
                416 => {
                    let range_total = header_of(&headers, "content-range")
                        .and_then(parse_content_range)
                        .and_then(|r| r.total);
                    if offset > 0 && range_total == Some(offset) {
                        // The local file is the whole object: verify only.
                        st.total = Some(offset);
                        st.limit = Some(offset);
                        return Ok(());
                    }
                    if offset > 0 && !st.restarted {
                        st.restarted = true;
                        file.set_len(0).map_err(io_error("truncate partial file"))?;
                        st.written = 0;
                        st.resumed_from = 0;
                        continue;
                    }
                    let bytes = read_error_body(body, opts.cancel);
                    if is_set(opts.cancel) {
                        return Err(DownloadError::Cancelled {
                            partial_bytes: st.written,
                        });
                    }
                    return Err(status_error(416, &headers, bytes, st.attempts).into());
                }
                s if (200..300).contains(&s) => {
                    return Err(stream_error(format!("unexpected HTTP {s} for a download")));
                }
                s => {
                    let bytes = read_error_body(body, opts.cancel);
                    if is_set(opts.cancel) {
                        return Err(DownloadError::Cancelled {
                            partial_bytes: st.written,
                        });
                    }
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
                        let cap = match (opts.expected_bytes, st.limit) {
                            (Some(a), Some(b)) => Some(a.min(b)),
                            (a, b) => a.or(b),
                        };
                        if let Some(cap) = cap {
                            let after = st.written.saturating_add(n as u64);
                            if after > cap {
                                return Err(DownloadError::LengthMismatch {
                                    expected: cap,
                                    actual: after,
                                });
                            }
                        }
                        file.write_all(&buf[..n])
                            .map_err(io_error("write partial file"))?;
                        st.written = st.written.saturating_add(n as u64);
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

/// Final URL without query or fragment: the identity of the resource that answered.
fn resource_key(final_url: &str) -> String {
    final_url
        .split(['?', '#'])
        .next()
        .unwrap_or(final_url)
        .to_string()
}

/// Refuse a response whose body ureq may have decoded. What the server really sent is the
/// value read off the wire, because ureq strips `Content-Encoding` when it decodes. A head
/// that was not observed proves nothing about the encoding, so it is refused as well: it
/// must never be read as "no encoding".
fn check_wire_encoding(wire: &WireHead, headers: &[(String, String)]) -> Result<(), DownloadError> {
    let WireHead::Observed { content_encoding } = wire else {
        return Err(stream_error(
            "the response head was not observed on the wire, so a transparent Content-Encoding \
             cannot be ruled out; refusing it for a download"
                .into(),
        ));
    };
    let encoding = content_encoding
        .as_deref()
        .or_else(|| header_of(headers, "content-encoding"));
    match encoding {
        Some(encoding) if !is_identity(encoding) => Err(stream_error(format!(
            "unexpected Content-Encoding {encoding} for a download"
        ))),
        _ => Ok(()),
    }
}

/// True when every coding listed is `identity` (the only one that leaves bytes untouched).
fn is_identity(value: &str) -> bool {
    value
        .split(',')
        .all(|coding| coding.trim().eq_ignore_ascii_case("identity"))
}

/// A `200` or `206` with neither a `Content-Length` nor chunked framing. `ureq` removes
/// `Content-Encoding` and `Content-Length` when it decodes gzip transparently, so this is how
/// that case (and an unframed, close-delimited body) shows up.
fn frames_like_decoding(headers: &[(String, String)], body: &ureq::Body) -> bool {
    let chunked = header_of(headers, "transfer-encoding")
        .is_some_and(|v| v.to_ascii_lowercase().contains("chunked"));
    header_of(headers, "content-length").is_none() && body.content_length().is_none() && !chunked
}

/// Open `.part` without following a link at that path.
fn open_part(part: &Path, resume: bool) -> Result<File, DownloadError> {
    open_part_with(part, resume, &|| {})
}

/// [`open_part`] with a hook that runs after the path was inspected and before it is
/// opened: the window in which a racing process can swap in a link. Production passes a
/// no-op; a test swaps a link in there to prove the open itself refuses to follow it.
fn open_part_with(
    part: &Path,
    resume: bool,
    between_check_and_open: &dyn Fn(),
) -> Result<File, DownloadError> {
    match fs::symlink_metadata(part) {
        Ok(meta) if meta.file_type().is_symlink() => {
            // Remove the link itself; its target is never touched.
            fs::remove_file(part).map_err(io_error("remove partial-file link"))?;
        }
        Ok(meta) if !meta.is_file() => {
            return Err(DownloadError::Io(
                "partial path exists and is not a regular file".into(),
            ));
        }
        Ok(_) if !resume => {
            fs::remove_file(part).map_err(io_error("remove stale partial file"))?;
        }
        _ => {}
    }
    between_check_and_open();
    let file = if resume {
        let mut existing = OpenOptions::new();
        existing.read(true).append(true);
        match open_nofollow(&mut existing, part) {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::NotFound => create_new(part)?,
            Err(e) => return Err(io_error("open partial file")(e)),
        }
    } else {
        create_new(part)?
    };
    let meta = file.metadata().map_err(io_error("stat partial file"))?;
    if !meta.is_file() {
        return Err(DownloadError::Io(
            "partial path is not a regular file".into(),
        ));
    }
    Ok(file)
}

fn create_new(part: &Path) -> Result<File, DownloadError> {
    let mut options = OpenOptions::new();
    options.read(true).append(true).create_new(true);
    open_nofollow(&mut options, part).map_err(io_error("create partial file"))
}

/// Exclusive cross-process lock on one destination, held from before `.part` is opened until
/// the download is verified, cleaned up or renamed.
///
/// It is an OS advisory lock on `<dest>.lock` (`flock` on Unix, `LockFileEx` on Windows, both
/// through `File::try_lock`), so the OS drops it when the holder exits or is killed: there is
/// no stale lock to clean up. Separate handles exclude each other, inside one process as well
/// as between processes. The lock file is never deleted: removing it after unlock would let
/// one process lock the old file while another creates and locks a new one at the same path.
/// A holder is not waited for; a call that finds the lock held fails at once.
#[derive(Debug)]
struct DestLock {
    file: File,
}

impl DestLock {
    fn acquire(dest: &Path) -> Result<Self, DownloadError> {
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        let file = open_nofollow(&mut options, &lock_path(dest))
            .map_err(io_error("open destination lock"))?;
        match file.try_lock() {
            Ok(()) => Ok(Self { file }),
            Err(TryLockError::WouldBlock) => Err(DownloadError::Io(
                "another download of this destination is in progress (its lock is held)".into(),
            )),
            Err(TryLockError::Error(e)) => Err(io_error("lock destination")(e)),
        }
    }
}

impl Drop for DestLock {
    fn drop(&mut self) {
        // Explicit, so a descriptor inherited by a forked child cannot keep the lock busy.
        let _ = self.file.unlock();
    }
}

fn lock_path(dest: &Path) -> PathBuf {
    let mut name = dest.as_os_str().to_owned();
    name.push(".lock");
    PathBuf::from(name)
}

/// Open `path` with `options`, never following a link at the final component. A link there
/// is an error, and no data has been written or truncated through it: none of the callers
/// truncates, and `create_new` fails on any existing entry.
#[cfg(unix)]
fn open_nofollow(options: &mut OpenOptions, path: &Path) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    options.custom_flags(libc::O_NOFOLLOW);
    options.open(path)
}

/// Windows has no `O_NOFOLLOW`: without `FILE_FLAG_OPEN_REPARSE_POINT` the open follows a
/// symlink or junction. With it the open lands on the link itself, which is then refused by
/// its attributes before the handle is returned or used.
#[cfg(windows)]
fn open_nofollow(options: &mut OpenOptions, path: &Path) -> io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    let file = options.open(path)?;
    if is_reparse_point(&file)? {
        return Err(io::Error::other(
            "path is a symbolic link or other reparse point",
        ));
    }
    Ok(file)
}

#[cfg(not(any(unix, windows)))]
fn open_nofollow(options: &mut OpenOptions, path: &Path) -> io::Result<File> {
    options.open(path)
}

#[cfg(windows)]
const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
#[cfg(windows)]
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;

/// Whether the open handle is a symlink, junction or other reparse point rather than the
/// file it would point to.
#[cfg(windows)]
fn is_reparse_point(file: &File) -> io::Result<bool> {
    use std::os::windows::fs::MetadataExt;
    Ok(file.metadata()?.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0)
}

/// Volume serial number and file index of an open handle: the identity of the file itself,
/// whatever path names it.
#[cfg(windows)]
fn file_key(file: &File) -> io::Result<(u32, u64)> {
    use std::os::windows::io::AsRawHandle;
    #[repr(C)]
    struct FileTime {
        low: u32,
        high: u32,
    }
    #[repr(C)]
    struct ByHandleFileInformation {
        attributes: u32,
        creation: FileTime,
        access: FileTime,
        write: FileTime,
        volume_serial: u32,
        size_high: u32,
        size_low: u32,
        links: u32,
        index_high: u32,
        index_low: u32,
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetFileInformationByHandle(
            handle: *mut std::ffi::c_void,
            info: *mut ByHandleFileInformation,
        ) -> i32;
    }
    let mut info = std::mem::MaybeUninit::<ByHandleFileInformation>::zeroed();
    // SAFETY: the handle is a live file handle and `info` is a valid out-parameter.
    let ok = unsafe { GetFileInformationByHandle(file.as_raw_handle().cast(), info.as_mut_ptr()) };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the call succeeded and filled the struct.
    let info = unsafe { info.assume_init() };
    Ok((
        info.volume_serial,
        (u64::from(info.index_high) << 32) | u64::from(info.index_low),
    ))
}

/// Fail when `path` no longer names the file `file` is open on.
fn ensure_same_file(file: &File, path: &Path) -> io::Result<()> {
    let on_disk = fs::symlink_metadata(path)?;
    if on_disk.file_type().is_symlink() {
        return Err(io::Error::other("partial path became a link"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let open = file.metadata()?;
        if open.dev() != on_disk.dev() || open.ino() != on_disk.ino() {
            return Err(io::Error::other("partial path was replaced"));
        }
    }
    #[cfg(windows)]
    {
        // A second handle on the path, itself refused if the path is now a link. It is
        // closed again before the caller renames.
        let mut options = OpenOptions::new();
        options.read(true);
        let current = open_nofollow(&mut options, path)?;
        if file_key(file)? != file_key(&current)? {
            return Err(io::Error::other("partial path was replaced"));
        }
    }
    #[cfg(not(any(unix, windows)))]
    let _ = file;
    Ok(())
}

/// Flush the directory entry of the renamed file, where the platform allows it.
fn sync_parent(dest: &Path) {
    #[cfg(unix)]
    if let Some(parent) = dest.parent() {
        let dir = if parent.as_os_str().is_empty() {
            Path::new(".")
        } else {
            parent
        };
        if let Ok(d) = File::open(dir) {
            let _ = d.sync_all();
        }
    }
    #[cfg(not(unix))]
    let _ = dest;
}

fn scrub_download_error(err: DownloadError, values: &[String]) -> DownloadError {
    match err {
        DownloadError::Http(e) => DownloadError::Http(scrub_error(e, values)),
        DownloadError::Io(m) => DownloadError::Io(redact_str(&redact_urls_in_text(&m), values)),
        other => other,
    }
}

fn read_error_body(body: ureq::Body, cancel: Option<&AtomicBool>) -> Vec<u8> {
    let mut body = body;
    let mut reader = body.as_reader();
    let mut checked = CheckedReader {
        inner: &mut reader,
        cancel,
    };
    read_limited(&mut checked, MAX_ERROR_BODY_BYTES).unwrap_or_default()
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
    end: Option<u64>,
    total: Option<u64>,
}

/// Strictly parse `bytes <start>-<end>/<total>` or `bytes */<total>`. `*` after the slash
/// means the total is unknown. Anything else, `end < start`, or `end >= total`, is invalid.
fn parse_content_range(value: &str) -> Option<ContentRange> {
    let rest = value.trim().strip_prefix("bytes ")?;
    let (range, total) = rest.split_once('/')?;
    let total = match total.trim() {
        "*" => None,
        t => Some(t.parse::<u64>().ok()?),
    };
    let (start, end) = match range.trim() {
        "*" => (None, None),
        r => {
            let (a, b) = r.split_once('-')?;
            (
                Some(a.trim().parse::<u64>().ok()?),
                Some(b.trim().parse::<u64>().ok()?),
            )
        }
    };
    if let (Some(s), Some(e)) = (start, end) {
        // `e + 1` (the exclusive end) must be representable: callers compute it.
        if e < s || e.checked_add(1).is_none() || total.is_some_and(|t| e >= t) {
            return None;
        }
    }
    Some(ContentRange { start, end, total })
}

/// SHA-256 of the whole file, read through the handle that wrote it.
fn sha256_handle(file: &mut File) -> io::Result<String> {
    file.seek(SeekFrom::Start(0))?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_range_parsing_is_strict() {
        assert!(parse_content_range("bytes 1-18446744073709551615/*").is_none());
        assert!(parse_content_range("bytes 0-18446744073709551615/18446744073709551616").is_none());
        assert!(parse_content_range("bytes 0-18446744073709551614/*").is_some());
        let r = parse_content_range("bytes 80-199/200").unwrap();
        assert_eq!((r.start, r.end, r.total), (Some(80), Some(199), Some(200)));
        let r = parse_content_range("bytes 80-199/*").unwrap();
        assert_eq!((r.start, r.end, r.total), (Some(80), Some(199), None));
        let r = parse_content_range("bytes */200").unwrap();
        assert_eq!((r.start, r.end, r.total), (None, None, Some(200)));
        for bad in [
            "bytes 80-garbage/*",
            "bytes garbage-90/*",
            "bytes 90-80/*",
            "bytes 0-200/200",
            "bytes 0-9",
            "items 0-9/10",
            "bytes 0-9/ten",
            "",
        ] {
            assert!(parse_content_range(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn resource_key_drops_query_and_fragment() {
        assert_eq!(
            resource_key("https://h.example/a/b?sig=1#x"),
            "https://h.example/a/b"
        );
    }

    /// Fresh scratch directory for one unit test.
    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("rightkit-http-unit-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    // ------------------------------------------------------------------ wire head
    fn observed(encoding: Option<&str>) -> WireHead {
        WireHead::Observed {
            content_encoding: encoding.map(str::to_string),
        }
    }

    #[test]
    fn an_unobserved_head_is_refused_not_read_as_no_encoding() {
        let err = check_wire_encoding(&WireHead::Unobserved, &[]).unwrap_err();
        assert!(
            matches!(&err, DownloadError::Http(HttpError::Stream(m)) if m.contains("not observed")),
            "{err:?}"
        );
        // Observed with no encoding is the only state that passes without a value.
        assert!(check_wire_encoding(&observed(None), &[]).is_ok());
    }

    #[test]
    fn only_identity_codings_pass() {
        assert!(check_wire_encoding(&observed(Some("identity")), &[]).is_ok());
        assert!(check_wire_encoding(&observed(Some(" Identity ")), &[]).is_ok());
        assert!(check_wire_encoding(&observed(Some("identity, identity")), &[]).is_ok());
        for bad in ["gzip", "identity, gzip", "br", "", "x-gzip"] {
            assert!(
                check_wire_encoding(&observed(Some(bad)), &[]).is_err(),
                "{bad:?} must be refused"
            );
        }
        // A header that survived (ureq did not decode) is checked when the wire gave none.
        let headers = vec![("content-encoding".to_string(), "gzip".to_string())];
        assert!(check_wire_encoding(&observed(None), &headers).is_err());
    }

    // ------------------------------------------------------------------ destination lock
    #[test]
    fn the_destination_lock_excludes_a_second_holder_until_released() {
        let dir = scratch("dest-lock");
        let dest = dir.join("out.bin");
        let first = DestLock::acquire(&dest).unwrap();
        // A second handle in the same process is as much a second writer as another process.
        let err = DestLock::acquire(&dest).expect_err("lock must be held");
        assert!(
            matches!(&err, DownloadError::Io(m) if m.contains("in progress")),
            "{err:?}"
        );
        // A different destination is independent.
        assert!(DestLock::acquire(&dir.join("other.bin")).is_ok());
        drop(first);
        assert!(DestLock::acquire(&dest).is_ok(), "released on drop");
        // The lock file stays: deleting it would let two holders lock different files.
        assert!(lock_path(&dest).exists());
        assert_eq!(lock_path(&dest), dir.join("out.bin.lock"));
        let _ = fs::remove_dir_all(&dir);
    }

    // ------------------------------------------------------------------ link swap
    fn link_file(target: &Path, link: &Path) -> io::Result<()> {
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(target, link)
        }
        #[cfg(windows)]
        {
            std::os::windows::fs::symlink_file(target, link)
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = (target, link);
            Err(io::Error::other("links are not supported on this platform"))
        }
    }

    /// The finding: `.part` is a regular file when inspected and a link to another file by
    /// the time it is opened. On Windows the open used to follow it, and a later `200`
    /// truncated the target. The open itself must refuse the link, before any write.
    #[test]
    fn a_part_swapped_for_a_link_after_the_check_is_never_followed() {
        let dir = scratch("swap-link");
        let victim = dir.join("victim.txt");
        fs::write(&victim, b"precious").unwrap();
        let part = dir.join("out.bin.part");
        fs::write(&part, b"partial").unwrap();

        let link_error = std::cell::RefCell::new(None::<String>);
        let result = open_part_with(&part, true, &|| {
            fs::remove_file(&part).unwrap();
            if let Err(e) = link_file(&victim, &part) {
                *link_error.borrow_mut() = Some(e.to_string());
            }
        });
        if let Some(reason) = link_error.into_inner() {
            // Windows needs Developer Mode or an elevated token to create a symlink. Say so
            // loudly: the swap was not exercised, so this run proves nothing about it.
            eprintln!(
                "SKIPPED a_part_swapped_for_a_link_after_the_check_is_never_followed: \
                 cannot create a symbolic link here ({reason})"
            );
            let _ = fs::remove_dir_all(&dir);
            return;
        }
        let err = result.expect_err("a link swapped in after the check must be refused");
        assert!(matches!(err, DownloadError::Io(_)), "{err:?}");
        assert_eq!(fs::read(&victim).unwrap(), b"precious");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_regular_part_is_opened_for_append_on_resume() {
        let dir = scratch("resume-open");
        let part = dir.join("out.bin.part");
        fs::write(&part, b"abc").unwrap();
        let mut file = open_part(&part, true).unwrap();
        file.write_all(b"def").unwrap();
        drop(file);
        assert_eq!(fs::read(&part).unwrap(), b"abcdef");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_same_file_check_notices_a_replaced_part() {
        let dir = scratch("same-file");
        let part = dir.join("out.bin.part");
        fs::write(&part, b"one").unwrap();
        let file = open_part(&part, true).unwrap();
        assert!(ensure_same_file(&file, &part).is_ok());
        // Replace the path with a different file while the first handle stays open. A
        // rename over an open file is allowed on Unix and, with the default share mode, on
        // Windows.
        let other = dir.join("other.bin");
        fs::write(&other, b"two").unwrap();
        if fs::rename(&other, &part).is_err() {
            eprintln!("SKIPPED the_same_file_check_notices_a_replaced_part: cannot replace an open file here");
            let _ = fs::remove_dir_all(&dir);
            return;
        }
        assert!(ensure_same_file(&file, &part).is_err());
        drop(file);
        let _ = fs::remove_dir_all(&dir);
    }
}
