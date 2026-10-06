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
