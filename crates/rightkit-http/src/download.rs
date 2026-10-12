//! Resumable download to a file, with optional SHA-256 and length verification.
//!
//! Bytes go to a partial file, created without following links. On success the file is
//! fsynced, hashed through the same open handle and renamed to `dest`. The URL policy runs
//! before the filesystem is touched. Every request goes through the same policy layer as
//! [`Client::send`]: timeouts, retry/backoff, redirect and size settings.
//!
//! # Where the download's own files live
//!
//! Nothing is created beside `dest`. The two files a destination needs sit in one hidden
//! directory, `.rk-download`, in the destination's parent. It is created owner-only through
//! rightkit-fs (see "Privacy" below), and a file or link found at that path is refused. No destination in the parent can be one of these files, because they are not in the
//! parent:
//!
//! | File | Name inside `.rk-download` | Role |
//! |---|---|---|
//! | claim | `<key>.lock` | An exclusive OS lock held from before the partial file is opened until the rename or cleanup. Its content is the destination's exact file name |
//! | partial | `<key>.part` | The bytes received so far |
//!
//! `<key>` is the lowercase hex SHA-256 of the destination's file name: the raw bytes on Unix,
//! the UTF-16LE code units on Windows. It is only a file name. The identity of a destination is
//! the claim file, and the partial file is trusted only when the claim that was just locked
//! records this destination's name; a partial file under any other claim state is deleted and
//! the download starts from zero.
//!
//! A destination is refused, before any request or filesystem access, when it names a directory
//! (the path as given ends in a path separator, or its last component is `.` or `..`: `file.bin/`
//! and `file.bin/.` would otherwise be read as `file.bin` and replace it), when its file name or a
//! directory in its parent path is `.rk-download` (compared with a simple Unicode case fold, so
//! `.RK-DOWNLOAD` and a spelling with the Kelvin sign U+212A are the same name), when the name ends in a dot
//! or space, and on Windows when it contains `:`, is a device name, or contains `~<digit>`. After
//! the URL policy and before any file is created, the destination's parent is also resolved
//! (links followed, the directory must exist) and refused with the same error when any component
//! of the resolved path is `.rk-download`, so `alias/<key>.lock` is refused when `alias` is a link
//! to a `.rk-download`. The resolved parent is the one the claim, the partial file and the rename
//! use. A destination that is an alias for the directory itself in some other way fails at the
//! rename, because a file cannot replace a directory.
//!
//! # Privacy
//!
//! One rule: a download is always private, and it is the one file this call created or made
//! private. The mode bits alone do not keep bytes private (on macOS an ACL `allow` entry
//! inherited from the parent is evaluated before them, on Windows a new file inherits the
//! parent's access), so the directory and the partial file are created through rightkit-fs
//! (`ensure_private_dir`, `create_private_new`): owner-only from the first instant (`0700`/`0600`
//! on Unix, a protected owner-only DACL on Windows), and on macOS never under a parent whose ACL
//! would hand them an inheritable `allow` entry, which is refused with
//! [`DownloadError::UnsafeAcl`] before anything is created or requested (an ACL made only of
//! `deny` entries is accepted). Other Unixes are mode-only.
//!
//! A new partial is the file this call created: its creation handle is kept until it has been
//! compared with the handle the download writes through, and a different file there fails the
//! call. A partial left by an earlier call is made private by this call before any byte is
//! appended (rightkit-fs `restrict_owner_only`, by path: on Unix the mode is narrowed to `0600`
//! and, on macOS, an ACL `allow` entry is refused with [`DownloadError::UnsafeAcl`]; on Windows
//! the whole DACL is replaced by one protected entry for the current user). If that fails, the
//! call fails before any request and the file is neither appended to nor deleted. On Unix
//! `is_owner_only` is then checked as well ([`DownloadError::PartialNotPrivate`] if it still
//! disagrees); on Windows it cannot test, so only the replacement is relied on.
//!
//! Publication copies nothing from the file it replaces: the staged file is renamed into place
//! as it is, owner-only, so the published file is `0600` on Unix and carries the owner-only
//! DACL on Windows, whatever mode, ACL or owner the replaced file had. A caller that wants
//! other access sets it after the call.
//!
//! The destination's parent is resolved on every platform (on Windows the resolved path has the
//! `\\?\` form and is used only inside this module; a junction in the parent is resolved too).
//!
//! Limits, stated rather than hidden:
//!
//! - Two spellings that the filesystem treats as one file (`Out.bin` and `out.bin` on a
//!   case-insensitive volume, or two Unicode forms of one name on APFS) have different keys, so
//!   they are two independent downloads with two partial files. They are not excluded from each
//!   other; each one publishes by an atomic rename and the later rename wins. No state is shared,
//!   so neither can corrupt the other's partial file.
//! - One claim file (`<key>.lock`, a few dozen bytes) stays in `.rk-download` for every
//!   destination name ever downloaded in that directory. It is never deleted, because deleting a
//!   lock file after unlocking it lets two holders lock different files.
//! - `.rk-download` and `dest` must be on one filesystem, or the final rename fails.
//! - Only the last component of each file is opened without following links. A caller who can
//!   swap `.rk-download` itself for a link between the check and the open can already write
//!   anywhere the parent allows. The same holds for a component of the destination's parent
//!   swapped for a link after the parent was resolved: the check covers the path as it was at
//!   that moment, not later.
//!
//! # Resume and restart decisions
//!
//! Rule: bytes from an earlier representation are never published unless a strong validator
//! proves the representation is unchanged. The validators compared are the strong `ETag`
//! (sent as `If-Range`; a `W/` tag is never strong), `Last-Modified` (compared for
//! contradiction only: a date is not a strong validator here), the `Content-Length` of a
//! `200` and the `Content-Range` total of a `206` (the saved total), the saved resource
//! key (final URL, query kept, fragment dropped), and the caller's `expected_sha256`, which
//! proves the finished file whatever its history. "Proven" below means: a strong ETag saw this
//! call, or `expected_sha256` is set. `expected_bytes` is never proof.
//!
//! Before a request, with N > 0 bytes on disk:
//!
//! | Prefix | Decision |
//! |---|---|
//! | Proven | Send `Range: bytes=N-`, plus `If-Range` with the strong ETag when there is one |
//! | Not proven (a partial file from an earlier call without `expected_sha256`; a `200` with no ETag or a weak one) | Discard it and request from zero, with no `Range` |
//!
//! Answer to a ranged request (everything "restart" truncates the partial file and forgets all
//! saved validators, then asks again from zero):
//!
//! | Server answer | Decision |
//! |---|---|
//! | `200` full body | Restart: this body replaces the prefix; its validators are saved |
//! | `206`, same resource, no contradiction, `start == N` | Append |
//! | `206`, other resource (redirect landed elsewhere) | Restart |
//! | `206`, ETag or `Last-Modified` differs from the saved one | Restart |
//! | `206`, total differs from the saved total (shrank or grew), or range ends past it | Restart |
//! | `206`, malformed `Content-Range`, `start != N`, or `Content-Length` disagrees | Error, nothing published |
//! | `206` with no validators at all, prefix proven | Append (the `If-Range` or the hash decides) |
//! | `416` | Restart, whatever its `Content-Range` says: a `416` is never a completion, so it can never publish the saved bytes |
//! | `304` | Error, nothing published (no conditional header except `If-Range` is sent) |
//! | redirect to a different resource | Followed by the redirect policy; the answer is then one of the rows above and the resource key decides |
//! | any other status | Error, nothing published |
//!
//! A `416` to a request that carried no `Range` is an error and publishes nothing. After a
//! restart the next request has no `Range`, so a server that answers every request with `416`
//! ends the download with that error instead of looping.
//!
//! After the body: length against the advertised total, `expected_bytes`, then
//! `expected_sha256` over the whole file; any mismatch deletes the partial file.

