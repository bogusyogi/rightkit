//! Session-owned file-read receipts and serialized workspace mutation.

use std::collections::HashMap;
use std::fs::{File, Metadata};
use std::hash::{Hash, Hasher};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock, Weak};

use crate::coding_tools::context::ToolContext;
use crate::coding_tools::tool_trait::{Tool, ToolResult, WorkspaceMutation, WorkspaceMutationKind};
use rightkit_agent_policy::{CapabilityClass, PermissionDecision, PermissionEngine};
use serde::Serialize;
use serde_json::{json, Value};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FileRevision {
    pub identity: PathBuf,
    pub size: u64,
    pub modified_ns: u128,
    /// Cheap screening hash over metadata plus bounded head/tail samples.
    /// Useful for fast rejection; NOT sufficient for conflict detection on
    /// its own - see `content_digest`.
    pub fast_hash: u64,
    /// EFF-005: authoritative content identity.
    ///
    /// `fast_hash` samples only the first and last 4 KiB, so a mutation
    /// confined to the middle of a file that preserves length and restores
    /// mtime produces an identical `size`/`modified_ns`/`fast_hash` and would
    /// be invisible to a stale-write check. A full-content SHA-256 closes
    /// that hole: any byte change alters the digest. For directories this is
    /// the digest of the walked tree fingerprint.
    pub content_digest: [u8; 32],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadReceipt {
    pub revision: FileRevision,
    pub complete: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MutationIntentKind {
    Create,
    Replace,
    Update,
    Move,
    Delete,
    Restore,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", content = "revision", rename_all = "snake_case")]
pub enum ExpectedPreimage {
    Missing,
    Revision(FileRevision),
}

impl ExpectedPreimage {
    fn as_revision(&self) -> Option<&FileRevision> {
        match self {
            Self::Missing => None,
            Self::Revision(revision) => Some(revision),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "intent", rename_all = "snake_case")]
pub enum MutationIntent {
    Create {
        path: PathBuf,
    },
    Replace {
        path: PathBuf,
        expected: FileRevision,
    },
    Update {
        path: PathBuf,
        expected: FileRevision,
    },
    Move {
        source: PathBuf,
        destination: PathBuf,
        expected_source: FileRevision,
        expected_destination: ExpectedPreimage,
    },
    Delete {
        path: PathBuf,
        expected: FileRevision,
    },
    Restore {
        path: PathBuf,
        expected: FileRevision,
    },
}

impl MutationIntent {
    pub fn create(path: impl Into<PathBuf>) -> Self {
        Self::Create { path: path.into() }
    }

    pub fn replace(path: impl Into<PathBuf>, expected: FileRevision) -> Self {
        Self::Replace {
            path: path.into(),
            expected,
        }
    }

    pub fn update(path: impl Into<PathBuf>, expected: FileRevision) -> Self {
        Self::Update {
            path: path.into(),
            expected,
        }
    }

    pub fn move_path(
        source: impl Into<PathBuf>,
        destination: impl Into<PathBuf>,
        expected_source: FileRevision,
        expected_destination: ExpectedPreimage,
    ) -> Self {
        Self::Move {
            source: source.into(),
            destination: destination.into(),
            expected_source,
            expected_destination,
        }
    }

    pub fn delete(path: impl Into<PathBuf>, expected: FileRevision) -> Self {
        Self::Delete {
            path: path.into(),
            expected,
        }
    }

    pub fn restore(path: impl Into<PathBuf>, expected: FileRevision) -> Self {
        Self::Restore {
            path: path.into(),
            expected,
        }
    }

    pub fn kind(&self) -> MutationIntentKind {
        match self {
            Self::Create { .. } => MutationIntentKind::Create,
            Self::Replace { .. } => MutationIntentKind::Replace,
            Self::Update { .. } => MutationIntentKind::Update,
            Self::Move { .. } => MutationIntentKind::Move,
            Self::Delete { .. } => MutationIntentKind::Delete,
            Self::Restore { .. } => MutationIntentKind::Restore,
        }
    }

    pub fn path(&self) -> &Path {
        match self {
            Self::Create { path }
            | Self::Replace { path, .. }
            | Self::Update { path, .. }
            | Self::Delete { path, .. }
            | Self::Restore { path, .. } => path,
            Self::Move { source, .. } => source,
        }
    }

    pub fn destination(&self) -> Option<&Path> {
        match self {
            Self::Move { destination, .. } => Some(destination),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MutationReceipt {
    pub intent: MutationIntentKind,
    pub path: PathBuf,
    pub destination: Option<PathBuf>,
    pub before: Option<FileRevision>,
    pub destination_before: Option<FileRevision>,
    pub after: Option<FileRevision>,
    /// The mutation committed, but a durability step after it failed (the parent directory sync
    /// or removing a displaced old tree). Not a failed mutation: the destination holds the new
    /// content. Reported to the caller beside the receipt.
    pub durability_warning: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MutationConflict {
    pub intent: MutationIntentKind,
    pub path: PathBuf,
    pub expected: ExpectedPreimage,
    pub current: ExpectedPreimage,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MutationError {
    Conflict(MutationConflict),
    InvalidIntent(String),
    Io(String),
}

impl std::fmt::Display for MutationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Conflict(conflict) => write!(
                f,
                "mutation conflict ({:?}) for '{}': expected {:?}, current {:?}",
                conflict.intent,
                conflict.path.display(),
                conflict.expected,
                conflict.current
            ),
            Self::InvalidIntent(message) | Self::Io(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for MutationError {}

#[derive(Debug, Clone)]
pub enum MutationBatchEntry {
    Write {
        intent: MutationIntent,
        bytes: Vec<u8>,
    },
    CreateDirectory {
        intent: MutationIntent,
    },
    Copy {
        source: PathBuf,
        expected_source: FileRevision,
        intent: MutationIntent,
    },
    Move {
        intent: MutationIntent,
    },
    Delete {
        intent: MutationIntent,
        recursive: bool,
    },
    Trash {
        intent: MutationIntent,
        destination: PathBuf,
    },
}

impl MutationBatchEntry {
    pub fn write(intent: MutationIntent, bytes: impl Into<Vec<u8>>) -> Self {
        Self::Write {
            intent,
            bytes: bytes.into(),
        }
    }

    fn intent(&self) -> &MutationIntent {
        match self {
            Self::Write { intent, .. }
            | Self::CreateDirectory { intent }
            | Self::Copy { intent, .. }
            | Self::Move { intent }
            | Self::Delete { intent, .. }
            | Self::Trash { intent, .. } => intent,
        }
    }

    fn lock_paths(&self) -> Vec<PathBuf> {
        match self {
            Self::Write { intent, .. }
            | Self::CreateDirectory { intent }
            | Self::Delete { intent, .. } => vec![intent.path().to_path_buf()],
            Self::Copy { source, intent, .. } => vec![source.clone(), intent.path().to_path_buf()],
            Self::Move { intent } => vec![
                intent.path().to_path_buf(),
                intent
                    .destination()
                    .expect("move intent has destination")
                    .to_path_buf(),
            ],
            Self::Trash {
                intent,
                destination,
            } => vec![intent.path().to_path_buf(), destination.clone()],
        }
    }

    fn journal_value(&self, index: usize) -> Value {
        match self {
            Self::Write { intent, bytes } => json!({
                "index": index,
                "intent": intent,
                "payloadBytes": bytes.len(),
            }),
            Self::Copy {
                source,
                expected_source,
                intent,
            } => json!({
                "index": index,
                "intent": intent,
                "source": source,
                "expectedSource": expected_source,
            }),
            Self::Trash {
                intent,
                destination,
            } => json!({
                "index": index,
                "intent": intent,
                "trashDestination": destination,
            }),
            Self::CreateDirectory { intent }
            | Self::Move { intent }
            | Self::Delete { intent, .. } => json!({
                "index": index,
                "intent": intent,
            }),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BatchEntryOutcome {
    Committed,
    Failed,
    NotAttempted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BatchEntryReceipt {
    pub index: usize,
    pub intent: MutationIntentKind,
    pub outcome: BatchEntryOutcome,
    pub mutation: Option<MutationReceipt>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BatchPathState {
    pub path: PathBuf,
    pub state: Option<ExpectedPreimage>,
    pub observation_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BatchMutationReceipt {
    pub journal_id: String,
    pub complete: bool,
    pub failure: Option<String>,
    pub entries: Vec<BatchEntryReceipt>,
    pub final_state: Vec<BatchPathState>,
    pub compensation_attempted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchMutationFailure {
    pub receipt: BatchMutationReceipt,
}

impl std::fmt::Display for BatchMutationFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "batch mutation {} is partial: {}",
            self.receipt.journal_id,
            self.receipt
                .failure
                .as_deref()
                .unwrap_or("unspecified failure")
        )
    }
}

impl std::error::Error for BatchMutationFailure {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WorkspaceRestoreProvenance {
    pub checkpoint_id: String,
    pub commit_oid: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WorkspaceGenerationRevision {
    pub generation: u64,
    pub revision: Option<u64>,
    pub observation_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WorkspaceRestoreReceipt {
    pub provenance: WorkspaceRestoreProvenance,
    pub succeeded: bool,
    pub failure: Option<String>,
    pub before: WorkspaceGenerationRevision,
    pub after: WorkspaceGenerationRevision,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceRestoreFailure {
    pub receipt: WorkspaceRestoreReceipt,
}

impl std::fmt::Display for WorkspaceRestoreFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "workspace restore {} did not settle successfully: {}",
            self.receipt.provenance.checkpoint_id,
            self.receipt
                .failure
                .as_deref()
                .unwrap_or("unknown restore failure")
        )
    }
}

impl std::error::Error for WorkspaceRestoreFailure {}

#[derive(Debug, Clone)]
struct MutationSnapshot {
    before: Option<FileRevision>,
    destination_before: Option<FileRevision>,
}

fn canonical_path_locks() -> &'static Mutex<HashMap<PathBuf, Weak<Mutex<()>>>> {
    static LOCKS: OnceLock<Mutex<HashMap<PathBuf, Weak<Mutex<()>>>>> = OnceLock::new();
    LOCKS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn canonical_workspace_gate() -> &'static RwLock<()> {
    static GATE: OnceLock<RwLock<()>> = OnceLock::new();
    GATE.get_or_init(|| RwLock::new(()))
}

fn canonical_workspace_generation() -> &'static AtomicU64 {
    static GENERATION: AtomicU64 = AtomicU64::new(0);
    &GENERATION
}

#[derive(Default)]
pub struct FileService {
    receipts: Mutex<HashMap<PathBuf, ReadReceipt>>,
    mutation_receipts: Mutex<Vec<MutationReceipt>>,
    batch_sequence: AtomicU64,
    #[cfg(test)]
    before_final_recheck_hook: Mutex<Option<Arc<dyn Fn(&MutationIntent) + Send + Sync + 'static>>>,
    #[cfg(test)]
    batch_fail_after_commits: Mutex<Option<usize>>,
}

impl FileService {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn record_read(&self, path: &Path, complete: bool) -> Result<ReadReceipt, String> {
        let receipt = ReadReceipt {
            revision: revision(path)?,
            complete,
        };
        self.receipts
            .lock()
            .map_err(|_| "file receipt ledger is unavailable; retry read_file".to_string())?
            .insert(path.to_path_buf(), receipt.clone());
        Ok(receipt)
    }

    pub fn observe_preimage(&self, path: &Path) -> Result<ExpectedPreimage, MutationError> {
        match std::fs::metadata(path) {
            Ok(_) => revision(path)
                .map(ExpectedPreimage::Revision)
                .map_err(MutationError::Io),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(ExpectedPreimage::Missing)
            }
            Err(error) => Err(MutationError::Io(format!(
                "inspect '{}': {error}",
                path.display()
            ))),
        }
    }

    pub fn mutation_receipts(&self) -> Result<Vec<MutationReceipt>, String> {
        self.mutation_receipts
            .lock()
            .map(|receipts| receipts.clone())
            .map_err(|_| "mutation receipt ledger is unavailable".to_string())
    }

    #[cfg(test)]
    fn set_before_final_recheck_hook(
        &self,
        hook: Arc<dyn Fn(&MutationIntent) + Send + Sync + 'static>,
    ) {
        *self.before_final_recheck_hook.lock().unwrap() = Some(hook);
    }

    fn recheck_intent_immediately_before_commit(
        &self,
        intent: &MutationIntent,
    ) -> Result<(), MutationError> {
        #[cfg(test)]
        {
            if let Some(hook) = self.before_final_recheck_hook.lock().unwrap().take() {
                hook(intent);
            }
        }
        self.validate_intent(intent).map(|_| ())
    }

    /// Typed write entry point for ordinary tools that rely on the session read
    /// receipt. `Create` is the only intent that does not require an existing
    /// receipt and can never overwrite an existing path.
    pub fn write_with_receipt_intent(
        &self,
        kind: MutationIntentKind,
        path: &Path,
        bytes: &[u8],
        hashline_validated: bool,
    ) -> Result<MutationReceipt, MutationError> {
        let intent = match kind {
            MutationIntentKind::Create => MutationIntent::create(path),
            MutationIntentKind::Replace => MutationIntent::replace(
                path,
                self.expected_revision_from_receipt(path, hashline_validated)?,
            ),
            MutationIntentKind::Update => MutationIntent::update(
                path,
                self.expected_revision_from_receipt(path, hashline_validated)?,
            ),
            MutationIntentKind::Restore => MutationIntent::restore(
                path,
                self.expected_revision_from_receipt(path, hashline_validated)?,
            ),
            MutationIntentKind::Move | MutationIntentKind::Delete => {
                return Err(MutationError::InvalidIntent(format!(
                    "{:?} is not a byte-write intent",
                    kind
                )))
            }
        };
        self.apply_byte_intent(intent, bytes)
    }

    /// Apply an explicit typed byte mutation. This is the qualification-facing
    /// primitive used by deterministic stale-preimage fixtures as well as by
    /// the read-receipt adapter above.
    pub fn apply_byte_intent(
        &self,
        intent: MutationIntent,
        bytes: &[u8],
    ) -> Result<MutationReceipt, MutationError> {
        if matches!(
            intent,
            MutationIntent::Move { .. } | MutationIntent::Delete { .. }
        ) {
            return Err(MutationError::InvalidIntent(
                "move/delete intents require their path lifecycle entry points".to_string(),
            ));
        }
        let path = intent.path().to_path_buf();
        self.with_locked_paths_typed(&[path], || self.apply_byte_intent_locked(&intent, bytes))
    }

    fn apply_byte_intent_locked(
        &self,
        intent: &MutationIntent,
        bytes: &[u8],
    ) -> Result<MutationReceipt, MutationError> {
        let path = intent.path().to_path_buf();
        let snapshot = self.validate_intent(intent)?;
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|error| {
                    MutationError::Io(format!(
                        "create parent directory for '{}': {error}",
                        path.display()
                    ))
                })?;
            }
        }
        self.recheck_intent_immediately_before_commit(intent)?;
        // rightkit_fs::atomic_write keeps an existing regular file's Unix mode bits
        // (Privacy::Inherit), so no local chmod restore is needed (#353).
        rightkit_fs::atomic_write(&path, bytes).map_err(|error| {
            MutationError::Io(format!(
                "atomic workspace write '{}': {error}",
                path.display()
            ))
        })?;
        self.finish_intent(intent, snapshot)
    }

    /// Compatibility entry point for callers carrying `ObservedVersion`.
    /// The observation is translated into the same typed create/update intent
    /// consumed by the ordinary FileService mutation authority.
    pub fn write_with_observed_version(
        &self,
        path: &Path,
        observed: &crate::coding_tools::observed_version::ObservedVersion,
        bytes: &[u8],
    ) -> Result<
        crate::coding_tools::observed_version::ObservedVersion,
        crate::coding_tools::observed_version::VersionCheckError,
    > {
        let intent = if observed.existed {
            MutationIntent::update(path, file_revision_from_observed(path, observed))
        } else {
            MutationIntent::create(path)
        };
        self.apply_byte_intent(intent, bytes)
            .map_err(|error| match error {
                MutationError::Conflict(_) => {
                    match crate::coding_tools::observed_version::ObservedVersion::capture(path) {
                        Ok(current) => {
                            crate::coding_tools::observed_version::VersionCheckError::Stale {
                                observed: observed.clone(),
                                current,
                            }
                        }
                        Err(message) => {
                            crate::coding_tools::observed_version::VersionCheckError::Io(message)
                        }
                    }
                }
                MutationError::InvalidIntent(message) | MutationError::Io(message) => {
                    crate::coding_tools::observed_version::VersionCheckError::Io(message)
                }
            })?;
        crate::coding_tools::observed_version::ObservedVersion::capture(path)
            .map_err(crate::coding_tools::observed_version::VersionCheckError::Io)
    }

    /// Backwards-compatible adapter. New mutation routes must name their
    /// semantic intent explicitly through `write_with_receipt_intent`.
    pub fn write_with_receipt(
        &self,
        path: &Path,
        bytes: &[u8],
        hashline_validated: bool,
    ) -> Result<(), String> {
        let kind = if path.exists() {
            MutationIntentKind::Update
        } else {
            MutationIntentKind::Create
        };
        self.write_with_receipt_intent(kind, path, bytes, hashline_validated)
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    pub fn create_directory_with_intent(
        &self,
        intent: MutationIntent,
    ) -> Result<MutationReceipt, MutationError> {
        let path = intent.path().to_path_buf();
        self.with_locked_paths_typed(&[path], || self.create_directory_intent_locked(&intent))
    }

    fn create_directory_intent_locked(
        &self,
        intent: &MutationIntent,
    ) -> Result<MutationReceipt, MutationError> {
        if !matches!(intent, MutationIntent::Create { .. }) {
            return Err(MutationError::InvalidIntent(
                "directory creation requires a create intent".to_string(),
            ));
        }
        let path = intent.path().to_path_buf();
        let snapshot = self.validate_intent(intent)?;
        self.recheck_intent_immediately_before_commit(intent)?;
        std::fs::create_dir_all(&path)
            .map_err(|error| MutationError::Io(format!("mkdir '{}': {error}", path.display())))?;
        self.finish_intent(intent, snapshot)
    }

    pub fn copy_with_intent(
        &self,
        source: &Path,
        expected_source: FileRevision,
        intent: MutationIntent,
    ) -> Result<MutationReceipt, MutationError> {
        let destination = intent.path().to_path_buf();
        self.with_locked_paths_typed(&[source.to_path_buf(), destination], || {
            self.copy_intent_locked(source, &expected_source, &intent)
        })
    }

    fn copy_intent_locked(
        &self,
        source: &Path,
        expected_source: &FileRevision,
        intent: &MutationIntent,
    ) -> Result<MutationReceipt, MutationError> {
        if !matches!(
            intent,
            MutationIntent::Create { .. } | MutationIntent::Replace { .. }
        ) {
            return Err(MutationError::InvalidIntent(
                "copy destination requires create or replace intent".to_string(),
            ));
        }
        let destination = intent.path().to_path_buf();
        self.assert_preimage(
            source,
            &ExpectedPreimage::Revision(expected_source.clone()),
            intent.kind(),
        )?;
        let snapshot = self.validate_intent(intent)?;
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent).map_err(|error| {
                MutationError::Io(format!(
                    "create destination parent '{}': {error}",
                    parent.display()
                ))
            })?;
        }
        let temporary = mutation_sibling(&destination, "copy");
        if let Err(error) = copy_tree(source, &temporary) {
            let _ = remove_existing(&temporary);
            return Err(MutationError::Io(error));
        }
        self.assert_preimage(
            source,
            &ExpectedPreimage::Revision(expected_source.clone()),
            intent.kind(),
        )?;
        if let Err(error) = self.recheck_intent_immediately_before_commit(intent) {
            let _ = remove_existing(&temporary);
            return Err(error);
        }
        let warning = match publish_replacing(&temporary, &destination) {
            Ok(warning) => warning,
            Err(error) => {
                let _ = remove_existing(&temporary);
                return Err(MutationError::Io(format!(
                    "publish copy '{}': {error}",
                    destination.display()
                )));
            }
        };
        self.finish_intent_with_warning(intent, snapshot, warning)
    }

    pub fn move_with_intent(
        &self,
        intent: MutationIntent,
    ) -> Result<MutationReceipt, MutationError> {
        let (source, destination) = match &intent {
            MutationIntent::Move {
                source,
                destination,
                ..
            } => (source.clone(), destination.clone()),
            _ => {
                return Err(MutationError::InvalidIntent(
                    "move path requires a move intent".to_string(),
                ))
            }
        };
        self.with_locked_paths_typed(&[source, destination], || self.move_intent_locked(&intent))
    }

    fn move_intent_locked(
        &self,
        intent: &MutationIntent,
    ) -> Result<MutationReceipt, MutationError> {
        let (source, destination) = match intent {
            MutationIntent::Move {
                source,
                destination,
                ..
            } => (source, destination),
            _ => {
                return Err(MutationError::InvalidIntent(
                    "move path requires a move intent".to_string(),
                ))
            }
        };
        let snapshot = self.validate_intent(intent)?;
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent).map_err(|error| {
                MutationError::Io(format!(
                    "create destination parent '{}': {error}",
                    parent.display()
                ))
            })?;
        }
        self.recheck_intent_immediately_before_commit(intent)?;
        let warning = match publish_replacing(source, destination) {
            Ok(warning) => warning,
            Err(error) => {
                return Err(MutationError::Io(format!(
                    "move '{}' to '{}': {error}",
                    source.display(),
                    destination.display()
                )));
            }
        };
        self.finish_intent_with_warning(intent, snapshot, warning)
    }

    pub fn delete_with_intent(
        &self,
        intent: MutationIntent,
        recursive: bool,
    ) -> Result<MutationReceipt, MutationError> {
        let path = intent.path().to_path_buf();
        self.with_locked_paths_typed(&[path], || self.delete_intent_locked(&intent, recursive))
    }

    fn delete_intent_locked(
        &self,
        intent: &MutationIntent,
        recursive: bool,
    ) -> Result<MutationReceipt, MutationError> {
        if !matches!(intent, MutationIntent::Delete { .. }) {
            return Err(MutationError::InvalidIntent(
                "delete path requires a delete intent".to_string(),
            ));
        }
        let path = intent.path().to_path_buf();
        let snapshot = self.validate_intent(intent)?;
        self.recheck_intent_immediately_before_commit(intent)?;
        if path.is_dir() {
            if recursive {
                std::fs::remove_dir_all(&path)
            } else {
                std::fs::remove_dir(&path)
            }
        } else {
            std::fs::remove_file(&path)
        }
        .map_err(|error| MutationError::Io(format!("delete '{}': {error}", path.display())))?;
        self.finish_intent(intent, snapshot)
    }

    pub fn trash_with_intent(
        &self,
        intent: MutationIntent,
        destination: &Path,
    ) -> Result<MutationReceipt, MutationError> {
        let path = intent.path().to_path_buf();
        self.with_locked_paths_typed(&[path, destination.to_path_buf()], || {
            self.trash_intent_locked(&intent, destination)
        })
    }

    fn trash_intent_locked(
        &self,
        intent: &MutationIntent,
        destination: &Path,
    ) -> Result<MutationReceipt, MutationError> {
        if !matches!(intent, MutationIntent::Delete { .. }) {
            return Err(MutationError::InvalidIntent(
                "trash path requires a delete intent".to_string(),
            ));
        }
        let path = intent.path().to_path_buf();
        let snapshot = self.validate_intent(intent)?;
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent).map_err(|error| {
                MutationError::Io(format!(
                    "create trash directory '{}': {error}",
                    parent.display()
                ))
            })?;
        }
        self.recheck_intent_immediately_before_commit(intent)?;
        std::fs::rename(&path, destination)
            .map_err(|error| MutationError::Io(format!("trash '{}': {error}", path.display())))?;
        self.finish_intent(intent, snapshot)
    }

    #[cfg(test)]
    fn set_batch_fail_after_commits(&self, count: usize) {
        *self.batch_fail_after_commits.lock().unwrap() = Some(count);
    }

    #[cfg(test)]
    fn batch_failure_injected(&self, committed: usize) -> bool {
        self.batch_fail_after_commits
            .lock()
            .unwrap()
            .is_some_and(|count| count == committed)
    }

    #[cfg(not(test))]
    fn batch_failure_injected(&self, _committed: usize) -> bool {
        false
    }

    /// EFF-006 qualification fault hook: die for real after N committed
    /// mutations, with the journal still recording `prepared`.
    ///
    /// The `#[cfg(test)]` hook above only simulates an in-process `Err`, which
    /// still runs the settlement path. The crash this atom is about kills the
    /// process BEFORE partial/complete settlement, so it must exit rather than
    /// unwind - destructors must not get a chance to settle the journal.
    #[cfg(not(feature = "coding_tools-host"))]
    fn eff006_qualification_fault(_committed: usize) {}

    #[cfg(feature = "coding_tools-host")]
    fn eff006_qualification_fault(committed: usize) {
        if let Ok(spec) = std::env::var("RIGHTKIT_QUAL_EFF006_FAULT") {
            if let Some(threshold) = spec.strip_prefix("after_commits:") {
                if threshold.trim().parse::<usize>() == Ok(committed) {
                    std::process::exit(88);
                }
            }
        }
    }

    /// Execute a prepared multi-path mutation set under one deterministic lock
    /// set. The protocol is prepare -> revision recheck -> durable intent
    /// journal -> sequential commit. Filesystem-global atomicity is explicitly
    /// not claimed: any interruption returns an exact receipt containing every
    /// committed/not-attempted entry and the observed final state of every
    /// touched path.
    pub fn write_batch_with_receipts(
        &self,
        workspace_root: &Path,
        entries: Vec<MutationBatchEntry>,
    ) -> Result<BatchMutationReceipt, BatchMutationFailure> {
        let journal_id = self.next_batch_journal_id();
        if entries.is_empty() {
            return Err(BatchMutationFailure {
                receipt: BatchMutationReceipt {
                    journal_id,
                    complete: false,
                    failure: Some("batch requires at least one mutation".to_string()),
                    entries: Vec::new(),
                    final_state: Vec::new(),
                    compensation_attempted: false,
                },
            });
        }
        let mut lock_paths = entries
            .iter()
            .flat_map(MutationBatchEntry::lock_paths)
            .collect::<Vec<_>>();
        lock_paths.sort();
        lock_paths.dedup();

        let outer = self.with_locked_paths_typed(&lock_paths, || {
            Ok(self.execute_batch_locked(workspace_root, &journal_id, &entries, &lock_paths))
        });
        match outer {
            Ok(result) => result,
            Err(error) => Err(BatchMutationFailure {
                receipt: self.batch_receipt_before_commit(
                    journal_id,
                    &entries,
                    &lock_paths,
                    error.to_string(),
                ),
            }),
        }
    }

    fn execute_batch_locked(
        &self,
        workspace_root: &Path,
        journal_id: &str,
        entries: &[MutationBatchEntry],
        lock_paths: &[PathBuf],
    ) -> Result<BatchMutationReceipt, BatchMutationFailure> {
        // Prepare: validate the complete set only after every canonical path
        // lock has been acquired. No ordinary target has mutated yet.
        for entry in entries {
            if let Err(error) = self.validate_batch_entry(entry) {
                return Err(BatchMutationFailure {
                    receipt: self.batch_receipt_before_commit(
                        journal_id.to_string(),
                        entries,
                        lock_paths,
                        format!("batch prepare conflict: {error}"),
                    ),
                });
            }
        }
        // Explicit second revision pass before the intent record becomes
        // commit-authoritative. Individual commits still recheck immediately
        // before their target mutation (EFF-005).
        for entry in entries {
            if let Err(error) = self.validate_batch_entry(entry) {
                return Err(BatchMutationFailure {
                    receipt: self.batch_receipt_before_commit(
                        journal_id.to_string(),
                        entries,
                        lock_paths,
                        format!("batch revision recheck conflict: {error}"),
                    ),
                });
            }
        }

        if let Err(error) =
            self.persist_batch_journal(workspace_root, journal_id, "prepared", entries, None)
        {
            return Err(BatchMutationFailure {
                receipt: self.batch_receipt_before_commit(
                    journal_id.to_string(),
                    entries,
                    lock_paths,
                    format!("batch intent journal failed before commit: {error}"),
                ),
            });
        }

        let mut receipts = entries
            .iter()
            .enumerate()
            .map(|(index, entry)| BatchEntryReceipt {
                index,
                intent: entry.intent().kind(),
                outcome: BatchEntryOutcome::NotAttempted,
                mutation: None,
                error: None,
            })
            .collect::<Vec<_>>();
        let mut committed = 0usize;

        for (index, entry) in entries.iter().enumerate() {
            // EFF-006: real process death between a durable `prepared` journal
            // and its partial/complete settlement.
            Self::eff006_qualification_fault(committed);
            if self.batch_failure_injected(committed) {
                let failure = format!(
                    "injected batch failure after {committed} of {} commit(s)",
                    entries.len()
                );
                let receipt =
                    self.finish_partial_batch_receipt(journal_id, receipts, lock_paths, failure);
                let _ = self.persist_batch_journal(
                    workspace_root,
                    journal_id,
                    "partial",
                    entries,
                    Some(&receipt),
                );
                return Err(BatchMutationFailure { receipt });
            }

            match self.commit_batch_entry_locked(entry) {
                Ok(mutation) => {
                    receipts[index].outcome = BatchEntryOutcome::Committed;
                    receipts[index].mutation = Some(mutation);
                    committed += 1;
                }
                Err(error) => {
                    receipts[index].outcome = BatchEntryOutcome::Failed;
                    receipts[index].error = Some(error.to_string());
                    let receipt = self.finish_partial_batch_receipt(
                        journal_id,
                        receipts,
                        lock_paths,
                        format!("batch commit {index} failed: {error}"),
                    );
                    let _ = self.persist_batch_journal(
                        workspace_root,
                        journal_id,
                        "partial",
                        entries,
                        Some(&receipt),
                    );
                    return Err(BatchMutationFailure { receipt });
                }
            }
        }

        let receipt = BatchMutationReceipt {
            journal_id: journal_id.to_string(),
            complete: true,
            failure: None,
            entries: receipts,
            final_state: self.capture_batch_final_state(lock_paths),
            compensation_attempted: false,
        };
        if let Err(error) = self.persist_batch_journal(
            workspace_root,
            journal_id,
            "complete",
            entries,
            Some(&receipt),
        ) {
            let mut unsettled = receipt;
            unsettled.failure = Some(format!(
                "all intended mutations committed but journal settlement failed: {error}"
            ));
            return Err(BatchMutationFailure { receipt: unsettled });
        }
        Ok(receipt)
    }

    fn validate_batch_entry(&self, entry: &MutationBatchEntry) -> Result<(), MutationError> {
        if let MutationBatchEntry::Copy {
            source,
            expected_source,
            ..
        } = entry
        {
            self.assert_preimage(
                source,
                &ExpectedPreimage::Revision(expected_source.clone()),
                entry.intent().kind(),
            )?;
        }
        self.validate_intent(entry.intent()).map(|_| ())
    }

    fn commit_batch_entry_locked(
        &self,
        entry: &MutationBatchEntry,
    ) -> Result<MutationReceipt, MutationError> {
        match entry {
            MutationBatchEntry::Write { intent, bytes } => {
                self.apply_byte_intent_locked(intent, bytes)
            }
            MutationBatchEntry::CreateDirectory { intent } => {
                self.create_directory_intent_locked(intent)
            }
            MutationBatchEntry::Copy {
                source,
                expected_source,
                intent,
            } => self.copy_intent_locked(source, expected_source, intent),
            MutationBatchEntry::Move { intent } => self.move_intent_locked(intent),
            MutationBatchEntry::Delete { intent, recursive } => {
                self.delete_intent_locked(intent, *recursive)
            }
            MutationBatchEntry::Trash {
                intent,
                destination,
            } => self.trash_intent_locked(intent, destination),
        }
    }

    fn next_batch_journal_id(&self) -> String {
        let sequence = self.batch_sequence.fetch_add(1, Ordering::Relaxed);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        format!("{:x}-{:x}-{sequence:x}", std::process::id(), nanos)
    }

    fn persist_batch_journal(
        &self,
        workspace_root: &Path,
        journal_id: &str,
        phase: &str,
        entries: &[MutationBatchEntry],
        receipt: Option<&BatchMutationReceipt>,
    ) -> Result<(), MutationError> {
        let journal_dir = workspace_root.join(".rightkit").join("mutation-journal");
        std::fs::create_dir_all(&journal_dir).map_err(|error| {
            MutationError::Io(format!(
                "create mutation journal '{}': {error}",
                journal_dir.display()
            ))
        })?;
        let path = journal_dir.join(format!("{journal_id}.json"));
        let intents = entries
            .iter()
            .enumerate()
            .map(|(index, entry)| entry.journal_value(index))
            .collect::<Vec<_>>();
        let payload = json!({
            "schema": 1,
            "journalId": journal_id,
            "phase": phase,
            "intents": intents,
            "receipt": receipt,
        });
        let encoded = serde_json::to_vec_pretty(&payload).map_err(|error| {
            MutationError::Io(format!(
                "serialize mutation journal '{journal_id}': {error}"
            ))
        })?;
        rightkit_fs::atomic_write(&path, &encoded)
            .map(|_| ())
            .map_err(|error| {
                MutationError::Io(format!(
                    "write mutation journal '{}': {error}",
                    path.display()
                ))
            })
    }

    fn batch_receipt_before_commit(
        &self,
        journal_id: String,
        entries: &[MutationBatchEntry],
        lock_paths: &[PathBuf],
        failure: String,
    ) -> BatchMutationReceipt {
        BatchMutationReceipt {
            journal_id,
            complete: false,
            failure: Some(failure),
            entries: entries
                .iter()
                .enumerate()
                .map(|(index, entry)| BatchEntryReceipt {
                    index,
                    intent: entry.intent().kind(),
                    outcome: BatchEntryOutcome::NotAttempted,
                    mutation: None,
                    error: None,
                })
                .collect(),
            final_state: self.capture_batch_final_state(lock_paths),
            compensation_attempted: false,
        }
    }

    fn finish_partial_batch_receipt(
        &self,
        journal_id: &str,
        entries: Vec<BatchEntryReceipt>,
        lock_paths: &[PathBuf],
        failure: String,
    ) -> BatchMutationReceipt {
        BatchMutationReceipt {
            journal_id: journal_id.to_string(),
            complete: false,
            failure: Some(failure),
            entries,
            final_state: self.capture_batch_final_state(lock_paths),
            compensation_attempted: false,
        }
    }

    fn capture_batch_final_state(&self, lock_paths: &[PathBuf]) -> Vec<BatchPathState> {
        lock_paths
            .iter()
            .map(|path| match self.observe_preimage(path) {
                Ok(state) => BatchPathState {
                    path: path.clone(),
                    state: Some(state),
                    observation_error: None,
                },
                Err(error) => BatchPathState {
                    path: path.clone(),
                    state: None,
                    observation_error: Some(error.to_string()),
                },
            })
            .collect()
    }

    /// Execute one privileged whole-workspace restore while excluding every
    /// ordinary mutation that uses this FileService. Validation may happen
    /// before this call, but the privileged filesystem commit must live inside
    /// the closure. The receipt always records provenance plus before/after
    /// generation and workspace-content revision.
    pub fn with_exclusive_workspace_restore<T>(
        &self,
        workspace_root: &Path,
        provenance: WorkspaceRestoreProvenance,
        operation: impl FnOnce() -> Result<T, String>,
    ) -> Result<(T, WorkspaceRestoreReceipt), WorkspaceRestoreFailure> {
        let guard = match canonical_workspace_gate().write() {
            Ok(guard) => guard,
            Err(_) => {
                let state = self.workspace_generation_revision(workspace_root);
                return Err(WorkspaceRestoreFailure {
                    receipt: WorkspaceRestoreReceipt {
                        provenance,
                        succeeded: false,
                        failure: Some(
                            "exclusive workspace mutation lease is unavailable; retry restore"
                                .to_string(),
                        ),
                        before: state.clone(),
                        after: state,
                    },
                });
            }
        };
        let before = self.workspace_generation_revision(workspace_root);
        if let Some(error) = before.observation_error.clone() {
            drop(guard);
            return Err(WorkspaceRestoreFailure {
                receipt: WorkspaceRestoreReceipt {
                    provenance,
                    succeeded: false,
                    failure: Some(format!(
                        "refusing privileged restore without before-workspace revision: {error}"
                    )),
                    before: before.clone(),
                    after: before,
                },
            });
        }

        let outcome = operation();
        canonical_workspace_generation().fetch_add(1, Ordering::AcqRel);
        let after = self.workspace_generation_revision(workspace_root);
        drop(guard);

        match outcome {
            Ok(value) if after.observation_error.is_none() => {
                let receipt = WorkspaceRestoreReceipt {
                    provenance,
                    succeeded: true,
                    failure: None,
                    before,
                    after,
                };
                Ok((value, receipt))
            }
            Ok(_) => Err(WorkspaceRestoreFailure {
                receipt: WorkspaceRestoreReceipt {
                    provenance,
                    succeeded: true,
                    failure: Some(format!(
                        "restore committed but after-workspace revision could not be observed: {}",
                        after
                            .observation_error
                            .as_deref()
                            .unwrap_or("unknown observation failure")
                    )),
                    before,
                    after,
                },
            }),
            Err(error) => Err(WorkspaceRestoreFailure {
                receipt: WorkspaceRestoreReceipt {
                    provenance,
                    succeeded: false,
                    failure: Some(error),
                    before,
                    after,
                },
            }),
        }
    }

    pub fn workspace_generation_revision(
        &self,
        workspace_root: &Path,
    ) -> WorkspaceGenerationRevision {
        let generation = canonical_workspace_generation().load(Ordering::Acquire);
        match workspace_content_revision(workspace_root) {
            Ok(revision) => WorkspaceGenerationRevision {
                generation,
                revision: Some(revision),
                observation_error: None,
            },
            Err(error) => WorkspaceGenerationRevision {
                generation,
                revision: None,
                observation_error: Some(error),
            },
        }
    }

    fn path_lock(&self, path: &Path) -> Result<Arc<Mutex<()>>, String> {
        let key = mutation_lock_identity(path);
        let mut locks = canonical_path_locks()
            .lock()
            .map_err(|_| "file mutation lock table is unavailable".to_string())?;
        if let Some(existing) = locks.get(&key).and_then(Weak::upgrade) {
            return Ok(existing);
        }
        let lock = Arc::new(Mutex::new(()));
        locks.insert(key, Arc::downgrade(&lock));
        Ok(lock)
    }

    fn with_locked_paths_typed<T>(
        &self,
        paths: &[PathBuf],
        operation: impl FnOnce() -> Result<T, MutationError>,
    ) -> Result<T, MutationError> {
        let _workspace_guard = canonical_workspace_gate().read().map_err(|_| {
            MutationError::Io(
                "workspace mutation lease is unavailable; retry the operation".to_string(),
            )
        })?;
        let mut ordered = paths.to_vec();
        ordered.sort();
        ordered.dedup();
        let locks = ordered
            .iter()
            .map(|path| self.path_lock(path).map_err(MutationError::Io))
            .collect::<Result<Vec<_>, _>>()?;
        let mut guards = Vec::with_capacity(locks.len());
        for lock in &locks {
            guards.push(lock.lock().map_err(|_| {
                MutationError::Io(
                    "file mutation lock is unavailable; retry the operation".to_string(),
                )
            })?);
        }
        operation()
    }

    fn expected_revision_from_receipt(
        &self,
        path: &Path,
        hashline_validated: bool,
    ) -> Result<FileRevision, MutationError> {
        let expected = self
            .receipts
            .lock()
            .map_err(|_| {
                MutationError::Io("file receipt ledger is unavailable; re-read file".to_string())
            })?
            .get(path)
            .cloned()
            .ok_or_else(|| {
                MutationError::InvalidIntent(format!(
                    "Refusing mutation of '{}': read the complete current file first, then retry.",
                    path.display()
                ))
            })?;
        if !expected.complete && !hashline_validated {
            return Err(MutationError::InvalidIntent(format!(
                "Refusing mutation of '{}': current read was partial. Re-read full file or use fresh HashLine anchors.",
                path.display()
            )));
        }
        Ok(expected.revision)
    }

    fn current_receipt_revision(&self, path: &Path) -> Result<FileRevision, String> {
        let expected = self
            .expected_revision_from_receipt(path, false)
            .map_err(|error| error.to_string())?;
        let current = revision(path)?;
        if current != expected {
            return Err(format!(
                "Conflict for '{}': file changed after read. Re-read it before retrying.",
                path.display()
            ));
        }
        Ok(expected)
    }

    fn require_current_receipt(&self, path: &Path) -> Result<(), String> {
        self.current_receipt_revision(path).map(|_| ())
    }

    fn validate_intent(&self, intent: &MutationIntent) -> Result<MutationSnapshot, MutationError> {
        match intent {
            MutationIntent::Create { path } => {
                self.assert_preimage(path, &ExpectedPreimage::Missing, intent.kind())?;
                Ok(MutationSnapshot {
                    before: None,
                    destination_before: None,
                })
            }
            MutationIntent::Replace { path, expected }
            | MutationIntent::Update { path, expected }
            | MutationIntent::Restore { path, expected } => {
                self.assert_preimage(
                    path,
                    &ExpectedPreimage::Revision(expected.clone()),
                    intent.kind(),
                )?;
                Ok(MutationSnapshot {
                    before: Some(expected.clone()),
                    destination_before: None,
                })
            }
            MutationIntent::Move {
                source,
                destination,
                expected_source,
                expected_destination,
            } => {
                self.assert_preimage(
                    source,
                    &ExpectedPreimage::Revision(expected_source.clone()),
                    MutationIntentKind::Move,
                )?;
                self.assert_preimage(destination, expected_destination, MutationIntentKind::Move)?;
                Ok(MutationSnapshot {
                    before: Some(expected_source.clone()),
                    destination_before: expected_destination.as_revision().cloned(),
                })
            }
            MutationIntent::Delete { path, expected } => {
                self.assert_preimage(
                    path,
                    &ExpectedPreimage::Revision(expected.clone()),
                    MutationIntentKind::Delete,
                )?;
                Ok(MutationSnapshot {
                    before: Some(expected.clone()),
                    destination_before: None,
                })
            }
        }
    }

    fn assert_preimage(
        &self,
        path: &Path,
        expected: &ExpectedPreimage,
        intent: MutationIntentKind,
    ) -> Result<(), MutationError> {
        let current = self.observe_preimage(path)?;
        if &current != expected {
            return Err(MutationError::Conflict(MutationConflict {
                intent,
                path: path.to_path_buf(),
                expected: expected.clone(),
                current,
            }));
        }
        Ok(())
    }

    fn finish_intent(
        &self,
        intent: &MutationIntent,
        snapshot: MutationSnapshot,
    ) -> Result<MutationReceipt, MutationError> {
        self.finish_intent_with_warning(intent, snapshot, None)
    }

    fn finish_intent_with_warning(
        &self,
        intent: &MutationIntent,
        snapshot: MutationSnapshot,
        durability_warning: Option<String>,
    ) -> Result<MutationReceipt, MutationError> {
        let after_path = intent.destination().unwrap_or_else(|| intent.path());
        let after = match intent.kind() {
            MutationIntentKind::Delete => None,
            _ => match self.observe_preimage(after_path)? {
                ExpectedPreimage::Missing => None,
                ExpectedPreimage::Revision(revision) => Some(revision),
            },
        };
        let receipt = MutationReceipt {
            intent: intent.kind(),
            path: intent.path().to_path_buf(),
            destination: intent.destination().map(Path::to_path_buf),
            before: snapshot.before,
            destination_before: snapshot.destination_before,
            after,
            durability_warning,
        };
        self.mutation_receipts
            .lock()
            .map_err(|_| MutationError::Io("mutation receipt ledger is unavailable".to_string()))?
            .push(receipt.clone());
        self.receipts
            .lock()
            .map_err(|_| {
                MutationError::Io("file receipt ledger is unavailable after mutation".to_string())
            })?
            .remove(intent.path());
        if let Some(destination) = intent.destination() {
            self.receipts
                .lock()
                .map_err(|_| {
                    MutationError::Io(
                        "file receipt ledger is unavailable after mutation".to_string(),
                    )
                })?
                .remove(destination);
        }
        canonical_workspace_generation().fetch_add(1, Ordering::AcqRel);
        Ok(receipt)
    }
}

/// Policy-aware filesystem lifecycle operations. This extends the existing
/// session-owned receipt/lock service instead of creating a second filesystem
/// abstraction. All path arguments are resolved through the same canonical
/// workspace boundary used by `read_file` and `write_file`.
pub struct FileLifecycleTool;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct FileStat {
    path: String,
    kind: &'static str,
    size: u64,
    read_only: bool,
    modified_ns: Option<u128>,
    /// ENG-19: `Some(true)` when the file's head bytes look binary (or it
    /// exceeds [`BINARY_SNIFF_MAX_BYTES`] worth of sampling would be
    /// truncated by size caps elsewhere), `Some(false)` when it looks like
    /// text, `None` for non-file entries (directories/symlinks) where the
    /// question does not apply.
    is_binary: Option<bool>,
    /// ENG-19: `true` when the file is larger than [`LARGE_FILE_BYTES`] â€”
    /// callers should treat full-content reads/diffs as unsupported for
    /// this entry and fall back to a capped preview instead.
    oversized: bool,
}

/// Bytes sampled from the start of a file to decide whether it is binary.
/// Matches the sample window `revision()` already reads for fingerprinting,
/// so this sniff costs no extra I/O beyond what `stat` already touches when
/// paired with a version check.
const BINARY_SNIFF_SAMPLE_BYTES: usize = 8192;

/// Size threshold past which a file is flagged `oversized` for read/diff
/// purposes. 5 MiB comfortably covers source files while catching the
/// generated/binary artifacts (bundles, media, lockfile blobs) that should
/// route to a capped preview instead of a full diff.
pub const LARGE_FILE_BYTES: u64 = 5 * 1024 * 1024;

/// Sniff whether `bytes` looks binary: a NUL byte anywhere in the sample, or
/// a high proportion of non-printable, non-whitespace control bytes, is
/// treated as binary. This is the same class of heuristic Git and most
/// editors use (NUL-byte presence is the primary signal); it deliberately
/// stays conservative rather than fully decoding as UTF-8, since tools call
/// this on a small head sample, not the whole file.
/// EFF-006: what an orphaned journal entry's path looks like NOW.
///
/// A `prepared` journal proves only that the batch was about to start. It is
/// never evidence that zero mutations executed - the process may have died
/// after any number of commits. Recovery therefore reports observed state per
/// path and never infers "nothing happened" from the phase alone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum JournalEntryState {
    /// On-disk content still matches the recorded expected pre-image, so this
    /// mutation had not been applied when the process died.
    NotApplied,
    /// On-disk content differs from the recorded expected pre-image. Either
    /// this mutation landed, or an external writer changed the path. Both are
    /// reported the same way because they are genuinely indistinguishable
    /// after the fact; claiming otherwise would be a false guarantee.
    ChangedSincePrepare,
    /// The journal entry carried no expected pre-image to compare against
    /// (a create), so only presence/absence is observable.
    NoPreimageRecorded { exists: bool },
    /// The path could not be inspected.
    Indeterminate { reason: String },
}

/// Reconciliation of one journal that never reached partial/complete.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct JournalReconciliation {
    pub journal_id: String,
    /// Phase recorded on disk, e.g. `prepared` for an interrupted batch.
    pub phase: String,
    pub entries: Vec<JournalEntryReconciliation>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct JournalEntryReconciliation {
    pub index: usize,
    pub path: PathBuf,
    pub state: JournalEntryState,
}

impl JournalReconciliation {
    /// True when at least one journaled path changed since prepare, i.e. the
    /// batch was genuinely partially applied.
    pub fn observed_partial_application(&self) -> bool {
        self.entries
            .iter()
            .any(|entry| matches!(entry.state, JournalEntryState::ChangedSincePrepare))
    }
}

/// Scan the workspace's mutation journal for batches that never settled and
/// report the exact resulting state of every path they intended to touch.
///
/// Nothing here repairs or replays: a partially applied non-idempotent batch
/// must not be blindly re-run. This surfaces truth for a caller to act on.
pub fn recover_mutation_journals(
    workspace_root: &Path,
) -> Result<Vec<JournalReconciliation>, String> {
    let journal_dir = workspace_root.join(".rightkit").join("mutation-journal");
    let entries = match std::fs::read_dir(&journal_dir) {
        Ok(entries) => entries,
        // No journal directory means no interrupted batches.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(format!(
                "read mutation journal '{}': {error}",
                journal_dir.display()
            ))
        }
    };

    let mut reconciliations = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let raw = match std::fs::read_to_string(&path) {
            Ok(raw) => raw,
            Err(error) => {
                return Err(format!("read journal '{}': {error}", path.display()));
            }
        };
        let document: Value = match serde_json::from_str(&raw) {
            Ok(document) => document,
            Err(error) => {
                return Err(format!("parse journal '{}': {error}", path.display()));
            }
        };
        let phase = document
            .get("phase")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        // `partial` and `complete` already carry an exact receipt; only an
        // unsettled journal needs reconstructing from the filesystem.
        if phase != "prepared" {
            continue;
        }
        let journal_id = document
            .get("journalId")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();

        let mut reconciled = Vec::new();
        for intent_value in document
            .get("intents")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
        {
            let index = intent_value
                .get("index")
                .and_then(Value::as_u64)
                .unwrap_or_default() as usize;
            let intent = intent_value.get("intent").cloned().unwrap_or(Value::Null);
            let target = intent
                .get("path")
                .or_else(|| intent.get("source"))
                .and_then(Value::as_str)
                .map(PathBuf::from);
            let Some(target) = target else {
                continue;
            };
            let expected_digest = intent
                .get("expected")
                .or_else(|| intent.get("expected_source"))
                .and_then(|expected| {
                    expected.get("content_digest").or_else(|| {
                        expected
                            .get("revision")
                            .and_then(|r| r.get("content_digest"))
                    })
                })
                .and_then(Value::as_array)
                .map(|bytes| {
                    bytes
                        .iter()
                        .filter_map(Value::as_u64)
                        .map(|byte| byte as u8)
                        .collect::<Vec<u8>>()
                });

            let state = match expected_digest {
                Some(expected) if expected.len() == 32 => match revision(&target) {
                    Ok(current) => {
                        if current.content_digest.as_slice() == expected.as_slice() {
                            JournalEntryState::NotApplied
                        } else {
                            JournalEntryState::ChangedSincePrepare
                        }
                    }
                    // Recorded as present at prepare time, absent now: it changed.
                    Err(_) if !target.exists() => JournalEntryState::ChangedSincePrepare,
                    Err(reason) => JournalEntryState::Indeterminate { reason },
                },
                _ => JournalEntryState::NoPreimageRecorded {
                    exists: target.exists(),
                },
            };
            reconciled.push(JournalEntryReconciliation {
                index,
                path: target,
                state,
            });
        }
        reconciled.sort_by_key(|entry| entry.index);
        reconciliations.push(JournalReconciliation {
            journal_id,
            phase,
            entries: reconciled,
        });
    }
    reconciliations.sort_by(|a, b| a.journal_id.cmp(&b.journal_id));
    Ok(reconciliations)
}

