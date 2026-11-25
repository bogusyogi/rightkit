//! Bounded, read-only comparison of folder totals from two scan snapshots.
//!
//! Folder totals are scanner-attributed observations.  They do not describe
//! reclaimable space, and this module never attempts to infer hard-link
//! ownership or filesystem state.

use crate::{
    VolumeIdentity, history,
    model::{EntryKind, FolderAccounting},
    scan,
    store::Snapshot,
};
use serde::{Serialize, Serializer};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// Maximum number of rows returned in any one result list.
pub const MAX_FOLDER_LIMIT: usize = 1_000;

/// Match CLI wide-integer JSON semantics: serde_json can safely carry signed
/// or unsigned 64-bit values, while wider negative values stay decimal text.
fn serialize_wide<S>(value: &i128, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    if let Ok(value) = i64::try_from(*value) {
        serializer.serialize_i64(value)
    } else if let Ok(value) = u64::try_from(*value) {
        serializer.serialize_u64(value)
    } else {
        serializer.serialize_str(&value.to_string())
    }
}

/// One folder's signed change between two comparable snapshots.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct FolderGrowth {
    pub volume: VolumeIdentity,
    pub path: PathBuf,
    pub previous_logical_bytes: u64,
    pub current_logical_bytes: u64,
    #[serde(serialize_with = "serialize_wide")]
    pub logical_growth_bytes: i128,
    pub previous_attributed_allocation_bytes: u64,
    pub current_attributed_allocation_bytes: u64,
    #[serde(serialize_with = "serialize_wide")]
    pub attributed_growth_bytes: i128,
}

/// Bounded folder-level history comparison.
///
/// `top_growth` and `top_shrink` contain changes for folders present in both
/// snapshots. `added_folders` and `removed_folders` contain one-sided rows,
/// represented as growth from or to zero. Every list has at most `limit`
/// entries. `truncated` is true when any eligible row was omitted. If caller
/// requests more than [`MAX_FOLDER_LIMIT`], `limit` is capped & `reasons`
/// reports that bound while comparison remains valid.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct FolderGrowthComparison {
    pub comparable: bool,
    pub limit: usize,
    pub top_growth: Vec<FolderGrowth>,
    pub top_shrink: Vec<FolderGrowth>,
    pub added_folders: Vec<FolderGrowth>,
    pub removed_folders: Vec<FolderGrowth>,
    pub truncated: bool,
    pub reasons: Vec<String>,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct FolderKey {
    volume: VolumeIdentity,
    path: PathBuf,
}

impl FolderKey {
    fn new(folder: &FolderAccounting) -> Self {
        Self {
            volume: folder.volume.clone(),
            path: folder.path.clone(),
        }
    }
}

fn unavailable(limit: usize, mut reasons: Vec<String>) -> FolderGrowthComparison {
    reasons.sort();
    reasons.dedup();
    FolderGrowthComparison {
        comparable: false,
        limit,
        top_growth: Vec::new(),
        top_shrink: Vec::new(),
        added_folders: Vec::new(),
        removed_folders: Vec::new(),
        truncated: false,
        reasons,
    }
}

fn valid_volume(volume: &VolumeIdentity) -> bool {
    let id = volume.id.trim();
    !id.is_empty() && !id.eq_ignore_ascii_case("unknown")
}

fn has_dot_component(path: &Path) -> bool {
    path.to_string_lossy()
        .split(['/', '\\'])
        .any(|component| component == "." || component == "..")
}

fn inside_roots(path: &Path, roots: &[PathBuf]) -> bool {
    roots.iter().any(|root| path.starts_with(root))
}

/// One snapshot's folders, plus the keys of those with a material gap inside.
type FolderMap = (BTreeMap<FolderKey, FolderAccounting>, BTreeSet<FolderKey>);

