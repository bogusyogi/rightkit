use crate::CrashReport;
use rightkit_logs::{EnqueueError, Outbox};
use std::path::{Path, PathBuf};

pub const CRASH_EVENT_TYPE: &str = "crash_report";
const SUBMITTED_DIR: &str = "submitted";

#[derive(Debug, Default, PartialEq, Eq)]
pub struct EnqueueSummary {
    pub queued: usize,
    /// Consent absent: reports stay on disk, untouched, and are never sent.
    pub held_no_consent: usize,
    pub unreadable: usize,
    pub failed: usize,
}

/// Finished, not-yet-queued reports (`crash_*.json`), oldest first.
pub fn list_pending(crash_dir: &Path) -> Vec<PathBuf> {
    let mut found: Vec<(std::time::SystemTime, PathBuf)> = std::fs::read_dir(crash_dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            name.starts_with("crash_") && name.ends_with(".json")
        })
        .filter_map(|entry| {
            let modified = entry.metadata().ok()?.modified().ok()?;
            Some((modified, entry.path()))
        })
        .collect();
    found.sort();
    found.into_iter().map(|(_, path)| path).collect()
}

/// Call at launch. Moves each report into the consented outbox (which redacts
/// again) and then files it under `submitted/` so it is queued exactly once.
/// Native dumps are never queued.
pub fn enqueue_pending(crash_dir: &Path, outbox: &Outbox) -> EnqueueSummary {
    let mut summary = EnqueueSummary::default();
    for path in list_pending(crash_dir) {
        let Some(report) = std::fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<CrashReport>(&bytes).ok())
        else {
            summary.unreadable += 1;
            continue;
        };
        match outbox.enqueue(CRASH_EVENT_TYPE, report.to_scrubbed_value()) {
            Ok(_) => {
                summary.queued += 1;
                let submitted = crash_dir.join(SUBMITTED_DIR);
                if std::fs::create_dir_all(&submitted).is_ok() {
                    let _ = std::fs::rename(&path, submitted.join(path.file_name().unwrap_or_default()));
                } else {
                    // Could not mark as queued: remove rather than risk a duplicate upload.
                    let _ = std::fs::remove_file(&path);
                }
            }
            Err(EnqueueError::NoConsent) => summary.held_no_consent += 1,
            Err(_) => summary.failed += 1,
        }
    }
    summary
}
