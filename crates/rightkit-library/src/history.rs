//! History authority for the engine.
//!
//! The store index and artifact cleanup logic live here so the sidecar can
//! own `GetHistoryPage`, `GetTranscript`, and `DeleteTranscript`.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::history_types::{
    HistoryPage, HistoryRow, HistorySettings, HistorySort, Kind, TranscriptDetail,
    TranscriptImport, TranscriptVariant, TranscriptVariantInfo, TranscriptVariantKind,
    HISTORY_PAGE_MAX, HISTORY_PAGE_SIZE,
};
use crate::{HistoryOrder, HistoryRecord, PageKey, Store};
use rightkit_search::{read_text_prefix, scan_page, Cursor, Param, Query, ScanBudget, TextMatcher};

fn path_to_string(path: &Path) -> String {
    path.to_string_lossy().to_string()
}

/// Publish `from` at `to`, replacing an existing file. On Windows `MoveFileExW` refuses the
/// replace with a sharing violation or access-denied error while another process holds the old
/// file open, so this retries: five attempts in total, 100 ms apart (inert elsewhere).
pub fn publish_replacing(from: &Path, to: &Path) -> rightkit_fs::Result<()> {
    use rightkit_fs::atomic::{
        publish_file_with_access_denied_retry, AccessDeniedRetry, AtomicWriteOptions, ReplaceMode,
        RetryPolicy,
    };
    publish_file_with_access_denied_retry(
        from,
        to,
        AtomicWriteOptions {
            replace: ReplaceMode::MoveWriteThrough,
            ..AtomicWriteOptions::default()
        },
        AccessDeniedRetry::new(RetryPolicy::new(4, std::time::Duration::from_millis(100))),
    )
}

/// Timed plain-text and Markdown renderings of SRT text (`[hh:mm:ss] text` lines and
/// `- **[hh:mm:ss]** text` bullets); both are empty when the SRT does not parse.
pub fn timed_text_and_md_from_srt(srt: &str) -> (String, String) {
    let mut txt = String::new();
    let mut md = String::new();
    let Ok(cues) = rightkit_transcript::parse_srt(srt) else {
        return (txt, md);
    };
    for cue in cues {
        let text = cue.lines.join(" ");
        let text = text.trim();
        if text.is_empty() {
            continue;
        }
        let ms = cue.start_ms;
        let stamp = format!(
            "{:02}:{:02}:{:02}",
            ms / 3_600_000,
            ms / 60_000 % 60,
            ms / 1_000 % 60
        );
        txt.push_str(&format!("[{stamp}] {text}\n"));
        md.push_str(&format!("- **[{stamp}]** {text}\n"));
    }
    (txt, md)
}

/// One open connection per `history.db` path, reused by the per-request reads
/// (#242, #187). Before, every Library page, transcript read and attach check
/// ran `Store::open`, which repeats DDL, PRAGMAs and migrations each time.
static OPEN_STORES: std::sync::LazyLock<
    Mutex<std::collections::HashMap<PathBuf, std::sync::Arc<Mutex<Store>>>>,
> = std::sync::LazyLock::new(|| Mutex::new(std::collections::HashMap::new()));

/// The cached connection for `path`, opened on first use.
fn cached_store(path: &Path) -> Result<std::sync::Arc<Mutex<Store>>, String> {
    let mut stores = OPEN_STORES.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(store) = stores.get(path) {
        if path.exists() {
            return Ok(store.clone());
        }
        // The database file was removed behind the engine (output folder wiped):
        // drop the dead handle and reopen so the next write recreates it.
        stores.remove(path);
    }
    let store = std::sync::Arc::new(Mutex::new(Store::open(path).map_err(|e| e.to_string())?));
    stores.insert(path.to_path_buf(), store.clone());
    Ok(store)
}

/// Lock a cached connection for one read. Each guard stays inside one function
/// and no such function calls another cached read while holding it.
fn lock_store(shared: &Mutex<Store>) -> std::sync::MutexGuard<'_, Store> {
    shared.lock().unwrap_or_else(|p| p.into_inner())
}

/// A shared history connection for the write paths (#242, #187). Hold the
/// guard from `lock()` inside one function; never across another history call.
#[derive(Clone)]
pub struct SharedStore(std::sync::Arc<Mutex<Store>>);

impl SharedStore {
    pub fn lock(&self) -> std::sync::MutexGuard<'_, Store> {
        lock_store(&self.0)
    }
}

/// The cached connection for `path` as a `SharedStore`.
pub fn shared_store(path: &Path) -> Result<SharedStore, String> {
    cached_store(path).map(SharedStore)
}

/// The one owner of the history database location inside an output folder.
pub fn db_path_in(output_dir: &Path) -> PathBuf {
    output_dir.join("history.db")
}

pub fn db_path(settings: &HistorySettings) -> Result<PathBuf, String> {
    let output_dir = PathBuf::from(&settings.output_dir);
    std::fs::create_dir_all(&output_dir).map_err(|e| format!("create output dir: {e}"))?;
    Ok(db_path_in(&output_dir))
}

