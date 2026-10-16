//! Windows backend over the Windows Search index (`SystemIndex`).
//!
//! Queries go through the OLE DB provider `Search.CollatorDSO` with Windows Search SQL:
//!
//! ```text
//! SELECT TOP n System.ItemPathDisplay, System.Size, System.DateModified, System.FileAttributes
//! FROM SystemIndex WHERE SCOPE='file:C:/Users/me' AND System.FileName LIKE '%invoice%'
//! ```
//!
//! This is a query service, not a crawl: only what Windows Search indexes is visible
//! (by default the user profile and Start menu, not the whole disk), sizes are the logical
//! `System.Size` (no allocated size, no hard-link or clone accounting), and there is no
//! change feed. The pure parts (SQL escaping, query building, result mapping, rollup
//! folding, status assessment) compile and are tested on every platform. COM exists only on
//! Windows; elsewhere every call reports `Unsupported`.
//!
//! Escaping: single quotes are doubled. Windows Search SQL has no `ESCAPE` clause, so the
//! LIKE wildcards `%`, `_`, `[` and `]` in user text cannot be escaped; they are replaced by
//! `_` (match any one character), which can only over-select, and every returned name is then
//! checked in Rust against the original text. A search can therefore come back with fewer
//! than `limit` rows when the provider's over-selection used up the `TOP` budget.

use super::*;
use crate::error::Error;
use crate::index::{EntryKind, Hit, Item, SearchOptions, SizeInfo};
use std::collections::VecDeque;
use std::ffi::OsString;
use std::path::PathBuf;
use std::time::Duration;

const ATTR_HIDDEN: u32 = 0x2;
const ATTR_DIRECTORY: u32 = 0x10;
const ATTR_REPARSE_POINT: u32 = 0x400;

/// Longest query token kept; longer user tokens are cut (the Rust post-filter still uses the
/// full original token only up to this length, so the two always agree).
pub const MAX_TOKEN_CHARS: usize = 128;
pub const MAX_TOKENS: usize = 8;
pub const MAX_LIMIT: usize = 10_000;
/// Default row cap of `folder_rollup`.
pub const DEFAULT_ROLLUP_ROWS: u64 = 2_000_000;

const COLUMNS: &str = "System.ItemPathDisplay, System.Size, System.DateModified, System.FileAttributes";

// ------------------------------------------------------------------ pure: SQL

/// Text safe inside a single-quoted Windows Search SQL literal: `'` doubled, NUL and other
/// control characters replaced by a space.
pub fn escape_sql_literal(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        match c {
            '\'' => out.push_str("''"),
            c if c.is_control() => out.push(' '),
            c => out.push(c),
        }
    }
    out
}

/// A LIKE operand for one token: wildcard characters neutralised to `_`, quotes doubled.
/// The result is an over-selecting superset of the literal token; filter with `name_matches`.
pub fn like_pattern(token: &str) -> String {
    let neutral: String = token
        .chars()
        .map(|c| if matches!(c, '%' | '_' | '[' | ']') { '_' } else { c })
        .collect();
    escape_sql_literal(&neutral)
}

/// Split user text into at most `MAX_TOKENS` whitespace-separated tokens of at most
/// `MAX_TOKEN_CHARS` characters.
pub fn tokens(text: &str) -> Vec<String> {
    text.split_whitespace()
        .take(MAX_TOKENS)
        .map(|t| t.chars().take(MAX_TOKEN_CHARS).collect())
        .collect()
}

/// Whether `name` contains every token, ignoring case. The exact check behind the LIKE
/// over-selection.
pub fn name_matches(name: &str, toks: &[String]) -> bool {
    let lower = name.to_lowercase();
    toks.iter().all(|t| lower.contains(&t.to_lowercase()))
}

/// `file:` scope URL for an absolute path (`C:\a\b` -> `file:C:/a/b`, `\\srv\sh` ->
/// `file://srv/sh`). `None` for a relative path.
pub fn scope_url(path: &str) -> Option<String> {
    let mut p = path.replace('\\', "/");
    if let Some(rest) = p.strip_prefix("//?/UNC/") {
        p = format!("//{rest}");
    } else if let Some(rest) = p.strip_prefix("//?/") {
        p = rest.to_string();
    }
    let b = p.as_bytes();
    let drive = b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':';
    let unc = p.starts_with("//") && p.len() > 2;
    if !drive && !unc {
        return None;
    }
    while p.len() > 3 && p.ends_with('/') {
        p.pop();
    }
    if drive && p.len() == 2 {
        p.push('/');
    }
    Some(format!("file:{p}"))
}

/// Name search below `scope` (None: the whole index).
pub fn name_query(
    scope: Option<&str>,
    toks: &[String],
    limit: usize,
) -> std::result::Result<String, Error> {
    if toks.is_empty() {
        return Err(Error::InvalidQuery("empty search text".into()));
    }
    let top = limit.clamp(1, MAX_LIMIT);
    let mut sql = format!("SELECT TOP {top} {COLUMNS} FROM SystemIndex WHERE ");
    let mut first = true;
    if let Some(s) = scope {
        let url = scope_url(s)
            .ok_or_else(|| Error::InvalidQuery("scope must be an absolute path".into()))?;
        sql.push_str(&format!("SCOPE='{}'", escape_sql_literal(&url)));
        first = false;
    }
    for t in toks {
        if !first {
            sql.push_str(" AND ");
        }
        first = false;
        sql.push_str(&format!("System.FileName LIKE '%{}%'", like_pattern(t)));
    }
    Ok(sql)
}

