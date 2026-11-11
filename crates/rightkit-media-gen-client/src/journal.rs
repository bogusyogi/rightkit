//! `<project>/imports/journal.json`: the durable record of caller request ids,
//! their generation engine job ids and completed imports. Every write is
//! read-modify-write under a cross-process file lock and lands atomically
//! (temp file, fsync, rename), so a crash leaves the old or the new journal.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rightkit_fs::{FileLock, LockKind, LockOptions};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::engine::MediaKind;
use crate::error::{io, Error, Result};

pub const JOURNAL_SCHEMA: u32 = 1;

/// Longest wait for the journal lock; the guarded section is one read-modify-write.
const LOCK_WAIT: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestState {
    /// Journaled; the engine job id not yet known.
    Pending,
    /// Mapped to one engine job.
    Submitted,
    /// The generation engine rejected the request before creating a job.
    Failed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RequestEntry {
    pub request_id: String,
    pub kind: MediaKind,
    pub provider: String,
    pub model: String,
    pub prompt: String,
    #[serde(default)]
    pub params: Value,
    pub state: RequestState,
    #[serde(default)]
    pub job_id: Option<String>,
    #[serde(default)]
    pub error: Option<Value>,
    pub created_at: u64,
    /// Placement intent journaled at submit (engine-owned JSON). Recovery replays it
    /// once the job's assets are imported, so a restart places the asset as `generate
    /// --place` would have.
    #[serde(default)]
    pub place: Option<Value>,
}

/// One imported asset, keyed `<job_id>:<asset_id>`. The engine registers this
/// as a timeline source.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImportRecord {
    pub request_id: Option<String>,
    pub job_id: String,
    pub asset_id: String,
    /// Absolute path of the project-owned copy.
    pub media_path: PathBuf,
    /// Path relative to the project root (`media/...`, forward slashes).
    pub media_rel: String,
    pub sha256: String,
    pub blake3: String,
    pub bytes: u64,
    pub kind: Option<MediaKind>,
    pub mime: String,
    pub media: Value,
    /// The generation engine's probe left a field null: probe with ffprobe before use.
    #[serde(default)]
    pub needs_probe: bool,
    pub provider: String,
    pub model: String,
    pub imported_at: u64,
    /// The generation engine's estimate for the job (recorded at wait time; provenance).
    #[serde(default)]
    pub est_usd: Option<f64>,
    /// The reconciled charge when the generation engine reports one.
    #[serde(default)]
    pub actual_usd: Option<f64>,
    /// True once the generation engine acknowledged durable import (`job ack`).
    pub acked: bool,
}

pub fn import_key(job_id: &str, asset_id: &str) -> String {
    format!("{job_id}:{asset_id}")
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct JournalData {
    #[serde(default)]
    pub schema: u32,
    #[serde(default)]
    pub requests: BTreeMap<String, RequestEntry>,
    #[serde(default)]
    pub imports: BTreeMap<String, ImportRecord>,
}

#[derive(Debug, Clone)]
pub struct Journal {
    path: PathBuf,
    lock_path: PathBuf,
}

impl Journal {
    pub fn open(project: &Path) -> Self {
        let dir = project.join("imports");
        Self {
            path: dir.join("journal.json"),
            lock_path: dir.join("journal.lock"),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn load(&self) -> Result<JournalData> {
        match fs::read(&self.path) {
            Ok(bytes) => {
                let mut d: JournalData =
                    serde_json::from_slice(&bytes).map_err(|e| Error::JournalCorrupt {
                        path: self.path.clone(),
                        detail: e.to_string(),
                    })?;
                if d.schema > JOURNAL_SCHEMA {
                    return Err(Error::JournalCorrupt {
                        path: self.path.clone(),
                        detail: format!("schema {} is newer than {JOURNAL_SCHEMA}", d.schema),
                    });
                }
                d.schema = JOURNAL_SCHEMA;
                Ok(d)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(JournalData {
                schema: JOURNAL_SCHEMA,
                ..Default::default()
            }),
            Err(e) => Err(io(&self.path)(e)),
        }
    }

    /// Current journal contents (atomic writes make a lock-free read safe).
    pub fn read(&self) -> Result<JournalData> {
        self.load()
    }

    /// Apply `f` under the lock; the journal is rewritten only when `f` succeeds.
    pub fn update<T>(&self, f: impl FnOnce(&mut JournalData) -> Result<T>) -> Result<T> {
        let dir = self.path.parent().unwrap_or(Path::new("."));
        fs::create_dir_all(dir).map_err(io(dir))?;
        let lock = FileLock::acquire_kind(
            &self.lock_path,
            LockKind::Flock,
            &LockOptions::wait(LOCK_WAIT).labeled("genright-journal"),
        )
        .map_err(|e| io(&self.lock_path)(std::io::Error::other(e)))?;
        let mut data = self.load()?;
        let out = f(&mut data)?;
        self.write_atomic(&data)?;
        drop(lock);
        Ok(out)
    }

    fn write_atomic(&self, data: &JournalData) -> Result<()> {
        let text = serde_json::to_vec_pretty(data).map_err(|e| Error::JournalCorrupt {
            path: self.path.clone(),
            detail: e.to_string(),
        })?;
        rightkit_fs::atomic_write(&self.path, &text)
            .map_err(|e| io(&self.path)(std::io::Error::other(e)))
    }
}