use std::ffi::OsStr;
use std::fmt;
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};
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
    /// Keep an existing partial file (see the module docs for where it lives) and continue it
    /// with a `Range` request.
    pub resume: bool,
    /// Checked before every read and every retry sleep. Setting it keeps the partial file.
    pub cancel: Option<&'a AtomicBool>,
    /// Called after each body chunk is written.
    pub on_progress: Option<&'a mut dyn FnMut(&DownloadProgress)>,
    /// Mid-body failures to resume after before giving up (the partial file is kept).
    pub max_reconnects: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DownloadProgress {
    /// Bytes present in the partial file, including any resumed prefix.
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
#[non_exhaustive]
pub enum DownloadError {
    Http(HttpError),
    Io(String),
    /// Cancelled; the partial file is kept and holds `partial_bytes` bytes.
    Cancelled {
        partial_bytes: u64,
    },
    /// Hash mismatch; the partial file was deleted.
    Integrity {
        expected: String,
        actual: String,
    },
    /// Length mismatch; the partial file was deleted.
    LengthMismatch {
        expected: u64,
        actual: u64,
    },
    /// The download's own directory or partial file cannot be made owner-only, because an
    /// access list entry that grants access to others would be inherited from the directory
    /// that holds it (or is on `path` itself, as on a leftover partial, which is not repaired).
    /// `path` is the `.rk-download` directory or the partial file inside it. Nothing was
    /// written. macOS:
    /// extended ACL `allow` entries (a `deny`-only list never causes this). The error is
    /// raised before any request when the directory is the cause.
    UnsafeAcl {
        path: PathBuf,
    },
    /// Unix: a partial file left by an earlier call still reads as readable by group or others
    /// after this call narrowed it (the check that follows `restrict_owner_only`). It was
    /// neither used nor deleted: nothing was appended to it and no request was sent.
    PartialNotPrivate {
        path: PathBuf,
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
            DownloadError::UnsafeAcl { path } => write!(
                f,
                "download refused: {} cannot be kept private, because an access list entry on it \
                 or on the directory that holds it would give other users access to the \
                 downloaded bytes",
                path.display()
            ),
            DownloadError::PartialNotPrivate { path } => write!(
                f,
                "download refused: the leftover partial file {} is not private (group or others \
                 can read it, by mode or by access list); it was neither used nor deleted",
                path.display()
            ),
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
    /// `Last-Modified` of the representation that supplied the bytes on disk.
    last_modified: Option<String>,
    /// Resource (final URL, query kept, fragment dropped) that supplied the bytes on disk and
    /// the ETag.
    source: Option<String>,
    attempts: u32,
    reconnects: u32,
}

impl FetchState {
    /// The saved ETag when it is a strong validator (not `W/`), the only kind `If-Range`
    /// accepts and the only one that can prove the bytes on disk are unchanged.
    fn strong_etag(&self) -> Option<&str> {
        self.etag.as_deref().filter(|t| is_strong_etag(t))
    }

    /// Forget the prefix and every validator that described it.
    fn forget_prefix(&mut self) {
        self.written = 0;
        self.resumed_from = 0;
        self.etag = None;
        self.last_modified = None;
        self.source = None;
        self.total = None;
        self.limit = None;
    }
}

/// An entity tag is strong when it is a quoted opaque tag without the `W/` prefix.
fn is_strong_etag(tag: &str) -> bool {
    let tag = tag.trim();
    !tag.starts_with("W/") && tag.starts_with('"')
}

/// Both values present and different: the new response describes another representation.
fn contradicts(saved: &Option<String>, seen: Option<&str>) -> bool {
    matches!((saved.as_deref(), seen), (Some(a), Some(b)) if a.trim() != b.trim())
}

/// Truncate the partial file and drop everything known about it, so the next request starts at
/// zero.
fn restart_from_zero(file: &mut File, st: &mut FetchState) -> Result<(), DownloadError> {
    file.set_len(0).map_err(io_error("truncate partial file"))?;
    st.forget_prefix();
    Ok(())
}

impl Client {
    /// Download `url` to `dest`, resuming a partial file when `opts.resume` is set.
    ///
    /// Where the download's own files live: in a hidden `.rk-download` directory in the
    /// destination's parent, never beside `dest` (see the module docs). Resume rules (the full
    /// decision table is in the module docs): a non-empty partial file is extended only when
    /// something proves it unchanged, which is a strong ETag seen in this call (sent as
    /// `If-Range`) or `expected_sha256`; otherwise it is discarded and the download starts from
    /// zero. A `206` appends. A `200` truncates and restarts. A `416` always restarts from zero
    /// and never publishes the saved bytes. A mid-body failure reconnects from the current
    /// length, up to `max_reconnects` times. The saved prefix and its validators belong to the
    /// resource (final URL, query kept) that supplied them: a reconnect that lands on another
    /// resource, or whose ETag, `Last-Modified` or total contradicts what was saved, restarts
    /// from zero.
    ///
    /// The destination's parent must exist; it is resolved (links followed) before any file is
    /// created, and the download is refused when any component of the resolved path is
    /// `.rk-download`, and when the path as given names a directory (it ends in a separator, or
    /// its last component is `.` or `..`). The directory and the partial file are owner-only
    /// from creation and, on macOS, never under a parent whose ACL would give them an `allow`
    /// entry ([`DownloadError::UnsafeAcl`]); a leftover partial that is not private is not used
    /// ([`DownloadError::PartialNotPrivate`]). The published file is the staged file as it is:
    /// `0600` on Unix, the owner-only DACL on Windows. Nothing is copied from a destination it
    /// replaces (mode, ACL, owner); set other access after the call (see the module docs).
    ///
    /// Safety rules: the URL is checked against the address policy before any file is
    /// opened. The destination is then claimed: an exclusive OS lock on the destination's claim
    /// file (in `.rk-download`, never deleted) is taken before the partial file is opened and
    /// held until the download has been verified, cleaned up or renamed, so two calls (threads
    /// or processes) can never append to one partial file. A call that finds the claim held
    /// fails at once with [`DownloadError::Io`] and sends no request. The partial file is
    /// never opened through a symbolic link: an existing link is removed (never followed) and
    /// the file is created exclusively, or on resume opened with `O_NOFOLLOW` (Unix) or
    /// `FILE_FLAG_OPEN_REPARSE_POINT` plus a reparse-point check on the opened handle
    /// (Windows), so a link swapped in after the check is refused before anything is written.
    /// The bytes are hashed through the same handle that wrote them, after an fsync, and the
    /// path is compared with the handle (device and inode, or volume and file index) just
    /// before the rename. Completion is checked against the real length of the file, not only
    /// the count of bytes this call received. A body that runs past `expected_bytes` (or the
    /// declared range or length) stops the download and deletes the partial file. A response
    /// whose framing shows transparent content decoding is refused, because byte offsets would
    /// then describe decoded data; so is one whose head was not observed on the wire, because
    /// then an encoding cannot be ruled out.
    ///
    /// Note: validators are not stored on disk. A partial file left by an earlier call is
    /// resumed only when `expected_sha256` is set (the hash then decides what is published);
    /// without it the file is discarded and the download restarts.
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
        // The download's own directory is reserved: a destination inside it, or named like it,
        // would share a directory with other downloads' claim and partial files.
        reject_reserved_destination(dest)?;
        // Policy first: a denied URL must not touch the filesystem.
        self.preflight_url(url)?;
        // Resolve the destination's parent once and use the resolved path for the layout, the
        // claim, the partial file and the rename, so the directory that was checked is the
        // directory that is written. A parent reached through a link to `.rk-download` is
        // refused here, before any coordination file exists.
        let target = resolve_destination(dest)?;
        // The claim and partial file are in a hidden directory of the destination's parent,
        // never beside `dest`. One writer per destination, from before the partial file is
        // opened until after the rename. The claim is declared before `file`, so it is
        // released after the handle is closed.
        let layout = Layout::for_destination(&target)?;
        ensure_coordination_dir(&layout.dir)?;
        let _claim = Claim::acquire(&layout)?;
        let part: &Path = &layout.part;
        let mut file = open_part(part, opts.resume)?;
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
                let _ = fs::remove_file(part);
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
            remove_part(part)?;
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
            remove_part(part)?;
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
                remove_part(part)?;
                return Err(DownloadError::Integrity {
                    expected: expected.clone(),
                    actual,
                });
            }
        }
        ensure_same_file(&file, part).map_err(io_error("check partial file"))?;
        // The staged file is published exactly as staged: owner-only, nothing copied from the
        // file it replaces (mode, ACL, owner).
        fs::rename(part, &target).map_err(io_error("rename into place"))?;
        drop(file);
        sync_parent(&target);
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
            // A prefix nothing proves is not extended: see the decision table in the module
            // docs. It is discarded before a ranged request can splice it onto other bytes.
            if st.written > 0 && !prefix_is_proven(st, opts) {
                restart_from_zero(file, st)?;
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
                if let Some(tag) = st.strong_etag() {
                    req = req.set_header("If-Range", tag.to_string());
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
                    let seen_etag = header_of(&headers, "etag");
                    let seen_modified = header_of(&headers, "last-modified");
                    let other_representation = st.source.as_deref().is_some_and(|s| s != source)
                        || contradicts(&st.etag, seen_etag)
                        || contradicts(&st.last_modified, seen_modified)
                        || matches!((st.total, range.total), (Some(a), Some(b)) if a != b)
                        || st.total.is_some_and(|t| end_exclusive > t);
                    if other_representation {
                        // Another resource, other validators, or a length that grew or
                        // shrank: the saved prefix describes something else. Start over.
                        restart_from_zero(file, st)?;
                        continue;
                    }
                    if range.total.is_some() {
                        st.total = range.total;
                    }
                    st.limit = Some(end_exclusive);
                    if st.etag.is_none() {
                        st.etag = seen_etag.map(str::to_string);
                    }
                    if st.last_modified.is_none() {
                        st.last_modified = seen_modified.map(str::to_string);
                    }
                    st.source.get_or_insert(source);
                }
                200 => {
                    if offset > 0 {
                        restart_from_zero(file, st)?;
                    }
                    st.total = header_of(&headers, "content-length")
                        .and_then(|v| v.trim().parse::<u64>().ok());
                    st.limit = st.total;
                    st.etag = header_of(&headers, "etag").map(str::to_string);
                    st.last_modified = header_of(&headers, "last-modified").map(str::to_string);
                    st.source = Some(source);
                }
                416 if offset > 0 => {
                    // A 416 never completes a download, whatever its Content-Range says: it
                    // describes the server's current object, not the bytes on disk, and
                    // nothing here can prove the two are the same. Discard the prefix; the
                    // next request has no Range, so a 416 to it is the error arm below.
                    restart_from_zero(file, st)?;
                    continue;
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
                        append_part(file, &buf[..n]).map_err(io_error("write partial file"))?;
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

/// Whether the bytes already on disk may be extended: a strong ETag seen in this call (the
/// server then checks it through `If-Range`), or an expected SHA-256 that the finished file
/// must match. Anything else (a partial file left by an earlier call, or a `200` with no or only a
/// weak ETag) could be an older representation, so it is not extended.
fn prefix_is_proven(st: &FetchState, opts: &DownloadOptions<'_>) -> bool {
    st.strong_etag().is_some() || opts.expected_sha256.is_some()
}

/// Final URL without its fragment: the identity of the resource that answered. The query is
/// part of it, because `/download?id=A` and `/download?id=B` are different resources. A
/// redirect that re-signs its query on every request therefore restarts the download
/// instead of resuming it, which is slower but never splices two resources.
fn resource_key(final_url: &str) -> String {
    final_url.split('#').next().unwrap_or(final_url).to_string()
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

/// Open the partial file without following a link at that path.
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
        let mut existing = part_options();
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
    // A partial with another name is the same bytes under both: appending to it, or
    // truncating it on a `200`, would rewrite the other file as well.
    if link_count(&file, &meta).map_err(io_error("stat partial file"))? > 1 {
        return Err(DownloadError::Io(
            "partial path is a hard link to another file".into(),
        ));
    }
    // Made private by this call before any byte is appended, a partial from an earlier call
    // included (it may have been made by an older version or another tool): on Unix the mode is
    // narrowed to `0600` and, on macOS, an ACL `allow` entry is refused (`UnsafeAcl`, the
    // file is neither used nor deleted); on Windows the whole DACL is replaced by one protected
    // entry for the current user. After the hard-link check, so another name for the same inode
    // is never changed. By path: nothing is opened here. A failure is an error before any
    // request.
    rightkit_fs::restrict_owner_only(part).map_err(fs_error("make partial file private"))?;
    // Then the proof, on Unix (rightkit-fs cannot test for it on Windows and reports true).
    #[cfg(unix)]
    if !rightkit_fs::private::is_owner_only(part).map_err(fs_error("check partial file"))? {
        return Err(DownloadError::PartialNotPrivate {
            path: part.to_path_buf(),
        });
    }
    #[cfg(windows)]
    let file = {
        let mut file = file;
        file.seek(SeekFrom::End(0))
            .map_err(io_error("seek partial file"))?;
        file
    };
    Ok(file)
}

/// How the partial file is opened: for reading and for writes that land at its end.
///
/// Unix uses append mode. On Windows an append-only handle may not truncate the file (it lacks
/// `FILE_WRITE_DATA`, so `set_len` is refused with access denied), and a restart has to
/// truncate; the handle is opened for plain writing there and `append_part` seeks to the end
/// before each write.
fn part_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    #[cfg(not(windows))]
    options.read(true).append(true);
    #[cfg(windows)]
    options.read(true).write(true);
    options
}

/// Write `bytes` at the end of the partial file, wherever the handle was last read from.
fn append_part(file: &mut File, bytes: &[u8]) -> io::Result<()> {
    #[cfg(windows)]
    file.seek(SeekFrom::End(0))?;
    file.write_all(bytes)
}

/// Create the partial file: exclusively, owner-only from the first instant and refusing an
/// ACL that would hand it access (rightkit-fs `create_private_new`), then open it for reading
/// and appending. `create_private_new` fails on any existing entry, a link included, and
/// never follows one (`O_EXCL | O_NOFOLLOW` on Unix, `CREATE_NEW` on Windows). Its handle is
/// write-only, so the file is opened again by path without following a link, which gives the
/// read access the hash needs. The creation handle stays open until the two handles are
/// compared ([`same_open_file`]): if another process renamed a different file over the empty
/// partial in between, the reopened handle is a different file and the call fails, writing to
/// nothing and deleting nothing.
fn create_new(part: &Path) -> Result<File, DownloadError> {
    let created = rightkit_fs::create_private_new(part).map_err(fs_error("create partial file"))?;
    let file =
        open_nofollow(&mut part_options(), part).map_err(io_error("open new partial file"))?;
    if !same_open_file(&created, &file).map_err(io_error("compare new partial file"))? {
        return Err(DownloadError::Io(
            "the partial file was replaced during creation; nothing was written to it".into(),
        ));
    }
    drop(created);
    Ok(file)
}

/// Name of the hidden directory, in the destination's parent, that holds the claim and the
/// partial file of every destination in that parent.
const COORD_DIR: &str = ".rk-download";

/// What this module keeps for one destination, all inside [`COORD_DIR`] in the destination's
/// parent, and the destination's exact file name.
struct Layout {
    /// The hidden directory itself.
    dir: PathBuf,
    /// `<key>.lock`: the claim file.
    claim: PathBuf,
    /// `<key>.part`: the partial file.
    part: PathBuf,
    /// The destination's file name, exactly (see [`name_bytes`]). The claim file records it.
    name: Vec<u8>,
}

impl Layout {
    /// `<key>` is the lowercase hex SHA-256 of the destination's file name. It is only a file
    /// name: what makes the files this destination's is the claim record, checked after the
    /// claim is locked. Pure: nothing is read or created.
    fn for_destination(dest: &Path) -> Result<Self, DownloadError> {
        let Some(file_name) = dest.file_name() else {
            return Err(DownloadError::Io(
                "destination has no file name (it ends in `..` or is a root)".into(),
            ));
        };
        let name = name_bytes(file_name);
        let key: String = Sha256::digest(&name)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let dir = dest.parent().unwrap_or(Path::new("")).join(COORD_DIR);
        Ok(Self {
            claim: dir.join(format!("{key}.lock")),
            part: dir.join(format!("{key}.part")),
            dir,
            name,
        })
    }
}

/// The exact bytes of a file name: the raw bytes on Unix, the UTF-16LE code units on Windows.
#[cfg(unix)]
fn name_bytes(name: &OsStr) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    name.as_bytes().to_vec()
}

#[cfg(windows)]
fn name_bytes(name: &OsStr) -> Vec<u8> {
    use std::os::windows::ffi::OsStrExt;
    name.encode_wide().flat_map(u16::to_le_bytes).collect()
}

#[cfg(not(any(unix, windows)))]
fn name_bytes(name: &OsStr) -> Vec<u8> {
    name.to_string_lossy().into_owned().into_bytes()
}

/// Make sure the hidden directory exists, is a real directory and is owner-only
/// (rightkit-fs `ensure_private_dir`). It is created owner-only (`0700` on Unix, a protected
/// owner-only DACL on Windows), and one that already exists is narrowed the same way. On macOS
/// the directory is not created under a parent whose ACL would hand it an inheritable `allow`
/// entry, and an existing one that carries an `allow` entry is refused
/// ([`DownloadError::UnsafeAcl`]). A file or link at that path is refused: nothing is created
/// through it. The parent must exist already, as it must for the destination itself.
fn ensure_coordination_dir(dir: &Path) -> Result<(), DownloadError> {
    rightkit_fs::ensure_private_dir(dir).map_err(|e| {
        match e {
        rightkit_fs::FsError::Symlink(_) | rightkit_fs::FsError::NotADirectory(_) => {
            DownloadError::Io(
                "the .rk-download path exists and is not a plain directory (it is a file or a link)"
                    .into(),
            )
        }
        other => fs_error("create download directory")(other),
    }
    })
}

/// Map a rightkit-fs error: an access-list refusal keeps its type, anything else becomes the
/// `Io` text it always was.
fn fs_error(what: &'static str) -> impl Fn(rightkit_fs::FsError) -> DownloadError {
    move |e| match e {
        rightkit_fs::FsError::UnsafeAcl(path) => DownloadError::UnsafeAcl { path },
        rightkit_fs::FsError::Io { source, .. } => DownloadError::Io(format!("{what}: {source}")),
        other => DownloadError::Io(format!("{what}: {other}")),
    }
}

/// Exclusive cross-process claim on one destination, held from before the partial file is
/// opened until the download is verified, cleaned up or renamed.
///
/// It is an OS advisory lock on the destination's claim file (`flock` on Unix, `LockFileEx` on
/// Windows, both through `File::try_lock`), so the OS drops it when the holder exits or is
/// killed: there is no stale lock to clean up. Separate handles exclude each other, inside one
/// process as well as between processes. The claim file is never deleted: removing it after
/// unlock would let one process lock the old file while another creates and locks a new one at
/// the same path. A holder is not waited for; a call that finds the claim held fails at once.
///
/// The claim file's content is the destination's exact file name. It is read and, if needed,
/// written only after the lock is held, and the partial file is trusted only when the record
/// matched: see [`Claim::record`].
#[derive(Debug)]
struct Claim {
    file: File,
}

impl Claim {
    fn acquire(layout: &Layout) -> Result<Self, DownloadError> {
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        let file = open_nofollow(&mut options, &layout.claim)
            .map_err(io_error("open destination claim"))?;
        let meta = file
            .metadata()
            .map_err(io_error("stat destination claim"))?;
        if !meta.is_file() {
            return Err(DownloadError::Io(
                "destination claim path is not a regular file".into(),
            ));
        }
        // The record is written into this file, so it must not be a second name for another.
        if link_count(&file, &meta).map_err(io_error("stat destination claim"))? > 1 {
            return Err(DownloadError::Io(
                "destination claim path is a hard link to another file".into(),
            ));
        }
        match file.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => {
                return Err(DownloadError::Io(
                    "another download of this destination is in progress (its claim is held)"
                        .into(),
                ))
            }
            Err(TryLockError::Error(e)) => return Err(io_error("lock destination")(e)),
        }
        let mut claim = Self { file };
        claim.record(layout)?;
        Ok(claim)
    }

    /// With the claim held: when the claim file already names this destination, any partial
    /// file beside it was made under this claim and stays. Otherwise (a first claim, or a file
    /// that something else made) a partial file is not trusted: it is removed, then the name is
    /// recorded. The record is written first and the partial file created after, so a partial
    /// file whose claim lacks the record can only have been put there by someone else.
    fn record(&mut self, layout: &Layout) -> Result<(), DownloadError> {
        let mut found = Vec::new();
        (&self.file)
            .take(layout.name.len() as u64 + 1)
            .read_to_end(&mut found)
            .map_err(io_error("read destination claim"))?;
        if found == layout.name {
            return Ok(());
        }
        match fs::remove_file(&layout.part) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(io_error("remove untrusted partial file")(e)),
        }
        self.file
            .set_len(0)
            .map_err(io_error("write destination claim"))?;
        self.file
            .seek(SeekFrom::Start(0))
            .map_err(io_error("write destination claim"))?;
        self.file
            .write_all(&layout.name)
            .map_err(io_error("write destination claim"))
    }
}