/// Immediate children of `dir`.
pub fn children_query(dir: &str) -> std::result::Result<String, Error> {
    let url = scope_url(dir)
        .ok_or_else(|| Error::InvalidQuery("directory must be an absolute path".into()))?;
    Ok(format!(
        "SELECT {COLUMNS} FROM SystemIndex WHERE DIRECTORY='{}'",
        escape_sql_literal(&url)
    ))
}

/// Everything below `scope`.
pub fn subtree_query(scope: &str) -> std::result::Result<String, Error> {
    let url = scope_url(scope)
        .ok_or_else(|| Error::InvalidQuery("scope must be an absolute path".into()))?;
    Ok(format!(
        "SELECT {COLUMNS} FROM SystemIndex WHERE SCOPE='{}'",
        escape_sql_literal(&url)
    ))
}

// ------------------------------------------------------------------ pure: mapping

/// One result row of the four selected columns.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WsRow {
    pub path: String,
    pub size: Option<u64>,
    /// FILETIME of `System.DateModified`.
    pub modified_filetime: Option<u64>,
    pub attributes: Option<u32>,
}

impl WsRow {
    pub fn is_dir(&self) -> bool {
        self.attributes.is_some_and(|a| a & ATTR_DIRECTORY != 0)
            && !self.is_link()
    }
    pub fn is_link(&self) -> bool {
        self.attributes.is_some_and(|a| a & ATTR_REPARSE_POINT != 0)
    }
    /// Last path component (either separator).
    pub fn name(&self) -> &str {
        let t = self.path.trim_end_matches(['\\', '/']);
        t.rsplit(['\\', '/']).next().unwrap_or("")
    }
}

/// FILETIME (100 ns since 1601) to Unix seconds.
pub fn filetime_to_unix(ft: u64) -> i64 {
    (ft / 10_000_000) as i64 - 11_644_473_600
}

/// A search result with its modification time.
#[derive(Clone, Debug)]
pub struct WindowsSearchHit {
    pub hit: Hit,
    /// Unix seconds, when the index has a modification time.
    pub modified_unix: Option<i64>,
}

fn kind_of(r: &WsRow) -> EntryKind {
    if r.is_link() {
        EntryKind::Symlink
    } else if r.is_dir() {
        EntryKind::Dir
    } else {
        EntryKind::File
    }
}

/// Map a row to the crate's `Item`. Windows Search reports no allocation size and no
/// hard-link identity, so `allocated` and `allocated_unique` equal `logical`.
pub fn row_to_item(r: &WsRow) -> Item {
    let kind = kind_of(r);
    let bytes = if kind == EntryKind::File {
        r.size.unwrap_or(0)
    } else {
        0
    };
    Item {
        path: PathBuf::from(&r.path),
        name: OsString::from(r.name()),
        kind,
        is_package: false,
        is_secondary_link: false,
        excluded: false,
        size: SizeInfo {
            logical: bytes,
            allocated: bytes,
            allocated_unique: bytes,
            files: u64::from(kind == EntryKind::File),
            is_package: false,
        },
    }
}

/// Score a name against the search text: exact 1000, prefix of the first token 500, else
/// 100; shorter names rank first within a class.
pub fn score_name(name: &str, text: &str, toks: &[String]) -> i32 {
    let lower = name.to_lowercase();
    let base = if lower == text.trim().to_lowercase() {
        1000
    } else if toks
        .first()
        .is_some_and(|t| lower.starts_with(&t.to_lowercase()))
    {
        500
    } else {
        100
    };
    base - (lower.chars().count().min(99) as i32)
}

/// Filter rows through the exact matcher, score, and sort best first.
pub fn map_hits(rows: &[WsRow], text: &str, limit: usize) -> Vec<WindowsSearchHit> {
    let toks = tokens(text);
    let mut hits: Vec<WindowsSearchHit> = rows
        .iter()
        .filter(|r| name_matches(r.name(), &toks))
        .map(|r| WindowsSearchHit {
            hit: Hit {
                item: row_to_item(r),
                score: score_name(r.name(), text, &toks),
            },
            modified_unix: r.modified_filetime.map(filetime_to_unix),
        })
        .collect();
    hits.sort_by(|a, b| {
        b.hit
            .score
            .cmp(&a.hit.score)
            .then_with(|| a.hit.item.path.cmp(&b.hit.item.path))
    });
    hits.truncate(limit);
    hits
}

/// Folder size from an index subtree query.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WindowsRollup {
    pub size: SizeInfo,
    /// Rows folded (files and folders).
    pub rows: u64,
    pub folders: u64,
    /// The row cap was hit: sizes are a lower bound.
    pub truncated: bool,
}

#[derive(Default)]
struct RollupAcc {
    out: WindowsRollup,
}

impl RollupAcc {
    fn add(&mut self, r: &WsRow) {
        self.out.rows += 1;
        if r.is_dir() {
            self.out.folders += 1;
        } else {
            let b = r.size.unwrap_or(0);
            self.out.size.logical += b;
            self.out.size.allocated += b;
            self.out.size.allocated_unique += b;
            self.out.size.files += 1;
        }
    }
}