/// Merge rows from `old_dir/history.db` into `new_dir/history.db`.
///
/// Lane S-10: when the user changes `output_dir`, the engine would otherwise
/// open a fresh empty index in the new directory — silently orphaning the
/// prior library. This helper carries the prior rows forward so the user's
/// history stays visible. The merge runs on the engine side (never the
/// shell), is non-fatal, never deletes or modifies the source file (the old
/// directory may still be a valid library the user switches back to), and
/// uses destination-wins semantics: rows already present in the new store
/// keep their current values.
///
/// `old_dir` may or may not contain a `history.db` file; absence is the
/// normal case for a brand-new output directory and is a no-op. A `history.db`
/// that cannot be inspected or is not a file is an error, not an absence. A present
/// but unreadable / corrupt file is an error: the caller must not switch
/// roots on a failed merge, so the library stays where it is. The source
/// is opened read-only and is never modified.
///
/// Returns the number of rows newly inserted into the destination store.
/// Returns `Ok(0)` for "no merge attempted" (missing source, same dir) and
/// `Err(_)` for any failure to read the source or to create or write the
/// destination. Rows merged before a failure stay in the destination
/// (destination-wins, so a retry converges).
pub fn merge_history(old_dir: &Path, new_dir: &Path) -> Result<usize, String> {
    let old_db = old_dir.join("history.db");
    // Absent is the normal case for a new folder; an entry that cannot be inspected (permission
    // denied, I/O error) or is not a file is an error, never "no library".
    match std::fs::metadata(&old_db) {
        Ok(meta) if meta.is_file() => {}
        Ok(_) => return Err(format!("{} is not a file", old_db.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(format!("cannot read {}: {e}", old_db.display())),
    }
    // Same-directory settings change: no merge, no-op.
    let old_canon =
        rightkit_fs::plain::canonicalize_plain(&old_db).unwrap_or_else(|_| old_db.clone());
    let new_db = new_dir.join("history.db");
    if let Ok(new_canon) = rightkit_fs::plain::canonicalize_plain(&new_db) {
        if new_canon == old_canon {
            return Ok(0);
        }
    }

    // The source first, read-only: a corrupt or unreadable old library fails the merge
    // before anything is created at the destination.
    let source = Store::open_read_only(&old_db)
        .map_err(|e| format!("open {} read-only: {e}", old_db.display()))?;

    // Make sure the destination dir exists; mirror `db_path` semantics so
    // an unmergeable destination (e.g. permission denied) surfaces as Err.
    std::fs::create_dir_all(new_dir).map_err(|e| format!("create new output dir: {e}"))?;

    let dest = Store::open(&new_db).map_err(|e| format!("open new history.db: {e}"))?;
    // Rebase artifact paths: rows record ABSOLUTE paths into the old output
    // dir, so a verbatim merge leaves the new library pointing at files the
    // user may delete with the old folder. Each inserted row's artifacts are
    // copied into the new dir (same relative layout) and its paths rewritten;
    // a copy failure keeps the original absolute path so the row stays
    // readable rather than dangling.
    let merged = dest
        .merge_from_store_with(&source, |row| {
            rebase_record_artifacts(row, old_dir, new_dir);
        })
        .map_err(|e| format!("merge history from {}: {e}", old_db.display()))?;
    if merged > 0 {
        eprintln!(
            "[rightkit-library] merged {merged} history row(s) from {} into {}",
            old_db.display(),
            new_db.display()
        );
    }
    Ok(merged)
}

/// Rebase one merged row's artifact paths from `old_dir` to `new_dir`,
/// copying each file (and the engine-owned siblings: timed text, sidecars,
/// `.fit.md`, `_replay/<id>`, slide dirs) into the same relative location
/// under the new root. `media_path` is deliberately left alone — it lives
/// in the shared media-download directory, not inside `output_dir`.
fn rebase_record_artifacts(rec: &mut HistoryRecord, old_dir: &Path, new_dir: &Path) {
    // Sidecar SOURCE paths must be derived from the row's original absolute
    // paths: after the primary slots are rebased they already point inside
    // `new_dir`, so `sibling_path` on them yields new-root names that
    // `rebase_artifact_file` refuses (not under `old_dir`) — the old-root
    // sidecars would silently never be copied.
    let orig_txt = rec.txt_path.clone();
    let orig_md = rec.md_path.clone();
    for slot in [&mut rec.txt_path, &mut rec.md_path, &mut rec.srt_path] {
        if let Some(path) = slot.as_deref() {
            if let Some(rebased) = rebase_artifact_file(Path::new(path), old_dir, new_dir) {
                *slot = Some(rebased.to_string_lossy().to_string());
            }
        }
    }
    // Engine-owned sidecars travel with their primary file.
    if let Some(txt) = orig_txt.as_deref() {
        for ext in ["timed.txt", "json", "vtt"] {
            if let Some(sib) = sibling_path(txt, ext) {
                let _ = rebase_artifact_file(&sib, old_dir, new_dir);
            }
        }
    }
    if let Some(md) = orig_md.as_deref() {
        for ext in ["timed.md", "fit.md"] {
            if let Some(sib) = sibling_path(md, ext) {
                let _ = rebase_artifact_file(&sib, old_dir, new_dir);
            }
        }
    }
    if let Some(slide_dir) = rec.slide_dir.as_deref() {
        let path = Path::new(slide_dir);
        if let Some(rebased) = rebase_artifact_dir(path, old_dir, new_dir) {
            rec.slide_dir = Some(rebased.to_string_lossy().to_string());
        }
    }
    // `_replay/<id>` is engine-owned per-record scratch the reader may
    // regenerate, but copying it keeps replays working across the move.
    if valid_path_component(&rec.id) {
        let replay_src = old_dir.join("_replay").join(&rec.id);
        if source_dir_in_root(&replay_src, old_dir).is_some() {
            let replay_dst = new_dir.join("_replay").join(&rec.id);
            let _ = copy_dir_recursive(&replay_src, &replay_dst, new_dir);
        }
    }
}

/// Copy `path` under `old_dir` to the same relative location under
/// `new_dir`. Returns `None` when the path isn't under the old root or the
/// copy fails — callers keep the row's original absolute path in that case.
fn rebase_artifact_file(path: &Path, old_dir: &Path, new_dir: &Path) -> Option<PathBuf> {
    let rel = safe_relative(path, old_dir)?;
    source_file_in_root(path, old_dir)?;
    let dst = new_dir.join(rel);
    prepare_destination(&dst, new_dir).ok()?;
    copy_file_new(path, &dst).ok()?;
    Some(dst)
}

fn rebase_artifact_dir(path: &Path, old_dir: &Path, new_dir: &Path) -> Option<PathBuf> {
    let rel = safe_relative(path, old_dir)?;
    source_dir_in_root(path, old_dir)?;
    let dst = new_dir.join(rel);
    copy_dir_recursive(path, &dst, new_dir).ok()?;
    Some(dst)
}

fn copy_dir_recursive(src: &Path, dst: &Path, new_root: &Path) -> std::io::Result<()> {
    source_dir_in_root(src, src.parent().unwrap_or(src)).ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::PermissionDenied, "unsafe source")
    })?;
    prepare_destination_dir(dst, new_root)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let path = entry.path();
        let target = dst.join(entry.file_name());
        let meta = std::fs::symlink_metadata(&path)?;
        if is_link_or_junction(&meta) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "symlink source",
            ));
        }
        if meta.is_dir() {
            copy_dir_recursive(&path, &target, new_root)?;
        } else {
            if !meta.is_file() || target.symlink_metadata().is_ok() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::AlreadyExists,
                    "unsafe destination",
                ));
            }
            prepare_destination(&target, new_root)?;
            copy_file_new(&path, &target)?;
        }
    }
    Ok(())
}

fn valid_path_component(value: &str) -> bool {
    let mut components = Path::new(value).components();
    matches!((components.next(), components.next()),
        (Some(std::path::Component::Normal(name)), None) if name == value)
}

fn safe_relative(path: &Path, root: &Path) -> Option<PathBuf> {
    let rel = path.strip_prefix(root).ok()?;
    if rel
        .components()
        .any(|c| !matches!(c, std::path::Component::Normal(_)))
    {
        return None;
    }
    Some(rel.to_path_buf())
}

fn source_path_in_root(path: &Path, root: &Path, want_dir: bool) -> Option<PathBuf> {
    let rel = safe_relative(path, root)?;
    let root_meta = std::fs::symlink_metadata(root).ok()?;
    if is_link_or_junction(&root_meta) {
        return None;
    }
    let root_canon = root.canonicalize().ok()?;
    let canon = path.canonicalize().ok()?;
    if !canon.starts_with(&root_canon) || canon == root_canon {
        return None;
    }
    let mut cur = root.to_path_buf();
    for component in rel.components() {
        cur.push(component.as_os_str());
        if std::fs::symlink_metadata(&cur)
            .ok()
            .is_some_and(|m| is_link_or_junction(&m))
        {
            return None;
        }
    }
    let meta = std::fs::symlink_metadata(path).ok()?;
    if (want_dir && !meta.is_dir()) || (!want_dir && !meta.is_file()) {
        return None;
    }
    Some(rel)
}

fn source_file_in_root(path: &Path, root: &Path) -> Option<PathBuf> {
    source_path_in_root(path, root, false)
}

fn source_dir_in_root(path: &Path, root: &Path) -> Option<PathBuf> {
    source_path_in_root(path, root, true)
}

fn prepare_destination(path: &Path, root: &Path) -> std::io::Result<()> {
    let rel = safe_relative(path, root).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "unsafe destination path",
        )
    })?;
    validate_destination_ancestors(path, root)?;
    let root_canon = root
        .canonicalize()
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::NotFound, e))?;
    let parent = path.parent().unwrap_or(root);
    std::fs::create_dir_all(parent)?;
    let parent_canon = parent.canonicalize()?;
    if !parent_canon.starts_with(&root_canon) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "destination outside root",
        ));
    }
    let mut cur = root.to_path_buf();
    for component in rel.parent().into_iter().flat_map(|p| p.components()) {
        cur.push(component.as_os_str());
        if is_link_or_junction(&std::fs::symlink_metadata(&cur)?) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "symlink destination",
            ));
        }
    }
    if path.symlink_metadata().is_ok() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "destination exists",
        ));
    }
    Ok(())
}

fn prepare_destination_dir(path: &Path, root: &Path) -> std::io::Result<()> {
    let rel = safe_relative(path, root).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "unsafe destination path",
        )
    })?;
    validate_destination_ancestors(path, root)?;
    let root_canon = root.canonicalize()?;
    if let Ok(meta) = path.symlink_metadata() {
        if is_link_or_junction(&meta) || !meta.is_dir() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "unsafe destination",
            ));
        }
    } else {
        std::fs::create_dir_all(path)?;
    }
    let canon = path.canonicalize()?;
    if !canon.starts_with(&root_canon) || canon == root_canon {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "destination outside root",
        ));
    }
    let mut cur = root.to_path_buf();
    for component in rel.components() {
        cur.push(component.as_os_str());
        if is_link_or_junction(&std::fs::symlink_metadata(&cur)?) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "symlink destination",
            ));
        }
    }
    Ok(())
}

