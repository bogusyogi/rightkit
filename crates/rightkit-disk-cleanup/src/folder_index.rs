//! The browsable folder index behind a storage page: a scan report folded into, per folder, its
//! size plus only its largest children (`TOP` of them), which is all a browser ever shows. Files
//! the scan kept and any beyond `TOP` per folder are summed into one summary row ("smaller files")
//! so a folder's rows still add up to its size. This module folds a report, saves and loads the
//! index in a compact binary layout, applies a re-read subtree in place (totals and rows of every
//! folder above move by the same change), and judges how complete a report is. Moved out of
//! Pulse's `scanner.rs`; what to call the summary row, the file magic and where to save are the
//! host's.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use crate::model::{EntryKind, ScanReport};

/// Most rows kept per folder.
pub const TOP: usize = 200;
/// Longest name read back from a saved index.
const MAX_SAVED_TEXT: usize = 1 << 20;

#[derive(Clone)]
pub struct Item {
    pub name: Box<str>,
    pub is_dir: bool,
    pub summary: bool,
    pub bytes: u64,
    /// Modified time in Unix seconds (a folder's: the newest below it); 0 when unknown.
    pub modified: u64,
}

#[derive(Default, Clone)]
pub struct Node {
    pub total: u64,
    pub dirs: u64,
    pub kept: u64,
    pub children: usize,
    pub items: Vec<Item>,
    /// Newest modified time at or below the folder, from the scan that built the
    /// node; 0 when unknown. Not saved: only a live refresh reads it.
    pub newest: u64,
}

#[derive(Clone)]
pub struct Index {
    pub root: PathBuf,
    pub root_label: String,
    pub scanned_at: u64,
    pub from_snapshot: bool,
    pub incomplete: bool,
    pub needs_access: bool,
    pub unread: u64,
    pub limited: bool,
    /// Rows carry modified times.
    pub has_dates: bool,
    pub nodes: HashMap<PathBuf, Node>,
}

/// An operating-system permission refusal. Matches the OS phrases themselves
/// (macOS "Permission denied" / "Operation not permitted", Windows "Access is
/// denied"), not the bare word, so a path that happens to contain "denied" and
/// errors such as "path not found" never read as a permission problem.
pub fn denied(text: &str) -> bool {
    let t = text.to_lowercase();
    [
        "permission denied",
        "operation not permitted",
        "access is denied",
        "access denied",
    ]
    .iter()
    .any(|phrase| t.contains(phrase))
}

/// Folders that could not be read for lack of permission. Placeholders
/// (iCloud files not stored locally) are expected and never counted.
pub fn needs_access(report: &ScanReport) -> bool {
    report.inspection_errors.iter().any(|e| denied(&e.message))
        || report
            .incomplete_reasons
            .iter()
            .any(|r| !r.to_lowercase().contains("placeholder") && denied(r))
}

/// How many distinct items could not be read: inspection errors plus the
/// per-item reasons (one item can carry several), without limits, budgets and
/// placeholders, which are not unreadable items.
pub fn unread_items(report: &ScanReport) -> u64 {
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    let mut count = report.inspection_errors.len() as u64;
    for reason in &report.incomplete_reasons {
        let lower = reason.to_lowercase();
        if lower.contains("placeholder") || lower.contains("limit") || lower.contains("budget") {
            continue;
        }
        let item = reason
            .strip_prefix("metadata unavailable: ")
            .and_then(|rest| rest.split_once(": ").map(|(path, _)| path))
            .unwrap_or(reason.as_str());
        if seen.insert(item) {
            count += 1;
        }
    }
    count
}

pub fn limited(report: &ScanReport) -> bool {
    report.incomplete_reasons.iter().any(|r| {
        let r = r.to_lowercase();
        !r.contains("placeholder") && (r.contains("budget") || r.contains("entry limit"))
    })
}

/// Reasons that make folder totals low: limits, budgets and unreadable
/// folders. Single entries without measurable size (sockets, special files)
/// do not change the totals and are not reported as a partial scan.
pub fn material(report: &ScanReport) -> bool {
    report.incomplete_reasons.iter().any(|r| {
        let lower = r.to_lowercase();
        !lower.contains("placeholder")
            && (["limit", "budget", "not inspectable"]
                .iter()
                .any(|k| lower.contains(k))
                || denied(r))
    })
}

/// A scan whose folder totals can be compared with another scan's: nothing
/// that lowers the totals (limits, budgets, unreadable folders, permission
/// refusals). Skipped links, long-path or vanished entries and single
/// entries without measurable size do not stop a comparison.
pub fn comparable(report: &ScanReport) -> bool {
    !material(report) && !needs_access(report)
}