/// Fold rows into a rollup, stopping at `max_rows`.
pub fn fold_rollup<'a>(rows: impl IntoIterator<Item = &'a WsRow>, max_rows: u64) -> WindowsRollup {
    let mut acc = RollupAcc::default();
    for r in rows {
        if acc.out.rows >= max_rows {
            acc.out.truncated = true;
            break;
        }
        acc.add(r);
    }
    acc.out
}

fn entry_of(r: &WsRow) -> EntryData {
    let kind = match kind_of(r) {
        EntryKind::Dir => KIND_DIR,
        EntryKind::Symlink => KIND_LINK,
        EntryKind::File => KIND_FILE,
        EntryKind::Other => KIND_OTHER,
    };
    let bytes = if kind == KIND_FILE {
        r.size.unwrap_or(0)
    } else {
        0
    };
    EntryData {
        kind,
        flags: if r.attributes.is_some_and(|a| a & ATTR_HIDDEN != 0) {
            FLAG_HIDDEN
        } else {
            0
        },
        nlink: 1,
        mtime: r.modified_filetime.map_or(0, filetime_to_unix),
        logical: bytes,
        alloc: bytes,
        private: bytes,
        ..EntryData::default()
    }
}

// ------------------------------------------------------------------ status

/// State of the `WSearch` service.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServiceState {
    Running,
    Stopped,
    NotInstalled,
    /// The service manager could not be queried.
    Unknown,
}

/// What is known about Windows Search on this machine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WindowsSearchStatus {
    pub service: ServiceState,
    /// `Search.CollatorDSO` is registered.
    pub provider_registered: bool,
    /// Whether the requested scope returned any indexed item; None when no scope was asked
    /// for or the probe could not run.
    pub scope_indexed: Option<bool>,
    /// Why `available()` is false.
    pub reason: Option<&'static str>,
}

impl WindowsSearchStatus {
    pub fn available(&self) -> bool {
        self.reason.is_none()
    }
}

/// Combine the probe results into a status with the first blocking reason.
pub fn assess(
    service: ServiceState,
    provider_registered: bool,
    scope_indexed: Option<bool>,
) -> WindowsSearchStatus {
    let reason = match (service, provider_registered, scope_indexed) {
        (ServiceState::NotInstalled, _, _) => Some("Windows Search is not installed"),
        (ServiceState::Stopped, _, _) => Some("Windows Search service is not running"),
        (ServiceState::Unknown, _, _) => Some("Windows Search service state could not be read"),
        (_, false, _) => Some("Search.CollatorDSO OLE DB provider is not registered"),
        (_, _, Some(false)) => Some("scope is not covered by the Windows Search index"),
        _ => None,
    };
    WindowsSearchStatus {
        service,
        provider_registered,
        scope_indexed,
        reason,
    }
}

/// Probe Windows Search. With `scope`, also runs a one-row query to see whether the folder
/// is indexed. Never fails: a broken or absent service is reported in the status.
pub fn windows_search_status(scope: Option<&Path>) -> WindowsSearchStatus {
    let service = sys::service_state();
    let provider = sys::provider_registered();
    let base = assess(service, provider, None);
    if !base.available() {
        return base;
    }
    let Some(scope) = scope else { return base };
    let probe = scope
        .to_str()
        .and_then(scope_url)
        .map(|u| {
            format!(
                "SELECT TOP 1 {COLUMNS} FROM SystemIndex WHERE SCOPE='{}'",
                escape_sql_literal(&u)
            )
        })
        .and_then(|sql| {
            let s = sys::Session::open().ok()?;
            let mut any = false;
            s.query(&sql, &mut |_| {
                any = true;
                false
            })
            .ok()?;
            Some(any)
        });
    assess(service, provider, probe)
}

// ------------------------------------------------------------------ public queries

/// Name search over the Windows Search index. `opts.scope` limits it to a folder; every
/// whitespace-separated token must occur in the file name (case-insensitive).
pub fn search_names(
    text: &str,
    opts: &SearchOptions,
) -> Result<Vec<WindowsSearchHit>> {
    let toks = tokens(text);
    let scope = match &opts.scope {
        Some(p) => Some(
            p.to_str()
                .ok_or_else(|| Error::InvalidQuery("scope is not valid Unicode".into()))?,
        ),
        None => None,
    };
    let sql = name_query(scope, &toks, opts.limit)?;
    let session = sys::Session::open()?;
    let mut rows = Vec::new();
    session.query(&sql, &mut |r| {
        rows.push(r);
        true
    })?;
    Ok(map_hits(&rows, text, opts.limit))
}

/// Size of everything the index holds below `scope`, capped at `max_rows` rows.
pub fn folder_rollup(scope: &Path, max_rows: u64) -> Result<WindowsRollup> {
    let s = scope
        .to_str()
        .ok_or_else(|| Error::InvalidQuery("scope is not valid Unicode".into()))?;
    let sql = subtree_query(s)?;
    let session = sys::Session::open()?;
    let mut acc = RollupAcc::default();
    let mut truncated = false;
    session.query(&sql, &mut |r| {
        if acc.out.rows >= max_rows {
            truncated = true;
            return false;
        }
        acc.add(&r);
        true
    })?;
    acc.out.truncated = truncated;
    Ok(acc.out)
}

// ------------------------------------------------------------------ Backend

/// `Backend` over Windows Search. Use it with `Config::watch = false`: there is no change
/// feed, so a stale index is refreshed by reopening or rescanning (USN support: see
/// `windows_usn`).
pub struct WindowsSearchBackend;