fn validate_destination_ancestors(path: &Path, root: &Path) -> std::io::Result<()> {
    let rel = safe_relative(path, root).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "unsafe destination path",
        )
    })?;
    let root_meta = std::fs::symlink_metadata(root)?;
    if is_link_or_junction(&root_meta) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "symlink destination root",
        ));
    }
    let root_canon = root.canonicalize()?;
    let mut cur = root.to_path_buf();
    for component in rel.parent().into_iter().flat_map(|p| p.components()) {
        cur.push(component.as_os_str());
        match std::fs::symlink_metadata(&cur) {
            Ok(meta) => {
                if is_link_or_junction(&meta) {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        "symlink destination",
                    ));
                }
                if !cur.canonicalize()?.starts_with(&root_canon) {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        "destination outside root",
                    ));
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => break,
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

fn is_link_or_junction(meta: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        meta.file_type().is_symlink() || (meta.file_attributes() & 0x400) != 0
    }
    #[cfg(not(windows))]
    {
        meta.file_type().is_symlink()
    }
}

/// Exclusive fresh-file copy through rightkit-fs: destination created exclusively
/// (an existing name is an error), never a hard link or clone, default new-file
/// mode, no copied extended attributes, nothing partial left on failure.
fn copy_file_new(src: &Path, dst: &Path) -> std::io::Result<()> {
    let options = rightkit_fs::CloneOptions::default().fresh_metadata(true);
    rightkit_fs::clone_or_link_or_copy_nofollow_with(src, dst, &options)
        .map(|_| ())
        .map_err(fs_error_to_io)
}

/// Map a shared copy error to the `std::io::Error` the in-house copy produced:
/// the underlying I/O error as is, and "already exists" as the OS "file exists"
/// error (`File exists (os error 17)` on Unix, `The file exists. (os error 80)` on Windows).
fn fs_error_to_io(error: rightkit_fs::FsError) -> std::io::Error {
    match error {
        rightkit_fs::FsError::Io { source, .. } => source,
        rightkit_fs::FsError::AlreadyExists(_) => {
            #[cfg(windows)]
            {
                std::io::Error::from_raw_os_error(80)
            }
            #[cfg(not(windows))]
            {
                std::io::Error::from_raw_os_error(17)
            }
        }
        other => std::io::Error::other(other.to_string()),
    }
}

/// Rows examined per page request while text-searching, so one call cannot
/// read an unbounded number of transcript bodies.
const SEARCH_SCAN_BUDGET: usize = 2000;
const SEARCH_SCAN_CHUNK: usize = 250;
/// Transcript bytes read per row while text-searching.
const SEARCH_BODY_BYTES: u64 = 4 * 1024 * 1024;

/// Cursor text for a row under `order`. Newest keeps the legacy two-part form
/// `"{created_at}\n{id}"`; the other orders add their tag and rank:
/// `"{order}\n{rank}\n{created_at}\n{id}"`.
fn rank_text(rank: f64) -> String {
    format!("{rank:?}")
}

fn order_tag(order: HistoryOrder) -> &'static str {
    match order {
        HistoryOrder::Newest => "newest",
        HistoryOrder::Oldest => "oldest",
        HistoryOrder::Longest => "longest",
        HistoryOrder::Shortest => "shortest",
    }
}

fn order_of(sort: HistorySort) -> HistoryOrder {
    match sort {
        HistorySort::Newest => HistoryOrder::Newest,
        HistorySort::Oldest => HistoryOrder::Oldest,
        HistorySort::Longest => HistoryOrder::Longest,
        HistorySort::Shortest => HistoryOrder::Shortest,
    }
}

/// The keyset position of `rec` as a cursor value (what `scan_page` stores).
fn record_cursor(order: HistoryOrder, rec: &HistoryRecord) -> Cursor {
    let key = order.key_of(rec);
    match order {
        HistoryOrder::Newest => Cursor::new(vec![Param::Text(key.created_at), Param::Text(key.id)]),
        _ => Cursor::new(vec![
            Param::Text(rank_text(key.rank)),
            Param::Text(key.created_at),
            Param::Text(key.id),
        ]),
    }
}

/// Client-facing cursor text for a keyset position.
fn cursor_text(order: HistoryOrder, cursor: &Cursor) -> Option<String> {
    match (order, cursor.values()) {
        (HistoryOrder::Newest, [Param::Text(created_at), Param::Text(id)]) => {
            Some(format!("{created_at}\n{id}"))
        }
        (_, [Param::Text(rank), Param::Text(created_at), Param::Text(id)])
            if order != HistoryOrder::Newest =>
        {
            Some(format!("{}\n{rank}\n{created_at}\n{id}", order_tag(order)))
        }
        _ => None,
    }
}

/// Parses a client cursor for `order`. A cursor from another order is rejected
/// rather than silently re-read under this one.
fn decode_cursor(order: HistoryOrder, cursor: &str) -> Result<Cursor, String> {
    let bad = || "invalid history cursor".to_string();
    let parts: Vec<&str> = cursor.split('\n').collect();
    match (order, parts.as_slice()) {
        (HistoryOrder::Newest, [created_at, id]) => Ok(Cursor::new(vec![
            Param::Text(created_at.to_string()),
            Param::Text(id.to_string()),
        ])),
        (other, [tag, rank, created_at, id])
            if other != HistoryOrder::Newest && *tag == order_tag(other) =>
        {
            rank.parse::<f64>().map_err(|_| bad())?;
            Ok(Cursor::new(vec![
                Param::Text(rank.to_string()),
                Param::Text(created_at.to_string()),
                Param::Text(id.to_string()),
            ]))
        }
        _ => Err(bad()),
    }
}

/// The store keyset position named by a decoded cursor.
fn page_key(order: HistoryOrder, after: Option<&Cursor>) -> Result<Option<PageKey>, String> {
    let bad = || "invalid history cursor".to_string();
    let Some(cursor) = after else {
        return Ok(None);
    };
    match (order, cursor.values()) {
        (HistoryOrder::Newest, [Param::Text(created_at), Param::Text(id)]) => Ok(Some(PageKey {
            rank: 0.0,
            created_at: created_at.clone(),
            id: id.clone(),
        })),
        (other, [Param::Text(rank), Param::Text(created_at), Param::Text(id)])
            if other != HistoryOrder::Newest =>
        {
            Ok(Some(PageKey {
                rank: rank.parse::<f64>().map_err(|_| bad())?,
                created_at: created_at.clone(),
                id: id.clone(),
            }))
        }
        _ => Err(bad()),
    }
}

/// Cursored history page in the requested order, with the total matching
/// rows on the first page (no cursor). Every order is a keyset walk over its
/// own index. The total is exact for an unfiltered first page (an index count);
/// a text query's matches depend on transcript bodies that SQL cannot see, so
/// no total is reported for it. Whole-query substring, not query syntax:
/// phrases, `-`, `OR` and quotes stay literal. A text query matches
/// title/url/tags/snippet and the first 4 MiB of the transcript body.
pub fn get_history_page(
    settings: &HistorySettings,
    query: Option<String>,
    cursor: Option<String>,
    limit: Option<u32>,
    sort: Option<HistorySort>,
) -> Result<HistoryPage, String> {
    let path = db_path(settings)?;
    let shared = cached_store(&path)?;
    let store = lock_store(&shared);
    let q = query.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let limit = limit
        .unwrap_or(HISTORY_PAGE_SIZE)
        .clamp(1, HISTORY_PAGE_MAX) as usize;
    let order = order_of(sort.unwrap_or_default());
    let after = cursor
        .as_deref()
        .map(|c| decode_cursor(order, c))
        .transpose()?;

    // Whole-query substring, not query syntax: phrases, `-`, `OR` and quotes stay literal.
    let matcher = q
        .map(|q| TextMatcher::new(&Query::literal(q)).map_err(|e| e.to_string()))
        .transpose()?;
    let total = match (cursor.is_none(), &matcher) {
        (true, None) => Some(store.count().map_err(|e| e.to_string())?),
        _ => None,
    };
    let budget = match q {
        Some(_) => ScanBudget::new(SEARCH_SCAN_BUDGET, SEARCH_SCAN_CHUNK),
        None => ScanBudget::new(limit + 1, limit + 1),
    };
    // One extra hit tells whether another match exists beyond this page.
    let mut page = scan_page(
        after,
        limit + 1,
        budget,
        |after, n| {
            let key = page_key(order, after)?;
            store
                .page_ordered(order, key.as_ref(), n)
                .map_err(|e| e.to_string())
        },
        |rec| record_cursor(order, rec),
        |rec| match &matcher {
            Some(matcher) => record_matches(rec, matcher),
            None => true,
        },
    )?;
    let next_cursor = if page.hits.len() > limit {
        page.hits.truncate(limit);
        page.hits
            .last()
            .map(|rec| encode_page_key(order, &order.key_of(rec)))
    } else {
        page.next.as_ref().and_then(|c| cursor_text(order, c))
    };
    Ok(HistoryPage {
        rows: page.hits.into_iter().map(HistoryRow::from).collect(),
        next_cursor,
        total,
    })
}

