//! A file the user chose for an agent's draft. The user picks it in a file
//! dialog in the app's own window (Settings, Agents); the webview hands the
//! chosen file's bytes and name to the `agent_import_file` command, and this
//! module copies them into that agent's app-owned export folder through
//! [`export::store_with`]. From there the file is an ordinary export: a neutral
//! `attachment-<n><ext>` name, `.blocked` for types that can run code, the same
//! per-file, per-folder and file-count caps, a canonical-path check on the new
//! file and the Defender scan on Windows.
//!
//! No path crosses any boundary. The agent never supplies a path or reaches
//! this code (no gate method calls it; asserted in `no_send_tests`), and the
//! app never opens a path the webview names, so a link or hard link cannot be
//! followed here. The `draft` method reads the file back by bare name through
//! `draft_files` exactly as it reads an export: handle-based, no link followed,
//! hard links refused, final path checked on the opened file.

use super::draft_files::TOTAL_MAX;
use super::export::{self, ExportError, ScanVerdict};
use super::outcome::Profile;
use super::registry;
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use rightkit_db::rusqlite::Connection;
use std::path::Path;

/// Longest file name accepted (only its extension is ever used).
const NAME_MAX: usize = 1024;

/// What the user sees after a successful import.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Imported {
    /// The neutral name the agent attaches by, such as `attachment-3.pdf`.
    pub name: String,
    pub size: u64,
    /// The type can run code: it was stored as `.blocked` and cannot be attached.
    pub extension_blocked: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportError {
    NoAgent,
    /// A read-only agent cannot make drafts, so it has no use for a file.
    ReadOnly,
    Empty,
    TooLarge,
    Encoding,
    DirFull,
    Blocked,
    ScanIncomplete,
    Failed,
}

impl ImportError {
    pub fn message(self) -> &'static str {
        match self {
            ImportError::NoAgent => "That agent is not paired or has been revoked.",
            ImportError::ReadOnly => {
                "That agent is read only. Set its profile to Draft before giving it a file."
            }
            ImportError::Empty => "The file is empty.",
            ImportError::TooLarge => {
                "The file is larger than 18 MiB, the most one draft can carry."
            }
            ImportError::Encoding => "The file could not be read.",
            ImportError::DirFull => export::DIR_FULL_MESSAGE,
            ImportError::Blocked => "Windows Defender flagged the file, so it was not kept.",
            ImportError::ScanIncomplete => {
                "The file could not be scanned in time, so it was not kept."
            }
            ImportError::Failed => "The file could not be saved.",
        }
    }
}

impl From<ExportError> for ImportError {
    fn from(e: ExportError) -> Self {
        match e {
            ExportError::TooLarge => ImportError::TooLarge,
            ExportError::DirFull => ImportError::DirFull,
            ExportError::Blocked => ImportError::Blocked,
            ExportError::ScanIncomplete => ImportError::ScanIncomplete,
            ExportError::Unsafe | ExportError::Io => ImportError::Failed,
        }
    }
}

/// The agent must be paired, not revoked, and able to draft.
pub fn check_agent(conn: &Connection, agent_id: &str) -> Result<(), ImportError> {
    let agent = registry::get_agent(conn, agent_id)
        .map_err(|_| ImportError::Failed)?
        .ok_or(ImportError::NoAgent)?;
    if agent.revoked_at.is_some() {
        return Err(ImportError::NoAgent);
    }
    if agent.profile == Profile::ReadOnly {
        return Err(ImportError::ReadOnly);
    }
    Ok(())
}

/// Decode the transported bytes. The length is checked before decoding, so an
/// oversized value is never expanded.
pub fn decode(b64: &str) -> Result<Vec<u8>, ImportError> {
    if b64.len() as u64 > TOTAL_MAX / 3 * 4 + 8 {
        return Err(ImportError::TooLarge);
    }
    STANDARD.decode(b64).map_err(|_| ImportError::Encoding)
}

/// Copy `bytes` into `agent_id`'s export folder under `root`. `raw_name` only
/// supplies the extension. Any failure leaves no file behind.
pub fn import(
    root: &Path,
    agent_id: &str,
    raw_name: &str,
    bytes: &[u8],
    scan: &dyn Fn(&Path) -> ScanVerdict,
) -> Result<Imported, ImportError> {
    if raw_name.len() > NAME_MAX {
        return Err(ImportError::Failed);
    }
    if bytes.is_empty() {
        return Err(ImportError::Empty);
    }
    // A file over the draft total could never be attached: refuse it now.
    if bytes.len() as u64 > TOTAL_MAX {
        return Err(ImportError::TooLarge);
    }
    let stored = export::store_with(root, agent_id, raw_name, bytes, scan)?;
    let name = stored
        .path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .ok_or(ImportError::Failed)?;
    Ok(Imported {
        name,
        size: stored.size,
        extension_blocked: stored.extension_blocked,
    })
}

/// The production scan: Windows Defender, at most 30 seconds.
pub fn defender_scan(path: &Path) -> ScanVerdict {
    export::defender_within(path, std::time::Duration::from_secs(30))
}
