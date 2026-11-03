//! Recoverable context folding for completed sub-trajectories.

use crate::transcript::Transcript;
use rightkit_agent_engine_protocol::TranscriptMessage;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

pub const MAX_FOLD_MESSAGES: usize = 512;
pub const MAX_FOLD_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_SESSION_ARCHIVES: usize = 64;
pub const MAX_SESSION_ARCHIVE_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum ContextFoldError {
    #[error("context folding is disabled for this session")]
    Disabled,
    #[error("context fold markers were not found in order")]
    MarkersNotFound,
    #[error("context fold crosses the stable prompt prefix")]
    StablePrefix,
    #[error("context fold contains too many messages")]
    TooManyMessages,
    #[error("context fold archive exceeds the byte limit")]
    ArchiveTooLarge,
    #[error("context fold archive capacity is exhausted")]
    ArchiveCapacity,
    #[error("invalid context fold archive reference")]
    InvalidReference,
    #[error("context fold archive is unavailable")]
    ArchiveUnavailable,
    #[error("context fold archive I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("context fold serialization failed: {0}")]
    Serialization(#[from] serde_json::Error),
}

#[derive(Debug, Clone)]
pub struct FoldedTrajectory {
    pub ref_id: String,
    pub summary: String,
    pub dropped_count: usize,
}

#[derive(Debug)]
pub struct FoldStore {
    full: HashMap<String, String>,
    disk_dir: Option<PathBuf>,
}

impl Default for FoldStore {
    fn default() -> Self {
        Self::new()
    }
}

impl FoldStore {
    pub fn new() -> Self {
        Self {
            full: HashMap::new(),
            disk_dir: None,
        }
    }

    pub fn with_dir(mut self, dir: PathBuf) -> Self {
        self.disk_dir = Some(dir);
        self
    }

    pub fn archive(
        &mut self,
        messages: &[TranscriptMessage],
        summary: String,
    ) -> Result<FoldedTrajectory, ContextFoldError> {
        if messages.len() > MAX_FOLD_MESSAGES {
            return Err(ContextFoldError::TooManyMessages);
        }
        let serialized = serde_json::to_string(messages)?;
        if serialized.len() > MAX_FOLD_BYTES {
            return Err(ContextFoldError::ArchiveTooLarge);
        }
        let ref_id = content_hash(&serialized);
        if !self.full.contains_key(&ref_id) && self.full.len() >= MAX_SESSION_ARCHIVES {
            return Err(ContextFoldError::ArchiveCapacity);
        }

        if let Some(dir) = &self.disk_dir {
            write_to_disk(dir, &ref_id, &serialized)?;
        }
        self.full.insert(ref_id.clone(), serialized);

        Ok(FoldedTrajectory {
            ref_id,
            summary,
            dropped_count: messages.len(),
        })
    }

    pub fn get(&self, ref_id: &str) -> Result<Vec<TranscriptMessage>, ContextFoldError> {
        validate_ref(ref_id)?;
        let serialized = if let Some(content) = self.full.get(ref_id) {
            content.clone()
        } else if let Some(dir) = &self.disk_dir {
            read_from_disk(dir, ref_id)?
        } else {
            return Err(ContextFoldError::ArchiveUnavailable);
        };
        if serialized.len() > MAX_FOLD_BYTES {
            return Err(ContextFoldError::ArchiveTooLarge);
        }
        let messages: Vec<TranscriptMessage> = serde_json::from_str(&serialized)?;
        if messages.len() > MAX_FOLD_MESSAGES {
            return Err(ContextFoldError::TooManyMessages);
        }
        Ok(messages)
    }

    pub fn len(&self) -> usize {
        self.full.len()
    }

    pub fn is_empty(&self) -> bool {
        self.full.is_empty()
    }
}

pub fn read_folded_trajectory(
    dir: &Path,
    ref_id: &str,
) -> Result<Vec<TranscriptMessage>, ContextFoldError> {
    validate_ref(ref_id)?;
    let serialized = read_from_disk(dir, ref_id)?;
    if serialized.len() > MAX_FOLD_BYTES {
        return Err(ContextFoldError::ArchiveTooLarge);
    }
    let messages: Vec<TranscriptMessage> = serde_json::from_str(&serialized)?;
    if messages.len() > MAX_FOLD_MESSAGES {
        return Err(ContextFoldError::TooManyMessages);
    }
    Ok(messages)
}

pub fn session_archive_dir(base: &Path, session_id: &str) -> PathBuf {
    base.join(rightkit_fs::digest::sha256_bytes(session_id.as_bytes()))
}

fn content_hash(s: &str) -> String {
    rightkit_fs::digest::sha256_bytes(s.as_bytes())
}

fn validate_ref(ref_id: &str) -> Result<(), ContextFoldError> {
    if ref_id.len() == 64
        && ref_id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        Ok(())
    } else {
        Err(ContextFoldError::InvalidReference)
    }
}