/// Cursor text for a store keyset position (the page's last row).
fn encode_page_key(order: HistoryOrder, key: &PageKey) -> String {
    match order {
        HistoryOrder::Newest => format!("{}\n{}", key.created_at, key.id),
        _ => format!(
            "{}\n{}\n{}\n{}",
            order_tag(order),
            rank_text(key.rank),
            key.created_at,
            key.id
        ),
    }
}

fn record_matches(rec: &HistoryRecord, matcher: &TextMatcher) -> bool {
    let fields = [
        rec.url.as_str(),
        rec.title.as_deref().unwrap_or(""),
        rec.snippet.as_deref().unwrap_or(""),
        rec.tags.as_deref().unwrap_or(""),
    ];
    matcher.matches_with(&fields, || transcript_body(rec))
}

/// First 4 MiB of a row's `txt_path` file, cut on a character boundary; empty
/// when the row has no transcript file or it cannot be read.
fn transcript_body(rec: &HistoryRecord) -> Vec<String> {
    rec.txt_path
        .as_deref()
        .and_then(|txt| read_text_prefix(Path::new(txt), SEARCH_BODY_BYTES).ok())
        .into_iter()
        .collect()
}

/// Preview bytes of `text` carried by the slim `get_transcript` reply.
pub const TRANSCRIPT_PREVIEW_BYTES: usize = 8 * 1024;

/// Slim detail: metadata, a bounded `text` preview and per-variant sizes. The
/// full variants come from `get_transcript_variant`, keeping the reply small.
pub fn get_transcript(settings: &HistorySettings, id: &str) -> Result<TranscriptDetail, String> {
    let rec = get_record(settings, id)?;
    let mut variants = Vec::new();
    let mut preview = String::new();
    for kind in TranscriptVariantKind::ALL {
        if let Some(body) = read_transcript_variant(&rec, kind) {
            if kind == TranscriptVariantKind::Text {
                let mut end = body.len().min(TRANSCRIPT_PREVIEW_BYTES);
                while !body.is_char_boundary(end) {
                    end -= 1;
                }
                preview = body[..end].to_string();
            }
            variants.push(TranscriptVariantInfo {
                kind,
                bytes: body.len() as u64,
            });
        }
    }
    let slides = collect_slide_paths(rec.slide_dir.as_deref())?;
    Ok(TranscriptDetail {
        id: rec.id,
        url: rec.url,
        kind: Kind::from_tag(&rec.kind),
        title: rec.title,
        text: preview,
        md_text: None,
        timed_text: None,
        timed_md_text: None,
        srt: None,
        slides,
        media_path: rec.media_path,
        media_meta: rec.media_meta,
        media_action: rec.media_action,
        variants,
        truncated_at_s: rec.truncated_at_s,
    })
}

/// One full variant on demand. `text` is `None` when the row lacks it.
pub fn get_transcript_variant(
    settings: &HistorySettings,
    id: &str,
    kind: TranscriptVariantKind,
) -> Result<TranscriptVariant, String> {
    let rec = get_record(settings, id)?;
    let text = read_transcript_variant(&rec, kind);
    Ok(TranscriptVariant {
        id: rec.id,
        kind,
        text,
    })
}

/// All-variants detail (can exceed the reply cap on large rows).
pub fn get_transcript_full(
    settings: &HistorySettings,
    id: &str,
) -> Result<TranscriptDetail, String> {
    let rec = get_record(settings, id)?;
    transcript_detail_from_record(rec)
}

fn read_transcript_variant(rec: &HistoryRecord, kind: TranscriptVariantKind) -> Option<String> {
    let read = |p: &String| std::fs::read_to_string(p).ok();
    let srt = || {
        rec.srt_path
            .as_ref()
            .and_then(read)
            .or_else(|| rec.srt_text.clone())
    };
    match kind {
        TranscriptVariantKind::Text => rec.txt_path.as_ref().and_then(read),
        TranscriptVariantKind::MdText => rec.md_path.as_ref().and_then(read),
        TranscriptVariantKind::Srt => srt(),
        TranscriptVariantKind::TimedText => rec
            .txt_path
            .as_ref()
            .and_then(|p| sibling_path(p, "timed.txt"))
            .and_then(|p| std::fs::read_to_string(p).ok())
            .or_else(|| srt().map(|s| timed_text_and_md_from_srt(&s).0)),
        TranscriptVariantKind::TimedMdText => rec
            .md_path
            .as_ref()
            .and_then(|p| sibling_path(p, "timed.md"))
            .and_then(|p| std::fs::read_to_string(p).ok())
            .or_else(|| srt().map(|s| timed_text_and_md_from_srt(&s).1)),
    }
}

pub fn get_transcript_row(settings: &HistorySettings, id: &str) -> Result<HistoryRow, String> {
    get_record(settings, id).map(HistoryRow::from)
}

pub fn delete_transcript(settings: &HistorySettings, id: &str) -> Result<(), String> {
    // WS-6: serializes row removal against `attach_transcript_to_media`'s
    // check+upsert, so a delete landing mid-transcription can never race a
    // late completion into resurrecting the row.
    let _lifecycle = lock_lifecycle();
    let path = db_path(settings)?;
    let shared = cached_store(&path)?;
    let store = lock_store(&shared);
    let rec = store
        .get(id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "transcript not found".to_string())?;
    delete_record_artifacts(&rec, settings)?;
    store.delete(id).map_err(|e| e.to_string())?;
    tombstone(id);
    Ok(())
}

// ---- WS-6: attach attempts -------------------------------------------------
//
// A download-only history row can gain a transcript later via a queue
// *attempt* (`JobInfo.attach_to` = the row's id). The attempt id is the queue
// identity; the row id is the library identity. Writing the attempt result
// must therefore update the existing row in place — and must never recreate
// a row that was deleted while the attempt ran.

/// Serializes the "does the target row still exist?" check plus the row
/// update against `delete_transcript`, which takes the same lock for its
/// whole read→artifacts→delete sequence. Without it, a delete between the
/// check and the upsert would silently resurrect the item.
static ROW_LIFECYCLE: Mutex<()> = Mutex::new(());

/// Ids deleted in this process. A row could be re-materialized by a cloud
/// merge/import *after* a delete; an attach completion that already passed
/// the "exists" check would then write the old attempt's transcript onto the
/// new row. The tombstone makes "was deleted since I started" observable.
/// Bounded — at capacity it clears (the missing-row check still guards the
/// common delete→late-completion race).
static TOMBSTONED: std::sync::LazyLock<Mutex<HashSet<String>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashSet::new()));
const TOMBSTONE_CAP: usize = 4096;

fn lock_lifecycle() -> std::sync::MutexGuard<'static, ()> {
    ROW_LIFECYCLE.lock().unwrap_or_else(|p| p.into_inner())
}

fn tombstone(id: &str) {
    if let Ok(mut set) = TOMBSTONED.lock() {
        if set.len() >= TOMBSTONE_CAP {
            set.clear();
        }
        set.insert(id.to_string());
    }
}

fn clear_tombstone(id: &str) {
    if let Ok(mut set) = TOMBSTONED.lock() {
        set.remove(id);
    }
}

fn is_tombstoned(id: &str) -> bool {
    TOMBSTONED
        .lock()
        .map(|set| set.contains(id))
        .unwrap_or(false)
}

