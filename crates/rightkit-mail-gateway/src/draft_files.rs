//! Attachments for the `draft` method. An agent names files it exported with
//! `attachment`, by bare name only (`attachment-<n><ext>`); nothing else is
//! accepted: no path, no drive, no stream, no link. Each name is resolved
//! inside the agent's own export directory (canonical form under the canonical
//! root), opened once without following a link and checked on the opened
//! handle (`draft_open`: a regular file, one hard link, the expected path),
//! then read from that handle with a size cap. The file goes out under
//! the same neutral name with a MIME type from a fixed table keyed by the
//! allow-listed extension; nothing a sender wrote is used.
//!
//! Validation is pure ([`names`]); the reads happen in [`fill`], in the gate's
//! effect step with no database lock held. A file the user chooses in the app
//! reaches the same folder through `import_file` and is attached by the same
//! bare name; a path is never accepted, whoever approved it.

use super::draft::{DraftCreator, DraftRequest};
use super::draft_open::{open_plain, OpenFail};
use super::draft_view::AttachmentInfo;
use super::export::{self, FILE_CAP};
use super::outcome::AgentError;
use rightkit_fs::canonicalize_plain;
use rightkit_fs::digest::Sha256Hasher;
use rightkit_fs::state::validate_name;
use serde_json::Value;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

/// Most attachments one draft carries.
pub const MAX_FILES: usize = 10;
/// Largest total attachment size one draft carries, before encoding.
pub const TOTAL_MAX: u64 = 18 * 1024 * 1024;

fn not_bare() -> String {
    format!("`attachments` entries must be bare file names from this agent's export folder, such as attachment-1.pdf. Paths are not accepted. A file the user gives this agent in {} (Settings, Agents) is listed by its attachment-<n> name too.", crate::host::product())
}
const BLOCKED: &str =
    "that attachment type is not allowed: types that can run code cannot be attached";

/// One file read for a draft. Content never reaches a log through `{:?}`.
#[derive(Clone, PartialEq, Eq)]
pub struct DraftAttachment {
    pub name: String,
    pub mime: &'static str,
    pub data: Vec<u8>,
}

impl std::fmt::Debug for DraftAttachment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DraftAttachment")
            .field("size", &self.data.len())
            .finish_non_exhaustive()
    }
}

/// The MIME type for an allow-listed extension (lower case, no dot). The only
/// source of a type: never the sender's.
pub fn mime_for_ext(ext: &str) -> Option<&'static str> {
    Some(match ext {
        "pdf" => "application/pdf",
        "txt" => "text/plain",
        "csv" => "text/csv",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "heic" => "image/heic",
        "tif" | "tiff" => "image/tiff",
        "bmp" => "image/bmp",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "odt" => "application/vnd.oasis.opendocument.text",
        "ods" => "application/vnd.oasis.opendocument.spreadsheet",
        "odp" => "application/vnd.oasis.opendocument.presentation",
        "rtf" => "application/rtf",
        "md" => "text/markdown",
        "json" => "application/json",
        "xml" => "application/xml",
        "eml" => "message/rfc822",
        "ics" => "text/calendar",
        "vcf" => "text/vcard",
        "zip" => "application/zip",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "m4a" => "audio/mp4",
        "mp4" => "video/mp4",
        "mov" => "video/quicktime",
        _ => return None,
    })
}

/// The extension (lower case, possibly empty) of an exact `attachment-<n>` or
/// `attachment-<n>.<ext>` name; `None` for any other spelling.
fn bare_ext(s: &str) -> Option<String> {
    let rest = s.strip_prefix("attachment-")?;
    let (num, ext) = match rest.split_once('.') {
        Some((n, e)) => (n, Some(e)),
        None => (rest, None),
    };
    if num.is_empty() || num.len() > 9 || !num.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    match ext {
        None => Some(String::new()),
        Some(e)
            if !e.is_empty() && e.len() <= 10 && e.bytes().all(|b| b.is_ascii_alphanumeric()) =>
        {
            Some(e.to_ascii_lowercase())
        }
        Some(_) => None,
    }
}

fn mime_of(name: &str) -> &'static str {
    bare_ext(name)
        .and_then(|e| mime_for_ext(&e))
        .unwrap_or("application/octet-stream")
}

fn bad(msg: &str) -> AgentError {
    AgentError::BadRequest(msg.into())
}

/// Validate the `attachments` param. Pure: no I/O. Absent or null is none.
/// A name is never echoed back.
pub fn names(params: &Value) -> Result<Vec<String>, AgentError> {
    let arr = match params.get("attachments") {
        None | Some(Value::Null) => return Ok(Vec::new()),
        Some(Value::Array(a)) => a,
        Some(_) => return Err(bad("`attachments` must be an array of file names")),
    };
    if arr.len() > MAX_FILES {
        return Err(bad("too many attachments (10 at most)"));
    }
    let mut out: Vec<String> = Vec::with_capacity(arr.len());
    for v in arr {
        let name = v.as_str().ok_or_else(|| bad(&not_bare()))?;
        let ext = bare_ext(name).ok_or_else(|| bad(&not_bare()))?;
        if !ext.is_empty() && mime_for_ext(&ext).is_none() {
            return Err(bad(BLOCKED));
        }
        if out.iter().any(|n| n == name) {
            return Err(bad("each attachment can be listed once"));
        }
        out.push(name.to_string());
    }
    Ok(out)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoadError {
    Missing,
    TooLarge,
    Unsafe,
    Io,
}

impl LoadError {
    fn reason(self) -> &'static str {
        match self {
            LoadError::Missing => "attachment_missing",
            LoadError::TooLarge => "attachment_too_large",
            LoadError::Unsafe => "attachment_unsafe",
            LoadError::Io => "attachment_io",
        }
    }
    fn agent_error(self) -> AgentError {
        match self {
            LoadError::Missing => bad("an attachment was not found in this agent's export folder"),
            LoadError::TooLarge => bad("attachments are too large (25 MiB each, 18 MiB in total)"),
            LoadError::Unsafe => {
                bad("an attachment is not a plain file in this agent's export folder")
            }
            LoadError::Io => AgentError::DraftFailed,
        }
    }
}