fn folder_map(snapshot: &Snapshot) -> Result<FolderMap, Vec<String>> {
    let mut reasons = Vec::new();
    let roots = &snapshot.report.roots;
    let observed_volumes: BTreeSet<_> = snapshot
        .report
        .volume_usage
        .iter()
        .map(|usage| usage.volume.id.as_str())
        .collect();
    let mut folders = BTreeMap::new();

    for root in roots {
        if has_dot_component(root) {
            reasons.push(format!(
                "selected scan root `{}` contains `.` or `..` path component",
                root.display()
            ));
        }
    }
    if snapshot
        .report
        .entries
        .iter()
        .any(|entry| entry.metadata.kind == EntryKind::Directory)
        && snapshot.report.folders.is_empty()
    {
        reasons.push("folder coverage is empty for a nonempty scan report".to_string());
    }

    // Benign gaps (vanished entries, links or placeholders not followed, files
    // with incomplete metadata) never refuse a comparison. A scan-wide material
    // gap does. Any other material gap (access denied, a depth limit) only
    // leaves out the folders it lies in or below; see `scan::scan_gaps`.
    let gaps = scan::scan_gaps(&snapshot.report);
    if gaps.whole_scan > 0 {
        reasons.push("snapshot report contains incomplete inspection evidence".to_string());
    }
    // The stored `folder.incomplete` flag is only trusted when the report
    // records no gap to derive it from (older snapshots flag every folder).
    let recorded = !snapshot.report.incomplete_reasons.is_empty()
        || !snapshot.report.inspection_errors.is_empty();
    let mut partial = BTreeSet::new();

    for folder in &snapshot.report.folders {
        if folder.path.as_os_str().is_empty() {
            reasons.push("folder row has an empty path".to_string());
        }
        if has_dot_component(&folder.path) {
            reasons.push(format!(
                "folder `{}` contains `.` or `..` path component",
                folder.path.display()
            ));
        }
        if !valid_volume(&folder.volume) {
            reasons.push(format!(
                "folder `{}` has empty or unknown volume identity",
                folder.path.display()
            ));
        } else if !observed_volumes.contains(folder.volume.id.as_str()) {
            reasons.push(format!(
                "folder `{}` has unobserved volume identity `{}`",
                folder.path.display(),
                folder.volume.id
            ));
        }
        if !inside_roots(&folder.path, roots) {
            reasons.push(format!(
                "folder `{}` is outside selected scan roots",
                folder.path.display()
            ));
        }
        let key = FolderKey::new(folder);
        if recorded {
            if gaps.covers(&folder.path) {
                partial.insert(key.clone());
            }
        } else if folder.incomplete {
            reasons.push(format!(
                "folder `{}` accounting is incomplete",
                folder.path.display()
            ));
        }
        if folders.insert(key, folder.clone()).is_some() {
            reasons.push(format!(
                "duplicate folder row for volume `{}` and path `{}`",
                folder.volume.id,
                folder.path.display()
            ));
        }
    }

    if reasons.is_empty() {
        Ok((folders, partial))
    } else {
        Err(reasons)
    }
}

fn growth(
    key: &FolderKey,
    previous: Option<&FolderAccounting>,
    current: Option<&FolderAccounting>,
) -> FolderGrowth {
    let previous_logical = previous.map_or(0, |folder| folder.logical_bytes);
    let current_logical = current.map_or(0, |folder| folder.logical_bytes);
    let previous_attributed = previous.map_or(0, |folder| folder.attributed_allocation_bytes);
    let current_attributed = current.map_or(0, |folder| folder.attributed_allocation_bytes);
    FolderGrowth {
        volume: key.volume.clone(),
        path: key.path.clone(),
        previous_logical_bytes: previous_logical,
        current_logical_bytes: current_logical,
        logical_growth_bytes: i128::from(current_logical) - i128::from(previous_logical),
        previous_attributed_allocation_bytes: previous_attributed,
        current_attributed_allocation_bytes: current_attributed,
        attributed_growth_bytes: i128::from(current_attributed) - i128::from(previous_attributed),
    }
}

fn growth_order(left: &FolderGrowth, right: &FolderGrowth) -> std::cmp::Ordering {
    right
        .attributed_growth_bytes
        .cmp(&left.attributed_growth_bytes)
        .then_with(|| right.logical_growth_bytes.cmp(&left.logical_growth_bytes))
        .then_with(|| left.volume.cmp(&right.volume))
        .then_with(|| left.path.cmp(&right.path))
}

