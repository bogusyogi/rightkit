use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Bounded retention for the crash directory (HeardRight defaults): newest N
/// native dumps, newest M reports, plus an overall size cap, oldest first.
#[derive(Debug, Clone, Copy)]
pub struct Retention {
    pub max_native_dumps: usize,
    pub max_reports: usize,
    pub max_dir_bytes: u64,
}

impl Default for Retention {
    fn default() -> Self {
        Self {
            max_native_dumps: 5,
            max_reports: 10,
            max_dir_bytes: 50 * 1024 * 1024,
        }
    }
}

type Entry = (PathBuf, SystemTime, u64);

/// Best-effort; never fails. Runs after every capture so a directory built up
/// under looser retention by an older binary is repaired too.
pub fn prune(dir: &Path, retention: Retention) {
    sweep_raw(dir);
    let mut native = list(dir, &["dump_", "sample_"]);
    let mut reports = list(dir, &["crash_"]);
    drop_oldest(&mut native, retention.max_native_dumps);
    drop_oldest(&mut reports, retention.max_reports);

    let mut all = list(dir, &["dump_", "sample_", "crash_"]);
    all.sort_by_key(|(_, modified, _)| *modified);
    let mut total: u64 = all.iter().map(|(_, _, size)| *size).sum();
    for (path, _, size) in &all {
        if total <= retention.max_dir_bytes {
            break;
        }
        if std::fs::remove_file(path).is_ok() {
            total = total.saturating_sub(*size);
        }
    }
}

/// Removes unscrubbed raw samples older than a few minutes: leftovers `.sample_raw_*.raw` that
/// releases up to 0.3.0 wrote in `dir`, and stale raw directories in the OS temp directory. A
/// capture deletes its own raw file before it returns, so anything older is a leftover from a crash or a
/// failed delete. Best-effort; the name is invisible to every other listing of the folder.
pub(crate) fn sweep_raw(dir: &Path) {
    crate::native::sweep_temp_raw();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if !crate::native::is_raw_name(&entry.file_name().to_string_lossy()) {
            continue;
        }
        let stale = entry
            .metadata()
            .ok()
            .and_then(|meta| meta.modified().ok())
            .and_then(|modified| modified.elapsed().ok())
            .is_some_and(|age| age > crate::native::RAW_MAX_AGE);
        if stale {
            // Not `is_file`: a stale directory or symlink under this name is removed as itself.
            let path = entry.path();
            let _ = std::fs::remove_file(&path).or_else(|_| std::fs::remove_dir_all(&path));
        }
    }
}

fn list(dir: &Path, prefixes: &[&str]) -> Vec<Entry> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !prefixes.iter().any(|p| name.starts_with(p)) {
                return None;
            }
            let meta = entry.metadata().ok()?;
            meta.is_file().then(|| {
                (
                    entry.path(),
                    meta.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                    meta.len(),
                )
            })
        })
        .collect()
}

fn drop_oldest(items: &mut [Entry], keep: usize) {
    if items.len() <= keep {
        return;
    }
    items.sort_by_key(|(_, modified, _)| *modified);
    let remove = items.len() - keep;
    for (path, _, _) in items.iter().take(remove) {
        let _ = std::fs::remove_file(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prune_removes_stale_raw_samples_and_keeps_fresh_ones() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "rightkit-crash-retention-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let stale = dir.join(".sample_raw_panic_1_2.raw");
        let fresh = dir.join(".sample_raw_panic_3_4.raw");
        let other = dir.join(".sample_raw_notes.txt"); // not a raw name: untouched
        for path in [&stale, &fresh, &other] {
            std::fs::write(path, b"raw").unwrap();
        }
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(3_600);
        std::fs::File::options()
            .write(true)
            .open(&stale)
            .unwrap()
            .set_modified(old)
            .unwrap();
        prune(&dir, Retention::default());
        assert!(!stale.exists());
        assert!(fresh.exists());
        assert!(other.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