/// Fast pre-flight check used when an attach attempt starts: has the target
/// row already gone? A store error answers `true` so a transient DB problem
/// doesn't kill the job — the completion path re-checks under the lifecycle
/// lock and surfaces the real error there.
pub fn attach_target_exists(out_dir: &Path, id: &str) -> bool {
    if is_tombstoned(id) {
        return false;
    }
    let Ok(shared) = cached_store(&out_dir.join("history.db")) else {
        return false;
    };
    let store = lock_store(&shared);
    store.get(id).ok().flatten().is_some()
}

/// The transcript surfaces a completed attach attempt writes onto its target
/// row. `extra_paths` carries the sibling artifacts (timed files, `.vtt`,
/// `.json`) written alongside the primaries so a discarded completion can
/// remove every file the attempt produced.
pub struct AttachTranscript {
    pub txt_path: PathBuf,
    pub md_path: PathBuf,
    pub srt_path: Option<PathBuf>,
    pub extra_paths: Vec<PathBuf>,
    pub srt_text: Option<String>,
    pub snippet: String,
    pub lang: Option<String>,
    pub tags: Option<String>,
    pub duration_s: Option<f64>,
    pub truncated_at_s: Option<f64>,
}

pub enum AttachOutcome {
    /// The target row was updated in place — media fields preserved.
    Attached,
    /// The target row is missing or tombstoned. The attempt's artifacts were
    /// removed and no row was (re)created.
    Discarded,
}

/// `<path>.bak` sibling used to hold the previous contents of a live file
/// while `attach_transcript_to_media_staged` publishes over it.
fn backup_sibling(tmp: &Path) -> PathBuf {
    let mut name = tmp.as_os_str().to_os_string();
    name.push(".bak");
    PathBuf::from(name)
}

/// A leftover `<final>.job-<n>-attach.tmp.bak` beside `final_path`: recovery material of an
/// earlier attach that was never resolved. `Ok(None)` only when the folder was listed completely
/// and holds none; a folder or entry that cannot be read is an error, never "no backup" (a
/// listing can fail while a write still works).
fn unresolved_attach_backup(final_path: &Path) -> Result<Option<PathBuf>, String> {
    let (Some(dir), Some(final_name)) = (
        final_path.parent(),
        final_path.file_name().and_then(|n| n.to_str()),
    ) else {
        return Err(format!(
            "{} has no folder or a non-UTF-8 name",
            final_path.display()
        ));
    };
    let entries = std::fs::read_dir(dir)
        .map_err(|e| format!("cannot list {} for recovery material: {e}", dir.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| {
            format!(
                "cannot read an entry of {} for recovery material: {e}",
                dir.display()
            )
        })?;
        let name = entry.file_name();
        if let Some((name_final, true)) = name.to_str().and_then(parse_attach_leftover) {
            if name_final == final_name {
                return Ok(Some(entry.path()));
            }
        }
    }
    Ok(None)
}

/// Undo a partial publish: restore every file in `done` from its backup (or
/// remove it when it did not exist before), then delete all backups. Returns
/// a note naming anything that could not be restored.
fn rollback_staged_publish(done: &[(PathBuf, PathBuf)], all: &[(PathBuf, PathBuf)]) -> String {
    let mut problems = Vec::new();
    for (tmp, final_path) in done {
        let bak = backup_sibling(tmp);
        if bak.exists() {
            if let Err(e) = std::fs::rename(&bak, final_path) {
                problems.push(format!("could not restore {}: {e}", final_path.display()));
            }
        } else if let Err(e) = std::fs::remove_file(final_path) {
            // Already gone is the wanted state; anything else is a file we failed to take back.
            if e.kind() != std::io::ErrorKind::NotFound {
                problems.push(format!("could not remove {}: {e}", final_path.display()));
            }
        }
    }
    for (tmp, _) in all {
        // Backups of files that were restored are already gone; any left over
        // belong to a file that could not be restored and stay for recovery.
        if problems.is_empty() {
            let _ = std::fs::remove_file(backup_sibling(tmp));
        }
    }
    problems.join("; ")
}

/// Start-up sweep for `<output>/_replay/<id>` folders whose history row is gone
/// (#117): a crash between the replay write and the row write, or a row
/// removed while the engine was down, leaves them behind. Removes only real
/// directories (a link is never followed or deleted) directly under `_replay`,
/// and only when the store can be read and has no row for that name. A missing
/// or unreadable database sweeps nothing. Returns how many were removed.
pub fn sweep_orphan_replays(output_dir: &Path) -> usize {
    let db = output_dir.join("history.db");
    let replay_root = output_dir.join("_replay");
    if !db.is_file() {
        return 0;
    }
    let Ok(entries) = std::fs::read_dir(&replay_root) else {
        return 0;
    };
    let Ok(shared) = cached_store(&db) else {
        return 0;
    };
    let _lifecycle = lock_lifecycle();
    let store = lock_store(&shared);
    let mut removed = 0usize;
    for entry in entries.flatten() {
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if !kind.is_dir() || kind.is_symlink() {
            continue;
        }
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        match store.get(name) {
            Ok(None) => {
                if std::fs::remove_dir_all(entry.path()).is_ok() {
                    removed += 1;
                }
            }
            Ok(Some(_)) => {}
            // Cannot prove an orphan: stop rather than guess.
            Err(_) => return removed,
        }
    }
    removed
}

/// Most distinct folders the start-up attach recovery will scan.
const ATTACH_RECOVERY_DIR_CAP: usize = 5000;

/// Split an attach leftover file name into `(final name, is_backup)`.
/// Matches exactly `<final>.job-<digits>-attach.tmp` and
/// `<final>.job-<digits>-attach.tmp.bak` (the id minted in
/// `src-tauri/src/commands_files.rs`); anything else returns `None`.
fn parse_attach_leftover(name: &str) -> Option<(&str, bool)> {
    let (rest, is_bak) = match name.strip_suffix(".bak") {
        Some(rest) => (rest, true),
        None => (name, false),
    };
    let rest = rest.strip_suffix(".tmp")?;
    let (final_name, id) = rest.rsplit_once('.')?;
    let digits = id.strip_prefix("job-")?.strip_suffix("-attach")?;
    if final_name.is_empty() || digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some((final_name, is_bak))
}