fn shrink_order(left: &FolderGrowth, right: &FolderGrowth) -> std::cmp::Ordering {
    left.attributed_growth_bytes
        .cmp(&right.attributed_growth_bytes)
        .then_with(|| left.logical_growth_bytes.cmp(&right.logical_growth_bytes))
        .then_with(|| left.volume.cmp(&right.volume))
        .then_with(|| left.path.cmp(&right.path))
}

fn path_order(left: &FolderGrowth, right: &FolderGrowth) -> std::cmp::Ordering {
    left.volume
        .cmp(&right.volume)
        .then_with(|| left.path.cmp(&right.path))
}

fn bounded(mut rows: Vec<FolderGrowth>, limit: usize) -> (Vec<FolderGrowth>, bool) {
    let truncated = rows.len() > limit;
    rows.truncate(limit);
    (rows, truncated)
}

/// Compare folder totals from two stored snapshots.
///
/// The general snapshot compatibility guard from [`history::compare`] runs
/// first. Any report with a scan-wide material gap, changed roots, remount,
/// missing identity, duplicate row, or out-of-scope folder makes the result
/// non-comparable and leaves all totals empty. Folders with a material gap
/// inside are left out, and benign gaps are reported in `reasons`, while the
/// comparison stays valid.
pub fn compare_folders(
    previous: &Snapshot,
    current: &Snapshot,
    limit: usize,
) -> FolderGrowthComparison {
    let effective_limit = limit.min(MAX_FOLDER_LIMIT);
    let limit_reason = (limit > MAX_FOLDER_LIMIT).then(|| {
        format!(
            "requested folder limit {} exceeds maximum {}; capped",
            limit, MAX_FOLDER_LIMIT
        )
    });
    let compatibility = history::compare(previous, current);
    if !compatibility.comparable {
        let mut reasons = compatibility.reasons;
        if let Some(reason) = &limit_reason {
            reasons.push(reason.clone());
        }
        return unavailable(effective_limit, reasons);
    }

    let (mut previous_folders, previous_partial) = match folder_map(previous) {
        Ok(folders) => folders,
        Err(mut reasons) => {
            if let Some(reason) = &limit_reason {
                reasons.push(reason.clone());
            }
            return unavailable(effective_limit, reasons);
        }
    };
    let (mut current_folders, current_partial) = match folder_map(current) {
        Ok(folders) => folders,
        Err(mut reasons) => {
            if let Some(reason) = &limit_reason {
                reasons.push(reason.clone());
            }
            return unavailable(effective_limit, reasons);
        }
    };

    // A folder with a material gap in either snapshot is left out of both: its
    // total is a lower bound there, so a difference would not be a change.
    let left_out: BTreeSet<_> = previous_partial.union(&current_partial).collect();
    let mut left_out_count = 0usize;
    for key in &left_out {
        let before = previous_folders.remove(*key).is_some();
        let after = current_folders.remove(*key).is_some();
        left_out_count += usize::from(before || after);
    }

    let keys: BTreeSet<_> = previous_folders
        .keys()
        .chain(current_folders.keys())
        .cloned()
        .collect();
    let mut growth_rows = Vec::new();
    let mut shrink_rows = Vec::new();
    let mut added_rows = Vec::new();
    let mut removed_rows = Vec::new();

    for key in keys {
        let before = previous_folders.get(&key);
        let after = current_folders.get(&key);
        let row = growth(&key, before, after);
        match (before.is_some(), after.is_some()) {
            (false, true) => added_rows.push(row),
            (true, false) => removed_rows.push(row),
            // Attributed allocation is the accounting-primary direction. A
            // logical-only change decides direction only when allocation is
            // unchanged; both signed values remain in every returned row.
            (true, true)
                if row.attributed_growth_bytes > 0
                    || (row.attributed_growth_bytes == 0 && row.logical_growth_bytes > 0) =>
            {
                growth_rows.push(row)
            }
            (true, true)
                if row.attributed_growth_bytes < 0
                    || (row.attributed_growth_bytes == 0 && row.logical_growth_bytes < 0) =>
            {
                shrink_rows.push(row)
            }
            (true, true) => {}
            (false, false) => unreachable!("union of folder keys contains an absent row"),
        }
    }

    growth_rows.sort_by(growth_order);
    shrink_rows.sort_by(shrink_order);
    added_rows.sort_by(path_order);
    removed_rows.sort_by(path_order);

    let (top_growth, growth_truncated) = bounded(growth_rows, effective_limit);
    let (top_shrink, shrink_truncated) = bounded(shrink_rows, effective_limit);
    let (added_folders, added_truncated) = bounded(added_rows, effective_limit);
    let (removed_folders, removed_truncated) = bounded(removed_rows, effective_limit);

    let mut reasons = Vec::new();
    if let Some(reason) = limit_reason {
        reasons.push(reason);
    }
    if left_out_count > 0 {
        reasons.push(format!(
            "{left_out_count} folder(s) with unreadable or truncated parts in either snapshot were left out of the comparison"
        ));
    }
    // Benign gaps do not block the comparison but stay visible.
    let benign: usize = [previous, current]
        .iter()
        .map(|snapshot| {
            scan::scan_gaps(&snapshot.report).benign + snapshot.report.skipped_links.len()
        })
        .sum();
    if benign > 0 {
        reasons.push(format!(
            "{benign} benign scan gap(s) across both snapshots (links or placeholders not followed, entries that vanished during the walk, files with incomplete metadata) did not block the comparison"
        ));
    }

    FolderGrowthComparison {
        comparable: true,
        limit: effective_limit,
        top_growth,
        top_shrink,
        added_folders,
        removed_folders,
        truncated: growth_truncated || shrink_truncated || added_truncated || removed_truncated,
        reasons,
    }
}