fn ensure_safe_dir(dir: &Path) -> Result<(), ContextFoldError> {
    if let Ok(metadata) = fs::symlink_metadata(dir) {
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(ContextFoldError::ArchiveUnavailable);
        }
    } else {
        fs::create_dir_all(dir)?;
    }
    Ok(())
}

fn archive_usage(dir: &Path) -> Result<(usize, u64), ContextFoldError> {
    let mut count = 0usize;
    let mut bytes = 0u64;
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let metadata = fs::symlink_metadata(entry.path())?;
        if metadata.file_type().is_symlink() {
            return Err(ContextFoldError::ArchiveUnavailable);
        }
        if metadata.is_file()
            && entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.len() == 69 && name.ends_with(".json"))
        {
            count = count.saturating_add(1);
            bytes = bytes.saturating_add(metadata.len());
        }
    }
    Ok((count, bytes))
}

fn write_to_disk(dir: &Path, ref_id: &str, content: &str) -> Result<(), ContextFoldError> {
    validate_ref(ref_id)?;
    ensure_safe_dir(dir)?;
    let final_path = dir.join(format!("{ref_id}.json"));
    if final_path.exists() {
        let metadata = fs::symlink_metadata(&final_path)?;
        if metadata.file_type().is_symlink()
            || !metadata.is_file()
            || metadata.len() > MAX_FOLD_BYTES as u64
            || fs::read_to_string(&final_path)? != content
        {
            return Err(ContextFoldError::ArchiveUnavailable);
        }
        return Ok(());
    }
    let (count, bytes) = archive_usage(dir)?;
    if count >= MAX_SESSION_ARCHIVES
        || bytes.saturating_add(content.len() as u64) > MAX_SESSION_ARCHIVE_BYTES
    {
        return Err(ContextFoldError::ArchiveCapacity);
    }

    rightkit_fs::atomic_write(&final_path, content.as_bytes())
        .map_err(|error| ContextFoldError::Io(error.into()))
}

fn read_from_disk(dir: &Path, ref_id: &str) -> Result<String, ContextFoldError> {
    validate_ref(ref_id)?;
    ensure_safe_dir(dir)?;
    let path = dir.join(format!("{ref_id}.json"));
    let metadata = fs::symlink_metadata(&path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            ContextFoldError::ArchiveUnavailable
        } else {
            ContextFoldError::Io(error)
        }
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(ContextFoldError::ArchiveUnavailable);
    }
    if metadata.len() > MAX_FOLD_BYTES as u64 {
        return Err(ContextFoldError::ArchiveTooLarge);
    }
    Ok(fs::read_to_string(path)?)
}

pub fn fold_subtrajectory(
    transcript: &mut Transcript,
    store: &mut FoldStore,
    subtask_id: &str,
    start_marker: &str,
    end_marker: &str,
    summary: String,
    stable_prefix_len: usize,
) -> Result<FoldedTrajectory, ContextFoldError> {
    let messages = transcript.messages();
    let start = messages
        .iter()
        .position(|message| message_contains(message, start_marker))
        .ok_or(ContextFoldError::MarkersNotFound)?;
    let end = messages
        .iter()
        .enumerate()
        .skip(start + 1)
        .find_map(|(index, message)| message_contains(message, end_marker).then_some(index))
        .ok_or(ContextFoldError::MarkersNotFound)?;
    if start < stable_prefix_len {
        return Err(ContextFoldError::StablePrefix);
    }

    let sub_trajectory = messages[start..end].to_vec();
    let folded = store.archive(&sub_trajectory, summary)?;
    let mut new_messages = Vec::with_capacity(messages.len() - (end - start) + 1);
    new_messages.extend_from_slice(&messages[..start]);
    new_messages.push(TranscriptMessage::Fold {
        id: format!("fold-{subtask_id}"),
        created_at: rightkit_agent_engine_protocol::now_iso(),
        subtask_id: subtask_id.to_string(),
        summary: folded.summary.clone(),
        dropped_count: folded.dropped_count as i32,
        archive_ref: folded.ref_id.clone(),
        pinned: false,
    });
    new_messages.extend_from_slice(&messages[end..]);
    transcript.replace(new_messages);
    Ok(folded)
}

fn message_contains(message: &TranscriptMessage, marker: &str) -> bool {
    match message {
        TranscriptMessage::User { content, .. } | TranscriptMessage::System { content, .. } => {
            content.contains(marker)
        }
        _ => false,
    }
}