impl Drop for Claim {
    fn drop(&mut self) {
        // Explicit, so a descriptor inherited by a forked child cannot keep the claim busy.
        let _ = self.file.unlock();
    }
}

/// Refuse a destination that could land in the download's own directory. Runs before any
/// filesystem access.
///
/// A destination's name or a directory in its parent path may not be `.rk-download`, compared
/// with [`fold`]; [`destination_name_problem`] lists the other spellings of a name that are
/// refused. Nothing else is reserved: the claim and partial file are named by a hash and live
/// only in that directory, so no other name in the parent can reach them.
fn reject_reserved_destination(dest: &Path) -> Result<(), DownloadError> {
    // Before anything is normalised: `file_name()` drops a trailing separator and a trailing
    // `.`, so `important.bin/.` and `important.bin/` would be read as `important.bin` and a
    // rename onto the rebuilt path would replace that file. The caller's own path names a
    // directory there (the rename onto it fails because the prefix is not one), so refuse it.
    if names_a_directory(&raw_units(dest), cfg!(windows)) {
        return Err(DownloadError::Io(
            "the destination names a directory (it ends in a path separator, `.` or `..`); \
             give the path of the file"
                .into(),
        ));
    }
    let Some(name) = dest.file_name() else {
        return Err(DownloadError::Io(
            "destination has no file name (it ends in `..` or is a root)".into(),
        ));
    };
    if let Some(why) = destination_name_problem(&name.to_string_lossy(), cfg!(windows)) {
        return Err(DownloadError::Io(why.into()));
    }
    if dest.parent().is_some_and(path_has_coordination_dir) {
        return Err(inside_coordination_dir());
    }
    Ok(())
}