/// The folder nodes of a report. Each folder gets its total, its child folders
/// and kept files as rows, and only its `TOP` largest rows; `label` names the row
/// that holds the bytes not itemised.
pub fn fold_nodes(report: &ScanReport, label: &str) -> HashMap<PathBuf, Node> {
    let root = report.roots.first().cloned().unwrap_or_default();
    let mut nodes: HashMap<PathBuf, Node> = HashMap::with_capacity(report.folders.len());
    for f in &report.folders {
        let node = nodes.entry(f.path.clone()).or_default();
        node.total = f.attributed_allocation_bytes;
        node.newest = f.newest_modified_at.unwrap_or(0);
    }
    for f in &report.folders {
        if f.path == root {
            continue;
        }
        let (Some(parent), Some(name)) = (f.path.parent(), f.path.file_name()) else {
            continue;
        };
        if let Some(node) = nodes.get_mut(parent) {
            node.dirs = node.dirs.saturating_add(f.attributed_allocation_bytes);
            node.items.push(Item {
                name: name.to_string_lossy().into(),
                is_dir: true,
                summary: false,
                bytes: f.attributed_allocation_bytes,
                modified: f.newest_modified_at.unwrap_or(0),
            });
        }
    }
    for e in &report.entries {
        if e.metadata.kind != EntryKind::File {
            continue;
        }
        let (Some(parent), Some(name)) = (e.path.parent(), e.path.file_name()) else {
            continue;
        };
        if let Some(node) = nodes.get_mut(parent) {
            node.kept = node.kept.saturating_add(e.attributed_allocation_bytes);
            node.items.push(Item {
                name: name.to_string_lossy().into(),
                is_dir: false,
                summary: false,
                bytes: e.attributed_allocation_bytes,
                modified: e.metadata.modified_at.unwrap_or(0),
            });
        }
    }
    for node in nodes.values_mut() {
        let other = node
            .total
            .saturating_sub(node.dirs.saturating_add(node.kept));
        node.children = node.items.len();
        if other > 0 {
            node.items.push(Item {
                name: label.into(),
                is_dir: false,
                summary: true,
                bytes: other,
                modified: 0,
            });
        }
        node.items.sort_by_key(|a| std::cmp::Reverse(a.bytes));
        node.items.truncate(TOP);
        node.items.shrink_to_fit();
    }
    nodes
}

/// Fold a report into the browsable index. `summary_label` names the row that holds the bytes not
/// itemised (small files, or all files for a saved scan); `root_label` names the scan root.
pub fn build_index(
    report: &ScanReport,
    root_label: String,
    summary_label: &str,
    scanned_at: u64,
    from_snapshot: bool,
) -> Index {
    let root = report.roots.first().cloned().unwrap_or_default();
    Index {
        root_label,
        root,
        scanned_at,
        from_snapshot,
        incomplete: report.accounting.incomplete && material(report),
        needs_access: needs_access(report),
        unread: unread_items(report),
        limited: limited(report),
        has_dates: true,
        nodes: fold_nodes(report, summary_label),
    }
}

/// Text as a 4-byte length, then its UTF-8 bytes.
fn write_text<W: Write>(out: &mut W, text: &str) -> std::io::Result<()> {
    let len = u32::try_from(text.len()).map_err(std::io::Error::other)?;
    out.write_all(&len.to_le_bytes())?;
    out.write_all(text.as_bytes())
}

/// Write the folder index, one folder after another, straight to `out`. The
/// layout: magic, version, root, root label, scan time, flags (bit 0 incomplete,
/// bit 1 needs access, bit 2 limited, bit 3 from a snapshot), folder count, then
/// per folder its path, child count, total, dirs, kept, row count and its rows
/// (name, kind bits: 1 folder, 2 summary row, then bytes and modified seconds).
pub fn write_index<W: Write>(
    out: &mut W,
    index: &Index,
    magic: &[u8; 4],
    version: u32,
) -> std::io::Result<()> {
    out.write_all(magic)?;
    out.write_all(&version.to_le_bytes())?;
    write_text(out, &index.root.to_string_lossy())?;
    write_text(out, &index.root_label)?;
    out.write_all(&index.scanned_at.to_le_bytes())?;
    let flags = u8::from(index.incomplete)
        | (u8::from(index.needs_access) << 1)
        | (u8::from(index.limited) << 2)
        | (u8::from(index.from_snapshot) << 3);
    out.write_all(&[flags])?;
    out.write_all(&index.unread.to_le_bytes())?;
    out.write_all(&(index.nodes.len() as u64).to_le_bytes())?;
    for (path, node) in &index.nodes {
        write_text(out, &path.to_string_lossy())?;
        out.write_all(&(node.children as u64).to_le_bytes())?;
        out.write_all(&node.total.to_le_bytes())?;
        out.write_all(&node.dirs.to_le_bytes())?;
        out.write_all(&node.kept.to_le_bytes())?;
        let rows = u32::try_from(node.items.len()).map_err(std::io::Error::other)?;
        out.write_all(&rows.to_le_bytes())?;
        for item in &node.items {
            write_text(out, &item.name)?;
            out.write_all(&[u8::from(item.is_dir) | (u8::from(item.summary) << 1)])?;
            out.write_all(&item.bytes.to_le_bytes())?;
            out.write_all(&item.modified.to_le_bytes())?;
        }
    }
    Ok(())
}

