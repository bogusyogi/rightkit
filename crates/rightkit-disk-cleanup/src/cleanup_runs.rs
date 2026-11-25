//! What a host does around a cleanup scan and an apply, beyond the rule scan itself: merging a
//! host's own findings into a report, the totals shown, one scan shared by overlapping requests,
//! the reason classes a host may log, and on Windows the identity stamps and the Recycle Bin
//! lookup and restore that the macOS `~/.Trash` check cannot do. Moved out of Pulse's
//! `cleanup.rs`; the host keeps its settings, cache, log and the Trash move itself.

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::Instant;

use crate::cleanup_scan as cs;

/// The two totals a host shows: eligible bytes that are safe to preselect, and the rest.
pub fn totals(report: &mut cs::Report) {
    report.safe_bytes = report
        .findings
        .iter()
        .filter(|f| f.eligible && f.risk == cs::RuleRisk::Safe)
        .map(|f| f.bytes)
        .sum();
    report.review_bytes = report
        .findings
        .iter()
        .filter(|f| f.eligible && f.risk != cs::RuleRisk::Safe)
        .map(|f| f.bytes)
        .sum();
}

/// Drops every finding whose category `is_developer` names, and recomputes the totals.
pub fn drop_developer_items(report: &mut cs::Report, is_developer: &dyn Fn(&str) -> bool) {
    report.findings.retain(|f| !is_developer(&f.category));
    totals(report);
}

/// Adds the host's own items (`items`, read only when `developer_on`) when developer items are on,
/// or drops every developer item when they are off, then sorts the report the way the core scan
/// does (category, then largest first) and recomputes the totals.
pub fn merge_developer_items(
    report: &mut cs::Report,
    developer_on: bool,
    is_developer: &dyn Fn(&str) -> bool,
    items: impl FnOnce() -> Vec<cs::Finding>,
) {
    if developer_on {
        report.findings.extend(items());
    } else {
        report.findings.retain(|f| !is_developer(&f.category));
    }
    report
        .findings
        .sort_by(|a, b| a.category.cmp(&b.category).then(b.bytes.cmp(&a.bytes)));
    totals(report);
}

/// The reason's class: the words before the first colon, at most 60 characters. No paths or names
/// reach a log through it.
pub fn reason_class(reason: &str) -> String {
    reason
        .split(':')
        .next()
        .unwrap_or("")
        .chars()
        .take(60)
        .collect()
}

/// How many skipped items fall in each reason class: counts only, never a file name.
pub fn skipped_by_reason(skipped: &[cs::Skipped]) -> BTreeMap<String, usize> {
    let mut classes: BTreeMap<String, usize> = BTreeMap::new();
    for item in skipped {
        *classes.entry(reason_class(&item.reason)).or_default() += 1;
    }
    classes
}

/// The last finished scan and when it finished, and the lock one scan holds. A request that
/// arrived before a scan finished shares its answer instead of queueing another full scan behind
/// the same lock (Pulse #544): a CLI that gave up and tried again used to leave one orphaned
/// multi-minute scan per try.
pub struct ScanFlight<T> {
    last: Mutex<Option<(Instant, T)>>,
    flight: Mutex<()>,
}

impl<T> Default for ScanFlight<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> ScanFlight<T> {
    pub const fn new() -> Self {
        Self {
            last: Mutex::new(None),
            flight: Mutex::new(()),
        }
    }
}

impl<T: Clone> ScanFlight<T> {
    /// Runs `scan` unless one that finished after this request began has an answer already.
    /// Overlapping calls wait on the one running scan, then reuse its answer.
    pub fn run(&self, scan: impl FnOnce() -> T) -> T {
        let requested = Instant::now();
        let _flight = self.flight.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((finished, answer)) =
            self.last.lock().unwrap_or_else(|e| e.into_inner()).as_ref()
            && *finished >= requested
        {
            return answer.clone();
        }
        let answer = scan();
        *self.last.lock().unwrap_or_else(|e| e.into_inner()) =
            Some((Instant::now(), answer.clone()));
        answer
    }
}

