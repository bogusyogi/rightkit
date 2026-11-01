//! A bounded in-memory index of append-only transcripts (`.jsonl`), so reading the newest lines of
//! a transcript that grows costs the appended bytes, not the whole file (#449: a phone polling
//! `chat.read` re-read an 11.75 MB Claude transcript about 15 times a minute).
//!
//! `scan_rev` calls `each` with what `parse` makes of each line, newest line first, exactly as a
//! fresh reverse read of the file would. Per (kind, path) it keeps the parsed lines that matter
//! (`parse` returns `None` for a line with no effect), from `frontier` to `committed`, the offset
//! just after the last newline:
//!
//! * Unchanged (same file id, length and modification time): nothing is read.
//! * Grown, with the file id the same, `committed` still inside the file and the probe bytes at
//!   its two ends equal: only `[committed, len)` is read. Its complete lines join the index; a last
//!   line with no newline yet is parsed for this call only and `committed` stays before it, so a
//!   half-written line is never skipped or kept.
//! * Shorter, replaced (other file id), probe bytes changed, or grown by more than `MAX_APPEND`:
//!   the entry is dropped and the file read from its end as before.
//! * A caller that needs lines older than `frontier` reads the file backwards from `frontier`
//!   and extends the index.
//!
//! The index is cut to `ENTRY_BYTES` per path (the oldest lines go first, `frontier` moves up) and
//! `MAX_PATHS` paths (least recently used first). A file longer than `SCAN_CAP` is not indexed:
//! it is read as before, with the cap. Nothing is written to disk.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

/// A read stops after this many bytes from the end of the file (see `said`).
pub(super) const SCAN_CAP: u64 = 64 * 1024 * 1024;
const CHUNK: u64 = 256 * 1024;
/// Parsed lines kept per path, estimated bytes.
const ENTRY_BYTES: usize = 4 * 1024 * 1024;
/// Paths indexed at once.
const MAX_PATHS: usize = 6;
/// More new bytes than this since the last call: read the file again from its end.
const MAX_APPEND: u64 = 8 * 1024 * 1024;
/// Bytes compared at the start of the file and just before `committed` to tell an append from a
/// rewrite.
const PROBE: u64 = 128;

/// Call `each(start, end, line)` with the non-empty lines of `file` between `lo` and `hi` (`lo` a
/// line start, `hi` a line start or the file's length), last line first, until it returns false
/// or `SCAN_CAP` bytes have been read. `Ok(true)` when it stopped at the cap with lines unread.
pub(super) fn lines_rev_range(
    file: &mut File,
    hi: u64,
    lo: u64,
    mut each: impl FnMut(u64, u64, &[u8]) -> bool,
) -> io::Result<bool> {
    let mut pos = hi;
    let mut scanned = 0u64;
    // The start of a line whose beginning is in a chunk not read yet.
    let mut carry: Vec<u8> = Vec::new();
    while pos > lo {
        if scanned >= SCAN_CAP {
            return Ok(true);
        }
        let take = (pos - lo).min(CHUNK);
        pos -= take;
        file.seek(SeekFrom::Start(pos))?;
        let mut buf = vec![0u8; usize::try_from(take).unwrap_or(usize::MAX)];
        file.read_exact(&mut buf)?;
        scanned += take;
        buf.extend_from_slice(&carry);
        let mut end = buf.len();
        while let Some(newline) = buf[..end].iter().rposition(|b| *b == b'\n') {
            let line = &buf[newline + 1..end];
            if !line.is_empty() && !each(pos + newline as u64 + 1, pos + end as u64, line) {
                return Ok(false);
            }
            end = newline;
        }
        carry = buf[..end].to_vec();
    }
    if !carry.is_empty() {
        each(lo, lo + carry.len() as u64, &carry);
    }
    Ok(false)
}

type FileId = (u64, u64);

fn file_id(meta: &std::fs::Metadata) -> FileId {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        (meta.dev(), meta.ino())
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        (0, meta.creation_time())
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = meta;
        (0, 0)
    }
}

struct Item<T> {
    /// The offset of the line's newline.
    end: u64,
    weight: usize,
    value: T,
}