pub fn sniff_binary(bytes: &[u8]) -> bool {
    if bytes.is_empty() {
        return false;
    }
    if bytes.contains(&0) {
        return true;
    }
    let control_count = bytes
        .iter()
        .filter(|&&b| b < 0x20 && !matches!(b, b'\n' | b'\r' | b'\t'))
        .count();
    // More than ~3% control bytes outside common whitespace is a strong
    // binary signal for a text-editing tool's purposes.
    control_count * 100 > bytes.len() * 3
}

/// Read up to [`BINARY_SNIFF_SAMPLE_BYTES`] from the start of `path` and
/// classify it via [`sniff_binary`]. Returns `Ok(None)` for anything that
/// isn't a plain, readable file (directories, unreadable entries) â€” callers
/// should treat that as "not applicable", not "confirmed text".
fn sniff_binary_file(path: &Path) -> Option<bool> {
    let mut file = File::open(path).ok()?;
    let mut sample = vec![0_u8; BINARY_SNIFF_SAMPLE_BYTES];
    let read = file.read(&mut sample).ok()?;
    Some(sniff_binary(&sample[..read]))
}

#[derive(Debug, Clone)]
struct FileOperation {
    operation: String,
    path: Option<String>,
    destination: Option<String>,
    overwrite: bool,
    recursive: bool,
}