/// Windows: the core has no (volume, file) identity off Unix and records "0" and "0". The host
/// records the real one at scan time: a hash of the volume serial and a hash of the NTFS file id
/// (see [`crate::item_actions::ids`]). A drive that gives none stays "0".
#[cfg(windows)]
pub fn stamp_identities(report: &mut cs::Report) {
    for finding in &mut report.findings {
        let path = std::path::Path::new(&finding.path);
        let Ok(metadata) = std::fs::symlink_metadata(path) else {
            continue;
        };
        if let Ok((volume, file)) = crate::item_actions::ids(path, &metadata) {
            finding.dev = volume.to_string();
            finding.ino = file.to_string();
        }
    }
}

/// Windows: re-reads each item's volume serial and file id right before the core's own checks and
/// drops any item that is not the one scanned. Dropped items are reported as skipped. Kept items
/// are handed on with the core's placeholder identity.
#[cfg(windows)]
pub fn confirm_identities(items: Vec<cs::Request>) -> (Vec<cs::Request>, Vec<cs::Skipped>) {
    let mut kept = Vec::new();
    let mut skipped = Vec::new();
    for mut request in items {
        let shown = request.path.clone();
        let skip = |reason: &str| cs::Skipped {
            path: shown.clone(),
            reason: reason.to_string(),
        };
        let wanted = match (request.dev.parse::<u64>(), request.ino.parse::<u64>()) {
            (Ok(dev), Ok(ino)) if (dev, ino) != (0, 0) => (dev, ino),
            _ => {
                skipped.push(skip("This item's identity was not recorded. Scan again."));
                continue;
            }
        };
        let path = std::path::PathBuf::from(&request.path);
        let Ok(metadata) = std::fs::symlink_metadata(&path) else {
            skipped.push(skip("Already gone"));
            continue;
        };
        match crate::item_actions::ids(&path, &metadata) {
            Ok(now) if now == wanted => {
                request.dev = "0".into();
                request.ino = "0".into();
                kept.push(request);
            }
            Ok(_) => skipped.push(skip("Changed since the scan")),
            Err(error) => skipped.push(skip(&error)),
        }
    }
    (kept, skipped)
}

#[cfg(windows)]
const NOT_THIS_ONE: &str = "Not found: the Recycle Bin holds only other items at this path";
#[cfg(windows)]
const AMBIGUOUS: &str = "Ambiguous: restore it by hand from the Recycle Bin";
#[cfg(windows)]
const DEST_EXISTS: &str = "Destination exists: something is already there";

/// Windows: the Recycle Bin entry's original path is the journalled path (the file system is not
/// case sensitive, as the owner's own lookup treats it).
#[cfg(windows)]
fn same_original(entry: &rightkit_fs::TrashEntry, wanted: &std::path::Path) -> bool {
    entry.original.as_ref().is_some_and(|original| {
        original.to_string_lossy().to_lowercase() == wanted.to_string_lossy().to_lowercase()
    })
}

/// Windows: the size of a file in the Recycle Bin (`None` for a folder or an unreadable entry; the
/// owner's `TrashEntry` carries no size yet, so it is read from the entry's own path).
#[cfg(windows)]
fn bin_size(entry: &rightkit_fs::TrashEntry) -> Option<u64> {
    std::fs::symlink_metadata(&entry.path)
        .ok()
        .filter(|meta| meta.is_file())
        .map(|meta| meta.len())
}

/// Windows: seconds since the Unix epoch the entry was deleted, as the journal's window is in.
#[cfg(windows)]
fn deleted_secs(entry: &rightkit_fs::TrashEntry) -> i64 {
    i64::try_from(entry.trashed_unix_ms / 1000).unwrap_or(i64::MAX)
}

/// Windows: where a journalled item is in the Recycle Bin. Original path, deletion time (inside
/// the recorded window, widened when the move's end was never recorded) and, for a file, size must
/// agree; exactly one entry is found, none is missing, several are unknown. A bin that cannot be
/// read is unknown, never missing.
#[cfg(windows)]
pub fn bin_lookup(item: &cs::ActivityItem) -> cs::BinLookup {
    let Ok(bin) = rightkit_fs::trash::list() else {
        return cs::BinLookup::Unknown;
    };
    let wanted = std::path::PathBuf::from(&item.path);
    let (from, to) = item.trash_window.unwrap_or((0, 0));
    let mut probe = item.clone();
    probe.trash_window = Some((
        from,
        to.max(from.saturating_add(cs::INTERRUPTED_WINDOW_SECS)),
    ));
    let matching: Vec<_> = bin
        .iter()
        .filter(|entry| same_original(entry, &wanted))
        .filter(|entry| cs::bin_entry_matches(&probe, deleted_secs(entry), bin_size(entry)))
        .collect();
    match matching.as_slice() {
        [] => cs::BinLookup::Missing,
        [only] => cs::BinLookup::Found {
            trash_path: None,
            deleted_at: deleted_secs(only).max(0) as u64,
        },
        _ => cs::BinLookup::Unknown,
    }
}

