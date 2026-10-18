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

/// Opaque, stable dedupe key for one persisted report file. The report bytes
/// are already durable before any enqueue is attempted (the hook writes the
/// file first), and the key is a pure function of those bytes, so every retry
/// — after a crash, an upload ack, or an archive failure — derives the same
/// key and hits the outbox's durable receipt instead of queueing a duplicate.
/// FNV-1a is a stability choice here, not a security one: the key only has to
/// be identical for identical bytes and distinct for distinct reports (whose
/// bytes embed time and pid).
fn stable_report_key(report_bytes: &[u8]) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in report_bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("crash-{:x}-{hash:016x}", report_bytes.len())
}

/// Call at launch. Hands each report to the consented outbox (which redacts
/// again) under its stable dedupe key, then files it under `submitted/`. The
/// outbox receipt is retired only after that archive rename succeeds; an
/// archive failure stays visible in `failed`, leaves the source untouched,
/// and a later retry returns the original acceptance — one queued event per
/// report, never two. This proves an idempotent local handoff, not
/// exactly-once remote delivery. Native dumps are never queued.
pub fn enqueue_pending(crash_dir: &Path, outbox: &Outbox) -> EnqueueSummary {
    let mut summary = EnqueueSummary::default();
    // One stable directory-wide sidecar fences discovery/read through archive
    // and receipt retirement. A waiter must rediscover sources after acquiring
    // it, so it cannot enqueue bytes read before another worker retired them.
    if !crash_dir.exists() {
        return summary;
    }
    let guard = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(crash_dir.join(".pending.lock"))
        .and_then(|file| {
            file.lock()?;
            Ok(file)
        });
    let _guard = match guard {
        Ok(guard) => guard,
        Err(_) => {
            summary.failed += 1;
            return summary;
        }
    };
    for path in list_pending(crash_dir) {
        if !outbox.consent().is_granted() {
            summary.held_no_consent += 1;
            continue;
        }
        let Some((bytes, report)) = std::fs::read(&path).ok().and_then(|bytes| {
            serde_json::from_slice::<CrashReport>(&bytes)
                .ok()
                .map(|report| (bytes, report))
        }) else {
            summary.unreadable += 1;
            continue;
        };
        let key = stable_report_key(&bytes);
        match outbox.enqueue_deduped(&key, CRASH_EVENT_TYPE, report.to_scrubbed_value()) {
            Ok(_) => {
                let submitted = crash_dir.join(SUBMITTED_DIR);
                let archived = std::fs::create_dir_all(&submitted).is_ok()
                    && std::fs::rename(&path, submitted.join(path.file_name().unwrap_or_default()))
                        .is_ok();
                if archived {
                    summary.queued += 1;
                    // Retire only now: before this point the receipt is what
                    // stops a retry from queueing the report twice. A failed
                    // retire is harmless — the source is already archived.
                    let _ = outbox.retire_dedupe(&key);
                } else {
                    summary.failed += 1;
                }
            }
            Err(EnqueueError::NoConsent) => summary.held_no_consent += 1,
            Err(_) => summary.failed += 1,
        }
    }
    summary
}

#[cfg(test)]
mod tests {
    use super::stable_report_key;

    #[test]
    fn report_keys_are_stable_and_distinct() {
        assert_eq!(stable_report_key(b"{}"), stable_report_key(b"{}"));
        assert_ne!(stable_report_key(b"{}"), stable_report_key(b"{ }"));
        assert_ne!(stable_report_key(b"a"), stable_report_key(b"b"));
    }

    #[test]
    fn waiting_handoff_rediscovers_after_source_archive_and_retirement() {
        let dir = std::env::temp_dir().join(format!(
            "rightkit-pending-waiter-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let report = crate::CrashReport {
            schema_version: 1,
            kind: "panic".into(),
            build: None,
            os: "test".into(),
            arch: "test".into(),
            app_name: "test".into(),
            app_version: "1".into(),
            unix_time: 1,
            pid: 1,
            thread: "main".into(),
            location: None,
            message: "boom".into(),
            backtrace: String::new(),
            native_artifacts: Vec::new(),
            native_capture_note: None,
            process_kind: None,
        };
        let bytes = serde_json::to_vec(&report).unwrap();
        let source = dir.join("crash_1.json");
        std::fs::write(&source, &bytes).unwrap();
        let consent = rightkit_logs::ConsentStore::new(dir.join("consent.json"));
        consent.grant("v1").unwrap();
        let outbox = rightkit_logs::Outbox::open(
            dir.join("outbox.json"),
            consent.clone(),
            rightkit_logs::OutboxConfig::default(),
        );
        let key = stable_report_key(&bytes);
        let id = outbox
            .enqueue_deduped(&key, super::CRASH_EVENT_TYPE, report.to_scrubbed_value())
            .unwrap();
        outbox.acknowledge(&[id]).unwrap();
        let guard = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(dir.join(".pending.lock"))
            .unwrap();
        guard.lock().unwrap();
        let waiter_dir = dir.clone();
        let waiter_outbox = rightkit_logs::Outbox::open(
            outbox.path(),
            consent,
            rightkit_logs::OutboxConfig::default(),
        );
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let waiter = std::thread::spawn(move || {
            started_tx.send(()).unwrap();
            done_tx
                .send(super::enqueue_pending(&waiter_dir, &waiter_outbox))
                .unwrap();
        });
        started_rx.recv().unwrap();
        assert!(matches!(
            done_rx.recv_timeout(std::time::Duration::from_millis(100)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        ));

        // Complete an earlier holder's handoff while the next holder waits.
        std::fs::create_dir(dir.join("submitted")).unwrap();
        std::fs::rename(&source, dir.join("submitted/crash_1.json")).unwrap();
        outbox.retire_dedupe(&key).unwrap();
        drop(guard);
        assert_eq!(
            done_rx
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap(),
            super::EnqueueSummary::default()
        );
        waiter.join().unwrap();
        let reopened = rightkit_logs::Outbox::open(
            outbox.path(),
            outbox.consent().clone(),
            rightkit_logs::OutboxConfig::default(),
        );
        assert!(reopened.is_empty());
    }
}