impl FileOperation {
    fn parse(value: &Value) -> Result<Self, String> {
        let operation = value
            .get("operation")
            .or_else(|| value.get("action"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| "Missing required argument: operation".to_string())?
            .to_ascii_lowercase();
        Ok(Self {
            operation,
            path: value
                .get("path")
                .and_then(Value::as_str)
                .map(str::to_string),
            destination: value
                .get("destination")
                .or_else(|| value.get("to"))
                .and_then(Value::as_str)
                .map(str::to_string),
            overwrite: value
                .get("overwrite")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            recursive: value
                .get("recursive")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        })
    }

    fn class(&self) -> CapabilityClass {
        match self.operation.as_str() {
            "stat" => CapabilityClass::ReadFile,
            "trash" | "delete" => CapabilityClass::Destructive,
            _ => CapabilityClass::WriteFile,
        }
    }
}

#[async_trait::async_trait]
impl Tool for FileLifecycleTool {
    fn name(&self) -> &str {
        "file_service"
    }

    fn description(&self) -> &str {
        "Typed workspace filesystem lifecycle: stat, mkdir, copy, move, trash/delete, or conflict-checked batch. Canonical path, symlink, overwrite, receipt, and workspace-boundary checks apply."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["operation"],
            "properties": {
                "operation": { "type": "string", "enum": ["stat", "mkdir", "copy", "move", "trash", "delete", "batch"] },
                "path": { "type": "string" },
                "destination": { "type": "string" },
                "overwrite": { "type": "boolean", "default": false },
                "recursive": { "type": "boolean", "default": false },
                "operations": {
                    "type": "array",
                    "minItems": 1,
                    "items": { "type": "object" }
                }
            }
        })
    }

    fn capability_class_for(&self, args: &Value) -> Option<CapabilityClass> {
        if args
            .get("operation")
            .or_else(|| args.get("action"))
            .and_then(Value::as_str)
            == Some("stat")
        {
            Some(CapabilityClass::ReadFile)
        } else if matches!(
            args.get("operation")
                .or_else(|| args.get("action"))
                .and_then(Value::as_str),
            Some("trash" | "delete")
        ) {
            Some(CapabilityClass::Destructive)
        } else {
            Some(CapabilityClass::WriteFile)
        }
    }

    fn durable_write_kind(&self, args: &Value) -> Option<rightkit_agent_policy::DurableWriteKind> {
        args.get("path")
            .and_then(Value::as_str)
            .and_then(|path| {
                crate::coding_tools::durable_write::repository_policy_kind(Path::new(path))
            })
            .or_else(|| {
                args.get("destination")
                    .and_then(Value::as_str)
                    .and_then(|path| {
                        crate::coding_tools::durable_write::repository_policy_kind(Path::new(path))
                    })
            })
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolResult {
        let operation = match FileOperation::parse(&args) {
            Ok(operation) => operation,
            Err(error) => return ToolResult::error(error),
        };
        if operation.operation == "batch" {
            return execute_batch(&args, ctx);
        }
        match execute_one(&operation, ctx) {
            Ok((content, mutations)) => ToolResult::ok(content).with_mutations(mutations),
            Err(error) => ToolResult::error(error),
        }
    }

    fn is_read_only(&self) -> bool {
        false
    }
}