fn list_one(
    session: &sys::Session,
    dir: &str,
    excludes: &Excludes,
    listing: &mut Listing,
) -> Result<Vec<(usize, String)>> {
    let sql = children_query(dir)?;
    let mut subdirs = Vec::new();
    session.query(&sql, &mut |r| {
        let name = r.name();
        if name.is_empty() || name.len() > u16::MAX as usize {
            return true;
        }
        let mut data = entry_of(&r);
        if data.kind == KIND_DIR {
            let excluded = excludes.matches(r.path.as_bytes())
                || (excludes.honor_cachedir_tag && dir_has_cachedir_tag(Path::new(&r.path)));
            if excluded {
                data.flags |= FLAG_EXCLUDED;
            } else {
                subdirs.push((listing.ents.len(), r.path.clone()));
            }
        }
        listing.ents.push(RawEnt {
            name_off: listing.names.len() as u32,
            name_len: name.len() as u16,
            data,
            child: NONE,
        });
        listing.names.extend_from_slice(name.as_bytes());
        true
    })?;
    Ok(subdirs)
}

fn error_kind(e: &Error) -> std::io::ErrorKind {
    match e {
        Error::Unsupported(_) => std::io::ErrorKind::Unsupported,
        Error::Io(e) => e.kind(),
        _ => std::io::ErrorKind::Other,
    }
}

impl Backend for WindowsSearchBackend {
    fn name(&self) -> &'static str {
        "windows-search"
    }

    fn check_supported(&self) -> Result<()> {
        match windows_search_status(None).reason {
            None => Ok(()),
            Some(r) => Err(Error::Unsupported(r)),
        }
    }

    fn stat(&self, path: &Path) -> std::io::Result<EntryData> {
        let m = std::fs::symlink_metadata(path)?;
        let ft = m.file_type();
        Ok(EntryData {
            kind: if ft.is_dir() {
                KIND_DIR
            } else if ft.is_symlink() {
                KIND_LINK
            } else if ft.is_file() {
                KIND_FILE
            } else {
                KIND_OTHER
            },
            nlink: 1,
            mtime: m
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_secs() as i64),
            logical: if ft.is_file() { m.len() } else { 0 },
            alloc: if ft.is_file() { m.len() } else { 0 },
            private: if ft.is_file() { m.len() } else { 0 },
            ..EntryData::default()
        })
    }

    fn crawl(
        &self,
        root: &Path,
        excludes: &Excludes,
        _threads: usize,
        sink: &mut dyn FnMut(Listing),
    ) {
        let fail = |id: u32, sink: &mut dyn FnMut(Listing), k: std::io::ErrorKind| {
            sink(Listing {
                id,
                error: Some(k),
                ..Listing::default()
            })
        };
        let session = match sys::Session::open() {
            Ok(s) => s,
            Err(e) => return fail(0, sink, error_kind(&e)),
        };
        let Some(root) = root.to_str() else {
            return fail(0, sink, std::io::ErrorKind::InvalidInput);
        };
        let mut next_id = 1u32;
        let mut queue: VecDeque<(u32, String)> = VecDeque::new();
        queue.push_back((0, root.to_string()));
        // Breadth first: a parent's listing is always delivered before its subdirectories.
        while let Some((id, dir)) = queue.pop_front() {
            let mut l = Listing {
                id,
                ..Listing::default()
            };
            match list_one(&session, &dir, excludes, &mut l) {
                Ok(subdirs) => {
                    for (i, path) in subdirs {
                        l.ents[i].child = next_id;
                        queue.push_back((next_id, path));
                        next_id += 1;
                    }
                }
                Err(e) => l.error = Some(error_kind(&e)),
            }
            sink(l);
        }
    }

    fn list_dir(&self, dir: &Path, excludes: &Excludes) -> std::io::Result<Listing> {
        let dir = dir
            .to_str()
            .ok_or(std::io::ErrorKind::InvalidInput)?;
        let session = sys::Session::open().map_err(|e| std::io::Error::from(error_kind(&e)))?;
        let mut l = Listing::default();
        list_one(&session, dir, excludes, &mut l)
            .map_err(|e| std::io::Error::from(error_kind(&e)))?;
        Ok(l)
    }

    fn watch(
        &self,
        _: &[PathBuf],
        _: Option<u64>,
        _: Duration,
        _: Sender<WatchMsg>,
    ) -> Result<Box<dyn Watcher>> {
        Err(Error::Unsupported(
            "Windows Search has no change feed; open with Config::watch = false",
        ))
    }

    fn current_event_id(&self) -> u64 {
        0
    }

    fn has_full_disk_access(&self) -> bool {
        true
    }
}

// ------------------------------------------------------------------ platform glue

#[cfg(not(windows))]
mod sys {
    use super::*;

    pub fn service_state() -> ServiceState {
        ServiceState::NotInstalled
    }
    pub fn provider_registered() -> bool {
        false
    }

    pub struct Session;

    impl Session {
        pub fn open() -> Result<Session> {
            Err(Error::Unsupported("Windows Search exists only on Windows"))
        }
        pub fn query(&self, _sql: &str, _on_row: &mut dyn FnMut(WsRow) -> bool) -> Result<()> {
            Err(Error::Unsupported("Windows Search exists only on Windows"))
        }
    }
}

