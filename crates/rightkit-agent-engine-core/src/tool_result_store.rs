//! Durable storage for large tool results.
//!
//! Instead of blunt-truncating large tool outputs (losing the tail forever),
//! persist the full output to a content-addressed file on disk and inject a
//! stable preview + reference pointer into context. The model can request the
//! full content later via a reference id.

use std::collections::{HashMap, VecDeque};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub const MAX_STORED_TOOL_RESULT_BYTES: usize = 8 * 1024 * 1024;
const MAX_TOOL_RESULT_STORE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_TOOL_RESULT_STORE_ENTRIES: usize = 128;

/// Hot-cache bounds applied to the in-memory copy when results are also
/// durable on disk. The disk corpus keeps `max_total_bytes`/`max_entries`;
/// memory only needs the few most recent results — every miss reads back
/// through `read_from_disk`, so holding the whole 64 MiB quota resident is
/// pure duplication.
const MEMORY_CACHE_ENTRIES: usize = 4;
const MEMORY_CACHE_BYTES: u64 = 8 * 1024 * 1024;

/// Outcome of projecting a tool result into bounded model context.
///
/// `Failed` is deliberately separate from `Inline`: an inline result is
/// complete and therefore recoverable from the transcript, while a failed
/// externalization is only a bounded diagnostic preview. Callers must not
/// treat that preview as a successful tool observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolResultStoreStatus {
    /// The complete result is present in the bounded inline observation.
    Inline,
    /// The complete result is available through its content-addressed ref.
    Externalized,
    /// The complete result could not be retained durably.
    Failed(ToolResultStoreFailure),
}

/// Typed reasons why a large result could not be retained durably.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolResultStoreFailure {
    /// The configured safety quota refused the complete result.
    SafetyLimit,
    /// The durable write failed before a retrievable reference was issued.
    WriteFailed(std::io::ErrorKind),
}

/// A stored tool result: full content kept out-of-context, preview shown in-context.
#[derive(Debug, Clone)]
pub struct StoredToolResult {
    /// Stable reference id (content hash).
    pub ref_id: String,
    /// Workspace-relative path where the full output can be read, when stored.
    pub virtual_path: Option<String>,
    /// The truncated preview shown in context.
    pub preview: String,
    /// Total byte length of the full content.
    pub full_len: usize,
    /// Whether this observation is complete, externally retrievable, or a
    /// bounded failure preview. A failure status is typed so callers cannot
    /// mistake a marker string for successful storage.
    pub status: ToolResultStoreStatus,
}

/// In-memory store mapping ref_id -> full content.
/// Optionally persists to disk via `with_dir()`; when disk-backed the map
/// becomes a small hot cache (see `MEMORY_CACHE_*`) and the disk corpus is
/// authoritative.
#[derive(Debug)]
pub struct ToolResultStore {
    full: HashMap<String, Arc<str>>,
    insertion_order: VecDeque<String>,
    disk_dir: Option<PathBuf>,
    max_item_bytes: usize,
    max_total_bytes: u64,
    max_entries: usize,
}

impl Default for ToolResultStore {
    fn default() -> Self {
        Self::new()
    }
}

impl ToolResultStore {
    pub fn new() -> Self {
        Self {
            full: HashMap::new(),
            insertion_order: VecDeque::new(),
            disk_dir: None,
            max_item_bytes: MAX_STORED_TOOL_RESULT_BYTES,
            max_total_bytes: MAX_TOOL_RESULT_STORE_BYTES,
            max_entries: MAX_TOOL_RESULT_STORE_ENTRIES,
        }
    }

    /// Enable disk persistence. The store will write large results to
    /// `<dir>/<ref_id>.txt` and fall back to reading from disk if a
    /// result is not in memory.
    pub fn with_dir(mut self, dir: PathBuf) -> Self {
        self.disk_dir = Some(dir);
        self
    }