fn execute_batch(args: &Value, ctx: &ToolContext) -> ToolResult {
    let Some(values) = args.get("operations").and_then(Value::as_array) else {
        return ToolResult::error("Batch requires a non-empty 'operations' array.");
    };
    if values.is_empty() {
        return ToolResult::error("Batch requires a non-empty 'operations' array.");
    }
    let mut operations = Vec::with_capacity(values.len());
    for value in values {
        let operation = match FileOperation::parse(value) {
            Ok(operation) if operation.operation != "batch" => operation,
            Ok(_) => return ToolResult::error("Nested batch operations are not supported."),
            Err(error) => return ToolResult::error(format!("Invalid batch operation: {error}")),
        };
        if let Err(error) = preflight(&operation, ctx) {
            return ToolResult::error(format!("Batch preflight failed before mutation: {error}"));
        }
        operations.push(operation);
    }

    // One canonical path participates at most once in a lifecycle batch. This
    // keeps expected preimages stable and makes the whole batch one retry unit.
    let mut targets = std::collections::HashSet::new();
    for operation in &operations {
        for raw in [&operation.path, &operation.destination]
            .into_iter()
            .flatten()
        {
            let path = match crate::coding_tools::read_file::resolve_in_workspace(
                raw,
                &ctx.workspace_root,
            ) {
                Ok(path) => path,
                Err(error) => return ToolResult::error(error),
            };
            if !targets.insert(path) {
                return ToolResult::error(format!(
                    "Batch preflight failed before mutation: '{}' appears in more than one operation.",
                    raw
                ));
            }
        }
    }

    struct PreparedLifecycleResult {
        batch_index: Option<usize>,
        message: String,
        mutations: Vec<WorkspaceMutation>,
    }

    let mut entries = Vec::new();
    let mut prepared = Vec::with_capacity(operations.len());
    for operation in &operations {
        let raw = operation.path.as_deref().expect("preflight requires path");
        let path =
            match crate::coding_tools::read_file::resolve_in_workspace(raw, &ctx.workspace_root) {
                Ok(path) => path,
                Err(error) => return ToolResult::error(error),
            };
        match operation.operation.as_str() {
            "stat" => {
                let metadata = match std::fs::symlink_metadata(&path) {
                    Ok(metadata) => metadata,
                    Err(error) => {
                        return ToolResult::error(format!("stat '{}': {error}", path.display()))
                    }
                };
                prepared.push(PreparedLifecycleResult {
                    batch_index: None,
                    message: serde_json::to_string(&file_stat(&path, &metadata))
                        .unwrap_or_default(),
                    mutations: Vec::new(),
                });
            }
            "mkdir" => {
                let batch_index = entries.len();
                entries.push(MutationBatchEntry::CreateDirectory {
                    intent: MutationIntent::create(&path),
                });
                prepared.push(PreparedLifecycleResult {
                    batch_index: Some(batch_index),
                    message: format!("Created directory '{}'.", path.display()),
                    mutations: vec![WorkspaceMutation {
                        path,
                        kind: WorkspaceMutationKind::Created,
                        previous_content: None,
                    }],
                });
            }
            "copy" | "move" => {
                let destination = match crate::coding_tools::read_file::resolve_in_workspace(
                    operation
                        .destination
                        .as_deref()
                        .expect("preflight destination"),
                    &ctx.workspace_root,
                ) {
                    Ok(destination) => destination,
                    Err(error) => return ToolResult::error(error),
                };
                let expected_source = match ctx.file_service.observe_preimage(&path) {
                    Ok(ExpectedPreimage::Revision(revision)) => revision,
                    Ok(ExpectedPreimage::Missing) => {
                        return ToolResult::error(format!(
                            "Source '{}' disappeared during batch prepare.",
                            path.display()
                        ))
                    }
                    Err(error) => return ToolResult::error(error.to_string()),
                };
                let destination_existed = destination.exists();
                let destination_previous = if destination.is_file() {
                    std::fs::read_to_string(&destination).ok()
                } else {
                    None
                };
                let expected_destination = if destination_existed {
                    match ctx.file_service.current_receipt_revision(&destination) {
                        Ok(revision) => ExpectedPreimage::Revision(revision),
                        Err(error) => return ToolResult::error(error),
                    }
                } else {
                    ExpectedPreimage::Missing
                };
                let batch_index = entries.len();
                if operation.operation == "copy" {
                    let intent = match expected_destination.clone() {
                        ExpectedPreimage::Missing => MutationIntent::create(&destination),
                        ExpectedPreimage::Revision(revision) => {
                            MutationIntent::replace(&destination, revision)
                        }
                    };
                    entries.push(MutationBatchEntry::Copy {
                        source: path.clone(),
                        expected_source,
                        intent,
                    });
                    prepared.push(PreparedLifecycleResult {
                        batch_index: Some(batch_index),
                        message: format!(
                            "Copied '{}' to '{}'.",
                            path.display(),
                            destination.display()
                        ),
                        mutations: vec![WorkspaceMutation {
                            path: destination,
                            kind: if destination_existed {
                                WorkspaceMutationKind::Modified
                            } else {
                                WorkspaceMutationKind::Created
                            },
                            previous_content: destination_previous,
                        }],
                    });
                } else {
                    entries.push(MutationBatchEntry::Move {
                        intent: MutationIntent::move_path(
                            &path,
                            &destination,
                            expected_source,
                            expected_destination,
                        ),
                    });
                    prepared.push(PreparedLifecycleResult {
                        batch_index: Some(batch_index),
                        message: format!(
                            "Moved '{}' to '{}'.",
                            path.display(),
                            destination.display()
                        ),
                        mutations: vec![
                            WorkspaceMutation {
                                path: path.clone(),
                                kind: WorkspaceMutationKind::Deleted,
                                previous_content: None,
                            },
                            WorkspaceMutation {
                                path: destination,
                                kind: WorkspaceMutationKind::Renamed { from: path },
                                previous_content: destination_previous,
                            },
                        ],
                    });
                }
            }
            "trash" | "delete" => {
                let expected = match ctx.file_service.observe_preimage(&path) {
                    Ok(ExpectedPreimage::Revision(revision)) => revision,
                    Ok(ExpectedPreimage::Missing) => {
                        return ToolResult::error(format!(
                            "Path '{}' disappeared during batch prepare.",
                            path.display()
                        ))
                    }
                    Err(error) => return ToolResult::error(error.to_string()),
                };
                let batch_index = entries.len();
                if operation.operation == "trash" {
                    let trash_root = ctx.workspace_root.join(".rightkit").join("trash");
                    let name = path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or("item");
                    let nonce = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_nanos();
                    let destination =
                        trash_root.join(format!("{nonce}-{}-{name}", std::process::id()));
                    entries.push(MutationBatchEntry::Trash {
                        intent: MutationIntent::delete(&path, expected),
                        destination: destination.clone(),
                    });
                    prepared.push(PreparedLifecycleResult {
                        batch_index: Some(batch_index),
                        message: json!({"trashed": path, "trashPath": destination}).to_string(),
                        mutations: vec![WorkspaceMutation {
                            path,
                            kind: WorkspaceMutationKind::Deleted,
                            previous_content: None,
                        }],
                    });
                } else {
                    entries.push(MutationBatchEntry::Delete {
                        intent: MutationIntent::delete(&path, expected),
                        recursive: operation.recursive,
                    });
                    prepared.push(PreparedLifecycleResult {
                        batch_index: Some(batch_index),
                        message: format!("Deleted '{}'.", path.display()),
                        mutations: vec![WorkspaceMutation {
                            path,
                            kind: WorkspaceMutationKind::Deleted,
                            previous_content: None,
                        }],
                    });
                }
            }
            other => return ToolResult::error(format!("Unknown filesystem operation '{other}'.")),
        }
    }

    if entries.is_empty() {
        return ToolResult::ok(
            json!({
                "completed": prepared.len(),
                "results": prepared.iter().map(|item| item.message.clone()).collect::<Vec<_>>()
            })
            .to_string(),
        );
    }

    let (receipt, failure) = match ctx
        .file_service
        .write_batch_with_receipts(&ctx.workspace_root, entries)
    {
        Ok(receipt) => (receipt, None),
        Err(failure) => {
            let message = failure.to_string();
            (failure.receipt, Some(message))
        }
    };
    let mut messages = Vec::new();
    let mut mutations = Vec::new();
    for item in &prepared {
        match item.batch_index {
            None => messages.push(item.message.clone()),
            Some(index)
                if receipt
                    .entries
                    .get(index)
                    .is_some_and(|entry| entry.outcome == BatchEntryOutcome::Committed) =>
            {
                messages.push(item.message.clone());
                mutations.extend(item.mutations.clone());
            }
            Some(_) => {}
        }
    }

    if let Some(failure) = failure {
        let exact = serde_json::to_string(&receipt)
            .unwrap_or_else(|_| "<receipt serialization failed>".to_string());
        return ToolResult::error(format!(
            "Batch did not complete: {failure}. Exact mutation receipt: {exact}"
        ))
        .with_mutations(mutations);
    }

    ToolResult::ok(json!({"completed": messages.len(), "results": messages}).to_string())
        .with_mutations(mutations)
}