#[cfg(windows)]
mod sys {
    use super::*;
    use std::mem::{offset_of, size_of, zeroed};
    use windows::core::{w, Interface, GUID, PCWSTR};
    use windows::Win32::System::Com::{
        CLSIDFromProgID, CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER,
        COINIT_MULTITHREADED,
    };
    use windows::Win32::System::Search::{
        DBBINDING, DBACCESSOR_ROWDATA, DBMEMOWNER_CLIENTOWNED, DBMEMOWNER_PROVIDEROWNED,
        DBPART_LENGTH, DBPART_STATUS, DBPART_VALUE, DBTYPE_BYREF, DBTYPE_FILETIME, DBTYPE_UI4,
        DBTYPE_UI8, DBTYPE_WSTR, HACCESSOR, IAccessor, ICommand, ICommandText, IDBCreateCommand,
        IDBCreateSession, IDBInitialize, IRowset,
    };
    use windows::Win32::System::Services::{
        CloseServiceHandle, OpenSCManagerW, OpenServiceW, QueryServiceStatus, SC_MANAGER_CONNECT,
        SERVICE_QUERY_STATUS, SERVICE_RUNNING, SERVICE_STATUS,
    };

    const DBGUID_DEFAULT: GUID = GUID::from_u128(0xC8B521FB_5CF3_11CE_ADE5_00AA0044773D);
    const ERROR_SERVICE_DOES_NOT_EXIST: i32 = 1060;
    const BATCH: usize = 64;

    fn io(e: windows::core::Error) -> Error {
        Error::Io(std::io::Error::other(format!(
            "windows search: {} (0x{:08x})",
            e.message(),
            e.code().0 as u32
        )))
    }

    pub fn service_state() -> ServiceState {
        unsafe {
            let Ok(scm) = OpenSCManagerW(PCWSTR::null(), PCWSTR::null(), SC_MANAGER_CONNECT)
            else {
                return ServiceState::Unknown;
            };
            let state = match OpenServiceW(scm, w!("WSearch"), SERVICE_QUERY_STATUS) {
                Ok(svc) => {
                    let mut st: SERVICE_STATUS = zeroed();
                    let r = QueryServiceStatus(svc, &mut st);
                    let _ = CloseServiceHandle(svc);
                    match r {
                        Ok(()) if st.dwCurrentState == SERVICE_RUNNING => ServiceState::Running,
                        Ok(()) => ServiceState::Stopped,
                        Err(_) => ServiceState::Unknown,
                    }
                }
                Err(e) if (e.code().0 as u32 & 0xFFFF) as i32 == ERROR_SERVICE_DOES_NOT_EXIST => {
                    ServiceState::NotInstalled
                }
                Err(_) => ServiceState::Unknown,
            };
            let _ = CloseServiceHandle(scm);
            state
        }
    }

    fn provider_clsid() -> Option<GUID> {
        unsafe {
            CLSIDFromProgID(w!("Search.CollatorDSO"))
                .or_else(|_| CLSIDFromProgID(w!("Search.CollatorDSO.1")))
                .ok()
        }
    }

    pub fn provider_registered() -> bool {
        // Registration is per machine; no COM apartment is needed to read it.
        provider_clsid().is_some()
    }

    struct ComGuard(bool);

    impl ComGuard {
        fn init() -> ComGuard {
            // S_OK and S_FALSE both need a matching CoUninitialize; RPC_E_CHANGED_MODE (the
            // thread already has an STA) is an error HRESULT and needs none.
            ComGuard(unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok())
        }
    }

    impl Drop for ComGuard {
        fn drop(&mut self) {
            if self.0 {
                unsafe { CoUninitialize() }
            }
        }
    }

    /// One OLE DB session on the Windows Search catalog. Create, use and drop it on the same
    /// thread.
    pub struct Session {
        factory: IDBCreateCommand,
        _source: IDBInitialize,
        _com: ComGuard,
    }

    /// Row buffer for the four bound columns. Each column has status, length and value.
    #[repr(C)]
    struct RowBuf {
        path_status: u32,
        path_len: usize,
        path: *const u16,
        size_status: u32,
        size_len: usize,
        size: u64,
        date_status: u32,
        date_len: usize,
        date: u64,
        attr_status: u32,
        attr_len: usize,
        attr: u32,
    }

    fn binding(
        ordinal: usize,
        status: usize,
        len: usize,
        value: usize,
        wtype: u16,
        owner: i32,
        max_len: usize,
    ) -> DBBINDING {
        let mut b: DBBINDING = unsafe { zeroed() };
        b.iOrdinal = ordinal;
        b.obStatus = status;
        b.obLength = len;
        b.obValue = value;
        b.dwPart = (DBPART_VALUE.0 | DBPART_LENGTH.0 | DBPART_STATUS.0) as u32;
        b.dwMemOwner = owner as u32;
        b.cbMaxLen = max_len;
        b.wType = wtype;
        b
    }