    /// Store a tool result. If it's under `preview_limit` bytes, returns it as-is
    /// with no reference. Otherwise persists the full content and returns a preview
    /// with a reference pointer appended.
    pub fn store(&mut self, content: &str, preview_limit: usize) -> StoredToolResult {
        if content.len() <= preview_limit {
            return StoredToolResult {
                ref_id: String::new(),
                virtual_path: None,
                preview: content.to_string(),
                full_len: content.len(),
                status: ToolResultStoreStatus::Inline,
            };
        }

        if self.max_entries == 0
            || content.len() > self.max_item_bytes
            || content.len() as u64 > self.max_total_bytes
        {
            return StoredToolResult {
                ref_id: String::new(),
                virtual_path: None,
                preview: oversized_output_preview(content, preview_limit, self.max_item_bytes),
                full_len: content.len(),
                status: ToolResultStoreStatus::Failed(ToolResultStoreFailure::SafetyLimit),
            };
        }

        let ref_id = content_hash(content);
        if let Some(dir) = &self.disk_dir {
            // CR-29: a failed externalization must never lose content. If
            // the write itself fails, there is no durable copy and no honest
            // reference to hand out; fall back to keeping the content inline
            // (bounded head+tail preview), never to a bare "archived" marker
            // over dropped bytes.
            if let Err(error) = write_to_disk(dir, &ref_id, content) {
                return StoredToolResult {
                    ref_id: String::new(),
                    virtual_path: None,
                    preview: unavailable_output_preview(content, preview_limit),
                    full_len: content.len(),
                    status: ToolResultStoreStatus::Failed(ToolResultStoreFailure::WriteFailed(
                        error.kind(),
                    )),
                };
            }
            // Prune failures are deliberately not surfaced as errors: the
            // content is already durable on disk, so a housekeeping failure
            // must not disown it (CR-29). In-memory bounds are still
            // enforced below.
            let _ = prune_disk_store(dir, &ref_id, self.max_total_bytes, self.max_entries);
        }
        if !self.full.contains_key(&ref_id) {
            self.insertion_order.push_back(ref_id.clone());
        }
        self.full.insert(ref_id.clone(), Arc::from(content));
        // Disk-backed stores bound memory by the small hot cache (the disk
        // corpus already carries the configured quota); memory-only stores
        // keep the configured quota since it is their only bound.
        let (cache_max_entries, cache_max_bytes) = if self.disk_dir.is_some() {
            (MEMORY_CACHE_ENTRIES, MEMORY_CACHE_BYTES)
        } else {
            (self.max_entries, self.max_total_bytes)
        };
        while self.full.len() > cache_max_entries
            || self
                .full
                .values()
                .map(|value| value.len() as u64)
                .sum::<u64>()
                > cache_max_bytes
        {
            let Some(expired) = self.insertion_order.pop_front() else {
                break;
            };
            self.full.remove(&expired);
        }

        let virtual_path = virtual_path(&ref_id);
        let preview = large_output_preview(content, preview_limit, &ref_id, &virtual_path);
        StoredToolResult {
            ref_id,
            virtual_path: Some(virtual_path),
            preview,
            full_len: content.len(),
            status: ToolResultStoreStatus::Externalized,
        }
    }

    /// Retrieve full content by reference id.
    /// First checks the hot cache (shared `Arc`, no content copy), then falls
    /// back to disk if available.
    pub fn get(&self, ref_id: &str) -> Option<Arc<str>> {
        if let Some(content) = self.full.get(ref_id) {
            return Some(content.clone());
        }

        if let Some(dir) = &self.disk_dir {
            if let Ok(content) = read_from_disk(dir, ref_id) {
                return Some(Arc::from(content));
            }
        }

        None
    }

    pub fn len(&self) -> usize {
        self.full.len()
    }

    pub fn is_empty(&self) -> bool {
        self.full.is_empty()
    }
}

/// Byte bound for engine-authored text injected into the transcript from
/// outside the normal tool-result path (stop-hook output, verification and
/// sentinel repair text). Matches the tool-output preview limit.
pub const GATE_TEXT_PREVIEW_LIMIT: usize = 4000;