/// The destination exactly as given: the bytes on Unix, the UTF-16 code units on Windows. No
/// lossy conversion, and no component parsing that would drop a trailing separator or `.`.
#[cfg(unix)]
fn raw_units(path: &Path) -> Vec<u32> {
    use std::os::unix::ffi::OsStrExt;
    path.as_os_str()
        .as_bytes()
        .iter()
        .map(|b| u32::from(*b))
        .collect()
}

#[cfg(windows)]
fn raw_units(path: &Path) -> Vec<u32> {
    use std::os::windows::ffi::OsStrExt;
    path.as_os_str().encode_wide().map(u32::from).collect()
}

#[cfg(not(any(unix, windows)))]
fn raw_units(path: &Path) -> Vec<u32> {
    path.to_string_lossy().chars().map(u32::from).collect()
}

/// Whether the raw path `units` names a directory rather than a file: it ends in a separator
/// (`/`; with `windows` also `\`), or its last component is `.` or `..`. `windows` is a
/// parameter so both rules are exercised on every platform.
fn names_a_directory(units: &[u32], windows: bool) -> bool {
    let is_separator = |unit: u32| unit == u32::from(b'/') || (windows && unit == u32::from(b'\\'));
    let Some(&last) = units.last() else {
        return false;
    };
    if is_separator(last) {
        return true;
    }
    let start = units
        .iter()
        .rposition(|unit| is_separator(*unit))
        .map_or(0, |index| index + 1);
    let dot = u32::from(b'.');
    matches!(&units[start..], [a] if *a == dot)
        || matches!(&units[start..], [a, b] if *a == dot && *b == dot)
}