// ---- home-scan history and the "what grew" summary (moved out of Pulse's `growth.rs`) -----------

/// Folders smaller than this are not kept in a saved home snapshot. A home scan has ~250k folders,
/// which pushed the snapshot past the store's 64 MiB cap, so nothing was saved and a previous home
/// scan could never be shown. The scan roots always stay.
pub const MIN_FOLDER_BYTES: u64 = 1024 * 1024;
/// Grown folders in a [`Growth`] summary.
pub const GROWN_SHOWN: usize = 5;
/// Shrunk folders in a [`Growth`] summary.
pub const SHRUNK_SHOWN: usize = 3;

/// One folder's signed change in attributed bytes.
#[derive(Serialize)]
pub struct Change {
    pub path: PathBuf,
    /// Signed change in attributed bytes.
    pub bytes: i64,
}

/// The "what grew since the last comparable scan" summary.
#[derive(Serialize, Default)]
pub struct Growth {
    pub available: bool,
    /// Unix seconds of the scan being compared against.
    pub since: Option<u64>,
    pub grown: Vec<Change>,
    pub shrunk: Vec<Change>,
    pub reason: Option<String>,
}

/// A copy of `report` fit for the history store: per-file entries are dropped (a folder
/// comparison does not need them) and so are folders under [`MIN_FOLDER_BYTES`], except the roots.
///
/// The scan marks itself incomplete for any skipped link, vanished or unmeasurable entry, and a
/// Windows home always holds junctions and symlinks that are skipped by design, so such a scan
/// would never be comparable. When `record_complete` is true (the caller found that nothing that
/// lowers the folder totals went wrong: no limit, budget or permission refusal) the copy is
/// recorded as complete for comparison.
pub fn slim_for_history(
    report: &crate::model::ScanReport,
    record_complete: bool,
) -> crate::model::ScanReport {
    let folders = report
        .folders
        .iter()
        .filter(|f| {
            f.attributed_allocation_bytes >= MIN_FOLDER_BYTES || report.roots.contains(&f.path)
        })
        .cloned()
        .collect();
    let mut slim = crate::model::ScanReport {
        roots: report.roots.clone(),
        entries: Vec::new(),
        folders,
        accounting: report.accounting.clone(),
        volume_usage: report.volume_usage.clone(),
        volume_deltas: report.volume_deltas.clone(),
        inspection_errors: report.inspection_errors.clone(),
        skipped_links: Vec::new(),
        incomplete_reasons: report.incomplete_reasons.clone(),
    };
    if record_complete {
        slim.accounting.incomplete = false;
        slim.inspection_errors.clear();
        slim.incomplete_reasons.clear();
        for folder in &mut slim.folders {
            folder.incomplete = false;
        }
    }
    slim
}