fn invalid(what: &str) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, what.to_string())
}

fn read_u8<R: Read>(input: &mut R) -> std::io::Result<u8> {
    let mut bytes = [0u8; 1];
    input.read_exact(&mut bytes)?;
    Ok(bytes[0])
}

fn read_u32<R: Read>(input: &mut R) -> std::io::Result<u32> {
    let mut bytes = [0u8; 4];
    input.read_exact(&mut bytes)?;
    Ok(u32::from_le_bytes(bytes))
}

fn read_u64<R: Read>(input: &mut R) -> std::io::Result<u64> {
    let mut bytes = [0u8; 8];
    input.read_exact(&mut bytes)?;
    Ok(u64::from_le_bytes(bytes))
}

fn read_text<R: Read>(input: &mut R) -> std::io::Result<String> {
    let len = read_u32(input)? as usize;
    if len > MAX_SAVED_TEXT {
        return Err(invalid("a saved name is too long"));
    }
    let mut bytes = vec![0u8; len];
    input.read_exact(&mut bytes)?;
    String::from_utf8(bytes).map_err(std::io::Error::other)
}

/// Read what `write_index` wrote. Streamed: the file is never held whole in memory.
pub fn read_index<R: Read>(
    input: &mut R,
    expected_magic: &[u8; 4],
    expected_version: u32,
) -> std::io::Result<Index> {
    let mut magic = [0u8; 4];
    input.read_exact(&mut magic)?;
    if magic != *expected_magic || read_u32(input)? != expected_version {
        return Err(invalid("not a storage index"));
    }
    let root = PathBuf::from(read_text(input)?);
    let root_label = read_text(input)?;
    let scanned_at = read_u64(input)?;
    let flags = read_u8(input)?;
    let unread = read_u64(input)?;
    let folders = read_u64(input)?;
    let mut nodes: HashMap<PathBuf, Node> = HashMap::new();
    for _ in 0..folders {
        let path = PathBuf::from(read_text(input)?);
        let children = usize::try_from(read_u64(input)?).map_err(std::io::Error::other)?;
        let total = read_u64(input)?;
        let dirs = read_u64(input)?;
        let kept = read_u64(input)?;
        let rows = read_u32(input)? as usize;
        if rows > TOP {
            return Err(invalid("a folder has too many rows"));
        }
        let mut items = Vec::with_capacity(rows);
        for _ in 0..rows {
            let name = read_text(input)?.into_boxed_str();
            let kind = read_u8(input)?;
            let bytes = read_u64(input)?;
            let modified = read_u64(input)?;
            items.push(Item {
                name,
                is_dir: kind & 1 != 0,
                summary: kind & 2 != 0,
                bytes,
                modified,
            });
        }
        nodes.insert(
            path,
            Node {
                total,
                dirs,
                kept,
                children,
                items,
                newest: 0,
            },
        );
    }
    Ok(Index {
        root,
        root_label,
        scanned_at,
        from_snapshot: flags & 8 != 0,
        incomplete: flags & 1 != 0,
        needs_access: flags & 2 != 0,
        unread,
        limited: flags & 4 != 0,
        has_dates: true,
        nodes,
    })
}

/// The folders to read, without duplicates and without any folder that lies
/// inside another one in the list (reading the outer folder covers it).
pub fn covering_folders(mut folders: Vec<PathBuf>) -> Vec<PathBuf> {
    folders.sort();
    folders.dedup();
    let mut kept: Vec<PathBuf> = Vec::new();
    for folder in &folders {
        if !folders
            .iter()
            .any(|other| other != folder && folder.starts_with(other))
        {
            kept.push(folder.clone());
        }
    }
    kept
}

/// Whether `text` contains `needle` ignoring case. `needle` is lower-case already.
pub fn contains_ci(text: &str, needle: &str) -> bool {
    if text.is_ascii() && needle.is_ascii() {
        let (haystack, pattern) = (text.as_bytes(), needle.as_bytes());
        if pattern.is_empty() {
            return true;
        }
        return haystack
            .windows(pattern.len())
            .any(|w| w.eq_ignore_ascii_case(pattern));
    }
    text.to_lowercase().contains(needle)
}