/// Whether any component of `path` is the download directory's name ([`fold`]ed).
fn path_has_coordination_dir(path: &Path) -> bool {
    path.components().any(|component| {
        matches!(component, Component::Normal(dir_name)
            if names_coordination_dir(&dir_name.to_string_lossy()))
    })
}

fn inside_coordination_dir() -> DownloadError {
    DownloadError::Io(
        "destinations inside the download's own .rk-download directory are refused".into(),
    )
}

/// The destination with its parent directory resolved (every link in it followed, so no `..`
/// or alias is left), and the same refusal as [`reject_reserved_destination`] when any
/// component of the resolved parent is `.rk-download`. That catches a destination reached
/// through a link, such as `alias/<key>.lock` where `alias` links to a `.rk-download`, which
/// the lexical check cannot see. The final component is the file name and is not followed. The
/// parent must exist; nothing is created here.
///
/// The returned path is what the layout, the claim, the partial file and the rename use, so
/// the directory that was checked is the directory that is written. Not covered: a component
/// of the parent swapped for a link after this check. Every later open re-resolves the path;
/// there is no directory handle to anchor it, so a caller who can do that can already write
/// anywhere the parent allows.
fn resolve_destination(dest: &Path) -> Result<PathBuf, DownloadError> {
    let Some(name) = dest.file_name() else {
        return Err(DownloadError::Io(
            "destination has no file name (it ends in `..` or is a root)".into(),
        ));
    };
    let parent = match dest.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    let resolved = fs::canonicalize(parent).map_err(io_error("resolve destination directory"))?;
    if path_has_coordination_dir(&resolved) {
        return Err(inside_coordination_dir());
    }
    Ok(resolved.join(name))
}