fn preflight(operation: &FileOperation, ctx: &ToolContext) -> Result<(), String> {
    if !matches!(
        operation.operation.as_str(),
        "stat" | "mkdir" | "copy" | "move" | "trash" | "delete"
    ) {
        return Err(format!(
            "Unknown filesystem operation '{}'.",
            operation.operation
        ));
    }
    let path_raw = operation
        .path
        .as_deref()
        .ok_or_else(|| format!("{} requires 'path'.", operation.operation))?;
    let path = crate::coding_tools::read_file::resolve_in_workspace(path_raw, &ctx.workspace_root)?;
    check_policy(operation.class(), &path, ctx)?;
    if matches!(operation.operation.as_str(), "copy" | "move") {
        // The source's bytes are read (copy) or relocated (move): a read-denied
        // source must never be duplicated to a readable location.
        crate::coding_tools::read_file::enforce_path_deny(
            ctx,
            &path,
            crate::coding_tools::read_file::PathAccess::Read,
        )?;
    }
    if operation.operation == "mkdir" {
        if path.exists() {
            return Err(format!("Path '{}' already exists.", path.display()));
        }
    } else if !path.exists() {
        return Err(format!("Path '{}' does not exist.", path.display()));
    } else if operation.operation != "stat" {
        reject_symlink(&path)?;
    }
    if matches!(operation.operation.as_str(), "copy" | "move") {
        let destination_raw = operation
            .destination
            .as_deref()
            .ok_or_else(|| format!("{} requires 'destination'.", operation.operation))?;
        let destination = crate::coding_tools::read_file::resolve_in_workspace(
            destination_raw,
            &ctx.workspace_root,
        )?;
        check_policy(CapabilityClass::WriteFile, &destination, ctx)?;
        if path == destination {
            return Err("Source and destination resolve to the same path.".to_string());
        }
        if path.is_dir() && destination.starts_with(&path) {
            return Err(format!(
                "Destination '{}' may not be inside source directory '{}'.",
                destination.display(),
                path.display()
            ));
        }
        if destination.exists() && !operation.overwrite {
            return Err(format!(
                "Destination '{}' exists; set overwrite=true after reading it.",
                destination.display()
            ));
        }
        if destination.exists() {
            reject_symlink(&destination)?;
            ctx.file_service.require_current_receipt(&destination)?;
        }
    }
    Ok(())
}

fn execute_one(
    operation: &FileOperation,
    ctx: &ToolContext,
) -> Result<(String, Vec<WorkspaceMutation>), String> {
    preflight(operation, ctx)?;
    let raw = operation.path.as_deref().expect("preflight requires path");
    let path = crate::coding_tools::read_file::resolve_in_workspace(raw, &ctx.workspace_root)?;
    match operation.operation.as_str() {
        "stat" => {
            let metadata = std::fs::symlink_metadata(&path)
                .map_err(|error| format!("stat '{}': {error}", path.display()))?;
            let stat = file_stat(&path, &metadata);
            Ok((serde_json::to_string(&stat).unwrap_or_default(), Vec::new()))
        }
        "mkdir" => {
            ctx.file_service
                .create_directory_with_intent(MutationIntent::create(&path))
                .map_err(|error| error.to_string())?;
            Ok((
                format!("Created directory '{}'.", path.display()),
                vec![WorkspaceMutation {
                    path,
                    kind: WorkspaceMutationKind::Created,
                    previous_content: None,
                }],
            ))
        }
        "copy" | "move" => copy_or_move(operation, path, ctx),
        "trash" => trash_path(path, ctx),
        "delete" => delete_path(path, operation.recursive, ctx),
        _ => Err(format!(
            "Unknown filesystem operation '{}'.",
            operation.operation
        )),
    }
}

fn check_policy(class: CapabilityClass, path: &Path, ctx: &ToolContext) -> Result<(), String> {
    let access = if class == CapabilityClass::ReadFile {
        crate::coding_tools::read_file::PathAccess::Read
    } else {
        crate::coding_tools::read_file::PathAccess::Write
    };
    crate::coding_tools::read_file::enforce_path_deny(ctx, path, access)?;
    if class != CapabilityClass::ReadFile {
        crate::coding_tools::durable_write::require_path_authorization(path, ctx)?;
    }
    let policy_path = crate::coding_tools::read_file::policy_target_path(path, &ctx.workspace_root);
    let verdict = PermissionEngine::new().evaluate(
        class,
        ctx.permission_mode,
        ctx.workspace_trust,
        Some(&policy_path),
        matches!(class, CapabilityClass::Destructive),
    );
    match verdict.decision {
        PermissionDecision::Allow => Ok(()),
        PermissionDecision::Ask => Err(format!(
            "Operation requires approval: {} (policy: {})",
            verdict.reason, verdict.policy_source
        )),
        PermissionDecision::Deny => Err(format!(
            "Permission denied: {} (policy: {})",
            verdict.reason, verdict.policy_source
        )),
    }
}

fn reject_symlink(path: &Path) -> Result<(), String> {
    if std::fs::symlink_metadata(path)
        .map(|metadata| metadata.file_type().is_symlink())
        .unwrap_or(false)
    {
        return Err(format!(
            "Refusing lifecycle mutation through symlink '{}'.",
            path.display()
        ));
    }
    Ok(())
}

/// Publish `from` at `to`, replacing what is there, through RightKit: `publish_dir` for a directory
/// (old tree kept until the new one is in place, restored on failure), `publish_file` for a file
/// (one atomic replace). A failure before the replace leaves `to` as it was, so no local backup is
/// needed; a destination of the other kind (file over directory or the reverse) is refused, never
/// replaced.
///
/// A failure AFTER the replace (`FsError::target_replaced()`: the parent directory sync or the
/// displaced old tree's removal) is a committed mutation: it returns `Ok(Some(message))` so the
/// caller finishes the intent and reports the durability problem separately.
fn publish_replacing(from: &Path, to: &Path) -> Result<Option<String>, String> {
    let is_dir = std::fs::symlink_metadata(from)
        .map(|meta| meta.is_dir())
        .unwrap_or(false);
    let published = if is_dir {
        rightkit_fs::publish_dir(from, to).map(|_| ())
    } else {
        rightkit_fs::publish_file(from, to)
    };
    match published {
        Ok(()) => Ok(None),
        Err(error) if error.target_replaced() => Ok(Some(error.to_string())),
        Err(error) => Err(error.to_string()),
    }
}

/// A separate sentence for a committed mutation whose durability step failed.
fn durability_note(receipt: &MutationReceipt) -> String {
    receipt
        .durability_warning
        .as_ref()
        .map(|warning| format!(" Committed, but a durability step failed: {warning}."))
        .unwrap_or_default()
}

fn mutation_sibling(destination: &Path, label: &str) -> PathBuf {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    destination.with_file_name(format!(".rightkit-{label}-{}-{nonce}", std::process::id()))
}

fn copy_or_move(
    operation: &FileOperation,
    source: PathBuf,
    ctx: &ToolContext,
) -> Result<(String, Vec<WorkspaceMutation>), String> {
    if !source.exists() {
        return Err(format!("Source '{}' does not exist.", source.display()));
    }
    reject_symlink(&source)?;
    let destination = crate::coding_tools::read_file::resolve_in_workspace(
        operation
            .destination
            .as_deref()
            .expect("preflight destination"),
        &ctx.workspace_root,
    )?;
    reject_symlink(&destination)?;
    let destination_existed = destination.exists();
    let destination_previous = if destination.is_file() {
        std::fs::read_to_string(&destination).ok()
    } else {
        None
    };

    if operation.operation == "copy" {
        let expected_source = match ctx
            .file_service
            .observe_preimage(&source)
            .map_err(|error| error.to_string())?
        {
            ExpectedPreimage::Revision(revision) => revision,
            ExpectedPreimage::Missing => {
                return Err(format!(
                    "Source '{}' disappeared before copy.",
                    source.display()
                ))
            }
        };
        let intent = if destination_existed {
            MutationIntent::replace(
                &destination,
                ctx.file_service.current_receipt_revision(&destination)?,
            )
        } else {
            MutationIntent::create(&destination)
        };
        let receipt = ctx
            .file_service
            .copy_with_intent(&source, expected_source, intent)
            .map_err(|error| error.to_string())?;
        Ok((
            format!(
                "Copied '{}' to '{}'.{}",
                source.display(),
                destination.display(),
                durability_note(&receipt)
            ),
            vec![WorkspaceMutation {
                path: destination,
                kind: if destination_existed {
                    WorkspaceMutationKind::Modified
                } else {
                    WorkspaceMutationKind::Created
                },
                previous_content: destination_previous,
            }],
        ))
    } else {
        let expected_source = match ctx
            .file_service
            .observe_preimage(&source)
            .map_err(|error| error.to_string())?
        {
            ExpectedPreimage::Revision(revision) => revision,
            ExpectedPreimage::Missing => {
                return Err(format!(
                    "Source '{}' disappeared before move.",
                    source.display()
                ))
            }
        };
        let expected_destination = if destination_existed {
            ExpectedPreimage::Revision(ctx.file_service.current_receipt_revision(&destination)?)
        } else {
            ExpectedPreimage::Missing
        };
        let receipt = ctx
            .file_service
            .move_with_intent(MutationIntent::move_path(
                &source,
                &destination,
                expected_source,
                expected_destination,
            ))
            .map_err(|error| error.to_string())?;
        Ok((
            format!(
                "Moved '{}' to '{}'.{}",
                source.display(),
                destination.display(),
                durability_note(&receipt)
            ),
            vec![
                WorkspaceMutation {
                    path: source.clone(),
                    kind: WorkspaceMutationKind::Deleted,
                    previous_content: None,
                },
                WorkspaceMutation {
                    path: destination,
                    kind: WorkspaceMutationKind::Renamed { from: source },
                    previous_content: destination_previous,
                },
            ],
        ))
    }
}

fn copy_tree(source: &Path, destination: &Path) -> Result<(), String> {
    let source_is_file = std::fs::symlink_metadata(source)
        .map(|metadata| metadata.file_type().is_file())
        .unwrap_or(false);
    if source_is_file {
        // RightKit's no-follow copy: a symlink (or reparse point) at the source is refused at open
        // time, so a link swapped in after `reject_symlink` is never followed. No hard link: the
        // copy must be independent of the source.
        rightkit_fs::clone_or_link_or_copy_nofollow_with(
            source,
            destination,
            &rightkit_fs::CloneOptions::default().without_hard_link(),
        )
        .map(|_| ())
        .map_err(|error| {
            format!(
                "copy '{}' to '{}': {error}",
                source.display(),
                destination.display()
            )
        })
    } else if source.is_dir() {
        std::fs::create_dir(destination).map_err(|error| {
            format!(
                "create copied directory '{}': {error}",
                destination.display()
            )
        })?;
        for entry in std::fs::read_dir(source)
            .map_err(|error| format!("read directory '{}': {error}", source.display()))?
        {
            let entry = entry.map_err(|error| format!("read directory entry: {error}"))?;
            reject_symlink(&entry.path())?;
            copy_tree(&entry.path(), &destination.join(entry.file_name()))?;
        }
        Ok(())
    } else {
        Err(format!("Unsupported source type '{}'.", source.display()))
    }
}

fn trash_path(
    path: PathBuf,
    ctx: &ToolContext,
) -> Result<(String, Vec<WorkspaceMutation>), String> {
    if !path.exists() {
        return Err(format!("Path '{}' does not exist.", path.display()));
    }
    reject_symlink(&path)?;
    let expected = match ctx
        .file_service
        .observe_preimage(&path)
        .map_err(|error| error.to_string())?
    {
        ExpectedPreimage::Revision(revision) => revision,
        ExpectedPreimage::Missing => {
            return Err(format!(
                "Path '{}' disappeared before trash.",
                path.display()
            ))
        }
    };
    let trash_root = ctx.workspace_root.join(".rightkit").join("trash");
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("item");
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let destination = trash_root.join(format!("{nonce}-{}-{name}", std::process::id()));
    ctx.file_service
        .trash_with_intent(MutationIntent::delete(&path, expected), &destination)
        .map_err(|error| error.to_string())?;
    Ok((
        json!({"trashed": path, "trashPath": destination}).to_string(),
        vec![WorkspaceMutation {
            path,
            kind: WorkspaceMutationKind::Deleted,
            previous_content: None,
        }],
    ))
}