/// Start-up recovery for an attach publish interrupted by a crash or kill.
/// Scans the folders of the files history rows reference (never the whole
/// output tree) for attach leftovers and, per file:
/// - `.bak`, final missing: rename the backup back to the final name;
/// - `.bak`, final present, identical content: delete the backup;
/// - `.bak`, final present, different content: rename it to
///   `<final>.recovered-<unix_millis>` (it may be the user's earlier transcript);
/// - `.tmp`: never published, delete it.
///
/// Files that do not match those exact patterns are never touched. Silent when
/// the store cannot be opened; best-effort throughout.
pub fn recover_interrupted_attach_publishes(output_dir: &Path) {
    const TARGET: &str = "rightkit_library::history";
    let db = output_dir.join("history.db");
    if !db.is_file() {
        return;
    }
    let Ok(store) = Store::open(&db) else {
        return;
    };
    let mut dirs: HashSet<PathBuf> = HashSet::new();
    let mut capped = false;
    let mut after: Option<(String, String)> = None;
    'rows: loop {
        let batch = match store.page(after.as_ref().map(|(a, b)| (a.as_str(), b.as_str())), 500) {
            Ok(batch) => batch,
            Err(_) => break,
        };
        let Some(last) = batch.last() else {
            break;
        };
        after = Some((last.created_at.clone(), last.id.clone()));
        let full = batch.len() == 500;
        for rec in &batch {
            for path in [&rec.media_path, &rec.txt_path, &rec.md_path, &rec.srt_path]
                .into_iter()
                .flatten()
            {
                if let Some(dir) = Path::new(path).parent() {
                    if dir.as_os_str().is_empty() || dirs.contains(dir) {
                        continue;
                    }
                    if dirs.len() >= ATTACH_RECOVERY_DIR_CAP {
                        capped = true;
                        break 'rows;
                    }
                    dirs.insert(dir.to_path_buf());
                }
            }
        }
        if !full {
            break;
        }
    }
    if capped {
        tracing::warn!(
            target: TARGET,
            cap = ATTACH_RECOVERY_DIR_CAP,
            "attach recovery scanned only the first folders; the cap was reached"
        );
    }
    drop(store);
    let _lifecycle = lock_lifecycle();
    for dir in &dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        let mut found: Vec<(PathBuf, String, bool)> = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            let Some((final_name, is_bak)) = parse_attach_leftover(name) else {
                continue;
            };
            if !path.is_file() {
                continue;
            }
            let final_name = final_name.to_string();
            found.push((path, final_name, is_bak));
        }
        // Backups first: a restore must happen before anything else is judged.
        found.sort_by_key(|(_, _, is_bak)| !*is_bak);
        for (path, final_name, is_bak) in found {
            if !is_bak {
                match std::fs::remove_file(&path) {
                    Ok(()) => tracing::info!(
                        target: TARGET,
                        path = %path.display(),
                        "removed unpublished attach temporary"
                    ),
                    Err(e) => tracing::warn!(
                        target: TARGET,
                        path = %path.display(),
                        error = %e,
                        "could not remove attach temporary"
                    ),
                }
                continue;
            }
            let final_path = dir.join(&final_name);
            if !final_path.exists() {
                match std::fs::rename(&path, &final_path) {
                    Ok(()) => tracing::info!(
                        target: TARGET,
                        backup = %path.display(),
                        restored = %final_path.display(),
                        "restored earlier transcript from interrupted attach publish"
                    ),
                    Err(e) => tracing::warn!(
                        target: TARGET,
                        backup = %path.display(),
                        error = %e,
                        "could not restore attach backup"
                    ),
                }
                continue;
            }
            let identical = match (std::fs::read(&path), std::fs::read(&final_path)) {
                (Ok(a), Ok(b)) => Some(a == b),
                _ => None,
            };
            match identical {
                Some(true) => match std::fs::remove_file(&path) {
                    Ok(()) => tracing::info!(
                        target: TARGET,
                        backup = %path.display(),
                        "removed attach backup identical to the published file"
                    ),
                    Err(e) => tracing::warn!(
                        target: TARGET,
                        backup = %path.display(),
                        error = %e,
                        "could not remove attach backup"
                    ),
                },
                Some(false) => {
                    let millis = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_millis())
                        .unwrap_or(0);
                    let aside = dir.join(format!("{final_name}.recovered-{millis}"));
                    match std::fs::rename(&path, &aside) {
                        Ok(()) => tracing::info!(
                            target: TARGET,
                            backup = %path.display(),
                            kept_as = %aside.display(),
                            "kept differing attach backup as a recovered file"
                        ),
                        Err(e) => tracing::warn!(
                            target: TARGET,
                            backup = %path.display(),
                            error = %e,
                            "could not set attach backup aside"
                        ),
                    }
                }
                None => tracing::warn!(
                    target: TARGET,
                    backup = %path.display(),
                    "could not compare attach backup with the published file; left in place"
                ),
            }
        }
    }
}

/// Same contract as `attach_transcript_to_media`, plus staged publication.
///
/// `staged` is `(temporary, final)` pairs, each in the same directory, holding
/// the attempt's complete output. Inside the lifecycle lock, and only when the
/// row still exists and `gate_closed()` is false, every temporary is renamed
/// over its final name and the row is updated; if any step fails the previous
/// final files are restored and the row is left untouched. When the gate is
/// closed or the row is gone, the finals are not touched at all (the caller
/// removes the temporaries) and `Discarded` is returned. With an empty
/// `staged` the behavior is exactly the unstaged one.
pub fn attach_transcript_to_media_staged(
    out_dir: &Path,
    target_id: &str,
    attach: &AttachTranscript,
    staged: &[(PathBuf, PathBuf)],
    gate_closed: &dyn Fn() -> bool,
) -> Result<AttachOutcome, String> {
    let _lifecycle = lock_lifecycle();
    let shared =
        cached_store(&out_dir.join("history.db")).map_err(|e| format!("open history.db: {e}"))?;
    let store = lock_store(&shared);
    let rec = store.get(target_id).map_err(|e| e.to_string())?;
    match rec {
        Some(_) if !staged.is_empty() && gate_closed() => {
            // Cancelled or removed after the writer's entry check: the item's
            // existing transcript files were never touched.
            Ok(AttachOutcome::Discarded)
        }
        Some(mut rec) if !is_tombstoned(target_id) => {
            rec.txt_path = Some(path_to_string(&attach.txt_path));
            rec.md_path = Some(path_to_string(&attach.md_path));
            if let Some(srt_path) = &attach.srt_path {
                rec.srt_path = Some(path_to_string(srt_path));
            }
            rec.srt_text = attach.srt_text.clone().or(rec.srt_text);
            rec.snippet = Some(attach.snippet.clone());
            if let Some(lang) = &attach.lang {
                rec.lang = Some(lang.clone());
            }
            if let Some(tags) = &attach.tags {
                rec.tags = Some(tags.clone());
            }
            if let Some(duration_s) = attach.duration_s {
                rec.duration_s = Some(duration_s);
            }
            // Attach only ever runs under Pro, which never truncates — the
            // attempt's value (None) is authoritative.
            rec.truncated_at_s = attach.truncated_at_s;
            rec.media_action = Some(media_action_for_attach(&rec).to_string());
            // Publish: back up any live final, rename each temporary over its
            // final name (same directory, atomic per file), then update the
            // row. Any failure restores the previous files; the row is only
            // written after every file is in place.
            let mut done: Vec<(PathBuf, PathBuf)> = Vec::new();
            let mut failure: Option<String> = None;
            // Never overwrite recovery material a crashed or failed attach left behind: its
            // `.bak` may be the only copy of the user's earlier transcript.
            for (tmp, final_path) in staged {
                // A recovered job reuses its attempt id, so its own backup name can already be
                // taken by the material a failed start-up recovery left; the folder scan covers
                // every other attempt of the same final.
                let own = backup_sibling(tmp);
                let own_taken = std::fs::symlink_metadata(&own).map(|_| true).or_else(|e| {
                    if e.kind() == std::io::ErrorKind::NotFound {
                        Ok(false)
                    } else {
                        Err(format!("cannot inspect {}: {e}", own.display()))
                    }
                })?;
                let leftover = if own_taken {
                    Some(own)
                } else {
                    unresolved_attach_backup(final_path)?
                };
                if let Some(bak) = leftover {
                    return Err(format!(
                        "attach refused: unresolved recovery material {} for {}; \
                         resolve it (start-up recovery or restore it by hand) first",
                        bak.display(),
                        final_path.display()
                    ));
                }
            }
            for (tmp, final_path) in staged {
                if final_path.is_file() {
                    if let Err(e) = std::fs::copy(final_path, backup_sibling(tmp)) {
                        let removed = std::fs::remove_file(backup_sibling(tmp));
                        failure = Some(match removed {
                            Err(rm) if rm.kind() != std::io::ErrorKind::NotFound => format!(
                                "back up {}: {e} (and the partial backup could not be removed: {rm})",
                                final_path.display()
                            ),
                            _ => format!("back up {}: {e}", final_path.display()),
                        });
                        break;
                    }
                }
                match publish_replacing(tmp, final_path) {
                    Ok(()) => {}
                    // The owner reports a failed directory sync or temp cleanup AFTER the
                    // replacement (`target_replaced()`): the new file IS in place, so this pair
                    // joins the rollback (its backup is the only copy of the old file) and the
                    // attach FAILS like any other step failure; it never continues to the row.
                    Err(e) if e.target_replaced() => {
                        done.push((tmp.clone(), final_path.clone()));
                        failure = Some(format!(
                            "publish {} (replaced, then a post-step failed): {e}",
                            final_path.display()
                        ));
                        break;
                    }
                    Err(e) => {
                        // Nothing replaced `final_path` (the owner leaves the destination
                        // untouched on every other error), so the backup is a copy of a file
                        // that is still live and can go.
                        let bak = backup_sibling(tmp);
                        if bak.exists() {
                            if let Err(rm) = std::fs::remove_file(&bak) {
                                failure = Some(format!(
                                    "publish {}: {e} (and the backup {} could not be removed: {rm})",
                                    final_path.display(),
                                    bak.display()
                                ));
                                break;
                            }
                        }
                        failure = Some(format!("publish {}: {e}", final_path.display()));
                        break;
                    }
                }
                done.push((tmp.clone(), final_path.clone()));
            }
            // Last look before the row is written: a cancel or removal that landed
            // while the files were being renamed rolls the publish back instead
            // of recording it (#62). Only the upsert itself stays unchecked.
            if failure.is_none() && !staged.is_empty() && gate_closed() {
                // `Discarded` only when the old files are really back; a failed restore is an
                // error that names what is left.
                let note = rollback_staged_publish(&done, staged);
                return if note.is_empty() {
                    Ok(AttachOutcome::Discarded)
                } else {
                    Err(format!(
                        "attach discarded (cancelled or removed), but its files could not be restored: {note}"
                    ))
                };
            }
            if failure.is_none() {
                if let Err(e) = store.upsert(&rec) {
                    failure = Some(e.to_string());
                }
            }
            if let Some(message) = failure {
                let note = rollback_staged_publish(&done, staged);
                return Err(if note.is_empty() {
                    message
                } else {
                    format!("{message} ({note})")
                });
            }
            for (tmp, _) in staged {
                // The attach is committed; a backup that cannot be removed is recovery material
                // for the next start-up (identical content is dropped, different content is kept
                // as `.recovered-*`) and blocks another attach of the same file until then.
                let bak = backup_sibling(tmp);
                if let Err(e) = std::fs::remove_file(&bak) {
                    if e.kind() != std::io::ErrorKind::NotFound {
                        tracing::warn!(
                            target: "rightkit_library::history",
                            backup = %bak.display(),
                            error = %e,
                            "attach committed but its backup could not be removed"
                        );
                    }
                }
            }
            Ok(AttachOutcome::Attached)
        }
        _ => {
            // Late completion against a deleted or tombstoned row: drop every
            // artifact the attempt wrote and never recreate the row.
            if staged.is_empty() {
                for path in [&attach.txt_path, &attach.md_path]
                    .into_iter()
                    .chain(attach.srt_path.iter())
                    .chain(attach.extra_paths.iter())
                {
                    let _ = std::fs::remove_file(path);
                }
            }
            Ok(AttachOutcome::Discarded)
        }
    }
}