/// Why `name` (a final path component) may not be a download destination, or `None`.
///
/// Rules, each for a way a name reaches the download's own directory:
/// - A name equal to `.rk-download` under a simple Unicode case fold ([`fold`]) is refused on
///   every platform. A file cannot replace a directory, so a name the filesystem equates with
///   the directory by some other rule fails at the rename and leaves everything in place.
/// - A trailing dot or space is refused on every platform. Windows drops them before the
///   lookup, so `.rk-download.` is `.rk-download` there.
/// - With `windows` set: a colon (an alternate data stream: `.rk-download::$DATA` names the
///   directory), a device name (`NUL`, `COM1`, `con.txt`: the device, whatever the extension),
///   and a `~` followed by a digit (an 8.3 short name such as `RK-DOW~1` may be the long name
///   `.rk-download`) are refused.
///
/// `windows` is a parameter so the Windows rules are exercised on every platform.
fn destination_name_problem(name: &str, windows: bool) -> Option<&'static str> {
    if name.ends_with('.') || name.ends_with(' ') {
        return Some(
            "destination names ending in a dot or space are refused: Windows drops them, so the \
             name would alias another destination or the download's own directory",
        );
    }
    if names_coordination_dir(name) {
        return Some("the destination name .rk-download is reserved for the download's own files");
    }
    if windows {
        if name.contains(':') {
            return Some("destination names containing ':' are refused (alternate data streams)");
        }
        let folded = name.to_ascii_lowercase();
        let stem = folded.split('.').next().unwrap_or("").trim_end();
        let device = matches!(stem, "con" | "prn" | "aux" | "nul" | "conin$" | "conout$")
            || (stem.len() == 4
                && (stem.starts_with("com") || stem.starts_with("lpt"))
                && stem.as_bytes()[3].is_ascii_digit());
        if device {
            return Some("destination names that are Windows device names are refused");
        }
        let bytes = folded.as_bytes();
        if bytes
            .windows(2)
            .any(|w| w[0] == b'~' && w[1].is_ascii_digit())
        {
            return Some("destination names with ~<digit> are refused (8.3 short-name aliases)");
        }
    }
    None
}

/// Whether `component` is the download directory's name, case-folded.
fn names_coordination_dir(component: &str) -> bool {
    fold(component) == fold(COORD_DIR)
}