fn delete_path(
    path: PathBuf,
    recursive: bool,
    ctx: &ToolContext,
) -> Result<(String, Vec<WorkspaceMutation>), String> {
    if !path.exists() {
        return Err(format!("Path '{}' does not exist.", path.display()));
    }
    reject_symlink(&path)?;
    let expected = match ctx
        .file_service
        .observe_preimage(&path)
        .map_err(|error| error.to_string())?
    {
        ExpectedPreimage::Revision(revision) => revision,
        ExpectedPreimage::Missing => {
            return Err(format!(
                "Path '{}' disappeared before delete.",
                path.display()
            ))
        }
    };
    ctx.file_service
        .delete_with_intent(MutationIntent::delete(&path, expected), recursive)
        .map_err(|error| error.to_string())?;
    Ok((
        format!("Deleted '{}'.", path.display()),
        vec![WorkspaceMutation {
            path,
            kind: WorkspaceMutationKind::Deleted,
            previous_content: None,
        }],
    ))
}

fn remove_existing(path: &Path) -> Result<(), String> {
    if path.is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    }
    .map_err(|error| format!("remove existing '{}': {error}", path.display()))
}

fn file_stat(path: &Path, metadata: &Metadata) -> FileStat {
    let kind = if metadata.file_type().is_symlink() {
        "symlink"
    } else if metadata.is_dir() {
        "directory"
    } else if metadata.is_file() {
        "file"
    } else {
        "other"
    };
    let is_binary = (kind == "file").then(|| sniff_binary_file(path)).flatten();
    FileStat {
        path: path.display().to_string(),
        kind,
        size: metadata.len(),
        read_only: metadata.permissions().readonly(),
        modified_ns: metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|duration| duration.as_nanos()),
        is_binary,
        oversized: metadata.len() > LARGE_FILE_BYTES,
    }
}

fn mutation_lock_identity(path: &Path) -> PathBuf {
    if let Ok(canonical) = path.canonicalize() {
        return canonical;
    }
    match (path.parent(), path.file_name()) {
        (Some(parent), Some(name)) => parent
            .canonicalize()
            .map(|canonical| canonical.join(name))
            .unwrap_or_else(|_| path.to_path_buf()),
        _ => path.to_path_buf(),
    }
}

fn workspace_content_revision(workspace_root: &Path) -> Result<u64, String> {
    let metadata = std::fs::symlink_metadata(workspace_root)
        .map_err(|error| format!("inspect workspace '{}': {error}", workspace_root.display()))?;
    if !metadata.is_dir() {
        return Err(format!(
            "workspace '{}' is not a directory",
            workspace_root.display()
        ));
    }
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    hash_workspace_revision_dir(workspace_root, workspace_root, &mut hasher)?;
    Ok(hasher.finish())
}

fn hash_workspace_revision_dir(
    workspace_root: &Path,
    directory: &Path,
    hasher: &mut std::collections::hash_map::DefaultHasher,
) -> Result<(), String> {
    let mut entries = std::fs::read_dir(directory)
        .map_err(|error| {
            format!(
                "read workspace directory '{}': {error}",
                directory.display()
            )
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("read workspace entry in '{}': {error}", directory.display()))?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = entry.path();
        let relative = path.strip_prefix(workspace_root).unwrap_or(&path);
        if relative.components().count() == 1
            && matches!(entry.file_name().to_str(), Some(".git" | ".rightkit"))
        {
            continue;
        }
        relative.to_string_lossy().replace('\\', "/").hash(hasher);
        let metadata = std::fs::symlink_metadata(&path)
            .map_err(|error| format!("inspect workspace entry '{}': {error}", path.display()))?;
        metadata.file_type().is_file().hash(hasher);
        metadata.file_type().is_dir().hash(hasher);
        metadata.file_type().is_symlink().hash(hasher);
        if metadata.file_type().is_symlink() {
            std::fs::read_link(&path)
                .map_err(|error| format!("read workspace symlink '{}': {error}", path.display()))?
                .hash(hasher);
        } else if metadata.is_dir() {
            hash_workspace_revision_dir(workspace_root, &path, hasher)?;
        } else if metadata.is_file() {
            metadata.len().hash(hasher);
            let mut file = File::open(&path)
                .map_err(|error| format!("open workspace file '{}': {error}", path.display()))?;
            let mut buffer = [0_u8; 64 * 1024];
            loop {
                let read = file.read(&mut buffer).map_err(|error| {
                    format!("read workspace file '{}': {error}", path.display())
                })?;
                if read == 0 {
                    break;
                }
                buffer[..read].hash(hasher);
            }
        }
    }
    Ok(())
}

fn file_revision_from_observed(
    path: &Path,
    observed: &crate::coding_tools::observed_version::ObservedVersion,
) -> FileRevision {
    FileRevision {
        identity: path.canonicalize().unwrap_or_else(|_| path.to_path_buf()),
        size: observed.size,
        modified_ns: observed.modified_ns,
        fast_hash: observed.fast_hash,
        // The observed version carries the digest captured when it was read;
        // recomputing here would read CURRENT content and always compare
        // equal, defeating the check.
        content_digest: observed.content_digest,
    }
}

fn hash_file_samples(
    path: &Path,
    metadata: &Metadata,
    hasher: &mut std::collections::hash_map::DefaultHasher,
) -> Result<(), String> {
    let mut file =
        File::open(path).map_err(|error| format!("open '{}': {error}", path.display()))?;
    let mut sample = [0_u8; 4096];
    let first = file
        .read(&mut sample)
        .map_err(|error| format!("read '{}': {error}", path.display()))?;
    metadata.len().hash(hasher);
    sample[..first].hash(hasher);
    if metadata.len() > sample.len() as u64 {
        file.seek(SeekFrom::End(-(sample.len() as i64)))
            .map_err(|error| format!("seek '{}': {error}", path.display()))?;
        let last = file
            .read(&mut sample)
            .map_err(|error| format!("read '{}': {error}", path.display()))?;
        sample[..last].hash(hasher);
    }
    Ok(())
}

fn hash_directory_tree(
    path: &Path,
    hasher: &mut std::collections::hash_map::DefaultHasher,
) -> Result<(), String> {
    let mut entries = std::fs::read_dir(path)
        .map_err(|error| format!("read directory '{}': {error}", path.display()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("read directory entry in '{}': {error}", path.display()))?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        entry.file_name().hash(hasher);
        let entry_path = entry.path();
        let metadata = std::fs::symlink_metadata(&entry_path)
            .map_err(|error| format!("inspect '{}': {error}", entry_path.display()))?;
        metadata.len().hash(hasher);
        metadata.file_type().is_file().hash(hasher);
        metadata.file_type().is_dir().hash(hasher);
        metadata.file_type().is_symlink().hash(hasher);
        if metadata.file_type().is_symlink() {
            std::fs::read_link(&entry_path)
                .map_err(|error| format!("read symlink '{}': {error}", entry_path.display()))?
                .hash(hasher);
        } else if metadata.is_dir() {
            hash_directory_tree(&entry_path, hasher)?;
        } else if metadata.is_file() {
            hash_file_samples(&entry_path, &metadata, hasher)?;
        }
    }
    Ok(())
}