/// Mirrors the frontend's `savedMediaKind`: the saved container wins over the
/// recorded `kind` — a `.mp4` is a video even when an older build labelled
/// the row audio. Unknown containers default to video, matching the UI.
fn media_action_for_attach(rec: &HistoryRecord) -> &'static str {
    let ext_is_video = rec
        .media_path
        .as_deref()
        .and_then(|p| Path::new(p).extension())
        .and_then(|e| e.to_str())
        .is_some_and(|e| {
            matches!(
                e.to_ascii_lowercase().as_str(),
                "mp4" | "m4v" | "mkv" | "mov" | "avi"
            )
        });
    if ext_is_video {
        return "both_video";
    }
    match rec.media_meta.as_deref() {
        Some(meta) if meta.contains(r#""kind":"audio""#) => "both_audio",
        _ => "both_video",
    }
}

/// WS-12: land a phone-published transcript in desktop history. `item_id` is
/// the canonical cross-device identity — a re-import upserts rather than
/// duplicating. Artifacts are materialized under `output_dir` with the same
/// `{id}.txt`/`{id}.md` naming the writer uses, so the reader, exports, and
/// delete-artifact sweeps treat the row like any local transcript.
/// `timed_json` (opaque iCloud timing blob) rides along as a sibling file so
/// nothing on the wire is dropped.
pub fn import_transcript(
    settings: &HistorySettings,
    item: &TranscriptImport,
) -> Result<String, String> {
    // The id becomes the file stem, so it must be path-safe verbatim —
    // CloudKit record names are `[A-Za-z0-9_-]` by construction; anything
    // else is a wire bug, not a sanitization problem.
    let safe_id = item.item_id.trim();
    if safe_id.is_empty()
        || !safe_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(format!(
            "import id {:?} is not path-safe — refusing to write artifacts",
            item.item_id
        ));
    }
    let out_dir = PathBuf::from(&settings.output_dir);
    std::fs::create_dir_all(&out_dir).map_err(|e| format!("create output dir: {e}"))?;
    let write = |name: String, body: &str| -> Result<PathBuf, String> {
        let path = out_dir.join(name);
        rightkit_fs::atomic::atomic_write(&path, body.as_bytes())
            .map_err(|e| format!("write {}: {e}", path.display()))?;
        Ok(path)
    };
    let txt_path = write(format!("{safe_id}.txt"), &item.text)?;
    let md_path = item
        .markdown
        .as_deref()
        .filter(|md| !md.trim().is_empty())
        .map(|md| write(format!("{safe_id}.md"), md))
        .transpose()?;
    if let Some(timed) = item
        .timed_json
        .as_deref()
        .filter(|json| !json.trim().is_empty())
    {
        write(format!("{safe_id}.timed.json"), timed)?;
    }
    let path = db_path(settings)?;
    let shared = cached_store(&path)?;
    // WS-6: the upsert + tombstone clear must not interleave with a delete —
    // a re-materialized row is attachable again only once it fully exists.
    let _lifecycle = lock_lifecycle();
    let store = lock_store(&shared);
    store
        .upsert(&HistoryRecord {
            id: safe_id.to_string(),
            url: item.source.clone(),
            kind: item
                .kind
                .clone()
                .filter(|kind| !kind.trim().is_empty())
                .unwrap_or_else(|| "unknown".into()),
            title: item.title.clone(),
            lang: None,
            duration_s: item.duration_s,
            created_at: (item.created_at_ms / 1000).to_string(),
            txt_path: Some(path_to_string(&txt_path)),
            md_path: md_path.as_deref().map(path_to_string),
            srt_path: None,
            slide_dir: None,
            tags: None,
            snippet: Some(item.text.chars().take(160).collect()),
            media_path: None,
            media_meta: None,
            media_action: None,
            srt_text: None,
            truncated_at_s: None,
        })
        .map_err(|e| e.to_string())?;
    // A re-materialized row is a live library item again — an attach attempt
    // started after the import must be allowed to land on it.
    clear_tombstone(safe_id);
    Ok(safe_id.to_string())
}

fn get_record(settings: &HistorySettings, id: &str) -> Result<HistoryRecord, String> {
    let path = db_path(settings)?;
    let shared = cached_store(&path)?;
    let store = lock_store(&shared);
    let found = store.get(id).map_err(|e| e.to_string())?;
    found.ok_or_else(|| "transcript not found".to_string())
}

fn transcript_detail_from_record(rec: HistoryRecord) -> Result<TranscriptDetail, String> {
    let text = rec
        .txt_path
        .as_ref()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .unwrap_or_default();
    // Pro rows keep timed artifacts on disk; Free rows keep the SRT on the
    // record itself. Either way the reader gets the same timed surfaces.
    let srt = rec
        .srt_path
        .as_ref()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .or_else(|| rec.srt_text.clone());
    let derived_timed = srt.as_deref().map(timed_text_and_md_from_srt);
    let md_text = rec
        .md_path
        .as_ref()
        .and_then(|p| std::fs::read_to_string(p).ok());
    let timed_text = rec
        .txt_path
        .as_ref()
        .and_then(|p| sibling_path(p, "timed.txt"))
        .and_then(|p| std::fs::read_to_string(p).ok())
        .or_else(|| derived_timed.as_ref().map(|(text, _)| text.clone()));
    let timed_md_text = rec
        .md_path
        .as_ref()
        .and_then(|p| sibling_path(p, "timed.md"))
        .and_then(|p| std::fs::read_to_string(p).ok())
        .or_else(|| derived_timed.as_ref().map(|(_, md)| md.clone()));
    let slides = collect_slide_paths(rec.slide_dir.as_deref())?;
    Ok(TranscriptDetail {
        id: rec.id,
        url: rec.url,
        kind: Kind::from_tag(&rec.kind),
        title: rec.title,
        text,
        md_text,
        timed_text,
        timed_md_text,
        srt,
        slides,
        media_path: rec.media_path,
        media_meta: rec.media_meta,
        media_action: rec.media_action,
        variants: Vec::new(),
        truncated_at_s: rec.truncated_at_s,
    })
}

