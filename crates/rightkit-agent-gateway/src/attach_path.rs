//! The file side of `attach_file`: an agent names one absolute path, and the
//! user decides on a card that shows that exact path and the file's size in
//! bytes. The agent never reaches the file: nothing here runs for a request.
//!
//! * Request time: the path is checked as text only (absolute, no `..`, no
//!   network or device form). No file is stat'ed, opened or read.
//! * Card time: [`AttachFiles::stat`] opens the file without following a final
//!   link or reparse point ([`super::attach_open`]), reads its metadata from
//!   that handle (size, modified time, file identity), never its bytes, and
//!   refuses a link or junction anywhere in the path. The card's hash binds
//!   the account, the agent, the path, the size, the modified time and the file
//!   identity ([`binding`]).
//! * Approve time, after the claim: the metadata is read again and the hash
//!   compared with the one the card showed; any difference refuses the attach.
//!   Only then is the file opened the same way, its identity, final path, size
//!   and time checked again on the opened handle against the binding, read in
//!   full from that handle, and its length compared with the size shown.
//!   The bytes go to [`AttachFiles::store`], which copies them into the agent's
//!   export folder like a file the user chose in Settings, under a neutral
//!   `attachment-<n>` name. The agent attaches that bare name with `draft`; a
//!   path is still never accepted by `draft`.

use super::draft_files::TOTAL_MAX;
use super::import_file::{self, ImportError, Imported};
use super::outcome::AgentError;
use rightkit_fs::digest::Sha256Hasher;
use serde::Serialize;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

/// Longest path accepted, in characters.
pub const PATH_MAX: usize = 1024;

/// What the card shows about the file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AttachSummary {
    /// The exact absolute path the agent named.
    pub path: String,
    /// The file's size in bytes when the card was made.
    pub size: u64,
}

/// A file's size, modified time (milliseconds since the Unix epoch) and
/// identity. `file_id` digests the volume and file index (Windows) or device
/// and inode (unix) together with the handle's final path, so a different
/// file with the same size and time, or the same path redirected through a
/// swapped folder, differs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileMeta {
    pub size: u64,
    pub mtime_ms: i64,
    pub file_id: [u8; 32],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileError {
    Missing,
    /// Not a regular file, or a link or junction in the path.
    NotPlain,
    /// The size, time or identity differs from what the card showed.
    Changed,
    TooLarge,
    Io,
}

impl FileError {
    /// The reason code stored on the approval and shown to the agent.
    pub fn reason(self) -> &'static str {
        match self {
            FileError::Missing => "file_missing",
            FileError::NotPlain => "file_not_plain",
            FileError::Changed => "file_changed",
            FileError::TooLarge => "file_too_large",
            FileError::Io => "file_unreadable",
        }
    }
    /// Fixed text for the window when the card cannot be made.
    pub fn card_message(self) -> &'static str {
        match self {
            FileError::Missing => "The file no longer exists.",
            FileError::NotPlain => "That path is not a plain file.",
            FileError::TooLarge => "The file is too large to attach.",
            FileError::Changed | FileError::Io => "The file could not be read.",
        }
    }
}

/// The file access of the attach flow, injected. Production: [`FsFiles`].
/// Tests: a recording fake.
pub trait AttachFiles: Send + Sync {
    /// Size, modified time and identity of a plain file, from a handle opened
    /// without following links. Metadata only: the bytes are not read.
    fn stat(&self, path: &str) -> Result<FileMeta, FileError>;
    /// Read the whole file, which must still be exactly `expect`.
    fn read(&self, path: &str, expect: FileMeta) -> Result<Vec<u8>, FileError>;
    /// Copy `bytes` into `agent_id`'s export folder. `path` only supplies the
    /// extension.
    fn store(&self, agent_id: &str, path: &str, bytes: &[u8]) -> Result<Imported, ImportError>;
}

/// Check a requested path as text. Pure: no file is touched.
pub fn validate_path(raw: &str) -> Result<(), AgentError> {
    let bad = |m: &str| AgentError::BadRequest(m.into());
    if raw.is_empty() || raw.chars().count() > PATH_MAX || raw.chars().any(|c| c.is_control()) {
        return Err(bad(
            "`path` is required (an absolute file path of 1 to 1024 characters)",
        ));
    }
    if raw.starts_with("\\\\") || raw.starts_with("//") {
        return Err(bad("network and device paths are not accepted"));
    }
    let p = Path::new(raw);
    if !p.is_absolute() {
        return Err(bad("`path` must be an absolute path"));
    }
    if p.components().any(|c| matches!(c, Component::ParentDir)) || p.file_name().is_none() {
        return Err(bad("`path` must name a file, without `..`"));
    }
    Ok(())
}

fn feed(h: &mut Sha256Hasher, s: &str) {
    h.update(&(s.len() as u64).to_be_bytes());
    h.update(s.as_bytes());
}

/// The hash a card shows and Approve passes back: SHA-256 over the account,
/// the agent, the path, the size, the modified time and the file identity.
pub fn binding(account: &str, agent_id: &str, path: &str, m: FileMeta) -> String {
    let mut h = Sha256Hasher::new();
    for s in ["attach-v1", account, agent_id, path] {
        feed(&mut h, s);
    }
    h.update(&m.size.to_be_bytes());
    h.update(&m.mtime_ms.to_be_bytes());
    h.update(&m.file_id);
    h.finalize()
}

/// The id an attach request is stored under (and declined or repeated by):
/// a hash of the account and the path.
pub fn target(account: &str, path: &str) -> String {
    let mut h = Sha256Hasher::new();
    feed(&mut h, account);
    feed(&mut h, path);
    format!("attach-{}", &h.finalize()[..32])
}

/// Production file access. `root` is the app's agent export folder.
pub struct FsFiles {
    root: PathBuf,
}

impl FsFiles {
    pub fn new(export_root: PathBuf) -> Self {
        FsFiles { root: export_root }
    }
}

impl AttachFiles for FsFiles {
    fn stat(&self, path: &str) -> Result<FileMeta, FileError> {
        super::attach_open::open_plain(Path::new(path)).map(|(_, meta)| meta)
    }

    fn read(&self, path: &str, expect: FileMeta) -> Result<Vec<u8>, FileError> {
        if expect.size > TOTAL_MAX {
            return Err(FileError::TooLarge);
        }
        // Opened exactly as at card time; the identity, final path, size and
        // time on this handle must equal the binding before any byte is read.
        // A path that is now a link, junction or other file is a changed file.
        let (file, now) = match super::attach_open::open_plain(Path::new(path)) {
            Err(FileError::NotPlain) => return Err(FileError::Changed),
            other => other?,
        };
        if now != expect {
            return Err(FileError::Changed);
        }
        let mut buf = Vec::with_capacity(expect.size as usize);
        file.take(expect.size + 1)
            .read_to_end(&mut buf)
            .map_err(|_| FileError::Io)?;
        if buf.len() as u64 != expect.size {
            return Err(FileError::Changed);
        }
        Ok(buf)
    }

    fn store(&self, agent_id: &str, path: &str, bytes: &[u8]) -> Result<Imported, ImportError> {
        let name = Path::new(path)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        import_file::import(
            &self.root,
            agent_id,
            &name,
            bytes,
            &import_file::defender_scan,
        )
    }
}