    fn bindings() -> [DBBINDING; 4] {
        [
            binding(
                1,
                offset_of!(RowBuf, path_status),
                offset_of!(RowBuf, path_len),
                offset_of!(RowBuf, path),
                (DBTYPE_WSTR.0 | DBTYPE_BYREF.0) as u16,
                DBMEMOWNER_PROVIDEROWNED.0,
                0,
            ),
            binding(
                2,
                offset_of!(RowBuf, size_status),
                offset_of!(RowBuf, size_len),
                offset_of!(RowBuf, size),
                DBTYPE_UI8.0 as u16,
                DBMEMOWNER_CLIENTOWNED.0,
                8,
            ),
            binding(
                3,
                offset_of!(RowBuf, date_status),
                offset_of!(RowBuf, date_len),
                offset_of!(RowBuf, date),
                DBTYPE_FILETIME.0 as u16,
                DBMEMOWNER_CLIENTOWNED.0,
                8,
            ),
            binding(
                4,
                offset_of!(RowBuf, attr_status),
                offset_of!(RowBuf, attr_len),
                offset_of!(RowBuf, attr),
                DBTYPE_UI4.0 as u16,
                DBMEMOWNER_CLIENTOWNED.0,
                4,
            ),
        ]
    }

    fn row_of(b: &RowBuf) -> WsRow {
        // DBSTATUS_S_OK is 0; anything else (null, truncated, unavailable) is treated as absent.
        let path = if b.path_status == 0 && !b.path.is_null() {
            let units = b.path_len / 2;
            let s = unsafe { std::slice::from_raw_parts(b.path, units) };
            String::from_utf16_lossy(s)
        } else {
            String::new()
        };
        WsRow {
            path,
            size: (b.size_status == 0).then_some(b.size),
            modified_filetime: (b.date_status == 0).then_some(b.date),
            attributes: (b.attr_status == 0).then_some(b.attr),
        }
    }

    impl Session {
        pub fn open() -> Result<Session> {
            match service_state() {
                ServiceState::Running => {}
                ServiceState::NotInstalled => {
                    return Err(Error::Unsupported("Windows Search is not installed"))
                }
                ServiceState::Stopped => {
                    return Err(Error::Unsupported("Windows Search service is not running"))
                }
                ServiceState::Unknown => {
                    return Err(Error::Unsupported(
                        "Windows Search service state could not be read",
                    ))
                }
            }
            let clsid = provider_clsid().ok_or(Error::Unsupported(
                "Search.CollatorDSO OLE DB provider is not registered",
            ))?;
            let com = ComGuard::init();
            unsafe {
                let source: IDBInitialize =
                    CoCreateInstance(&clsid, None::<&windows::core::IUnknown>, CLSCTX_INPROC_SERVER).map_err(io)?;
                source.Initialize().map_err(io)?;
                let create_session: IDBCreateSession = source.cast().map_err(io)?;
                let session = create_session
                    .CreateSession(None::<&windows::core::IUnknown>, &IDBCreateCommand::IID)
                    .map_err(io)?;
                let factory: IDBCreateCommand = session.cast().map_err(io)?;
                Ok(Session {
                    factory,
                    _source: source,
                    _com: com,
                })
            }
        }

        /// Run `sql` and hand each row to `on_row` until it returns false or the rows end.
        pub fn query(&self, sql: &str, on_row: &mut dyn FnMut(WsRow) -> bool) -> Result<()> {
            let wide: Vec<u16> = sql.encode_utf16().chain(Some(0)).collect();
            unsafe {
                let cmd_unk = self
                    .factory
                    .CreateCommand(None::<&windows::core::IUnknown>, &ICommandText::IID)
                    .map_err(io)?;
                let text: ICommandText = cmd_unk.cast().map_err(io)?;
                text.SetCommandText(&DBGUID_DEFAULT, PCWSTR(wide.as_ptr()))
                    .map_err(io)?;
                let cmd: ICommand = text.cast().map_err(io)?;
                let mut rowset_unk: Option<windows::core::IUnknown> = None;
                cmd.Execute(
                    None::<&windows::core::IUnknown>,
                    &IRowset::IID,
                    None,
                    None,
                    Some(&mut rowset_unk),
                )
                .map_err(io)?;
                let rowset: IRowset = rowset_unk
                    .ok_or(Error::Unsupported("Windows Search returned no rowset"))?
                    .cast()
                    .map_err(io)?;
                let accessor: IAccessor = rowset.cast().map_err(io)?;
                let binds = bindings();
                let mut hacc = HACCESSOR(0);
                accessor
                    .CreateAccessor(
                        DBACCESSOR_ROWDATA.0 as u32,
                        binds.len(),
                        binds.as_ptr(),
                        size_of::<RowBuf>(),
                        &mut hacc,
                        None,
                    )
                    .map_err(io)?;
                let result = self.drain(&rowset, hacc, on_row);
                let _ = accessor.ReleaseAccessor(hacc, None);
                result
            }
        }