/// Bound gate/hook text before it enters the transcript (audit M9). Text at or
/// under the limit passes through; larger text spills to the store and only the
/// head+tail preview with its reference is returned.
pub fn bound_gate_text(store: &mut ToolResultStore, text: &str) -> String {
    store.store(text, GATE_TEXT_PREVIEW_LIMIT).preview
}

/// Read a stored tool result from disk by reference id.
/// Returns None if the file doesn't exist or can't be read.
pub fn read_tool_result(dir: &Path, ref_id: &str) -> Option<String> {
    read_from_disk(dir, ref_id).ok()
}

pub(crate) fn content_hash(s: &str) -> String {
    rightkit_fs::digest::sha256_bytes(s.as_bytes())
}

fn virtual_path(ref_id: &str) -> String {
    format!(".rightkit/tool-results/{ref_id}.txt")
}

/// Returns whether `summary` is a bounded preview whose full source is not
/// recoverable. Compaction must preserve this marker so the model sees the
/// typed failure diagnostic instead of a generic successful-looking mask.
pub(crate) fn is_unrecoverable_preview(summary: &str) -> bool {
    summary.starts_with("[Large tool output not archived:")
        || summary.starts_with("[Large tool output could not be archived;")
}

fn large_output_preview(
    content: &str,
    preview_limit: usize,
    ref_id: &str,
    virtual_path: &str,
) -> String {
    let half = (preview_limit / 2).max(1);
    let head = head_on_line_boundary(content, half);
    let tail = tail_on_line_boundary(content, half);

    format!(
        "[Large tool output externalized]\n\
         Full output: {virtual_path}\n\
         Reference: ref:{ref_id}\n\
         Bytes: {}\n\n\
         --- head ---\n\
         {head}\n\n\
         --- tail ---\n\
         {tail}",
        content.len()
    )
}

fn oversized_output_preview(content: &str, preview_limit: usize, max_item_bytes: usize) -> String {
    format!(
        "[Large tool output not archived: {} bytes exceeds the {}-byte safety limit]\n{}",
        content.len(),
        max_item_bytes,
        bounded_head_tail(content, preview_limit)
    )
}

fn unavailable_output_preview(content: &str, preview_limit: usize) -> String {
    format!(
        "[Large tool output could not be archived; no retrieval reference was issued]\n{}",
        bounded_head_tail(content, preview_limit)
    )
}

fn bounded_head_tail(content: &str, preview_limit: usize) -> String {
    let half = (preview_limit / 2).max(1);
    format!(
        "--- head ---\n{}\n\n--- tail ---\n{}",
        head_on_line_boundary(content, half),
        tail_on_line_boundary(content, half)
    )
}

fn head_on_line_boundary(content: &str, max_chars: usize) -> String {
    if content.chars().count() <= max_chars {
        return content.to_string();
    }

    let candidate: String = content.chars().take(max_chars).collect();
    if let Some(idx) = candidate.rfind('\n') {
        let snapped = candidate[..idx].trim_end_matches('\r');
        if !snapped.is_empty() {
            return snapped.to_string();
        }
    }

    candidate
}

fn tail_on_line_boundary(content: &str, max_chars: usize) -> String {
    let total_chars = content.chars().count();
    if total_chars <= max_chars {
        return content.to_string();
    }

    let skip = total_chars.saturating_sub(max_chars);
    let candidate: String = content.chars().skip(skip).collect();
    if let Some(idx) = candidate.find('\n') {
        let snapped = candidate[idx + 1..].trim_start_matches('\r');
        if !snapped.is_empty() {
            return snapped.to_string();
        }
    }

    candidate
}