/// EFF-005: stream the whole file through SHA-256. This is the authoritative
/// preimage identity; nothing about a mutation decision may rely on sampling
/// alone.
fn hash_file_contents(path: &Path) -> Result<[u8; 32], String> {
    let mut file =
        File::open(path).map_err(|error| format!("open '{}': {error}", path.display()))?;
    let mut hasher = rightkit_fs::digest::Sha256Hasher::new();
    let mut buffer = vec![0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| format!("read '{}': {error}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher.finalize_bytes().into())
}

/// Observe a path's current authoritative revision.
///
/// Exposed so a consumer crate can construct a journal fixture with genuine
/// pre-images rather than fabricated ones.
pub fn observe_revision_for_tests(path: &Path) -> Result<FileRevision, String> {
    revision(path)
}

fn revision(path: &Path) -> Result<FileRevision, String> {
    let metadata = std::fs::metadata(path)
        .map_err(|error| format!("inspect '{}': {error}", path.display()))?;
    let modified_ns = metadata
        .modified()
        .map_err(|error| format!("read modification time for '{}': {error}", path.display()))?
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|error| {
            format!(
                "invalid modification time for '{}': {error}",
                path.display()
            )
        })?
        .as_nanos();
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    let content_digest = if metadata.is_file() {
        // Preserve the original FileRevision / ObservedVersion fingerprint
        // contract for ordinary files; directory support must not perturb it.
        hash_file_samples(path, &metadata, &mut hasher)?;
        // EFF-005: the authoritative identity reads EVERY byte, so a
        // same-size middle-of-file edit with a restored mtime cannot pass as
        // unchanged.
        hash_file_contents(path)?
    } else if metadata.is_dir() {
        "directory".hash(&mut hasher);
        hash_directory_tree(path, &mut hasher)?;
        let mut tree = std::collections::hash_map::DefaultHasher::new();
        hash_directory_tree(path, &mut tree)?;
        let mut digest = [0_u8; 32];
        digest[..8].copy_from_slice(&tree.finish().to_le_bytes());
        digest
    } else {
        metadata.len().hash(&mut hasher);
        let mut digest = [0_u8; 32];
        digest[..8].copy_from_slice(&metadata.len().to_le_bytes());
        digest
    };
    Ok(FileRevision {
        identity: path.canonicalize().unwrap_or_else(|_| path.to_path_buf()),
        size: metadata.len(),
        modified_ns,
        fast_hash: hasher.finish(),
        content_digest,
    })
}

#[cfg(test)]
mod lifecycle_tests {
    use super::*;
    use rightkit_agent_policy::{PermissionMode, WorkspaceTrust};

    fn ctx(root: &Path) -> ToolContext {
        ToolContext::new(
            "file-service-test",
            root,
            PermissionMode::Bypass,
            WorkspaceTrust::Trusted,
        )
    }

    fn revision_from(service: &FileService, path: &Path) -> FileRevision {
        match service.observe_preimage(path).unwrap() {
            ExpectedPreimage::Revision(revision) => revision,
            ExpectedPreimage::Missing => panic!("expected existing preimage"),
        }
    }

    #[test]
    fn typed_create_never_overwrites_existing_path() {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("create.txt");
        std::fs::write(&file, "external-existing").unwrap();
        let service = FileService::default();

        let error = service
            .apply_byte_intent(MutationIntent::create(&file), b"agent-create")
            .expect_err("create must not overwrite an existing path");

        assert!(matches!(
            error,
            MutationError::Conflict(MutationConflict {
                intent: MutationIntentKind::Create,
                ..
            })
        ));
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "external-existing");
    }

    /// EFF-005 adversarial case through the REAL FileService mutation path.
    ///
    /// The `ObservedVersion` unit test proves the digest detects a
    /// middle-of-file mutation, but that alone would not prove the digest is
    /// CONSULTED when FileService decides whether a write is stale. Every
    /// other stale test here changes the file's size or its head/tail bytes,
    /// which the old sampling identity already caught - so none of them would
    /// fail if the digest were ignored.
    ///
    /// This mutates only unsampled middle bytes, preserves length exactly, and
    /// restores the modification time, then asserts the cheap signals really
    /// do collide before asserting FileService still conflicts.
    #[test]
    fn typed_stale_middle_of_file_mutation_conflicts_through_file_service() {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("large.bin");

        let size = 64 * 1024;
        let mut original = vec![b'A'; size];
        original[..16].copy_from_slice(b"HEAD_MARKER_1234");
        let tail = size - 16;
        original[tail..].copy_from_slice(b"TAIL_MARKER_5678");
        std::fs::write(&file, &original).unwrap();

        let service = FileService::default();
        let expected = revision_from(&service, &file);
        let original_mtime = std::fs::metadata(&file).unwrap().modified().unwrap();

        // External writer touches ONLY the middle, keeps the length, and puts
        // the timestamp back.
        let mut mutated = original.clone();
        let middle = size / 2;
        mutated[middle..middle + 8].copy_from_slice(b"TAMPERED");
        std::fs::write(&file, &mutated).unwrap();
        let handle = std::fs::File::options().write(true).open(&file).unwrap();
        handle.set_modified(original_mtime).unwrap();
        drop(handle);

        // Prove the attack is real: every cheap signal collides, so only the
        // authoritative content digest can catch this.
        let current = revision_from(&service, &file);
        assert_eq!(current.size, expected.size, "size collides");
        assert_eq!(current.modified_ns, expected.modified_ns, "mtime collides");
        assert_eq!(
            current.fast_hash, expected.fast_hash,
            "head/tail sampling collides - this is why sampling alone is unsafe"
        );
        assert_ne!(current.content_digest, expected.content_digest);

        // FileService must refuse the stale write and leave the newer bytes.
        let error = service
            .apply_byte_intent(MutationIntent::replace(&file, expected), b"agent-overwrite")
            .expect_err("a stale middle-of-file mutation must conflict");
        assert!(matches!(
            error,
            MutationError::Conflict(MutationConflict {
                intent: MutationIntentKind::Replace,
                ..
            })
        ));
        assert_eq!(
            std::fs::read(&file).unwrap(),
            mutated,
            "the external writer's newer bytes must survive"
        );
    }

    #[test]
    fn typed_stale_replace_conflicts_and_preserves_newer_bytes() {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("replace.txt");
        std::fs::write(&file, "v1").unwrap();
        let service = FileService::default();
        let expected = revision_from(&service, &file);
        std::fs::write(&file, "newer-external-replace").unwrap();

        let error = service
            .apply_byte_intent(MutationIntent::replace(&file, expected), b"agent")
            .expect_err("stale replace must conflict");
        assert!(matches!(
            error,
            MutationError::Conflict(MutationConflict {
                intent: MutationIntentKind::Replace,
                ..
            })
        ));
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "newer-external-replace"
        );
    }

    #[test]
    fn typed_stale_update_conflicts_and_preserves_newer_bytes() {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("update.txt");
        std::fs::write(&file, "v1").unwrap();
        let service = FileService::default();
        let expected = revision_from(&service, &file);
        std::fs::write(&file, "newer-external-update").unwrap();

        let error = service
            .apply_byte_intent(MutationIntent::update(&file, expected), b"agent")
            .expect_err("stale update must conflict");
        assert!(matches!(
            error,
            MutationError::Conflict(MutationConflict {
                intent: MutationIntentKind::Update,
                ..
            })
        ));
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "newer-external-update"
        );
    }

    #[test]
    fn typed_stale_move_conflicts_and_preserves_newer_source() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("move-source.txt");
        let destination = root.path().join("move-destination.txt");
        std::fs::write(&source, "v1").unwrap();
        let service = FileService::default();
        let expected = revision_from(&service, &source);
        std::fs::write(&source, "newer-external-move").unwrap();

        let error = service
            .move_with_intent(MutationIntent::move_path(
                &source,
                &destination,
                expected,
                ExpectedPreimage::Missing,
            ))
            .expect_err("stale move must conflict");
        assert!(matches!(
            error,
            MutationError::Conflict(MutationConflict {
                intent: MutationIntentKind::Move,
                ..
            })
        ));
        assert_eq!(
            std::fs::read_to_string(&source).unwrap(),
            "newer-external-move"
        );
        assert!(!destination.exists());
    }

    #[test]
    fn typed_stale_delete_conflicts_and_preserves_newer_bytes() {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("delete.txt");
        std::fs::write(&file, "v1").unwrap();
        let service = FileService::default();
        let expected = revision_from(&service, &file);
        std::fs::write(&file, "newer-external-delete").unwrap();

        let error = service
            .delete_with_intent(MutationIntent::delete(&file, expected), false)
            .expect_err("stale delete must conflict");
        assert!(matches!(
            error,
            MutationError::Conflict(MutationConflict {
                intent: MutationIntentKind::Delete,
                ..
            })
        ));
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "newer-external-delete"
        );
    }

    #[test]
    fn typed_stale_restore_conflicts_and_preserves_newer_bytes() {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("restore.txt");
        std::fs::write(&file, "v1").unwrap();
        let service = FileService::default();
        let expected = revision_from(&service, &file);
        std::fs::write(&file, "newer-external-restore").unwrap();

        let error = service
            .apply_byte_intent(MutationIntent::restore(&file, expected), b"old-checkpoint")
            .expect_err("stale restore must conflict");
        assert!(matches!(
            error,
            MutationError::Conflict(MutationConflict {
                intent: MutationIntentKind::Restore,
                ..
            })
        ));
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "newer-external-restore"
        );
    }

    fn inject_external_write_before_final_recheck(
        service: &FileService,
        path: PathBuf,
        bytes: &'static [u8],
    ) {
        service.set_before_final_recheck_hook(Arc::new(move |_| {
            std::fs::write(&path, bytes).unwrap();
        }));
    }

    #[test]
    fn final_recheck_fences_create_race_after_lock_acquisition() {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("create-race.txt");
        let service = FileService::default();
        inject_external_write_before_final_recheck(&service, file.clone(), b"external-create");

        let error = service
            .apply_byte_intent(MutationIntent::create(&file), b"agent-create")
            .expect_err("surprise create in final window must conflict");

        assert!(matches!(
            error,
            MutationError::Conflict(MutationConflict {
                intent: MutationIntentKind::Create,
                ..
            })
        ));
        assert_eq!(std::fs::read(&file).unwrap(), b"external-create");
    }

    #[test]
    fn final_recheck_fences_replace_race_after_lock_acquisition() {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("replace-race.txt");
        std::fs::write(&file, "v1").unwrap();
        let service = FileService::default();
        let expected = revision_from(&service, &file);
        inject_external_write_before_final_recheck(&service, file.clone(), b"external-replace");

        let error = service
            .apply_byte_intent(MutationIntent::replace(&file, expected), b"agent-replace")
            .expect_err("replace race in final window must conflict");

        assert!(matches!(
            error,
            MutationError::Conflict(MutationConflict {
                intent: MutationIntentKind::Replace,
                ..
            })
        ));
        assert_eq!(std::fs::read(&file).unwrap(), b"external-replace");
    }

    #[test]
    fn final_recheck_fences_update_race_after_lock_acquisition() {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("update-race.txt");
        std::fs::write(&file, "v1").unwrap();
        let service = FileService::default();
        let expected = revision_from(&service, &file);
        inject_external_write_before_final_recheck(&service, file.clone(), b"external-update");

        let error = service
            .apply_byte_intent(MutationIntent::update(&file, expected), b"agent-update")
            .expect_err("update race in final window must conflict");

        assert!(matches!(
            error,
            MutationError::Conflict(MutationConflict {
                intent: MutationIntentKind::Update,
                ..
            })
        ));
        assert_eq!(std::fs::read(&file).unwrap(), b"external-update");
    }

    #[test]
    fn final_recheck_fences_move_race_after_lock_acquisition() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("move-race-source.txt");
        let destination = root.path().join("move-race-destination.txt");
        std::fs::write(&source, "v1").unwrap();
        let service = FileService::default();
        let expected = revision_from(&service, &source);
        inject_external_write_before_final_recheck(&service, source.clone(), b"external-move");

        let error = service
            .move_with_intent(MutationIntent::move_path(
                &source,
                &destination,
                expected,
                ExpectedPreimage::Missing,
            ))
            .expect_err("move race in final window must conflict");

        assert!(matches!(
            error,
            MutationError::Conflict(MutationConflict {
                intent: MutationIntentKind::Move,
                ..
            })
        ));
        assert_eq!(std::fs::read(&source).unwrap(), b"external-move");
        assert!(!destination.exists());
    }

    #[test]
    fn final_recheck_fences_delete_race_after_lock_acquisition() {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("delete-race.txt");
        std::fs::write(&file, "v1").unwrap();
        let service = FileService::default();
        let expected = revision_from(&service, &file);
        inject_external_write_before_final_recheck(&service, file.clone(), b"external-delete");

        let error = service
            .delete_with_intent(MutationIntent::delete(&file, expected), false)
            .expect_err("delete race in final window must conflict");

        assert!(matches!(
            error,
            MutationError::Conflict(MutationConflict {
                intent: MutationIntentKind::Delete,
                ..
            })
        ));
        assert_eq!(std::fs::read(&file).unwrap(), b"external-delete");
    }

    #[test]
    fn final_recheck_fences_restore_race_after_lock_acquisition() {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("restore-race.txt");
        std::fs::write(&file, "v1").unwrap();
        let service = FileService::default();
        let expected = revision_from(&service, &file);
        inject_external_write_before_final_recheck(&service, file.clone(), b"external-restore");

        let error = service
            .apply_byte_intent(MutationIntent::restore(&file, expected), b"checkpoint")
            .expect_err("restore race in final window must conflict");

        assert!(matches!(
            error,
            MutationError::Conflict(MutationConflict {
                intent: MutationIntentKind::Restore,
                ..
            })
        ));
        assert_eq!(std::fs::read(&file).unwrap(), b"external-restore");
    }

    #[test]
    fn mutation_receipt_records_intent_before_and_after_revision() {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("receipt.txt");
        std::fs::write(&file, "before").unwrap();
        let service = FileService::default();
        let before = revision_from(&service, &file);

        let receipt = service
            .apply_byte_intent(
                MutationIntent::update(&file, before.clone()),
                b"after-longer",
            )
            .unwrap();

        assert_eq!(receipt.intent, MutationIntentKind::Update);
        assert_eq!(receipt.path, file);
        assert_eq!(receipt.before.as_ref(), Some(&before));
        let after = receipt
            .after
            .as_ref()
            .expect("successful update has after revision");
        assert_ne!(after, &before);
        assert_eq!(service.mutation_receipts().unwrap().last(), Some(&receipt));
    }

    #[test]
    fn batch_failure_injection_reports_exact_n_of_m_state() {
        const COUNT: usize = 3;
        for fail_after in 0..=COUNT {
            let root = tempfile::tempdir().unwrap();
            let service = FileService::default();
            service.set_batch_fail_after_commits(fail_after);
            let paths = (0..COUNT)
                .map(|index| root.path().join(format!("batch-{index}.txt")))
                .collect::<Vec<_>>();
            let entries = paths
                .iter()
                .enumerate()
                .map(|(index, path)| {
                    MutationBatchEntry::write(
                        MutationIntent::create(path),
                        format!("agent-{index}").into_bytes(),
                    )
                })
                .collect::<Vec<_>>();

            let result = service.write_batch_with_receipts(root.path(), entries);
            let receipt = if fail_after == COUNT {
                result.expect("N=M boundary must report complete")
            } else {
                result
                    .expect_err("injected N-of-M boundary must not report success")
                    .receipt
            };

            assert_eq!(receipt.complete, fail_after == COUNT);
            assert_eq!(receipt.entries.len(), COUNT);
            for (index, entry) in receipt.entries.iter().enumerate() {
                let should_exist = index < fail_after || fail_after == COUNT;
                assert_eq!(
                    entry.outcome == BatchEntryOutcome::Committed,
                    should_exist,
                    "unexpected entry outcome at fail_after={fail_after}, index={index}: {entry:?}"
                );
                assert_eq!(paths[index].exists(), should_exist);
                if should_exist {
                    assert_eq!(
                        std::fs::read_to_string(&paths[index]).unwrap(),
                        format!("agent-{index}")
                    );
                }
            }
            assert_eq!(receipt.final_state.len(), COUNT);
            for state in &receipt.final_state {
                let index = paths.iter().position(|path| path == &state.path).unwrap();
                let should_exist = index < fail_after || fail_after == COUNT;
                assert_eq!(
                    matches!(state.state, Some(ExpectedPreimage::Revision(_))),
                    should_exist,
                    "final-state receipt disagrees with filesystem: {state:?}"
                );
                assert!(state.observation_error.is_none());
            }
            assert!(!receipt.compensation_attempted);

            let journal = root
                .path()
                .join(".rightkit")
                .join("mutation-journal")
                .join(format!("{}.json", receipt.journal_id));
            let journal: Value = serde_json::from_slice(&std::fs::read(journal).unwrap()).unwrap();
            assert_eq!(
                journal["phase"],
                if fail_after == COUNT {
                    "complete"
                } else {
                    "partial"
                }
            );
            assert_eq!(journal["intents"].as_array().unwrap().len(), COUNT);
        }
    }

    #[tokio::test]
    async fn file_service_typed_lifecycle_round_trip() {
        let root = tempfile::tempdir().unwrap();
        let ctx = ctx(root.path());
        let tool = FileLifecycleTool;
        let mkdir = tool
            .execute(json!({"operation":"mkdir", "path":"dir"}), &ctx)
            .await;
        assert!(!mkdir.is_error, "{}", mkdir.content);
        std::fs::write(root.path().join("dir/source.txt"), "one").unwrap();

        let copy = tool
            .execute(
                json!({"operation":"copy", "path":"dir/source.txt", "destination":"copy.txt"}),
                &ctx,
            )
            .await;
        assert!(!copy.is_error, "{}", copy.content);
        assert_eq!(
            std::fs::read_to_string(root.path().join("copy.txt")).unwrap(),
            "one"
        );

        let moved = tool
            .execute(
                json!({"operation":"move", "path":"copy.txt", "destination":"moved.txt"}),
                &ctx,
            )
            .await;
        assert!(!moved.is_error, "{}", moved.content);
        assert!(!root.path().join("copy.txt").exists());

        let stat = tool
            .execute(json!({"operation":"stat", "path":"moved.txt"}), &ctx)
            .await;
        assert!(!stat.is_error, "{}", stat.content);
        assert_eq!(
            serde_json::from_str::<Value>(&stat.content).unwrap()["kind"],
            "file"
        );

        let trashed = tool
            .execute(json!({"operation":"trash", "path":"moved.txt"}), &ctx)
            .await;
        assert!(!trashed.is_error, "{}", trashed.content);
        assert!(!root.path().join("moved.txt").exists());
    }

    #[tokio::test]
    async fn file_service_overwrite_requires_fresh_complete_receipt() {
        let root = tempfile::tempdir().unwrap();
        let ctx = ctx(root.path());
        std::fs::write(root.path().join("source.txt"), "new").unwrap();
        let destination = root.path().join("destination.txt");
        std::fs::write(&destination, "old").unwrap();
        let args = json!({
            "operation":"copy",
            "path":"source.txt",
            "destination":"destination.txt",
            "overwrite":true
        });
        let refused = FileLifecycleTool.execute(args.clone(), &ctx).await;
        assert!(refused.is_error);
        assert!(refused.content.contains("read the complete current file"));
        ctx.file_service.record_read(&destination, true).unwrap();
        let copied = FileLifecycleTool.execute(args, &ctx).await;
        assert!(!copied.is_error, "{}", copied.content);
        assert_eq!(std::fs::read_to_string(destination).unwrap(), "new");
    }

    #[tokio::test]
    async fn lifecycle_batch_surfaces_exact_partial_receipt_after_injected_boundary() {
        let root = tempfile::tempdir().unwrap();
        let ctx = ctx(root.path());
        ctx.file_service.set_batch_fail_after_commits(1);

        let result = FileLifecycleTool
            .execute(
                json!({
                    "operation":"batch",
                    "operations":[
                        {"operation":"mkdir", "path":"first"},
                        {"operation":"mkdir", "path":"second"},
                        {"operation":"mkdir", "path":"third"}
                    ]
                }),
                &ctx,
            )
            .await;

        assert!(result.is_error, "injected partial batch must be an error");
        assert!(root.path().join("first").is_dir());
        assert!(!root.path().join("second").exists());
        assert!(!root.path().join("third").exists());
        assert_eq!(result.mutations.len(), 1);
        assert_eq!(result.mutations[0].path, root.path().join("first"));
        assert!(result.content.contains("Exact mutation receipt:"));
        assert!(result
            .content
            .contains("injected batch failure after 1 of 3 commit(s)"));
    }

    #[tokio::test]
    async fn file_service_batch_preflight_prevents_partial_mutation() {
        let root = tempfile::tempdir().unwrap();
        let result = FileLifecycleTool
            .execute(
                json!({
                    "operation":"batch",
                    "operations":[
                        {"operation":"mkdir", "path":"would-have-existed"},
                        {"operation":"copy", "path":"missing", "destination":"out"}
                    ]
                }),
                &ctx(root.path()),
            )
            .await;
        assert!(result.is_error);
        assert!(!root.path().join("would-have-existed").exists());
    }

    /// ENG-19: `stat` flags a binary file instead of silently reporting it
    /// as an ordinary text file.
    #[tokio::test]
    async fn file_service_stat_flags_binary_content() {
        let root = tempfile::tempdir().unwrap();
        let ctx = ctx(root.path());
        std::fs::write(root.path().join("blob.bin"), [0u8, 1, 2, 3, 0, 5]).unwrap();
        std::fs::write(root.path().join("plain.txt"), "hello world\n").unwrap();

        let binary_stat = FileLifecycleTool
            .execute(json!({"operation":"stat", "path":"blob.bin"}), &ctx)
            .await;
        assert!(!binary_stat.is_error, "{}", binary_stat.content);
        assert_eq!(
            serde_json::from_str::<Value>(&binary_stat.content).unwrap()["isBinary"],
            true
        );

        let text_stat = FileLifecycleTool
            .execute(json!({"operation":"stat", "path":"plain.txt"}), &ctx)
            .await;
        assert!(!text_stat.is_error, "{}", text_stat.content);
        assert_eq!(
            serde_json::from_str::<Value>(&text_stat.content).unwrap()["isBinary"],
            false
        );
    }

    /// ENG-19: `stat` flags files above the large-file threshold as
    /// `oversized` so callers know to route to a capped preview.
    #[tokio::test]
    async fn file_service_stat_flags_oversized_files() {
        let root = tempfile::tempdir().unwrap();
        let ctx = ctx(root.path());
        std::fs::write(
            root.path().join("big.txt"),
            vec![b'a'; (LARGE_FILE_BYTES + 1) as usize],
        )
        .unwrap();

        let stat = FileLifecycleTool
            .execute(json!({"operation":"stat", "path":"big.txt"}), &ctx)
            .await;
        assert!(!stat.is_error, "{}", stat.content);
        assert_eq!(
            serde_json::from_str::<Value>(&stat.content).unwrap()["oversized"],
            true
        );
    }

    /// EFF-003/EFF-004 qualification fixture: production ordinary workspace
    /// writers are either FileService itself or an explicitly named privileged/
    /// internal adapter, and ordinary callers must state a typed mutation intent.
    #[test]
    fn ordinary_workspace_writer_inventory_has_no_unowned_direct_writer() {
        fn collect_rs(root: &Path, out: &mut Vec<PathBuf>) {
            for entry in std::fs::read_dir(root).unwrap() {
                let entry = entry.unwrap();
                let path = entry.path();
                if path.is_dir() {
                    collect_rs(&path, out);
                } else if path.extension().and_then(|value| value.to_str()) == Some("rs") {
                    out.push(path);
                }
            }
        }

        let source_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = Vec::new();
        collect_rs(&source_root, &mut files);
        let direct_mutators = [
            "std::fs::write(",
            "std::fs::rename(",
            "std::fs::remove_file(",
            "std::fs::remove_dir(",
            "std::fs::remove_dir_all(",
            "std::fs::create_dir(",
            "std::fs::create_dir_all(",
            "std::fs::copy(",
            "tokio::fs::write(",
            "tokio::fs::rename(",
            "rightkit_fs::atomic_write(",
            "OpenOptions::new(",
            "File::create(",
        ];
        // Exact non-ordinary adapters are intentionally visible here. Adding a new
        // production direct writer requires classifying it rather than silently
        // bypassing FileService. Checkpoint restore remains privileged but is lease-
        // gated by EFF-007 in this lane before qualification readiness.
        let exempt = [
            "artifact_crud.rs",                // internal artifact metadata/history store
            "browser.rs",                      // private browser profile directory
            "config_tool.rs",                  // Host control-plane config store
            "git_lfs_batch_client.rs",         // Git/LFS cache adapter
            "git_read.rs",                     // privileged Git adapter (info/exclude upkeep)
            "git_merge.rs",                    // privileged Git adapter
            "git_write.rs",                    // privileged Git adapter
            "lsp_tool/config_status.rs",       // LSP control-plane config
            "sandbox/windows_appcontainer.rs", // OS sandbox adapter
            "schedule_cron_tool/storage_cron_uuid.rs", // scheduler state store
            "send_message.rs",                 // inter-agent spool
            "shell/result_sandbox.rs",         // full shell output in Host's private session store
            "task_tools/storage_rendering.rs", // task state store
            "team_tools.rs",                   // team coordination state store
            "workspace_checkpoint/mod.rs", // privileged Git checkpoint adapter; exclusive FileService lease required
            "workspace_checkpoint/store.rs", // checkpoint metadata store
            "worktree_pool/db.rs",         // worktree bookkeeping
            "worktree_pool/pool.rs", // creates Host-managed linked worktrees at the owned pool root
            "worktree_pool/sync.rs", // privileged worktree adapter
        ];

        let mut violations = Vec::new();
        for path in files {
            let relative = path
                .strip_prefix(&source_root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            if relative.contains("tests") {
                continue;
            }
            let source = std::fs::read_to_string(&path).unwrap();
            let production_end = ["#[cfg(test)]", "#[cfg(all(test", "#[cfg(any(test"]
                .iter()
                .filter_map(|marker| source.find(marker))
                .min()
                .unwrap_or(source.len());
            let production = &source[..production_end];
            if relative == "workspace_checkpoint/mod.rs"
                && !production.contains("with_exclusive_workspace_restore(")
            {
                violations.push(
                    "workspace_checkpoint/mod.rs: privileged restore bypasses FileService exclusive lease"
                        .to_string(),
                );
                continue;
            }
            if relative != "file_service.rs" && production.contains(".write_with_receipt(") {
                violations.push(format!(
                    "{relative}: compatibility write_with_receipt infers intent; use write_with_receipt_intent"
                ));
                continue;
            }
            if !direct_mutators
                .iter()
                .any(|primitive| production.contains(primitive))
            {
                continue;
            }
            if relative == "file_service.rs" || exempt.contains(&relative.as_str()) {
                continue;
            }
            violations.push(relative);
        }
        assert!(
            violations.is_empty(),
            "ordinary production filesystem writers bypass FileService: {violations:?}"
        );
    }

    /// EFF-003 (widened): the inventory above scans only this crate, which is
    /// why it missed `api_run_changes.rs::apply_undo` performing raw
    /// remove_file/rename/write on the user workspace. Ordinary workspace
    /// mutation is not confined to the tools crate, so the binary crate's
    /// workspace-facing surfaces are inventoried too.
    ///
    /// The binary is overwhelmingly control-plane (daemon state, journals,
    /// scheduler, config, caches), so this deliberately scans only the files
    /// that actually touch the USER workspace rather than exempting the rest
    /// wholesale - an overbroad exemption would hide the next bypass exactly
    /// the way the previous scope did.
    #[test]
    fn binary_crate_workspace_writers_route_through_file_service() {
        let bin_src = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../bins/rightkit-agent/src")
            .canonicalize();
        let Ok(bin_src) = bin_src else {
            // The binary crate is not present in this checkout layout; the
            // tools-crate inventory above still applies.
            return;
        };

        // Files that mutate the user's workspace (as opposed to the host app's
        // own state). Each must go through FileService.
        let workspace_facing = ["api_run_changes.rs"];

        let direct_mutators = [
            "std::fs::write(",
            "std::fs::rename(",
            "std::fs::remove_file(",
            "std::fs::remove_dir_all(",
            "tokio::fs::write(",
            "tokio::fs::rename(",
        ];

        let mut violations = Vec::new();
        for relative in workspace_facing {
            let path = bin_src.join(relative);
            let Ok(source) = std::fs::read_to_string(&path) else {
                continue;
            };
            let production_end = ["#[cfg(test)]", "#[cfg(all(test", "#[cfg(any(test"]
                .iter()
                .filter_map(|marker| source.find(marker))
                .min()
                .unwrap_or(source.len());
            let production = &source[..production_end];
            for primitive in direct_mutators {
                let mut cursor = 0;
                while let Some(found) = production[cursor..].find(primitive) {
                    let at = cursor + found;
                    cursor = at + primitive.len();
                    // Distinguish target, not just primitive: the host app's own
                    // control-plane store (`self.root/<run>.bin`) is not user
                    // workspace mutation. Exempting the whole FILE would hide
                    // the next real bypass, which is how `apply_undo` survived
                    // the previous inventory - so classify per call site.
                    let window_start = at.saturating_sub(160);
                    let window = &production[window_start..cursor];
                    if window.contains("self.root") {
                        continue;
                    }
                    violations.push(format!("{relative}: direct `{primitive}` on the workspace"));
                }
            }
        }
        assert!(
            violations.is_empty(),
            "binary-crate workspace writers must route through FileService: {violations:?}"
        );
    }

    #[test]
    fn exclusive_workspace_restore_blocks_ordinary_edit_and_records_generation() {
        use std::sync::mpsc;
        use std::time::Duration;

        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("shared.txt");
        std::fs::write(&file, "v1").unwrap();
        let restore_service = FileService::new();
        let edit_service = FileService::new();
        let expected = revision_from(&edit_service, &file);
        let (restore_entered_tx, restore_entered_rx) = mpsc::channel();
        let (release_restore_tx, release_restore_rx) = mpsc::channel();
        let restore_root = root.path().to_path_buf();
        let restore_file = file.clone();
        let restore_thread = std::thread::spawn(move || {
            restore_service.with_exclusive_workspace_restore(
                &restore_root,
                WorkspaceRestoreProvenance {
                    checkpoint_id: "checkpoint-1".to_string(),
                    commit_oid: "commit-1".to_string(),
                },
                || {
                    restore_entered_tx.send(()).unwrap();
                    release_restore_rx.recv().unwrap();
                    std::fs::write(&restore_file, "restored")
                        .map_err(|error| format!("test restore write: {error}"))?;
                    Ok(())
                },
            )
        });
        restore_entered_rx
            .recv_timeout(Duration::from_secs(1))
            .unwrap();

        let (edit_started_tx, edit_started_rx) = mpsc::channel();
        let (edit_done_tx, edit_done_rx) = mpsc::channel();
        let edit_file = file.clone();
        let edit_thread = std::thread::spawn(move || {
            edit_started_tx.send(()).unwrap();
            let result = edit_service
                .apply_byte_intent(
                    MutationIntent::update(&edit_file, expected),
                    b"ordinary-edit",
                )
                .map(|_| ());
            edit_done_tx.send(result).unwrap();
        });
        edit_started_rx
            .recv_timeout(Duration::from_secs(1))
            .unwrap();
        assert!(
            edit_done_rx
                .recv_timeout(Duration::from_millis(100))
                .is_err(),
            "ordinary edit completed while exclusive restore lease was held"
        );

        release_restore_tx.send(()).unwrap();
        let (_, receipt) = restore_thread.join().unwrap().unwrap();
        let edit_result = edit_done_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        edit_thread.join().unwrap();

        assert!(matches!(
            edit_result,
            Err(MutationError::Conflict(MutationConflict {
                intent: MutationIntentKind::Update,
                ..
            }))
        ));
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "restored");
        assert!(receipt.succeeded);
        assert_eq!(receipt.provenance.checkpoint_id, "checkpoint-1");
        assert_eq!(receipt.provenance.commit_oid, "commit-1");
        assert_eq!(receipt.after.generation, receipt.before.generation + 1);
        assert_ne!(receipt.before.revision, receipt.after.revision);
        assert!(receipt.before.observation_error.is_none());
        assert!(receipt.after.observation_error.is_none());
    }

    #[cfg(unix)]
    #[test]
    fn write_with_receipt_preserves_unix_mode() {
        use std::os::unix::fs::PermissionsExt;

        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("script.sh");
        std::fs::write(&file, "#!/bin/sh\necho old\n").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o750)).unwrap();

        let service = FileService::default();
        service.record_read(&file, true).unwrap();
        service
            .write_with_receipt(&file, b"#!/bin/sh\necho new\n", false)
            .unwrap();

        let mode = std::fs::metadata(&file).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o750);
    }

    #[test]
    fn copy_tree_copies_regular_files_independently() {
        let root = tempfile::tempdir().unwrap();
        let src = root.path().join("src");
        std::fs::create_dir(&src).unwrap();
        std::fs::write(src.join("a.txt"), "alpha").unwrap();
        let dst = root.path().join("dst");
        copy_tree(&src, &dst).unwrap();
        assert_eq!(std::fs::read_to_string(dst.join("a.txt")).unwrap(), "alpha");
        std::fs::write(dst.join("a.txt"), "changed").unwrap();
        assert_eq!(std::fs::read_to_string(src.join("a.txt")).unwrap(), "alpha");
    }

    #[cfg(unix)]
    #[test]
    fn copy_tree_refuses_a_symlink_entry_without_following_it() {
        use std::os::unix::fs::symlink;
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret"), "secret").unwrap();
        let src = root.path().join("src");
        std::fs::create_dir(&src).unwrap();
        symlink(outside.path().join("secret"), src.join("link")).unwrap();
        let dst = root.path().join("dst");
        assert!(copy_tree(&src, &dst).is_err());
        assert!(!dst.join("link").exists());
        // A symlink given directly as the source is refused by the owner copy too.
        assert!(copy_tree(&src.join("link"), &root.path().join("direct")).is_err());
        assert!(!root.path().join("direct").exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn file_service_rejects_symlink_mutation_and_escape() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret"), "secret").unwrap();
        symlink(outside.path().join("secret"), root.path().join("link")).unwrap();
        let result = FileLifecycleTool
            .execute(
                json!({"operation":"copy", "path":"link", "destination":"copy"}),
                &ctx(root.path()),
            )
            .await;
        assert!(result.is_error);
        assert!(result.content.contains("symlink") || result.content.contains("escapes"));
        assert!(!root.path().join("copy").exists());
    }
}