        unsafe fn drain(
            &self,
            rowset: &IRowset,
            hacc: HACCESSOR,
            on_row: &mut dyn FnMut(WsRow) -> bool,
        ) -> Result<()> {
            // The provider fills the caller's HROW array through `holder[0]`.
            let mut hrows = [0usize; BATCH];
            let mut holder: [*mut usize; BATCH] = [std::ptr::null_mut(); BATCH];
            loop {
                holder[0] = hrows.as_mut_ptr();
                let mut got = 0usize;
                rowset
                    .GetNextRows(0, 0, &mut got, &mut holder)
                    .map_err(io)?;
                if got == 0 {
                    return Ok(());
                }
                let mut go = true;
                for &h in &hrows[..got] {
                    if go {
                        let mut buf: RowBuf = zeroed();
                        rowset
                            .GetData(h, hacc, &mut buf as *mut RowBuf as *mut _)
                            .map_err(io)?;
                        go = on_row(row_of(&buf));
                    }
                }
                rowset
                    .ReleaseRows(
                        got,
                        hrows.as_ptr(),
                        std::ptr::null(),
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                    )
                    .map_err(io)?;
                if !go {
                    return Ok(());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(path: &str, size: Option<u64>, attrs: Option<u32>) -> WsRow {
        WsRow {
            path: path.into(),
            size,
            modified_filetime: Some(133_000_000_000_000_000),
            attributes: attrs,
        }
    }

    #[test]
    fn single_quotes_are_doubled_and_controls_removed() {
        assert_eq!(escape_sql_literal("it's"), "it''s");
        assert_eq!(escape_sql_literal("a'b'c"), "a''b''c");
        assert_eq!(escape_sql_literal("x\0y\r\nz"), "x y  z");
        assert_eq!(escape_sql_literal("plain"), "plain");
        // the classic breakout stays inside the literal
        let sql = escape_sql_literal("' OR 1=1 --");
        assert_eq!(sql, "'' OR 1=1 --");
        assert!(!sql.replace("''", "").contains('\''));
    }

    #[test]
    fn like_wildcards_are_neutralised() {
        assert_eq!(like_pattern("100%_[a]"), "100___a_");
        assert_eq!(like_pattern("o'brien"), "o''brien");
        assert!(!like_pattern("%%").contains('%'));
    }

    #[test]
    fn tokens_are_bounded() {
        assert_eq!(tokens("  a  b\tc "), vec!["a", "b", "c"]);
        assert_eq!(tokens("").len(), 0);
        let many = "a b c d e f g h i j";
        assert_eq!(tokens(many).len(), MAX_TOKENS);
        let long = "x".repeat(500);
        assert_eq!(tokens(&long)[0].chars().count(), MAX_TOKEN_CHARS);
    }

    #[test]
    fn exact_matcher_undoes_the_over_selection() {
        let t = tokens("100%");
        // LIKE '%100_%' would also match 100a.txt; the exact check does not.
        assert!(name_matches("sale 100%.txt", &t));
        assert!(!name_matches("100a.txt", &t));
        let t = tokens("INVOICE 2024");
        assert!(name_matches("invoice-march-2024.pdf", &t));
        assert!(!name_matches("invoice.pdf", &t));
    }

    #[test]
    fn scope_urls_cover_drives_unc_and_relative() {
        assert_eq!(scope_url(r"C:\Users\me").as_deref(), Some("file:C:/Users/me"));
        assert_eq!(scope_url(r"C:\Users\me\").as_deref(), Some("file:C:/Users/me"));
        assert_eq!(scope_url("C:").as_deref(), Some("file:C:/"));
        assert_eq!(scope_url(r"C:\").as_deref(), Some("file:C:/"));
        assert_eq!(scope_url(r"\\srv\share\d").as_deref(), Some("file://srv/share/d"));
        assert_eq!(scope_url(r"\\?\C:\x").as_deref(), Some("file:C:/x"));
        assert_eq!(scope_url(r"\\?\UNC\srv\sh").as_deref(), Some("file://srv/sh"));
        assert_eq!(scope_url("docs/a"), None);
        assert_eq!(scope_url(""), None);
    }

    #[test]
    fn name_query_is_built_from_escaped_parts() {
        let t = tokens("o'brien 5%");
        let sql = name_query(Some(r"C:\Users\o'neil"), &t, 25).unwrap();
        assert_eq!(
            sql,
            "SELECT TOP 25 System.ItemPathDisplay, System.Size, System.DateModified, \
             System.FileAttributes FROM SystemIndex WHERE SCOPE='file:C:/Users/o''neil' \
             AND System.FileName LIKE '%o''brien%' AND System.FileName LIKE '%5_%'"
        );
        let no_scope = name_query(None, &tokens("a"), 0).unwrap();
        assert!(no_scope.starts_with("SELECT TOP 1 "));
        assert!(no_scope.ends_with("WHERE System.FileName LIKE '%a%'"));
        let big = name_query(None, &tokens("a"), usize::MAX).unwrap();
        assert!(big.contains(&format!("TOP {MAX_LIMIT} ")));
    }

    #[test]
    fn name_query_rejects_empty_text_and_relative_scopes() {
        assert!(matches!(
            name_query(None, &[], 10),
            Err(Error::InvalidQuery(_))
        ));
        assert!(matches!(
            name_query(Some("rel/dir"), &tokens("a"), 10),
            Err(Error::InvalidQuery(_))
        ));
        assert!(children_query("rel").is_err());
        assert!(subtree_query("rel").is_err());
    }

    #[test]
    fn injection_attempts_cannot_close_the_literal() {
        let evil = "x' OR System.FileName LIKE '%";
        let sql = name_query(Some("C:\\a"), &tokens(evil), 5).unwrap();
        // every quote the user supplied is doubled: removing doubled pairs leaves only the
        // structural quotes of the query itself
        let body = sql.split(" WHERE ").nth(1).unwrap();
        let structural = body.replace("''", "");
        assert_eq!(structural.matches('\'').count() % 2, 0);
        let sc = scope_url("C:\\a'; DROP").unwrap();
        let q = subtree_query("C:\\a'; DROP").unwrap();
        assert!(q.contains(&escape_sql_literal(&sc)));
        assert_eq!(
            children_query(r"D:\x").unwrap(),
            "SELECT System.ItemPathDisplay, System.Size, System.DateModified, \
             System.FileAttributes FROM SystemIndex WHERE DIRECTORY='file:D:/x'"
        );
    }

    #[test]
    fn rows_map_to_items() {
        let f = row(r"C:\Users\me\Doc.TXT", Some(1234), Some(0x20));
        let it = row_to_item(&f);
        assert_eq!(it.kind, EntryKind::File);
        assert_eq!(it.name, OsString::from("Doc.TXT"));
        assert_eq!(it.path, PathBuf::from(r"C:\Users\me\Doc.TXT"));
        assert_eq!(it.size.logical, 1234);
        assert_eq!(it.size.allocated_unique, 1234);
        assert_eq!(it.size.files, 1);
        let d = row_to_item(&row(r"C:\Users\me\Dir", None, Some(0x10)));
        assert_eq!(d.kind, EntryKind::Dir);
        assert_eq!(d.size.files, 0);
        let l = row_to_item(&row(r"C:\J", Some(9), Some(0x410)));
        assert_eq!(l.kind, EntryKind::Symlink);
        assert_eq!(l.size.logical, 0);
        // no attributes: treated as a file
        assert_eq!(row_to_item(&row(r"C:\x", Some(1), None)).kind, EntryKind::File);
        assert_eq!(row(r"C:\Users\me\Dir\", None, Some(0x10)).name(), "Dir");
    }

    #[test]
    fn filetime_converts_to_unix_seconds() {
        assert_eq!(filetime_to_unix(116_444_736_000_000_000), 0);
        assert_eq!(filetime_to_unix(116_444_736_010_000_000), 1);
    }

    #[test]
    fn hits_are_filtered_scored_sorted_and_limited() {
        let rows = vec![
            row(r"C:\a\notes invoice.txt", Some(5), Some(0x20)),
            row(r"C:\a\invoice", Some(1), Some(0x20)),
            row(r"C:\a\invoice 2024.pdf", Some(2), Some(0x20)),
            row(r"C:\a\unrelated.txt", Some(3), Some(0x20)),
        ];
        let hits = map_hits(&rows, "invoice", 10);
        let names: Vec<_> = hits
            .iter()
            .map(|h| h.hit.item.name.to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["invoice", "invoice 2024.pdf", "notes invoice.txt"]);
        assert!(hits[0].hit.score > hits[1].hit.score);
        assert_eq!(hits[0].modified_unix, Some(filetime_to_unix(133_000_000_000_000_000)));
        assert_eq!(map_hits(&rows, "invoice", 2).len(), 2);
        assert!(map_hits(&rows, "zzz", 5).is_empty());
    }

    #[test]
    fn rollup_sums_files_and_counts_folders() {
        let rows = vec![
            row(r"C:\d", None, Some(0x10)),
            row(r"C:\d\a", Some(10), Some(0x20)),
            row(r"C:\d\b", Some(20), Some(0x20)),
            row(r"C:\d\c", None, None),
        ];
        let r = fold_rollup(&rows, 100);
        assert_eq!(r.rows, 4);
        assert_eq!(r.folders, 1);
        assert_eq!(r.size.files, 3);
        assert_eq!(r.size.logical, 30);
        assert_eq!(r.size.allocated, 30);
        assert!(!r.truncated);
        let cut = fold_rollup(&rows, 2);
        assert_eq!(cut.rows, 2);
        assert!(cut.truncated);
        // exactly at the cap is not truncation
        assert!(!fold_rollup(&rows, 4).truncated);
    }

    #[test]
    fn entries_map_to_backend_data() {
        let d = entry_of(&row(r"C:\h", None, Some(0x12)));
        assert_eq!(d.kind, KIND_DIR);
        assert_eq!(d.flags & FLAG_HIDDEN, FLAG_HIDDEN);
        let f = entry_of(&row(r"C:\f", Some(7), Some(0x20)));
        assert_eq!((f.kind, f.logical, f.alloc, f.nlink), (KIND_FILE, 7, 7, 1));
        assert_eq!(entry_of(&row(r"C:\l", Some(7), Some(0x400))).kind, KIND_LINK);
    }

    #[test]
    fn status_reports_the_first_blocking_reason() {
        let ok = assess(ServiceState::Running, true, None);
        assert!(ok.available());
        assert!(assess(ServiceState::Running, true, Some(true)).available());
        let off = assess(ServiceState::Stopped, true, None);
        assert!(!off.available());
        assert_eq!(off.reason, Some("Windows Search service is not running"));
        assert!(!assess(ServiceState::NotInstalled, false, None).available());
        assert!(!assess(ServiceState::Unknown, true, None).available());
        assert_eq!(
            assess(ServiceState::Running, false, None).reason,
            Some("Search.CollatorDSO OLE DB provider is not registered")
        );
        assert_eq!(
            assess(ServiceState::Running, true, Some(false)).reason,
            Some("scope is not covered by the Windows Search index")
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn off_windows_everything_is_unsupported() {
        assert!(!windows_search_status(None).available());
        assert!(matches!(
            search_names("a", &SearchOptions::default()),
            Err(Error::Unsupported(_))
        ));
        assert!(matches!(
            WindowsSearchBackend.check_supported(),
            Err(Error::Unsupported(_))
        ));
        assert!(WindowsSearchBackend
            .list_dir(Path::new("C:/x"), &Excludes::default())
            .is_err());
    }
}