pub(super) struct Entry<T> {
    kind: String,
    path: PathBuf,
    id: FileId,
    len: u64,
    mtime: Option<SystemTime>,
    /// Just after the last newline seen.
    committed: u64,
    /// Every line from here to `committed` is in `items` (or has no effect). A line start.
    frontier: u64,
    head: Vec<u8>,
    tail: Vec<u8>,
    /// Oldest first.
    items: std::collections::VecDeque<Item<T>>,
    bytes: usize,
}

/// The index of one kind of reader: least recently used first.
pub(super) type Index<T> = Mutex<Vec<Entry<T>>>;

pub(super) const fn new_index<T>() -> Index<T> {
    Mutex::new(Vec::new())
}

fn read_at(file: &mut File, at: u64, len: u64) -> io::Result<Vec<u8>> {
    file.seek(SeekFrom::Start(at))?;
    let mut buf = vec![0u8; usize::try_from(len).unwrap_or(0)];
    file.read_exact(&mut buf)?;
    Ok(buf)
}

/// The two probes of a file whose lines up to `committed` are indexed.
fn probes(file: &mut File, committed: u64) -> io::Result<(Vec<u8>, Vec<u8>)> {
    let size = committed.min(PROBE);
    let head = read_at(file, 0, size)?;
    let tail = read_at(file, committed - size, size)?;
    Ok((head, tail))
}

fn same_file<T>(file: &mut File, entry: &Entry<T>) -> bool {
    match probes(file, entry.committed) {
        Ok((head, tail)) => head == entry.head && tail == entry.tail,
        Err(_) => false,
    }
}

/// Call `each` with `parse(line)` for every line of `path` that parses to `Some`, newest line
/// first, until it returns false; the same sequence a whole read from the end gives, from the
/// index where it can. `weigh` estimates a value's bytes. `Ok(true)` when the read hit
/// `SCAN_CAP` with lines unread (only for a file longer than the cap).
pub(super) fn scan_rev<T>(
    index: &Index<T>,
    kind: &str,
    path: &Path,
    parse: &dyn Fn(&[u8]) -> Option<T>,
    weigh: &dyn Fn(&T) -> usize,
    each: &mut dyn FnMut(&T) -> bool,
) -> io::Result<bool> {
    let mut file = File::open(path)?;
    let meta = file.metadata()?;
    let len = meta.len();
    if len > SCAN_CAP {
        return lines_rev_range(&mut file, len, 0, |_, _, line| match parse(line) {
            Some(value) => each(&value),
            None => true,
        });
    }
    let id = file_id(&meta);
    let mtime = meta.modified().ok();
    // Taken out while in use: a concurrent call for the same path reads on its own.
    let taken = {
        let mut slots = index.lock().unwrap_or_else(|p| p.into_inner());
        slots
            .iter()
            .position(|e| e.kind == kind && e.path == path)
            .map(|at| slots.remove(at))
    };
    let warm = taken.filter(|e| {
        e.id == id
            && len >= e.committed
            && len - e.committed <= MAX_APPEND
            && ((e.len == len && e.mtime == mtime) || same_file(&mut file, e))
    });
    let result = match warm {
        Some(entry) => warm_scan(entry, &mut file, len, parse, weigh, each),
        None => cold_scan(
            Entry {
                kind: kind.to_string(),
                path: path.to_path_buf(),
                id,
                len,
                mtime,
                committed: 0,
                frontier: 0,
                head: Vec::new(),
                tail: Vec::new(),
                items: Default::default(),
                bytes: 0,
            },
            &mut file,
            parse,
            weigh,
            each,
        ),
    };
    let (mut entry, cut) = result?;
    // The oldest lines go when the entry is over its budget; the frontier follows.
    while entry.bytes > ENTRY_BYTES {
        let Some(old) = entry.items.pop_front() else {
            break;
        };
        entry.bytes -= old.weight;
        entry.frontier = old.end + 1;
    }
    if let Ok((head, tail)) = probes(&mut file, entry.committed) {
        entry.head = head;
        entry.tail = tail;
        entry.len = len;
        entry.mtime = mtime;
        let mut slots = index.lock().unwrap_or_else(|p| p.into_inner());
        slots.push(entry);
        if slots.len() > MAX_PATHS {
            slots.remove(0);
        }
    }
    Ok(cut)
}