/// Windows: the core only knows `~/.Trash`, so each item is found in the Recycle Bin and restored
/// there. An entry is the item only if its original path, deletion time (inside the journalled
/// apply window) and, for a file, size agree with what was recorded; the Recycle Bin API exposes no
/// modified time. Exactly one entry must match: none is "not found", several are refused as
/// ambiguous, never guessed. An item goes back only if its original path is free and lies under
/// `home`.
#[cfg(windows)]
pub fn restore_from_bin(home: &std::path::Path, id: &str) -> Result<cs::RestoreResult, String> {
    let activity = cs::history_with(home, &mut bin_lookup)?
        .into_iter()
        .find(|a| a.id == id)
        .ok_or("No such action")?;
    let bin = rightkit_fs::trash::list().map_err(|e| e.to_string())?;
    let mut journal = cs::RestoreJournal::open(home, id)?;
    let mut restored_items = Vec::new();
    let mut result = cs::RestoreResult::default();
    for item in activity.items.iter().filter(|item| !item.restored) {
        let wanted = std::path::PathBuf::from(&item.path);
        let skip = |reason: &str| cs::Skipped {
            path: item.path.clone(),
            reason: reason.to_string(),
        };
        if let Some(why) = &item.interrupted {
            result.skipped.push(skip(why));
            continue;
        }
        if !wanted.starts_with(home) {
            result.skipped.push(skip("Location not allowed"));
            continue;
        }
        let same_path: Vec<_> = bin
            .iter()
            .filter(|entry| same_original(entry, &wanted))
            .collect();
        if same_path.is_empty() {
            result
                .skipped
                .push(skip("Not found: no longer in the Recycle Bin"));
            continue;
        }
        let matching: Vec<_> = same_path
            .into_iter()
            .filter(|entry| cs::bin_entry_matches(item, deleted_secs(entry), bin_size(entry)))
            .collect();
        let found = match matching.as_slice() {
            [] => {
                result.skipped.push(skip(NOT_THIS_ONE));
                continue;
            }
            [only] => (*only).clone(),
            several => {
                let why = format!("{AMBIGUOUS} ({} match)", several.len());
                result.skipped.push(skip(&why));
                continue;
            }
        };
        if wanted.symlink_metadata().is_ok() {
            result.skipped.push(skip(DEST_EXISTS));
            continue;
        }
        // Write ahead: no intent on disk, no move.
        if let Err(why) = journal.begin(item) {
            result.skipped.push(skip(&why));
            continue;
        }
        // The journal writes above take time: look once more right before the move, so a name
        // taken in the meantime is never replaced.
        if wanted.symlink_metadata().is_ok() {
            journal.finish(item, false);
            result.skipped.push(skip(DEST_EXISTS));
            continue;
        }
        // The identity the owner restores by, from the matched entry's own public fields: the path
        // it was trashed from, its file key, the time it was trashed and where the bin holds it.
        // The owner moves it back with an exclusive rename (never over something there) and drops
        // the bin's record.
        let identity = rightkit_fs::TrashIdentity {
            original: wanted.clone(),
            key: found.key,
            trashed_unix_ms: found.trashed_unix_ms,
            trashed_as: Some(found.path.clone()),
        };
        match rightkit_fs::trash::restore(&identity) {
            Ok(_) => {
                journal.finish(item, true);
                restored_items.push(item.clone());
                result.restored_items += 1;
                result.restored_bytes += item.bytes;
            }
            Err(error) => {
                journal.finish(item, false);
                result.skipped.push(skip(&error.to_string()));
            }
        }
    }
    if !restored_items.is_empty() {
        cs::mark_restored(home, id, &restored_items)?;
    }
    journal.close();
    Ok(result)
}