fn write_to_disk(dir: &Path, ref_id: &str, content: &str) -> std::io::Result<()> {
    // A195: externalized tool output never reaches disk with a raw secret.
    let redacted = rightkit_agent_engine_protocol::secret_scan::redact_irreversible(content);
    let content: &str = &redacted;
    if dir
        .parent()
        .and_then(|parent| fs::symlink_metadata(parent).ok())
        .is_some_and(|metadata| metadata.file_type().is_symlink())
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "tool-result parent directory must not be a symlink",
        ));
    }
    if std::fs::symlink_metadata(dir).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "tool-result directory must not be a symlink",
        ));
    }
    fs::create_dir_all(dir)?;
    if let Some(host_dir) = dir
        .parent()
        .filter(|parent| parent.file_name().and_then(|name| name.to_str()) == Some(".rightkit"))
    {
        let gitignore = host_dir.join(".gitignore");
        if !gitignore.exists() {
            fs::write(gitignore, "*\n")?;
        }
    }
    let path = dir.join(format!("{}.txt", ref_id));
    // Publish atomically (no torn content-addressed file) and only if absent.
    match rightkit_fs::atomic_write_with_options(
        &path,
        rightkit_fs::AtomicWriteOptions {
            replace: rightkit_fs::ReplaceMode::NoReplace,
            ..rightkit_fs::AtomicOptions::default().into()
        },
        |file| std::io::Write::write_all(file, content.as_bytes()),
    ) {
        Ok(()) => Ok(()),
        Err(rightkit_fs::FsError::AlreadyExists(_)) => {
            let metadata = fs::symlink_metadata(&path)?;
            if !metadata.is_file() || metadata.file_type().is_symlink() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "tool-result file must be a regular file",
                ));
            }
            if metadata.len() != content.len() as u64 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::AlreadyExists,
                    "tool-result content-address collision",
                ));
            }
            if fs::read(&path)? != content.as_bytes() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::AlreadyExists,
                    "tool-result content-address collision",
                ));
            }
            Ok(())
        }
        Err(error) => Err(error.into()),
    }
}

fn prune_disk_store(
    dir: &Path,
    keep_ref: &str,
    max_total_bytes: u64,
    max_entries: usize,
) -> std::io::Result<()> {
    let mut entries = fs::read_dir(dir)?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            let metadata = entry.metadata().ok()?;
            if !metadata.is_file() || path.extension().and_then(|ext| ext.to_str()) != Some("txt") {
                return None;
            }
            let modified = metadata.modified().ok();
            Some((path, metadata.len(), modified))
        })
        .collect::<Vec<_>>();
    entries.sort_by_key(|(_, _, modified)| *modified);
    let mut total = entries.iter().map(|(_, len, _)| *len).sum::<u64>();
    let mut count = entries.len();
    for (path, len, _) in entries {
        if total <= max_total_bytes && count <= max_entries {
            break;
        }
        if path.file_stem().and_then(|stem| stem.to_str()) == Some(keep_ref) {
            continue;
        }
        if fs::remove_file(&path).is_ok() {
            total = total.saturating_sub(len);
            count = count.saturating_sub(1);
        }
    }
    let remaining = fs::read_dir(dir)?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let ty = entry.file_type().ok()?;
            let metadata = entry.metadata().ok()?;
            (ty.is_file() && entry.path().extension().and_then(|ext| ext.to_str()) == Some("txt"))
                .then_some(metadata.len())
        })
        .collect::<Vec<_>>();
    if remaining.len() > max_entries || remaining.iter().sum::<u64>() > max_total_bytes {
        return Err(std::io::Error::other(
            "tool-result store could not be pruned to its configured bounds",
        ));
    }
    Ok(())
}

fn read_from_disk(dir: &Path, ref_id: &str) -> std::io::Result<String> {
    if !valid_ref_id(ref_id) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "invalid tool-result reference",
        ));
    }
    let path = dir.join(format!("{}.txt", ref_id));
    let metadata = fs::symlink_metadata(&path)?;
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() > MAX_STORED_TOOL_RESULT_BYTES as u64
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "stored tool result is not a bounded regular file",
        ));
    }
    let file = fs::File::open(path)?;
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_STORED_TOOL_RESULT_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_STORED_TOOL_RESULT_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "stored tool result exceeds the read bound",
        ));
    }
    String::from_utf8(bytes)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
}

fn valid_ref_id(ref_id: &str) -> bool {
    matches!(ref_id.len(), 16 | 64) && ref_id.bytes().all(|byte| byte.is_ascii_hexdigit())
}
