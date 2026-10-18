//! A failed ledger reload must abort the mutation. A second `Outbox` instance on the same ledger
//! holds a stale cache; if its read fails and its write still succeeds, persisting that cache
//! would erase an event and a dedupe receipt another instance already accepted.

use rightkit_logs::{ConsentStore, Outbox, OutboxConfig};
use serde_json::json;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

fn temp_dir(label: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let path = std::env::temp_dir().join(format!(
        "rightkit-logs-stale-reload-{label}-{}-{nonce}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&path);
    fs::create_dir_all(&path).expect("create stale-reload fixture directory");
    path
}

fn open(dir: &Path) -> Outbox {
    let consent = ConsentStore::new(dir.join("consent.json"));
    Outbox::open(dir.join("outbox.json"), consent, OutboxConfig::default())
}

/// Makes reads of `path` fail while a rename-over replacement can still succeed. Unix: mode 000.
/// Windows: a read handle that shares write and delete but not read. `new` returns `None` when
/// the host or account (for example root) does not enforce it, so the test skips.
struct ReadBlock {
    #[cfg(unix)]
    path: PathBuf,
    #[cfg(windows)]
    _handle: fs::File,
}

impl ReadBlock {
    #[cfg(unix)]
    fn new(path: &Path) -> Option<Self> {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o000)).ok()?;
        let block = Self {
            path: path.to_path_buf(),
        };
        if fs::read(path).is_ok() {
            // Privileged account: the block does not apply (Drop restores the mode).
            return None;
        }
        Some(block)
    }

    #[cfg(windows)]
    fn new(path: &Path) -> Option<Self> {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_SHARE_WRITE: u32 = 0x2;
        const FILE_SHARE_DELETE: u32 = 0x4;
        let handle = fs::OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .open(path)
            .ok()?;
        if fs::read(path).is_ok() {
            return None;
        }
        Some(Self { _handle: handle })
    }
}

#[cfg(unix)]
impl Drop for ReadBlock {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&self.path, fs::Permissions::from_mode(0o600));
    }
}

#[test]
fn failed_reload_aborts_every_mutation_and_keeps_the_newer_ledger() {
    let dir = temp_dir("abort");
    let ledger = dir.join("outbox.json");

    let stale = open(&dir);
    let fresh = open(&dir);
    fresh.consent().grant("2026-10").expect("grant consent");
    assert!(
        stale.is_empty(),
        "the stale instance cached an empty ledger"
    );

    let id = fresh
        .enqueue_deduped("report-1", "crash", json!({"n": 1}))
        .expect("fresh instance accepts the event and its receipt");
    assert!(ledger.exists());
    assert!(stale.is_empty(), "the stale cache has not seen the event");

    let Some(block) = ReadBlock::new(&ledger) else {
        eprintln!("skipping: cannot make the ledger unreadable on this host");
        return;
    };

    assert!(
        stale.enqueue("usage", json!({"n": 2})).is_err(),
        "enqueue must fail when the reload fails"
    );
    assert!(
        stale.acknowledge(&[id]).is_err(),
        "acknowledge must fail when the reload fails"
    );
    assert!(
        stale.record_failure(&[id], 123).is_err(),
        "record_failure must fail when the reload fails"
    );
    assert!(
        stale.retire_dedupe("report-1").is_err(),
        "retire_dedupe must fail when the reload fails"
    );
    assert!(
        stale.status().error.is_some(),
        "the failed reload is visible in status"
    );
    drop(block);

    // The newer ledger, its event and its dedupe receipt are intact.
    let check = open(&dir);
    assert_eq!(check.len(), 1, "the accepted event survived");
    assert_eq!(
        check
            .enqueue_deduped("report-1", "crash", json!({"n": 99}))
            .expect("repeat of the accepted key"),
        id,
        "the dedupe receipt survived: the repeat returns the original id"
    );
    assert_eq!(check.len(), 1, "the repeat stored nothing");

    // Once reads work again the stale instance reloads, sees the newer state, and mutates it.
    let second = stale
        .enqueue("usage", json!({"n": 3}))
        .expect("enqueue after the read recovers");
    assert_ne!(second, id);
    assert_eq!(
        stale.len(),
        2,
        "reload merged the newer event with the new one"
    );
    assert!(
        stale.status().error.is_none(),
        "a good reload clears the error"
    );
    assert_eq!(open(&dir).len(), 2);
    let _ = fs::remove_dir_all(&dir);
}