/// Simple case fold using only `std`: each character upper-cased, then lower-cased. Unlike an
/// ASCII lowercase it maps the Kelvin sign (U+212A) and the long s (U+017F), which case-
/// insensitive volumes fold onto `k` and `s`, onto those letters. Unicode normalization is not
/// applied: the directory name is plain ASCII, and the only non-ASCII characters that normalize
/// to an ASCII letter are folded here already.
fn fold(name: &str) -> String {
    name.chars()
        .flat_map(char::to_uppercase)
        .flat_map(char::to_lowercase)
        .collect()
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

/// Number of directory entries that name the open file.
#[cfg(unix)]
fn link_count(_file: &File, meta: &fs::Metadata) -> io::Result<u64> {
    use std::os::unix::fs::MetadataExt;
    Ok(meta.nlink())
}

#[cfg(windows)]
fn link_count(file: &File, _meta: &fs::Metadata) -> io::Result<u64> {
    Ok(u64::from(handle_info(file)?.links))
}

#[cfg(not(any(unix, windows)))]
fn link_count(_file: &File, _meta: &fs::Metadata) -> io::Result<u64> {
    Ok(1)
}

/// Volume serial number and file index of an open handle: the identity of the file itself,
/// whatever path names it.
#[cfg(windows)]
fn file_key(file: &File) -> io::Result<(u32, u64)> {
    let info = handle_info(file)?;
    Ok((
        info.volume_serial,
        (u64::from(info.index_high) << 32) | u64::from(info.index_low),
    ))
}

#[cfg(windows)]
fn handle_info(file: &File) -> io::Result<ByHandleFileInformation> {
    use std::os::windows::io::AsRawHandle;
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
    Ok(unsafe { info.assume_init() })
}

#[cfg(windows)]
#[repr(C)]
struct FileTime {
    low: u32,
    high: u32,
}

#[cfg(windows)]
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

/// Whether two open handles are the same file: device and inode on Unix, volume serial number
/// and file index on Windows. [`ensure_same_file`] compares a handle with a path; this compares
/// two handles.
fn same_open_file(a: &File, b: &File) -> io::Result<bool> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let (a, b) = (a.metadata()?, b.metadata()?);
        Ok(a.dev() == b.dev() && a.ino() == b.ino())
    }
    #[cfg(windows)]
    {
        Ok(file_key(a)? == file_key(b)?)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (a, b);
        Ok(true)
    }
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
    fn resource_key_keeps_the_query_and_drops_the_fragment() {
        assert_eq!(
            resource_key("https://h.example/a/b?sig=1#x"),
            "https://h.example/a/b?sig=1"
        );
        assert_ne!(
            resource_key("https://h.example/d?id=A"),
            resource_key("https://h.example/d?id=B")
        );
    }

    /// The only reserved name is the download directory. `.lock` and `.part` are ordinary
    /// destination names now: the claim and partial file are not beside the destination.
    #[test]
    fn the_download_directory_is_refused_as_a_name_and_as_a_parent() {
        for bad in [
            ".rk-download",
            ".RK-DOWNLOAD",
            ".Rk-Download",
            // Kelvin sign (U+212A): a case-insensitive volume folds it onto `k`.
            ".r\u{212A}-download",
            "dir/.rk-download",
            "dir/.rk-download/out.bin",
            "dir/.RK-Download/out.bin",
            "dir/.rk-download/sub/out.bin",
            ".r\u{212A}-download/out.bin",
        ] {
            assert!(
                matches!(
                    reject_reserved_destination(Path::new(bad)),
                    Err(DownloadError::Io(_))
                ),
                "{bad}"
            );
        }
        for ok in [
            "out",
            "out.bin",
            "lock",
            "out.lock",
            "OUT.LOCK",
            "out.part",
            "dir/x.Part",
            "out.locked",
            "out.lock.bin",
            ".rk-download.bin",
            "rk-download",
            "dir/.rk-downloads/out.bin",
        ] {
            assert!(reject_reserved_destination(Path::new(ok)).is_ok(), "{ok}");
        }
    }

    #[test]
    fn names_that_windows_normalizes_onto_the_download_directory_are_refused() {
        // Trailing dots and spaces are refused everywhere, so the check does not depend on
        // the platform that runs it.
        for bad in [
            ".rk-download.",
            ".rk-download..",
            ".rk-download ",
            ".RK-DOWNLOAD. ",
            "out.lock.",
            "out.part ",
            "out.",
            "out ",
            ".",
        ] {
            assert!(destination_name_problem(bad, false).is_some(), "{bad:?}");
            assert!(destination_name_problem(bad, true).is_some(), "{bad:?}");
        }
        for bad in [
            ".rk-download::$DATA",
            ".rk-download:stream",
            "out.bin:stream",
            "NUL",
            "nul.txt",
            "Com1",
            "lpt9.log",
            "CON",
            "RK-DOW~1",
            "x~2",
        ] {
            assert!(destination_name_problem(bad, true).is_some(), "{bad:?}");
            // The extra rules are Windows-only: a Unix name may hold ':' or '~1'.
            assert!(destination_name_problem(bad, false).is_none(), "{bad:?}");
        }
        for ok in [
            "out",
            "out.bin",
            "lock",
            "out.lock",
            "out.part",
            "out.locked",
            "out.lock.bin",
            "a~b",
            "com",
            "com10",
        ] {
            assert!(destination_name_problem(ok, true).is_none(), "{ok:?}");
            assert!(destination_name_problem(ok, false).is_none(), "{ok:?}");
        }
    }

    #[test]
    fn a_destination_without_a_file_name_is_refused() {
        for bad in ["..", "dir/..", "/"] {
            assert!(
                matches!(
                    reject_reserved_destination(Path::new(bad)),
                    Err(DownloadError::Io(_))
                ),
                "{bad}"
            );
        }
    }

    fn units(path: &str) -> Vec<u32> {
        path.chars().map(u32::from).collect()
    }

    /// `file.bin/` and `file.bin/.` are read by `file_name()` as `file.bin`; the raw path is
    /// what decides, so they are refused before any normalisation.
    #[test]
    fn a_path_that_names_a_directory_is_refused_before_it_is_normalised() {
        for bad in [
            "file.bin/",
            "file.bin/.",
            "file.bin//",
            "file.bin/./",
            "dir/..",
            "a/b/..",
            ".",
            "..",
            "/",
        ] {
            assert!(names_a_directory(&units(bad), false), "{bad}");
            assert!(names_a_directory(&units(bad), true), "{bad}");
            assert!(
                matches!(
                    reject_reserved_destination(Path::new(bad)),
                    Err(DownloadError::Io(m)) if m.contains("names a directory")
                ),
                "{bad}"
            );
        }
        for ok in [
            "file.bin",
            "dir/file.bin",
            ".hidden",
            "a/.b",
            "a/..b",
            "a/...",
            "a/b.",
            "",
        ] {
            assert!(!names_a_directory(&units(ok), false), "{ok:?}");
            assert!(!names_a_directory(&units(ok), true), "{ok:?}");
        }
        // A backslash separates only on Windows: on Unix `file.bin\\` is a file name.
        for windows_only in ["file.bin\\", "dir\\.", "dir\\.."] {
            assert!(
                names_a_directory(&units(windows_only), true),
                "{windows_only}"
            );
            assert!(
                !names_a_directory(&units(windows_only), false),
                "{windows_only}"
            );
        }
    }

    /// A partial that is a second name for another file must be refused before any write or
    /// truncation, and the other name must keep its bytes.
    #[test]
    fn a_hard_linked_part_is_refused_and_its_other_name_untouched() {
        let dir = scratch("hard-link");
        let victim = dir.join("precious.txt");
        fs::write(&victim, b"precious").unwrap();
        let part = dir.join("out.bin.part");
        if let Err(e) = fs::hard_link(&victim, &part) {
            panic!("cannot create a hard link here ({e}); this test must run");
        }
        for resume in [true, false] {
            let r = open_part(&part, resume);
            if resume {
                let err = r.expect_err("a hard-linked partial must be refused");
                assert!(matches!(err, DownloadError::Io(_)), "{err:?}");
            } else {
                // Without resume the stale name is removed and a fresh file created.
                assert!(r.is_ok());
            }
            assert_eq!(fs::read(&victim).unwrap(), b"precious");
        }
        let _ = fs::remove_dir_all(&dir);
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

    // ------------------------------------------------------------------ layout and claim
    fn layout_in(dir: &Path, name: &str) -> Layout {
        let layout = Layout::for_destination(&dir.join(name)).unwrap();
        ensure_coordination_dir(&layout.dir).unwrap();
        layout
    }

    /// Nothing the download keeps is beside the destination, and no destination spelling in
    /// the parent can name it: the files are in `.rk-download`, named by a hash.
    #[test]
    fn the_download_files_are_in_the_hidden_directory_under_a_hash_name() {
        let dir = scratch("layout");
        let layout = Layout::for_destination(&dir.join("out.bin")).unwrap();
        assert_eq!(layout.dir, dir.join(COORD_DIR));
        assert_eq!(layout.claim.parent(), Some(layout.dir.as_path()));
        assert_eq!(layout.part.parent(), Some(layout.dir.as_path()));
        let claim = layout
            .claim
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let part = layout
            .part
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let key = claim.strip_suffix(".lock").expect("claim ends in .lock");
        assert_eq!(part, format!("{key}.part"));
        assert_eq!(key.len(), 64);
        assert!(key
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)));
        // A bare file name resolves against the current directory.
        let bare = Layout::for_destination(Path::new("out.bin")).unwrap();
        assert_eq!(bare.dir, Path::new(COORD_DIR));
        let _ = fs::remove_dir_all(&dir);
    }

    /// Two spellings get two sets of files, whatever the filesystem thinks of the spellings,
    /// so neither can reach the other's partial file; the same spelling gets the same files.
    #[test]
    fn different_spellings_have_different_files_and_one_spelling_has_one_set() {
        let dir = scratch("spellings");
        let a = Layout::for_destination(&dir.join("out.bin")).unwrap();
        let again = Layout::for_destination(&dir.join("out.bin")).unwrap();
        assert_eq!((&a.claim, &a.part), (&again.claim, &again.part));
        for other in [
            "Out.bin",
            "OUT.BIN",
            "out.bin ",
            "out.bi\u{212A}n",
            "out.b\u{0131}n",
        ] {
            let b = Layout::for_destination(&dir.join(other)).unwrap();
            assert_ne!(a.claim, b.claim, "{other:?}");
            assert_ne!(a.part, b.part, "{other:?}");
            assert_eq!(a.dir, b.dir, "{other:?}");
        }
        // A destination named like the old beside-the-destination files shares nothing.
        for old in ["out.bin.lock", "out.bin.part"] {
            let b = Layout::for_destination(&dir.join(old)).unwrap();
            assert_ne!(a.claim, b.claim, "{old}");
            assert_ne!(a.part, b.part, "{old}");
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_destination_claim_excludes_a_second_holder_until_released() {
        let dir = scratch("claim");
        let layout = layout_in(&dir, "out.bin");
        let first = Claim::acquire(&layout).unwrap();
        // A second handle in the same process is as much a second writer as another process.
        let err = Claim::acquire(&layout).expect_err("claim must be held");
        assert!(
            matches!(&err, DownloadError::Io(m) if m.contains("in progress")),
            "{err:?}"
        );
        // A different destination in the same directory is independent.
        assert!(Claim::acquire(&layout_in(&dir, "other.bin")).is_ok());
        drop(first);
        assert!(Claim::acquire(&layout).is_ok(), "released on drop");
        // The claim file stays: deleting it would let two holders lock different files.
        assert!(layout.claim.exists());
        let _ = fs::remove_dir_all(&dir);
    }

    /// The claim file is the record of whose partial file this is. A partial file is kept only
    /// under a claim that names the destination; under any other claim state it is removed.
    #[test]
    fn a_partial_file_is_trusted_only_under_a_claim_that_names_its_destination() {
        let dir = scratch("claim-record");
        let layout = layout_in(&dir, "out.bin");

        // A partial file with no claim at all was not made by a download of this destination.
        fs::write(&layout.part, b"planted").unwrap();
        drop(Claim::acquire(&layout).unwrap());
        assert!(
            !layout.part.exists(),
            "an unclaimed partial file is removed"
        );
        assert_eq!(fs::read(&layout.claim).unwrap(), layout.name);

        // A partial file made under the recorded claim survives the next claim.
        fs::write(&layout.part, b"mine").unwrap();
        drop(Claim::acquire(&layout).unwrap());
        assert_eq!(fs::read(&layout.part).unwrap(), b"mine");

        // A claim file that names something else (another name, a longer file that starts
        // with this name, an empty file) does not vouch for the partial file.
        let mut longer = layout.name.clone();
        longer.extend_from_slice(b"-and-more");
        for other in [b"another name".to_vec(), longer, Vec::new()] {
            fs::write(&layout.claim, &other).unwrap();
            fs::write(&layout.part, b"theirs").unwrap();
            drop(Claim::acquire(&layout).unwrap());
            assert!(!layout.part.exists(), "{other:?}");
            assert_eq!(fs::read(&layout.claim).unwrap(), layout.name, "{other:?}");
        }
        let _ = fs::remove_dir_all(&dir);
    }

    /// A claim file that is a second name for another file would have the record written
    /// into that file: refused before anything is written.
    #[test]
    fn a_hard_linked_claim_is_refused_and_its_other_name_untouched() {
        let dir = scratch("claim-hard-link");
        let layout = layout_in(&dir, "out.bin");
        let victim = dir.join("precious.txt");
        fs::write(&victim, b"precious").unwrap();
        if let Err(e) = fs::hard_link(&victim, &layout.claim) {
            panic!("cannot create a hard link here ({e}); this test must run");
        }
        let err = Claim::acquire(&layout).expect_err("a hard-linked claim must be refused");
        assert!(matches!(err, DownloadError::Io(_)), "{err:?}");
        assert_eq!(fs::read(&victim).unwrap(), b"precious");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_download_directory_is_created_owner_only_and_a_file_or_link_there_is_refused() {
        let dir = scratch("coord-dir");
        let coord = dir.join(COORD_DIR);
        ensure_coordination_dir(&coord).unwrap();
        assert!(fs::metadata(&coord).unwrap().is_dir());
        // Idempotent.
        ensure_coordination_dir(&coord).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&coord).unwrap().permissions().mode();
            assert_eq!(
                mode & 0o077,
                0,
                "group and others must have no access: {mode:o}"
            );
        }

        let file_dir = scratch("coord-file");
        let as_file = file_dir.join(COORD_DIR);
        fs::write(&as_file, b"not a directory").unwrap();
        assert!(matches!(
            ensure_coordination_dir(&as_file),
            Err(DownloadError::Io(_))
        ));
        assert_eq!(fs::read(&as_file).unwrap(), b"not a directory");

        #[cfg(unix)]
        {
            let target = scratch("coord-link-target");
            let link_dir = scratch("coord-link");
            let as_link = link_dir.join(COORD_DIR);
            std::os::unix::fs::symlink(&target, &as_link).unwrap();
            assert!(matches!(
                ensure_coordination_dir(&as_link),
                Err(DownloadError::Io(_))
            ));
            assert_eq!(fs::read_dir(&target).unwrap().count(), 0);
            let _ = fs::remove_dir_all(&target);
            let _ = fs::remove_dir_all(&link_dir);
        }
        let _ = fs::remove_dir_all(&dir);
        let _ = fs::remove_dir_all(&file_dir);
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
            // A runner that cannot create a link cannot show the protection works: fail.
            panic!("cannot create a symbolic link here ({reason}); this test must run");
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
        // The leftover partial was made with the default mode; opening it made it private.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&part).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn two_handles_compare_by_file_identity_not_by_name() {
        let dir = scratch("same-open");
        let (a, b) = (dir.join("a.bin"), dir.join("b.bin"));
        fs::write(&a, b"one").unwrap();
        fs::write(&b, b"one").unwrap();
        let (a1, a2, b1) = (
            File::open(&a).unwrap(),
            File::open(&a).unwrap(),
            File::open(&b).unwrap(),
        );
        assert!(same_open_file(&a1, &a2).unwrap());
        assert!(!same_open_file(&a1, &b1).unwrap());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_new_part_is_created_empty_and_private_through_both_handles() {
        let dir = scratch("create-new");
        let part = dir.join("x.part");
        let mut file = create_new(&part).unwrap();
        assert_eq!(file.metadata().unwrap().len(), 0);
        file.write_all(b"abc").unwrap();
        drop(file);
        assert_eq!(fs::read(&part).unwrap(), b"abc");
        // An existing entry is never created over.
        assert!(create_new(&part).is_err());
        assert_eq!(fs::read(&part).unwrap(), b"abc");
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
        // A runner that cannot replace an open file cannot show the check works: fail, never
        // return success without having run the check.
        if let Err(e) = fs::rename(&other, &part) {
            panic!("cannot replace an open file here ({e}); this test must run");
        }
        assert!(ensure_same_file(&file, &part).is_err());
        drop(file);
        let _ = fs::remove_dir_all(&dir);
    }
}