#[cfg(windows)]
fn is_reparse(m: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    m.file_attributes() & 0x400 != 0
}

#[cfg(not(windows))]
fn is_reparse(_: &fs::Metadata) -> bool {
    false
}

fn io_kind(e: &std::io::Error) -> LoadError {
    if e.kind() == std::io::ErrorKind::NotFound {
        LoadError::Missing
    } else {
        LoadError::Io
    }
}

/// The agent's canonical directory, checked to be a direct child of the
/// canonical root and not a link. Nothing is created. The file open re-checks
/// the directory on its own handle (see `draft_open`).
fn agent_dir(root: &Path, agent_id: &str) -> Result<PathBuf, LoadError> {
    if !export::agent_dir_ok(agent_id) {
        return Err(LoadError::Unsafe);
    }
    let canon_root = canonicalize_plain(root).map_err(|e| io_kind(&e))?;
    let dir = root.join(agent_id);
    let meta = dir.symlink_metadata().map_err(|e| io_kind(&e))?;
    if meta.file_type().is_symlink() || is_reparse(&meta) || !meta.is_dir() {
        return Err(LoadError::Unsafe);
    }
    let canon_dir = canonicalize_plain(&dir).map_err(|e| io_kind(&e))?;
    if canon_dir.parent() != Some(canon_root.as_path()) {
        return Err(LoadError::Unsafe);
    }
    Ok(canon_dir)
}

/// Read one file of the agent directory, at most `room` bytes. The file is
/// opened once, without following a link; every check and the read use that
/// handle, so nothing swapped in after the open is read.
fn read_one(canon_dir: &Path, name: &str, room: u64) -> Result<Vec<u8>, LoadError> {
    validate_name(name).map_err(|_| LoadError::Unsafe)?;
    let file = open_plain(canon_dir, name).map_err(|e| match e {
        OpenFail::Missing => LoadError::Missing,
        OpenFail::Unsafe => LoadError::Unsafe,
        OpenFail::Io => LoadError::Io,
    })?;
    let len = file.metadata().map_err(|_| LoadError::Io)?.len();
    let cap = room.min(FILE_CAP);
    if len > cap {
        return Err(LoadError::TooLarge);
    }
    let mut buf = Vec::with_capacity(len as usize);
    file.take(cap + 1)
        .read_to_end(&mut buf)
        .map_err(|_| LoadError::Io)?;
    if buf.len() as u64 > cap {
        return Err(LoadError::TooLarge);
    }
    Ok(buf)
}

pub fn load(
    root: &Path,
    agent_id: &str,
    list: &[String],
) -> Result<Vec<DraftAttachment>, LoadError> {
    let canon_dir = agent_dir(root, agent_id)?;
    let mut used: u64 = 0;
    let mut out = Vec::with_capacity(list.len());
    for name in list {
        let data = read_one(&canon_dir, name, TOTAL_MAX - used)?;
        used += data.len() as u64;
        out.push(DraftAttachment {
            name: name.clone(),
            mime: mime_of(name),
            data,
        });
    }
    Ok(out)
}

/// Read the named files into the request. On failure: the error for the agent
/// and a reason code for the audit row. Called with no database lock held.
pub fn fill(
    creator: &dyn DraftCreator,
    agent_id: &str,
    req: &mut DraftRequest,
) -> Result<(), (AgentError, &'static str)> {
    if req.attachment_names.is_empty() {
        return Ok(());
    }
    let Some(root) = creator.export_root() else {
        return Err((
            bad("attachments are not available in this context"),
            "attachment_unavailable",
        ));
    };
    match load(&root, agent_id, &req.attachment_names) {
        Ok(files) => {
            req.attachments = files;
            Ok(())
        }
        Err(e) => Err((e.agent_error(), e.reason())),
    }
}

/// Total bytes of the attachments read so far.
pub fn total_bytes(files: &[DraftAttachment]) -> usize {
    files.iter().map(|f| f.data.len()).sum()
}

/// Name, type and size of each file, as the draft's hash records them.
pub fn infos(files: &[DraftAttachment]) -> Vec<AttachmentInfo> {
    files
        .iter()
        .map(|f| AttachmentInfo {
            name: f.name.clone(),
            mime_type: f.mime.to_string(),
            size: f.data.len() as i64,
        })
        .collect()
}

fn feed(h: &mut Sha256Hasher, s: &str) {
    h.update(&(s.len() as u64).to_be_bytes());
    h.update(s.as_bytes());
}

/// One SHA-256 (hex) over each attachment's name, type, size and the SHA-256
/// of its bytes, in order. Stored with the draft record, for the record
/// only: nothing compares it at approve (the approval binds to the
/// draft's content hash, which covers each attachment's name, type and size,
/// not its bytes). A changed byte, name or order gives a different digest.
pub fn digest(files: &[DraftAttachment]) -> String {
    let mut h = Sha256Hasher::new();
    h.update(&(files.len() as u64).to_be_bytes());
    for f in files {
        feed(&mut h, &f.name);
        feed(&mut h, f.mime);
        h.update(&(f.data.len() as u64).to_be_bytes());
        let mut d = Sha256Hasher::new();
        d.update(&f.data);
        h.update(&d.finalize_bytes());
    }
    h.finalize()
}