/// Whether `name` ends in `.ext` (ignoring case), with something before the dot.
pub fn extension_is(name: &str, ext: &str) -> bool {
    name.rsplit_once('.')
        .is_some_and(|(stem, found)| !stem.is_empty() && found.eq_ignore_ascii_case(ext))
}

/// `bytes` moved by `delta`, never below zero.
fn shift(bytes: u64, delta: i128) -> u64 {
    (i128::from(bytes) + delta).clamp(0, i128::from(u64::MAX)) as u64
}

/// Set the size shown for the folder `name` in `node`, if its row is kept.
/// The folder's modified time is `newest` when `exact` (the folder just read
/// again), else raised to at least `newest` (a folder above it).
fn set_dir_bytes(node: &mut Node, name: &str, bytes: u64, newest: u64, exact: bool) {
    if let Some(item) = node
        .items
        .iter_mut()
        .find(|i| !i.summary && i.is_dir && &*i.name == name)
    {
        item.bytes = bytes;
        item.modified = if exact {
            newest
        } else {
            item.modified.max(newest)
        };
    }
}

/// Recompute the "smaller files" row of `node` and keep the largest rows.
fn refresh_summary(node: &mut Node, summary_label: &str) {
    node.items.retain(|i| !i.summary);
    let other = node
        .total
        .saturating_sub(node.dirs.saturating_add(node.kept));
    if other > 0 {
        node.items.push(Item {
            name: summary_label.into(),
            is_dir: false,
            summary: true,
            bytes: other,
            modified: 0,
        });
    }
    node.items.sort_by_key(|a| std::cmp::Reverse(a.bytes));
    node.items.truncate(TOP);
}

/// Whether two folder nodes show the same total, count and rows.
fn same_node(a: &Node, b: &Node) -> bool {
    a.total == b.total
        && a.children == b.children
        && a.items.len() == b.items.len()
        && a.items.iter().zip(&b.items).all(|(x, y)| {
            x.bytes == y.bytes && x.is_dir == y.is_dir && x.summary == y.summary && x.name == y.name
        })
}

/// Put the freshly read `fresh` nodes of `folder`'s subtree into `index`, and
/// bring the folders above it in step: their totals and folder counts move by
/// the same change, and their rows for the folder below follow. An empty `fresh`
/// takes the folder out. `new_child`: the folder was not in the index; its
/// parent gets a row for it and counts it. Returns whether anything differed
/// from before (a size, a count, a row, or the number of folders below). `summary_label` names
/// the row that holds the bytes not itemised.
pub fn apply_nodes(
    index: &mut Index,
    folder: &Path,
    mut fresh: HashMap<PathBuf, Node>,
    new_child: bool,
    summary_label: &str,
) -> bool {
    let exists = fresh.contains_key(folder);
    let new_total = fresh.get(folder).map_or(0, |node| node.total);
    let new_newest = fresh.get(folder).map_or(0, |node| node.newest);
    let before = index.nodes.get(folder).cloned();
    if before.is_none() && !exists {
        return false;
    }
    let old_total = before.as_ref().map_or(0, |node| node.total);
    let delta = i128::from(new_total) - i128::from(old_total);

    let mut dropped = 0usize;
    index.nodes.retain(|path, _| {
        let inside = path.starts_with(folder);
        dropped += usize::from(inside);
        !inside
    });
    let changed = dropped != fresh.len()
        || match (before.as_ref(), fresh.get(folder)) {
            (Some(old), Some(new)) => !same_node(old, new),
            _ => true,
        };
    index.nodes.extend(fresh.drain());

    let mut child = folder.to_path_buf();
    let mut child_total = exists.then_some(new_total);
    let mut first = true;
    while let Some(parent) = child.parent().map(Path::to_path_buf) {
        let Some(name) = child.file_name().map(|n| n.to_string_lossy().into_owned()) else {
            break;
        };
        let Some(node) = index.nodes.get_mut(&parent) else {
            break;
        };
        node.total = shift(node.total, delta);
        node.dirs = shift(node.dirs, delta);
        match child_total {
            Some(bytes) if first && new_child => {
                node.items.push(Item {
                    name: name.as_str().into(),
                    is_dir: true,
                    summary: false,
                    bytes,
                    modified: new_newest,
                });
                node.children = node.children.saturating_add(1);
            }
            Some(bytes) => set_dir_bytes(node, &name, bytes, new_newest, first),
            None => {
                node.items
                    .retain(|i| i.summary || !i.is_dir || &*i.name != name.as_str());
                node.children = node.children.saturating_sub(1);
            }
        }
        refresh_summary(node, summary_label);
        child_total = Some(node.total);
        child = parent;
        first = false;
    }
    changed
}