/// No usable entry: read from the end, keeping the lines read.
fn cold_scan<T>(
    mut entry: Entry<T>,
    file: &mut File,
    parse: &dyn Fn(&[u8]) -> Option<T>,
    weigh: &dyn Fn(&T) -> usize,
    each: &mut dyn FnMut(&T) -> bool,
) -> io::Result<(Entry<T>, bool)> {
    let len = entry.len;
    // Newest first.
    let mut found: Vec<Item<T>> = Vec::new();
    let mut committed = len;
    let mut stopped_at: Option<u64> = None;
    let mut first = true;
    let cut = lines_rev_range(file, len, 0, |start, end, line| {
        let value = parse(line);
        // A last line with no newline yet is parsed for this call only.
        let partial = first && end == len;
        first = false;
        if partial {
            committed = start;
        }
        let go = value.as_ref().is_none_or(&mut *each);
        if let Some(value) = value
            && !partial
        {
            let weight = weigh(&value) + 64;
            found.push(Item { end, weight, value });
        }
        if !go {
            stopped_at = Some(start);
        }
        go
    })?;
    entry.committed = committed;
    entry.frontier = stopped_at.unwrap_or(0);
    for item in found.into_iter().rev() {
        entry.bytes += item.weight;
        entry.items.push_back(item);
    }
    Ok((entry, cut))
}

/// A valid entry: read only `[committed, len)`, then the indexed lines, then older lines from
/// disk only when `each` has not stopped by the frontier.
fn warm_scan<T>(
    mut entry: Entry<T>,
    file: &mut File,
    len: u64,
    parse: &dyn Fn(&[u8]) -> Option<T>,
    weigh: &dyn Fn(&T) -> usize,
    each: &mut dyn FnMut(&T) -> bool,
) -> io::Result<(Entry<T>, bool)> {
    let appended = if len > entry.committed {
        read_at(file, entry.committed, len - entry.committed)?
    } else {
        Vec::new()
    };
    let complete = appended
        .iter()
        .rposition(|b| *b == b'\n')
        .map_or(0, |at| at + 1);
    let base = entry.committed;
    let mut start = 0usize;
    for (at, byte) in appended[..complete].iter().enumerate() {
        if *byte != b'\n' {
            continue;
        }
        let line = &appended[start..at];
        if !line.is_empty()
            && let Some(value) = parse(line)
        {
            let weight = weigh(&value) + 64;
            entry.bytes += weight;
            entry.items.push_back(Item {
                end: base + at as u64,
                weight,
                value,
            });
        }
        start = at + 1;
    }
    entry.committed = base + complete as u64;
    let mut stopped = false;
    // The last line with no newline yet comes first, as in a whole read.
    let partial = &appended[complete..];
    if !partial.is_empty()
        && let Some(value) = parse(partial)
        && !each(&value)
    {
        stopped = true;
    }
    if !stopped {
        for item in entry.items.iter().rev() {
            if !each(&item.value) {
                stopped = true;
                break;
            }
        }
    }
    if stopped || entry.frontier == 0 {
        return Ok((entry, false));
    }
    // Older than the index: read backwards from the frontier and extend it.
    let mut found: Vec<Item<T>> = Vec::new();
    let mut stopped_at: Option<u64> = None;
    let cut = lines_rev_range(file, entry.frontier, 0, |start, end, line| {
        let value = parse(line);
        let go = value.as_ref().is_none_or(&mut *each);
        if let Some(value) = value {
            let weight = weigh(&value) + 64;
            found.push(Item { end, weight, value });
        }
        if !go {
            stopped_at = Some(start);
        }
        go
    })?;
    entry.frontier = stopped_at.unwrap_or(0);
    for item in found {
        entry.bytes += item.weight;
        entry.items.push_front(item);
    }
    Ok((entry, cut))
}