/// Save `report` as a snapshot in `dir` and keep only the newest `keep` snapshots of the same roots.
pub fn save_and_prune(
    dir: &Path,
    report: crate::model::ScanReport,
    keep: usize,
) -> Result<(), String> {
    let roots = report.roots.clone();
    crate::store::save(dir, &Snapshot::new(report, Vec::new())).map_err(|e| e.to_string())?;
    let mut mine: Vec<Snapshot> = crate::store::history(dir)
        .map_err(|e| e.to_string())?
        .into_iter()
        .filter(|s| s.report.roots == roots)
        .collect();
    // History is ordered oldest first.
    while mine.len() > keep {
        let old = mine.remove(0);
        let _ = std::fs::remove_file(dir.join(format!("{}.json", old.id)));
    }
    Ok(())
}

/// The newest saved scan in `dir` of exactly `root`, with its unix time. Used to show something on
/// launch without scanning.
pub fn latest_scan_in(dir: &Path, root: &Path) -> Option<(crate::model::ScanReport, u64)> {
    let all = crate::store::history(dir).ok()?;
    all.into_iter()
        .rev()
        .find(|s| {
            s.report.roots.len() == 1 && s.report.roots[0] == root && !s.report.folders.is_empty()
        })
        .map(|s| (s.report, s.created_at))
}

/// Top grown and shrunk folders: the newest snapshot in `dir` against the latest earlier one of the
/// same roots that [`compare_folders`] accepts as comparable. Folders under `dir` itself (the scan
/// sees its own snapshots) and the scan roots are left out.
pub fn growth_in(dir: &Path) -> Result<Growth, String> {
    let all = crate::store::history(dir).map_err(|e| e.to_string())?;
    let Some(current) = all.last() else {
        return Ok(unavailable_growth("No saved scan yet."));
    };
    let mut why = String::from("No earlier scan of this folder to compare with.");
    for previous in all[..all.len() - 1].iter().rev() {
        if previous.report.roots != current.report.roots {
            continue;
        }
        let comparison = compare_folders(previous, current, 1_000);
        if !comparison.comparable {
            why = comparison.reasons.first().cloned().unwrap_or(why);
            continue;
        }
        let mut grown = Vec::new();
        let mut shrunk = Vec::new();
        for row in comparison
            .top_growth
            .iter()
            .chain(&comparison.added_folders)
            .chain(&comparison.top_shrink)
            .chain(&comparison.removed_folders)
        {
            // The scan sees its own snapshots; they are not the user's data.
            if row.path.starts_with(dir) || current.report.roots.contains(&row.path) {
                continue;
            }
            let bytes = i64::try_from(row.attributed_growth_bytes).unwrap_or(0);
            if bytes > 0 {
                grown.push(Change {
                    path: row.path.clone(),
                    bytes,
                });
            } else if bytes < 0 {
                shrunk.push(Change {
                    path: row.path.clone(),
                    bytes,
                });
            }
        }
        return Ok(Growth {
            available: true,
            since: Some(previous.created_at),
            grown: pick_changes(grown, true, GROWN_SHOWN),
            shrunk: pick_changes(shrunk, false, SHRUNK_SHOWN),
            reason: None,
        });
    }
    Ok(unavailable_growth(&why))
}

fn unavailable_growth(reason: &str) -> Growth {
    Growth {
        reason: Some(reason.to_string()),
        ..Growth::default()
    }
}

/// Largest changes first (`up`: growth; else shrinkage), without listing a folder whose change is
/// almost entirely explained by one of its own subfolders (the subfolder is the news).
pub fn pick_changes(mut rows: Vec<Change>, up: bool, take: usize) -> Vec<Change> {
    rows.sort_by(|a, b| {
        if up {
            b.bytes.cmp(&a.bytes)
        } else {
            a.bytes.cmp(&b.bytes)
        }
    });
    rows.dedup_by(|a, b| a.path == b.path);
    let explained = |outer: &Change| {
        rows.iter().any(|inner| {
            inner.path != outer.path
                && inner.path.starts_with(&outer.path)
                && (inner.bytes as i128).abs() * 10 >= (outer.bytes as i128).abs() * 8
        })
    };
    let keep: Vec<bool> = rows.iter().map(|r| !explained(r)).collect();
    rows.into_iter()
        .zip(keep)
        .filter_map(|(r, k)| k.then_some(r))
        .take(take)
        .collect()
}