pub fn sibling_path(path: &str, extension: &str) -> Option<PathBuf> {
    let p = Path::new(path);
    let stem = p.file_stem()?.to_string_lossy();
    Some(p.with_file_name(format!("{stem}.{extension}")))
}

pub fn delete_record_artifacts(
    rec: &HistoryRecord,
    settings: &HistorySettings,
) -> Result<(), String> {
    let output_root = PathBuf::from(&settings.output_dir);
    let mut files = Vec::new();
    for path in [
        rec.txt_path.as_deref(),
        rec.md_path.as_deref(),
        rec.srt_path.as_deref(),
    ]
    .into_iter()
    .flatten()
    {
        files.push(PathBuf::from(path));
    }

    // Per-artifact validation: a row merged in from another output dir may
    // legitimately point at files outside THIS root. Refusing to delete that
    // artifact must not abort the whole record deletion — the row itself is
    // still removable — so out-of-scope/unavailable paths are skipped with a
    // warning while in-scope removal errors still propagate.
    if let Some(media_path) = rec.media_path.as_deref() {
        match safe_media_artifact_path(Path::new(media_path), settings, &output_root) {
            Ok(Some(path)) if path.is_file() => {
                std::fs::remove_file(&path)
                    .map_err(|e| format!("delete media {}: {e}", path.display()))?;
            }
            Ok(_) => {}
            Err(e) => {
                tracing::warn!(
                    target: "rightkit_library::history",
                    error = %e,
                    "skipping media artifact outside scope"
                );
            }
        }
    }
    if let Some(txt) = rec.txt_path.as_deref() {
        for ext in ["timed.txt", "json", "vtt"] {
            if let Some(path) = sibling_path(txt, ext) {
                files.push(path);
            }
        }
    }
    if let Some(md) = rec.md_path.as_deref() {
        for ext in ["timed.md", "fit.md"] {
            if let Some(path) = sibling_path(md, ext) {
                files.push(path);
            }
        }
    }

    for file in files {
        match safe_artifact_path(&file, &output_root) {
            Ok(Some(path)) if path.is_file() => {
                std::fs::remove_file(&path)
                    .map_err(|e| format!("delete artifact {}: {e}", path.display()))?;
            }
            Ok(_) => {}
            Err(e) => {
                tracing::warn!(
                    target: "rightkit_library::history",
                    error = %e,
                    "skipping artifact outside output dir"
                );
            }
        }
    }

    if let Some(slide_dir) = rec.slide_dir.as_deref() {
        match safe_artifact_path(Path::new(slide_dir), &output_root) {
            Ok(Some(path)) if path.is_dir() => {
                std::fs::remove_dir_all(&path)
                    .map_err(|e| format!("delete slide directory {}: {e}", path.display()))?;
            }
            Ok(_) => {}
            Err(e) => {
                tracing::warn!(
                    target: "rightkit_library::history",
                    error = %e,
                    "skipping slide dir outside output dir"
                );
            }
        }
    }

    // `_replay/<id>` is engine-owned scratch for this record — but the id
    // comes from the row, so it must be exactly one normal path component
    // AND equal it verbatim: `components()` normalizes, so "job-a/" and
    // "job-a/." still yield a sole Normal("job-a") component — but joining
    // the raw string lets the trailing element resolve through a hostile
    // `_replay/job-a` symlink and delete its target. Only the validated
    // component is ever joined.
    let mut components = std::path::Path::new(&rec.id).components();
    let sole_component = match (components.next(), components.next()) {
        (Some(std::path::Component::Normal(name)), None) => name,
        _ => {
            tracing::warn!(
                target: "rightkit_library::history",
                id = %rec.id,
                "skipping replay delete for non-component record id"
            );
            return Ok(());
        }
    };
    if sole_component != std::ffi::OsStr::new(&rec.id) {
        tracing::warn!(
            target: "rightkit_library::history",
            id = %rec.id,
            "skipping replay delete for suffixed record id"
        );
        return Ok(());
    }
    let replay_dir = output_root.join("_replay").join(sole_component);
    // Strict replay-subtree containment, root excluded: canonicalize only
    // the `_replay` PARENT — that proves the subtree itself isn't a symlink
    // out of the output dir. The entry path is never canonicalized:
    // deleting a canonical path would delete a hostile symlink's TARGET
    // (a `_replay/<id>` link to the output root would delete the library).
    let in_scope = std::fs::canonicalize(output_root.join("_replay"))
        .ok()
        .zip(output_root.canonicalize().ok())
        .is_some_and(|(base, root)| base.starts_with(&root) && base != root);
    if !in_scope {
        if replay_dir.symlink_metadata().is_ok() {
            tracing::warn!(
                target: "rightkit_library::history",
                replay_dir = %replay_dir.display(),
                "skipping replay dir outside output dir"
            );
        }
        return Ok(());
    }
    // `symlink_metadata` (never `metadata`) decides the entry kind: for a
    // symlink/junction the link itself is removed — its target stays
    // untouched. `remove_dir` handles directory junctions on Windows;
    // `remove_file` covers file links and plain files.
    match std::fs::symlink_metadata(&replay_dir) {
        Ok(meta) if meta.is_dir() => {
            let _ = std::fs::remove_dir_all(&replay_dir);
        }
        Ok(meta) => {
            let result = if meta.file_type().is_symlink() {
                std::fs::remove_dir(&replay_dir).or_else(|_| std::fs::remove_file(&replay_dir))
            } else {
                std::fs::remove_file(&replay_dir)
            };
            let _ = result;
        }
        Err(_) => {}
    }

    Ok(())
}

pub fn safe_artifact_path(path: &Path, output_root: &Path) -> Result<Option<PathBuf>, String> {
    if !path.exists() {
        return Ok(None);
    }
    let root = output_root
        .canonicalize()
        .map_err(|e| format!("output directory not available: {e}"))?;
    let artifact = path
        .canonicalize()
        .map_err(|e| format!("artifact path not available: {e}"))?;
    if !artifact.starts_with(&root) {
        return Err(format!(
            "refusing to delete artifact outside output directory: {}",
            artifact.display()
        ));
    }
    Ok(Some(artifact))
}

fn safe_media_artifact_path(
    path: &Path,
    settings: &HistorySettings,
    output_root: &Path,
) -> Result<Option<PathBuf>, String> {
    if !path.exists() {
        return Ok(None);
    }
    let artifact = path
        .canonicalize()
        .map_err(|e| format!("media path not available: {e}"))?;
    let output = output_root
        .canonicalize()
        .map_err(|e| format!("output directory not available: {e}"))?;
    if artifact.starts_with(&output) {
        return Ok(Some(artifact));
    }

    let downloads = settings.media_download_dir(output_root);
    let downloads = downloads
        .canonicalize()
        .map_err(|e| format!("download directory not available: {e}"))?;
    if !artifact.starts_with(&downloads) {
        return Err(format!(
            "refusing to delete media outside download directory: {}",
            artifact.display()
        ));
    }
    Ok(Some(artifact))
}

pub fn collect_slide_paths(slide_dir: Option<&str>) -> Result<Vec<String>, String> {
    let Some(slide_dir) = slide_dir.map(str::trim).filter(|p| !p.is_empty()) else {
        return Ok(Vec::new());
    };
    let dir = Path::new(slide_dir);
    if !dir.is_dir() {
        return Ok(Vec::new());
    }

    let mut slides = Vec::new();
    for entry in std::fs::read_dir(dir).map_err(|e| format!("read slide dir: {e}"))? {
        let path = entry.map_err(|e| format!("read slide entry: {e}"))?.path();
        if !path.is_file() || !is_slide_image_path(&path) {
            continue;
        }
        let path = rightkit_fs::plain::canonicalize_plain(&path).unwrap_or(path);
        slides.push(path.to_string_lossy().to_string());
    }
    slides.sort_by_key(|p| p.to_ascii_lowercase());
    Ok(slides)
}

fn is_slide_image_path(path: &Path) -> bool {
    // Matches the formats the bundled `image` crate decodes — webp is not
    // included (it would pull a native C build).
    matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .as_deref(),
        Some("png" | "jpg" | "jpeg" | "gif" | "bmp" | "tif" | "tiff")
    )
}