#[cfg(test)]
mod eff006_crash_honesty_tests {
    use super::*;

    fn workspace() -> tempfile::TempDir {
        tempfile::TempDir::new().unwrap()
    }

    /// EFF-006: a journal left at `prepared` must never be read as proof that
    /// zero mutations executed. Recovery inspects the real filesystem and
    /// reports which journaled paths actually changed.
    #[test]
    fn prepared_journal_with_committed_mutations_is_reported_as_partial() {
        let dir = workspace();
        let root = dir.path();
        let applied = root.join("applied.txt");
        let untouched = root.join("untouched.txt");
        std::fs::write(&applied, "before\n").unwrap();
        std::fs::write(&untouched, "before\n").unwrap();

        // Pre-images as they were when the batch was prepared.
        let applied_revision = revision(&applied).unwrap();
        let untouched_revision = revision(&untouched).unwrap();

        // Hand-write the durable journal exactly as `persist_batch_journal`
        // leaves it: phase `prepared`, no receipt, because the process died
        // before settlement.
        let journal_dir = root.join(".rightkit").join("mutation-journal");
        std::fs::create_dir_all(&journal_dir).unwrap();
        let payload = json!({
            "schema": 1,
            "journalId": "batch-1",
            "phase": "prepared",
            "intents": [
                {
                    "index": 0,
                    "intent": MutationIntent::replace(&applied, applied_revision),
                    "payloadBytes": 5,
                },
                {
                    "index": 1,
                    "intent": MutationIntent::replace(&untouched, untouched_revision),
                    "payloadBytes": 5,
                }
            ],
            "receipt": Value::Null,
        });
        std::fs::write(
            journal_dir.join("batch-1.json"),
            serde_json::to_vec_pretty(&payload).unwrap(),
        )
        .unwrap();

        // The crash happened AFTER the first mutation committed.
        std::fs::write(&applied, "after\n").unwrap();

        let reconciliations = recover_mutation_journals(root).unwrap();
        assert_eq!(reconciliations.len(), 1);
        let report = &reconciliations[0];
        assert_eq!(report.journal_id, "batch-1");
        assert_eq!(report.phase, "prepared");

        // The decisive assertion: `prepared` did NOT mean nothing happened.
        assert!(
            report.observed_partial_application(),
            "recovery must surface that the batch was partially applied"
        );
        assert_eq!(
            report.entries[0].state,
            JournalEntryState::ChangedSincePrepare,
            "the committed mutation must be reported as changed"
        );
        assert_eq!(
            report.entries[1].state,
            JournalEntryState::NotApplied,
            "the uncommitted mutation must be reported as not applied"
        );
    }

    /// A prepared batch where the process died before ANY commit reports every
    /// entry as not applied - so the honest signal is not just "always partial".
    #[test]
    fn prepared_journal_with_no_commits_reports_nothing_applied() {
        let dir = workspace();
        let root = dir.path();
        let first = root.join("a.txt");
        let second = root.join("b.txt");
        std::fs::write(&first, "before\n").unwrap();
        std::fs::write(&second, "before\n").unwrap();

        let journal_dir = root.join(".rightkit").join("mutation-journal");
        std::fs::create_dir_all(&journal_dir).unwrap();
        let payload = json!({
            "schema": 1,
            "journalId": "batch-2",
            "phase": "prepared",
            "intents": [
                {"index": 0, "intent": MutationIntent::replace(&first, revision(&first).unwrap())},
                {"index": 1, "intent": MutationIntent::replace(&second, revision(&second).unwrap())},
            ],
            "receipt": Value::Null,
        });
        std::fs::write(
            journal_dir.join("batch-2.json"),
            serde_json::to_vec_pretty(&payload).unwrap(),
        )
        .unwrap();

        let report = &recover_mutation_journals(root).unwrap()[0];
        assert!(!report.observed_partial_application());
        assert!(report
            .entries
            .iter()
            .all(|entry| entry.state == JournalEntryState::NotApplied));
    }

    /// Settled journals are not reconciled - they already carry an exact receipt.
    #[test]
    fn settled_journals_are_not_reconstructed_from_the_filesystem() {
        let dir = workspace();
        let root = dir.path();
        let journal_dir = root.join(".rightkit").join("mutation-journal");
        std::fs::create_dir_all(&journal_dir).unwrap();
        for (id, phase) in [("batch-c", "complete"), ("batch-p", "partial")] {
            std::fs::write(
                journal_dir.join(format!("{id}.json")),
                serde_json::to_vec_pretty(&json!({
                    "schema": 1,
                    "journalId": id,
                    "phase": phase,
                    "intents": [],
                    "receipt": Value::Null,
                }))
                .unwrap(),
            )
            .unwrap();
        }
        assert!(recover_mutation_journals(root).unwrap().is_empty());
    }

    /// A workspace that never ran a batch has nothing to reconcile.
    #[test]
    fn absent_journal_directory_is_not_an_error() {
        let dir = workspace();
        assert!(recover_mutation_journals(dir.path()).unwrap().is_empty());
    }
}
