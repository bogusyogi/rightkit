//! The tree on memory-mapped fixed-width columns, with incrementally
//! maintained folder rollups, hard-link ownership and APFS clone groups.
//!
//! Per-slot columns (names, parent, first-child/sibling links, sizes, flags)
//! and per-folder rollup columns live in mapped files, so only touched pages are
//! resident. Updates are in-place column writes plus slot appends; slots freed
//! by deletions are reused and `Index::compact` rebuilds the store.
//!
//! Crash safety: a header holds a `dirty` flag and a generation. The flag is
//! flushed with msync before the first column write after a commit;
//! `commit` flushes dirty columns, then writes the header clean with the next
//! generation. A header found dirty at open means a torn update: the store is
//! discarded and re-crawled.
//!
//! Size model (see README):
//! * `logical`   = data fork + resource fork length.
//! * `allocated` = ALLOCSIZE (all forks), every clone counted in full.
//! * `allocated_unique`: every file contributes its `private` bytes; each clone group
//!   contributes its largest shared part once per folder containing a member.
//! * A hard-linked inode contributes through exactly one owner link; other links are
//!   flagged SECONDARY_LINK and add nothing to any rollup.
//!
//! Clone groups (0.5.0): a group is allocated only when a second member joins. A file that is
//! the only member of its clone id is one entry of a small `key -> slot` table (its shared
//! bytes are read back from its columns); the second member promotes the entry to a full
//! `Group`, and removing down to one member demotes it again. Keys carry the device, so clone
//! ids and file ids of different volumes never merge.

use crate::backend::*;
use crate::col::Col;
use crate::names::{NameStore, MAX_SEGS};
use memmap2::MmapMut;
use std::collections::{HashMap, HashSet};
use std::fs::OpenOptions;
use std::io;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(C)]
pub struct Rollup {
    pub logical: u64,
    pub alloc: u64,
    pub unique: u64,
    pub files: u64,
}

#[derive(Clone, Copy)]
pub struct Entry {
    pub parent: u32,
    pub d: EntryData,
}

#[derive(Default)]
struct Group {
    members: Vec<(u32, u64)>,
    per_dir: HashMap<u32, u32>,
    s: u64,
}

/// Identity of a clone group: the APFS clone id (or, for shared extents without one, the file
/// id) within one device. Qualifying by device keeps ids of different volumes apart: before
/// 0.5.0 the key was the bare id, so a store holding two volumes merged unrelated groups and
/// over-counted `allocated_unique` (12,038,144 B on the measured root store, which held a
/// second volume's whole tree).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
struct CloneKey {
    id: u64,
    dev: u32,
    /// `id` is a file id (no clone id was reported), not a clone id.
    fallback: bool,
}

impl CloneKey {
    fn of(d: &EntryData) -> CloneKey {
        if d.cloneid != 0 {
            CloneKey {
                id: d.cloneid,
                dev: d.dev,
                fallback: false,
            }
        } else {
            CloneKey {
                id: d.fileid,
                dev: d.dev,
                fallback: true,
            }
        }
    }
}

/// Sizes to create a store at, so a rewrite allocates what it will hold (plus a sixteenth)
/// instead of growing into it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SizeHint {
    pub slots: usize,
    pub dirs: usize,
    pub names: usize,
    pub name_bytes: u64,
}

/// Heap the store tracks, by owner (an estimate: allocator rounding and bookkeeping excluded).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HeapParts {
    pub links: usize,
    pub clone_solo: usize,
    pub clone_multi: usize,
    pub free_lists: usize,
    pub other: usize,
}

impl HeapParts {
    pub fn total(&self) -> usize {
        self.links + self.clone_solo + self.clone_multi + self.free_lists + self.other
    }
}

/// Bytes of a hashbrown table with `capacity` usable slots holding pairs of `pair` bytes: the
/// bucket array, one control byte per bucket and a trailing control group.
fn map_heap(capacity: usize, pair: usize) -> usize {
    if capacity == 0 {
        return 0;
    }
    let buckets = if capacity < 8 {
        capacity + 1
    } else {
        capacity / 7 * 8
    };
    buckets * pair + buckets + 16
}

/// The header is rewritten at least this often while the store is clean, so the snapshot age
/// (`Config::snapshot_max_age`) keeps meaning "no older than this when last confirmed".
const HEADER_REFRESH_SECS: u64 = 3600;

const LIVE: u8 = 0x80;
/// Hard-linked file or one with shared (clone) extents: the only entries `rebuild_aux` must inspect.
const SPECIAL: u8 = 0x40;
const MAGIC: &[u8; 8] = b"RKFSIX03";
const LEGACY_MAGIC: &[u8; 8] = b"RKFSIX02";
const H_DIRTY: usize = 12;
const H_FP: usize = 16;
const H_GEN: usize = 24;
const H_EVENT: usize = 32;
const H_SAVED: usize = 40;
const H_NSLOTS: usize = 48;
const H_NDIRS: usize = 56;
const H_NAMES: usize = 64;
const H_BUCKETS: usize = 72;
const H_NROOTS: usize = 80;
const H_NSEGS: usize = 84;
const H_ROOTS: usize = 88;
const MAX_ROOTS: usize = 64;
const H_SEGS: usize = H_ROOTS + MAX_ROOTS * 4;
const HDR_LEN: usize = H_SEGS + MAX_SEGS * 4;
/// Bytes one slot occupies across the per-slot columns (eight u32, two u8, one u16, five u64).
const SLOT_BYTES: u64 = (8 * std::mem::size_of::<u32>()
    + 2 * std::mem::size_of::<u8>()
    + std::mem::size_of::<u16>()
    + 5 * std::mem::size_of::<u64>()) as u64;
/// Bytes one folder occupies across the per-folder columns (owner and rollup).
const DIR_BYTES: u64 = (std::mem::size_of::<u32>() + std::mem::size_of::<Rollup>()) as u64;
/// With a footprint cap set, the footprint is re-measured after this many adds.
const FOOTPRINT_CHECK_EVERY: u32 = 256;

pub fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn clamp_mtime(m: i64) -> u32 {
    m.clamp(0, u32::MAX as i64) as u32
}

pub struct OpenInfo {
    pub event_id: u64,
    pub saved_at: u64,
}

pub struct Store {
    dir: Option<PathBuf>,
    hdr: Option<MmapMut>,
    pub names: NameStore,
    c_name: Col<u32>,
    c_parent: Col<u32>,
    c_first: Col<u32>,
    c_next: Col<u32>,
    c_prev: Col<u32>,
    c_dir: Col<u32>,
    c_meta: Col<u8>,
    c_flags: Col<u8>,
    c_nlink: Col<u16>,
    c_mtime: Col<u32>,
    c_dev: Col<u32>,
    c_fileid: Col<u64>,
    c_logical: Col<u64>,
    c_alloc: Col<u64>,
    c_private: Col<u64>,
    c_clone: Col<u64>,
    d_owner: Col<u32>,
    d_roll: Col<Rollup>,
    nslots: usize,
    ndirs: usize,
    free: Vec<u32>,
    dir_free: Vec<u32>,
    pub roots: Vec<u32>,
    links: HashMap<(u32, u64), Vec<u32>>,
    /// Clone groups with one member: key to that member's slot.
    clone_solo: HashMap<CloneKey, u32>,
    legacy_clone_keys: bool,
    name_refs: Vec<u32>,
    live_names: usize,
    live_name_bytes: u64,
    /// Clone groups with two or more members.
    clone_multi: HashMap<CloneKey, Group>,
    package_exts: Vec<Vec<u8>>,
    live: usize,
    fingerprint: u64,
    generation: u64,
    /// Event id and time of the header on disk (what the last commit, or open, saw).
    committed_event: u64,
    committed_at: u64,
    dirty: bool,
    /// Bytes of dirty pages flushed by commits (and header writes) since `take_flushed_bytes`.
    flushed_bytes: u64,
    /// Refuse `add_child` while the footprint is above this many bytes.
    max_footprint: Option<u64>,
    adds_since_check: u32,
    footprint_capped: bool,
    refused_adds: u64,
    /// Record removed/touched slots (content index feed). Off unless content search is on.
    pub log_changes: bool,
    pub removed_log: Vec<u32>,
    pub touched_log: Vec<u32>,
}

fn apply(v: &mut u64, d: i64) {
    *v = v.wrapping_add(d as u64);
}

fn rd_u64(h: &MmapMut, at: usize) -> u64 {
    u64::from_le_bytes(h[at..at + 8].try_into().unwrap())
}
fn rd_u32(h: &MmapMut, at: usize) -> u32 {
    u32::from_le_bytes(h[at..at + 4].try_into().unwrap())
}

impl Store {
    pub fn footprint_headroom(&self) -> Option<u64> {
        self.max_footprint
            .map(|cap| cap.saturating_sub(self.footprint_bytes()))
    }

    pub fn replacement_growth(&self, entries: usize, name_bytes: u64) -> u64 {
        (entries as u64)
            .saturating_mul(SLOT_BYTES + DIR_BYTES)
            .saturating_add(self.names.additional_footprint(entries, name_bytes))
    }

    /// Fresh store. `dir` must exist for a file-backed store; `None` is anonymous (no persistence).
    #[cfg(test)]
    pub fn create(
        dir: Option<&Path>,
        package_exts: Vec<Vec<u8>>,
        fingerprint: u64,
    ) -> io::Result<Store> {
        Self::create_sized(dir, package_exts, fingerprint, SizeHint::default())
    }

    /// As `create`, with columns allocated for `hint` (a sixteenth of headroom) so a rewrite
    /// does not grow into its size and does not end with a doubled tail.
    pub fn create_sized(
        dir: Option<&Path>,
        package_exts: Vec<Vec<u8>>,
        fingerprint: u64,
        hint: SizeHint,
    ) -> io::Result<Store> {
        let hdr = match dir {
            Some(d) => {
                let f = OpenOptions::new()
                    .read(true)
                    .write(true)
                    .create(true)
                    .truncate(true)
                    .open(d.join("header.bin"))?;
                f.set_len(HDR_LEN.max(4096) as u64)?;
                Some(unsafe { MmapMut::map_mut(&f)? })
            }
            None => None,
        };
        let names = NameStore::create_sized(dir, hint.names, hint.name_bytes)?;
        let n = (hint.slots + hint.slots / 16).max(1024);
        let nd = (hint.dirs + hint.dirs / 16).max(256);
        let (a, b, c, d, e, f, g, h, i, j, k, l, m, n2, o, p, q, r) = (
            Col::<u32>::create(dir, "c-name.col", n)?,
            Col::<u32>::create(dir, "c-parent.col", n)?,
            Col::<u32>::create(dir, "c-first.col", n)?,
            Col::<u32>::create(dir, "c-next.col", n)?,
            Col::<u32>::create(dir, "c-prev.col", n)?,
            Col::<u32>::create(dir, "c-dir.col", n)?,
            Col::<u8>::create(dir, "c-meta.col", n)?,
            Col::<u8>::create(dir, "c-flags.col", n)?,
            Col::<u16>::create(dir, "c-nlink.col", n)?,
            Col::<u32>::create(dir, "c-mtime.col", n)?,
            Col::<u32>::create(dir, "c-dev.col", n)?,
            Col::<u64>::create(dir, "c-fileid.col", n)?,
            Col::<u64>::create(dir, "c-logical.col", n)?,
            Col::<u64>::create(dir, "c-alloc.col", n)?,
            Col::<u64>::create(dir, "c-private.col", n)?,
            Col::<u64>::create(dir, "c-clone.col", n)?,
            Col::<u32>::create(dir, "d-owner.col", nd)?,
            Col::<Rollup>::create(dir, "d-roll.col", nd)?,
        );
        let mut s = Store::assemble(
            dir,
            hdr,
            names,
            (a, b, c, d, e, f, g, h, i, j, k, l, m, n2, o, p, q, r),
            package_exts,
            fingerprint,
        );
        s.mark_dirty()?;
        Ok(s)
    }

    #[allow(clippy::type_complexity)]
    fn assemble(
        dir: Option<&Path>,
        hdr: Option<MmapMut>,
        names: NameStore,
        c: (
            Col<u32>,
            Col<u32>,
            Col<u32>,
            Col<u32>,
            Col<u32>,
            Col<u32>,
            Col<u8>,
            Col<u8>,
            Col<u16>,
            Col<u32>,
            Col<u32>,
            Col<u64>,
            Col<u64>,
            Col<u64>,
            Col<u64>,
            Col<u64>,
            Col<u32>,
            Col<Rollup>,
        ),
        package_exts: Vec<Vec<u8>>,
        fingerprint: u64,
    ) -> Store {
        Store {
            dir: dir.map(|d| d.to_path_buf()),
            hdr,
            names,
            c_name: c.0,
            c_parent: c.1,
            c_first: c.2,
            c_next: c.3,
            c_prev: c.4,
            c_dir: c.5,
            c_meta: c.6,
            c_flags: c.7,
            c_nlink: c.8,
            c_mtime: c.9,
            c_dev: c.10,
            c_fileid: c.11,
            c_logical: c.12,
            c_alloc: c.13,
            c_private: c.14,
            c_clone: c.15,
            d_owner: c.16,
            d_roll: c.17,
            nslots: 0,
            ndirs: 0,
            free: Vec::new(),
            dir_free: Vec::new(),
            roots: Vec::new(),
            links: HashMap::new(),
            clone_solo: HashMap::new(),
            legacy_clone_keys: false,
            name_refs: Vec::new(),
            live_names: 0,
            live_name_bytes: 0,
            clone_multi: HashMap::new(),
            package_exts,
            live: 0,
            fingerprint,
            generation: 0,
            committed_event: 0,
            committed_at: 0,
            dirty: false,
            flushed_bytes: 0,
            max_footprint: None,
            adds_since_check: 0,
            footprint_capped: false,
            refused_adds: 0,
            log_changes: false,
            removed_log: Vec::new(),
            touched_log: Vec::new(),
        }
    }

    /// Reopen a committed store. `Ok(None)` when absent, dirty (torn), or for a different configuration.
    pub fn open_existing(
        dir: &Path,
        package_exts: Vec<Vec<u8>>,
        fingerprint: u64,
    ) -> io::Result<Option<(Store, OpenInfo)>> {
        let Ok(f) = OpenOptions::new()
            .read(true)
            .write(true)
            .open(dir.join("header.bin"))
        else {
            return Ok(None);
        };
        if f.metadata()?.len() < HDR_LEN as u64 {
            return Ok(None);
        }
        let hdr = unsafe { MmapMut::map_mut(&f)? };
        if (&hdr[0..8] != MAGIC && &hdr[0..8] != LEGACY_MAGIC)
            || rd_u32(&hdr, H_DIRTY) != 0
            || rd_u64(&hdr, H_FP) != fingerprint
        {
            return Ok(None);
        }
        let (nslots, ndirs) = (
            rd_u64(&hdr, H_NSLOTS) as usize,
            rd_u64(&hdr, H_NDIRS) as usize,
        );
        let (ncount, buckets) = (
            rd_u64(&hdr, H_NAMES) as usize,
            rd_u64(&hdr, H_BUCKETS) as usize,
        );
        let nsegs = (rd_u32(&hdr, H_NSEGS) as usize).min(MAX_SEGS);
        let nroots = (rd_u32(&hdr, H_NROOTS) as usize).min(MAX_ROOTS);
        let seg_used: Vec<u32> = (0..nsegs).map(|i| rd_u32(&hdr, H_SEGS + i * 4)).collect();
        let roots: Vec<u32> = (0..nroots).map(|i| rd_u32(&hdr, H_ROOTS + i * 4)).collect();
        let opened = (|| -> io::Result<Store> {
            let names = NameStore::open(dir, ncount, seg_used, buckets)?;
            let mk = |nm: &str| -> io::Result<Col<u32>> { Col::open(dir, nm, nslots) };
            let cols = (
                mk("c-name.col")?,
                mk("c-parent.col")?,
                mk("c-first.col")?,
                mk("c-next.col")?,
                mk("c-prev.col")?,
                mk("c-dir.col")?,
                Col::<u8>::open(dir, "c-meta.col", nslots)?,
                Col::<u8>::open(dir, "c-flags.col", nslots)?,
                Col::<u16>::open(dir, "c-nlink.col", nslots)?,
                mk("c-mtime.col")?,
                mk("c-dev.col")?,
                Col::<u64>::open(dir, "c-fileid.col", nslots)?,
                Col::<u64>::open(dir, "c-logical.col", nslots)?,
                Col::<u64>::open(dir, "c-alloc.col", nslots)?,
                Col::<u64>::open(dir, "c-private.col", nslots)?,
                Col::<u64>::open(dir, "c-clone.col", nslots)?,
                Col::<u32>::open(dir, "d-owner.col", ndirs)?,
                Col::<Rollup>::open(dir, "d-roll.col", ndirs)?,
            );
            let mut s = Store::assemble(Some(dir), None, names, cols, package_exts, fingerprint);
            s.nslots = nslots;
            s.ndirs = ndirs;
            s.roots = roots;
            Ok(s)
        })();
        let Ok(mut s) = opened else { return Ok(None) };
        let (event_id, saved_at) = (rd_u64(&hdr, H_EVENT), rd_u64(&hdr, H_SAVED));
        s.generation = rd_u64(&hdr, H_GEN);
        s.committed_event = event_id;
        s.committed_at = saved_at;
        s.legacy_clone_keys = &hdr[0..8] == LEGACY_MAGIC;
        s.hdr = Some(hdr);
        s.rebuild_aux();
        s.trim();
        Ok(Some((s, OpenInfo { event_id, saved_at })))
    }

    /// Rebuild resident helpers (free lists, hard-link and clone groups) from the columns. Rollups are not touched.
    fn rebuild_aux(&mut self) {
        self.live = 0;
        self.free.clear();
        self.name_refs = vec![0; self.names.len()];
        self.live_names = 0;
        self.live_name_bytes = 0;
        for s in (0..self.nslots).rev() {
            if self.c_meta.get(s) & LIVE != 0 {
                self.live += 1;
                self.retain_name(self.c_name.get(s));
            } else {
                self.free.push(s as u32);
            }
        }
        self.dir_free = (0..self.ndirs)
            .rev()
            .filter(|&i| self.d_owner.get(i) == NONE)
            .map(|i| i as u32)
            .collect();
        let mut by_link: HashMap<(u32, u64), Vec<u32>> = HashMap::new();
        let mut shared: Vec<u32> = Vec::new();
        for s in 0..self.nslots {
            let m = self.c_meta.get(s);
            if m & LIVE == 0 || m & SPECIAL == 0 {
                continue;
            }
            let (nl, fid) = (self.c_nlink.get(s), self.c_fileid.get(s));
            let secondary = self.c_flags.get(s) & FLAG_SECONDARY_LINK != 0;
            if nl > 1 && fid != 0 {
                let v = by_link.entry((self.c_dev.get(s), fid)).or_default();
                if secondary {
                    v.push(s as u32);
                } else {
                    v.insert(0, s as u32);
                }
            }
            if !secondary && self.c_alloc.get(s) > self.c_private.get(s) {
                shared.push(s as u32);
            }
        }
        self.links = by_link;
        self.clone_solo.clear();
        self.clone_multi.clear();
        for s in shared {
            let d = self.entry(s).d;
            self.clone_join(s, d.alloc - d.private, false);
        }
    }

    /// (hard-link groups, clone groups, clone members) held resident.
    pub fn aux_counts(&self) -> (usize, usize, usize) {
        (
            self.links.len(),
            self.clone_solo.len() + self.clone_multi.len(),
            self.clone_solo.len()
                + self
                    .clone_multi
                    .values()
                    .map(|g| g.members.len())
                    .sum::<usize>(),
        )
    }

    /// Clone groups that were promoted to a full `Group` (two or more members).
    #[cfg(test)]
    pub fn multi_clone_groups(&self) -> usize {
        self.clone_multi.len()
    }

    pub fn special_count(&self) -> usize {
        (0..self.nslots)
            .filter(|&i| self.c_meta.get(i) & SPECIAL != 0)
            .count()
    }

    pub fn is_file_backed(&self) -> bool {
        self.hdr.is_some()
    }

    /// Make "an update is in flight" durable before the first write after a commit.
    fn mark_dirty(&mut self) -> io::Result<()> {
        if self.dirty {
            return Ok(());
        }
        self.dirty = true;
        if let Some(h) = self.hdr.as_mut() {
            h[0..8].copy_from_slice(if self.legacy_clone_keys {
                LEGACY_MAGIC
            } else {
                MAGIC
            });
            h[H_DIRTY..H_DIRTY + 4].copy_from_slice(&1u32.to_le_bytes());
            h[H_FP..H_FP + 8].copy_from_slice(&self.fingerprint.to_le_bytes());
            h.flush()?;
            self.flushed_bytes += crate::col::page_size() as u64;
        }
        Ok(())
    }

    fn touch(&mut self) {
        if !self.dirty {
            let _ = self.mark_dirty();
        }
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty && self.hdr.is_some()
    }

    /// Flush every column that has dirty pages, then publish a clean header with the next
    /// generation. A column nothing was written to is not touched, and a clean store whose
    /// header already carries `event_id` (and was confirmed within the last hour) writes
    /// nothing at all.
    pub fn commit(&mut self, event_id: u64) -> io::Result<()> {
        self.commit_with(event_id, |_| Ok(()))
    }

    /// As `commit`, but the header is always rewritten (an explicit snapshot: the caller asked
    /// for the state, with its time, to be on disk now).
    pub fn commit_forced(&mut self, event_id: u64) -> io::Result<()> {
        self.committed_at = 0;
        self.commit(event_id)
    }

    fn commit_with(
        &mut self,
        event_id: u64,
        mut before: impl FnMut(&str) -> io::Result<()>,
    ) -> io::Result<()> {
        if self.hdr.is_none() {
            return Ok(());
        }
        if !self.dirty
            && event_id == self.committed_event
            && now_secs().saturating_sub(self.committed_at) < HEADER_REFRESH_SECS
        {
            return Ok(());
        }
        macro_rules! flush_col {
            ($field:ident) => {
                if !self.$field.is_clean() {
                    before(stringify!($field))?;
                    let result = self.$field.flush();
                    self.flushed_bytes += self.$field.take_partial_flush_bytes();
                    self.flushed_bytes += result?;
                }
            };
        }
        if !self.names.is_clean() {
            before("names")?;
            self.names.flush(&mut self.flushed_bytes)?;
        }
        flush_col!(c_name);
        flush_col!(c_parent);
        flush_col!(c_first);
        flush_col!(c_next);
        flush_col!(c_prev);
        flush_col!(c_dir);
        flush_col!(c_meta);
        flush_col!(c_flags);
        flush_col!(c_nlink);
        flush_col!(c_mtime);
        flush_col!(c_dev);
        flush_col!(c_fileid);
        flush_col!(c_logical);
        flush_col!(c_alloc);
        flush_col!(c_private);
        flush_col!(c_clone);
        flush_col!(d_owner);
        flush_col!(d_roll);
        before("header")?;
        self.generation += 1;
        let (gen, ns, nd, nn, nb) = (
            self.generation,
            self.nslots as u64,
            self.ndirs as u64,
            self.names.count as u64,
            self.names.buckets() as u64,
        );
        let roots = self.roots.clone();
        let segs = self.names.seg_used.clone();
        let saved = now_secs();
        let h = self.hdr.as_mut().unwrap();
        h[0..8].copy_from_slice(if self.legacy_clone_keys {
            LEGACY_MAGIC
        } else {
            MAGIC
        });
        h[8..12].copy_from_slice(&(if self.legacy_clone_keys { 2u32 } else { 3u32 }).to_le_bytes());
        h[H_FP..H_FP + 8].copy_from_slice(&self.fingerprint.to_le_bytes());
        h[H_GEN..H_GEN + 8].copy_from_slice(&gen.to_le_bytes());
        h[H_EVENT..H_EVENT + 8].copy_from_slice(&event_id.to_le_bytes());
        h[H_SAVED..H_SAVED + 8].copy_from_slice(&saved.to_le_bytes());
        h[H_NSLOTS..H_NSLOTS + 8].copy_from_slice(&ns.to_le_bytes());
        h[H_NDIRS..H_NDIRS + 8].copy_from_slice(&nd.to_le_bytes());
        h[H_NAMES..H_NAMES + 8].copy_from_slice(&nn.to_le_bytes());
        h[H_BUCKETS..H_BUCKETS + 8].copy_from_slice(&nb.to_le_bytes());
        let nr = roots.len().min(MAX_ROOTS);
        h[H_NROOTS..H_NROOTS + 4].copy_from_slice(&(nr as u32).to_le_bytes());
        for (i, r) in roots.iter().take(nr).enumerate() {
            h[H_ROOTS + i * 4..H_ROOTS + i * 4 + 4].copy_from_slice(&r.to_le_bytes());
        }
        h[H_NSEGS..H_NSEGS + 4].copy_from_slice(&(segs.len() as u32).to_le_bytes());
        for (i, u) in segs.iter().enumerate() {
            h[H_SEGS + i * 4..H_SEGS + i * 4 + 4].copy_from_slice(&u.to_le_bytes());
        }
        h[H_DIRTY..H_DIRTY + 4].copy_from_slice(&0u32.to_le_bytes());
        if let Err(e) = h.flush() {
            h[H_DIRTY..H_DIRTY + 4].copy_from_slice(&1u32.to_le_bytes());
            self.dirty = true;
            return Err(e);
        }
        self.dirty = false;
        self.committed_event = event_id;
        self.committed_at = saved;
        self.flushed_bytes += crate::col::page_size() as u64;
        Ok(())
    }

    /// Bytes of dirty pages committed since the last call (zero for an anonymous store).
    /// The index adds them to its write budget.
    pub fn take_flushed_bytes(&mut self) -> u64 {
        std::mem::take(&mut self.flushed_bytes)
    }

    /// Bytes the next `commit` will write: every page dirtied since the last one, plus the
    /// header when anything is pending (a clean store has nothing to write).
    pub fn pending_write_bytes(&self) -> u64 {
        if self.hdr.is_none() {
            return 0;
        }
        let cols = self.names.dirty_bytes()
            + [
                self.c_name.dirty_bytes(),
                self.c_parent.dirty_bytes(),
                self.c_first.dirty_bytes(),
                self.c_next.dirty_bytes(),
                self.c_prev.dirty_bytes(),
                self.c_dir.dirty_bytes(),
                self.c_meta.dirty_bytes(),
                self.c_flags.dirty_bytes(),
                self.c_nlink.dirty_bytes(),
                self.c_mtime.dirty_bytes(),
                self.c_dev.dirty_bytes(),
                self.c_fileid.dirty_bytes(),
                self.c_logical.dirty_bytes(),
                self.c_alloc.dirty_bytes(),
                self.c_private.dirty_bytes(),
                self.c_clone.dirty_bytes(),
                self.d_owner.dirty_bytes(),
                self.d_roll.dirty_bytes(),
            ]
            .iter()
            .sum::<u64>();
        if cols == 0 && !self.dirty {
            0
        } else {
            cols + crate::col::page_size() as u64
        }
    }

    /// Pending dirty pages plus any cursor-only or age-only header publication.
    pub fn pending_commit_bytes(&self, event_id: u64) -> u64 {
        let pending = self.pending_write_bytes();
        if pending == 0
            && self.hdr.is_some()
            && (event_id != self.committed_event
                || now_secs().saturating_sub(self.committed_at) >= HEADER_REFRESH_SECS)
        {
            crate::col::page_size() as u64
        } else {
            pending
        }
    }

    /// Bytes the store's files span (column and name capacities plus the header): what the
    /// disk holds for it, used or not.
    pub fn capacity_bytes(&self) -> u64 {
        let slot_cols = self.c_name.capacity_bytes()
            + self.c_parent.capacity_bytes()
            + self.c_first.capacity_bytes()
            + self.c_next.capacity_bytes()
            + self.c_prev.capacity_bytes()
            + self.c_dir.capacity_bytes()
            + self.c_meta.capacity_bytes()
            + self.c_flags.capacity_bytes()
            + self.c_nlink.capacity_bytes()
            + self.c_mtime.capacity_bytes()
            + self.c_dev.capacity_bytes()
            + self.c_fileid.capacity_bytes()
            + self.c_logical.capacity_bytes()
            + self.c_alloc.capacity_bytes()
            + self.c_private.capacity_bytes()
            + self.c_clone.capacity_bytes();
        slot_cols
            + self.d_owner.capacity_bytes()
            + self.d_roll.capacity_bytes()
            + self.names.capacity_bytes()
            + HDR_LEN.max(4096) as u64
    }

    /// Bytes of live data: live slots and folders, the names, the header. What a rewrite of
    /// the store would occupy before headroom.
    pub fn live_bytes(&self) -> u64 {
        let buckets = (self.live_names + 1).next_power_of_two().max(1024);
        self.live as u64 * SLOT_BYTES
            + self.dir_count() as u64 * DIR_BYTES
            + self.live_name_bytes
            + self.live_names as u64 * 10
            + buckets as u64 * 4
            + HDR_LEN.max(4096) as u64
    }

    /// Capacity not holding live data: unused tails, dead slots, name garbage.
    pub fn unused_bytes(&self) -> u64 {
        self.capacity_bytes().saturating_sub(self.live_bytes())
    }

    /// Whether a rewrite would reclaim enough to be worth its writes: unused capacity above
    /// `min_bytes` and above a fifth of the capacity. A store settled by 0.5.0 growth (at
    /// most an eighth unused) never qualifies on capacity alone, so growth and compaction do
    /// not take turns.
    pub fn wants_compaction(&self, min_bytes: u64) -> bool {
        let (cap, unused) = (self.capacity_bytes(), self.unused_bytes());
        unused > min_bytes && unused.saturating_mul(5) > cap
    }

    /// Bytes the store's data occupies on disk (slots and folders in use, names, header). It
    /// counts used extents, not the doubled column capacity, and is what a rebuild rewrites.
    pub fn footprint_bytes(&self) -> u64 {
        self.nslots as u64 * SLOT_BYTES
            + self.ndirs as u64 * DIR_BYTES
            + self.names.footprint_bytes()
            + HDR_LEN as u64
    }

    /// Refuse `add_child` while `footprint_bytes()` is above `cap` (`None`: no cap). Entries
    /// already stored are never removed because of it.
    pub fn set_max_footprint(&mut self, cap: Option<u64>) {
        self.max_footprint = cap;
        self.footprint_capped = cap.is_some_and(|cap| self.footprint_bytes() > cap);
        self.adds_since_check = 0;
    }

    /// Adds refused because of the footprint cap since the store was created or opened.
    pub fn refused_adds(&self) -> u64 {
        self.refused_adds
    }

    /// Whether another entry may not be added. The footprint is measured every
    /// `FOOTPRINT_CHECK_EVERY` adds, and on every add while it is over the cap.
    fn growth_blocked(&mut self) -> bool {
        let Some(cap) = self.max_footprint else {
            return false;
        };
        self.adds_since_check += 1;
        if self.footprint_capped || self.adds_since_check >= FOOTPRINT_CHECK_EVERY {
            self.adds_since_check = 0;
            self.footprint_capped = self.footprint_bytes() > cap;
        }
        self.footprint_capped
    }

    /// Drop clean mapped pages from this process's resident set (they refault on demand).
    /// Resident bytes of every mapping this store owns (columns, names, header).
    pub fn mapped_resident_bytes(&self) -> usize {
        let hdr = self
            .hdr
            .as_ref()
            .map_or(0, |m| crate::col::resident_bytes(m.as_ptr(), m.len()));
        hdr + self.names.resident_bytes()
            + [
                self.c_name.resident_bytes(),
                self.c_parent.resident_bytes(),
                self.c_first.resident_bytes(),
                self.c_next.resident_bytes(),
                self.c_prev.resident_bytes(),
                self.c_dir.resident_bytes(),
                self.c_meta.resident_bytes(),
                self.c_flags.resident_bytes(),
                self.c_nlink.resident_bytes(),
                self.c_mtime.resident_bytes(),
                self.c_dev.resident_bytes(),
                self.c_fileid.resident_bytes(),
                self.c_logical.resident_bytes(),
                self.c_alloc.resident_bytes(),
                self.c_private.resident_bytes(),
                self.c_clone.resident_bytes(),
                self.d_owner.resident_bytes(),
                self.d_roll.resident_bytes(),
            ]
            .iter()
            .sum::<usize>()
    }

    /// Tracked heap: the capacity of the collections this store owns on the heap (free lists,
    /// hard-link and clone tables, package list). An estimate of what the index allocates
    /// itself; allocator bookkeeping and transient buffers are not included.
    pub fn heap_bytes(&self) -> usize {
        self.heap_parts().total()
    }

    /// `heap_bytes` split by owner.
    pub fn heap_parts(&self) -> HeapParts {
        let links = map_heap(
            self.links.capacity(),
            std::mem::size_of::<((u32, u64), Vec<u32>)>(),
        ) + self.links.values().map(|v| v.capacity() * 4).sum::<usize>();
        let clone_solo = map_heap(
            self.clone_solo.capacity(),
            std::mem::size_of::<(CloneKey, u32)>(),
        );
        let clone_multi = map_heap(
            self.clone_multi.capacity(),
            std::mem::size_of::<(CloneKey, Group)>(),
        ) + self
            .clone_multi
            .values()
            .map(|g| {
                g.members.capacity() * std::mem::size_of::<(u32, u64)>()
                    + map_heap(g.per_dir.capacity(), std::mem::size_of::<(u32, u32)>())
            })
            .sum::<usize>();
        let pkgs: usize = self.package_exts.iter().map(|p| p.capacity() + 24).sum();
        HeapParts {
            links,
            clone_solo,
            clone_multi,
            free_lists: self.free.capacity() * 4 + self.dir_free.capacity() * 4,
            other: self.roots.capacity() * 4 + pkgs + self.name_refs.capacity() * 4,
        }
    }

    pub fn trim(&mut self) {
        self.names.trim();
        self.c_name.trim();
        self.c_parent.trim();
        self.c_first.trim();
        self.c_next.trim();
        self.c_prev.trim();
        self.c_dir.trim();
        self.c_mtime.trim();
        self.c_dev.trim();
        self.c_meta.trim();
        self.c_flags.trim();
        self.c_nlink.trim();
        self.c_fileid.trim();
        self.c_logical.trim();
        self.c_alloc.trim();
        self.c_private.trim();
        self.c_clone.trim();
        self.d_owner.trim();
        self.d_roll.trim();
    }

    /// Drop the resident pages of every mapping except the search set (meta and name columns,
    /// name offsets, lengths and text): the pages crawls, rescans and rollup work touch go,
    /// the pages a name search reads stay.
    #[allow(dead_code)]
    pub fn trim_cold(&mut self) {
        self.names.trim_cold();
        self.c_parent.trim();
        self.c_first.trim();
        self.c_next.trim();
        self.c_prev.trim();
        self.c_dir.trim();
        self.c_mtime.trim();
        self.c_dev.trim();
        self.c_flags.trim();
        self.c_nlink.trim();
        self.c_fileid.trim();
        self.c_logical.trim();
        self.c_alloc.trim();
        self.c_private.trim();
        self.c_clone.trim();
        self.d_owner.trim();
        self.d_roll.trim();
    }

    /// Sizes a rewrite of the live tree needs, counted exactly by walking it: slots and
    /// folders reached from roots & distinct names they use.
    pub fn copy_hint(&self) -> SizeHint {
        let mut hint = SizeHint::default();
        let mut seen = vec![0u64; self.names.len().div_ceil(64)];
        for &r in &self.roots {
            let mut stack = vec![r];
            while let Some(x) = stack.pop() {
                let i = x as usize;
                hint.slots += 1;
                if (self.c_meta.get(i) & 3) == KIND_DIR {
                    hint.dirs += 1;
                }
                let nid = self.c_name.get(i) as usize;
                if seen[nid >> 6] & (1u64 << (nid & 63)) == 0 {
                    seen[nid >> 6] |= 1u64 << (nid & 63);
                    hint.names += 1;
                    hint.name_bytes += self.names.get(nid as u32).len() as u64;
                }
                let mut c = self.c_first.get(i);
                while c != NONE {
                    stack.push(c);
                    c = self.c_next.get(c as usize);
                }
            }
        }
        hint
    }

    /// Copy the live tree depth first into `fresh` (contiguous slots, no garbage names, no
    /// dead rows). Leaves this store untouched until publication.
    pub fn copy_into(&self, fresh: &mut Store) -> io::Result<()> {
        for &r in &self.roots {
            let nr = fresh.add_root(self.name_of(r), self.entry(r).d)?;
            let mut stack = vec![(r, nr)];
            while let Some((o, n)) = stack.pop() {
                for c in self.children(o).into_iter().rev() {
                    let e = self.entry(c);
                    let nc = fresh.add_child(n, self.name_of(c), e.d)?;
                    if e.d.kind == KIND_DIR {
                        stack.push((c, nc));
                    }
                }
            }
        }
        Ok(())
    }

    /// Slots on the free list (reusable) as a fraction signal for compaction.
    pub fn free_slots(&self) -> usize {
        self.free.len()
    }

    // ---------------------------------------------------------------- accessors

    pub fn relocate(&mut self, dir: &Path) {
        self.dir = Some(dir.to_path_buf());
        self.names.relocate(dir);
    }

    pub fn len(&self) -> usize {
        self.live
    }
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.live == 0
    }
    pub fn slots(&self) -> usize {
        self.nslots
    }
    /// Live folders: allocated folder rows minus those on the free list.
    pub fn dir_count(&self) -> usize {
        self.ndirs - self.dir_free.len()
    }
    pub fn is_live(&self, s: u32) -> bool {
        (s as usize) < self.nslots && self.c_meta.get(s as usize) & LIVE != 0
    }

    pub fn entry(&self, s: u32) -> Entry {
        let i = s as usize;
        let m = self.c_meta.get(i);
        Entry {
            parent: self.c_parent.get(i),
            d: EntryData {
                kind: m & 3,
                flags: self.c_flags.get(i),
                nlink: self.c_nlink.get(i) as u32,
                mtime: self.c_mtime.get(i) as i64,
                dev: self.c_dev.get(i),
                fileid: self.c_fileid.get(i),
                logical: self.c_logical.get(i),
                alloc: self.c_alloc.get(i),
                private: self.c_private.get(i),
                cloneid: self.c_clone.get(i),
            },
        }
    }

    fn put_data(&mut self, s: u32, d: &EntryData) {
        let i = s as usize;
        let special = d.kind != KIND_DIR && ((d.nlink > 1 && d.fileid != 0) || d.alloc > d.private);
        self.c_meta
            .set(i, LIVE | (d.kind & 3) | if special { SPECIAL } else { 0 });
        self.c_flags.set(i, d.flags);
        self.c_nlink.set(i, d.nlink.min(u16::MAX as u32) as u16);
        self.c_mtime.set(i, clamp_mtime(d.mtime));
        self.c_dev.set(i, d.dev);
        self.c_fileid.set(i, d.fileid);
        self.c_logical.set(i, d.logical);
        self.c_alloc.set(i, d.alloc);
        self.c_private.set(i, d.private);
        self.c_clone.set(i, d.cloneid);
    }

    pub fn kind_flags(&self, s: u32) -> (u8, u8) {
        (
            self.c_meta.get(s as usize) & 3,
            self.c_flags.get(s as usize),
        )
    }

    pub fn name_of(&self, s: u32) -> &[u8] {
        self.names.get(self.c_name.get(s as usize))
    }

    fn parent_of(&self, s: u32) -> u32 {
        self.c_parent.get(s as usize)
    }

    fn dir_of(&self, s: u32) -> Option<usize> {
        let d = self.c_dir.get(s as usize);
        (d != NONE).then_some(d as usize)
    }

    pub fn rollup(&self, s: u32) -> Rollup {
        self.dir_of(s)
            .map_or(Rollup::default(), |d| self.d_roll.get(d))
    }

    pub fn children(&self, s: u32) -> Vec<u32> {
        let mut v = Vec::new();
        let mut c = self.c_first.get(s as usize);
        while c != NONE {
            v.push(c);
            c = self.c_next.get(c as usize);
        }
        v
    }

    fn is_package(&self, name: &[u8]) -> bool {
        match name.iter().rposition(|&b| b == b'.') {
            Some(p) if p + 1 < name.len() => {
                let ext = &name[p + 1..];
                self.package_exts.iter().any(|e| {
                    e.len() == ext.len()
                        && e.iter().zip(ext).all(|(a, b)| *a == b.to_ascii_lowercase())
                })
            }
            _ => false,
        }
    }

    pub fn package_exts_clone(&self) -> Vec<Vec<u8>> {
        self.package_exts.clone()
    }

    fn alloc_slot(&mut self) -> io::Result<u32> {
        self.touch();
        self.live += 1;
        if let Some(s) = self.free.pop() {
            return Ok(s);
        }
        let s = self.nslots;
        if s >= NONE as usize - 1 {
            return Err(io::Error::other("slot space exhausted"));
        }
        let n = s + 1;
        for r in [
            self.c_name.ensure(n),
            self.c_parent.ensure(n),
            self.c_first.ensure(n),
            self.c_next.ensure(n),
            self.c_prev.ensure(n),
            self.c_dir.ensure(n),
            self.c_meta.ensure(n),
            self.c_flags.ensure(n),
            self.c_nlink.ensure(n),
            self.c_mtime.ensure(n),
            self.c_dev.ensure(n),
            self.c_fileid.ensure(n),
            self.c_logical.ensure(n),
            self.c_alloc.ensure(n),
            self.c_private.ensure(n),
            self.c_clone.ensure(n),
        ] {
            r?;
        }
        self.nslots = n;
        Ok(s as u32)
    }

    fn new_dir_idx(&mut self, slot: u32) -> io::Result<()> {
        let di = if let Some(d) = self.dir_free.pop() {
            d as usize
        } else {
            let d = self.ndirs;
            self.d_owner.ensure(d + 1)?;
            self.d_roll.ensure(d + 1)?;
            self.ndirs = d + 1;
            d
        };
        self.d_owner.set(di, slot);
        self.d_roll.set(di, Rollup::default());
        self.c_dir.set(slot as usize, di as u32);
        Ok(())
    }

    fn retain_name(&mut self, id: u32) {
        self.name_refs.resize(self.names.len(), 0);
        let count = &mut self.name_refs[id as usize];
        if *count == 0 {
            self.live_names += 1;
            self.live_name_bytes += self.names.get(id).len() as u64;
        }
        *count += 1;
    }

    fn release_name(&mut self, id: u32) {
        let count = &mut self.name_refs[id as usize];
        *count -= 1;
        if *count == 0 {
            self.live_names -= 1;
            self.live_name_bytes -= self.names.get(id).len() as u64;
        }
    }

    fn init_slot(&mut self, s: u32, name: u32, parent: u32, d: &EntryData) -> io::Result<()> {
        let i = s as usize;
        self.c_name.set(i, name);
        self.retain_name(name);
        self.c_parent.set(i, parent);
        self.c_first.set(i, NONE);
        self.c_next.set(i, NONE);
        self.c_prev.set(i, NONE);
        self.c_dir.set(i, NONE);
        self.put_data(s, d);
        if d.kind == KIND_DIR {
            self.new_dir_idx(s)?;
        }
        Ok(())
    }

    /// Add a root entry (name is the absolute path).
    pub fn add_root(&mut self, path: &[u8], mut d: EntryData) -> io::Result<u32> {
        d.kind = KIND_DIR;
        if self.roots.len() >= MAX_ROOTS {
            return Err(io::Error::other("too many roots"));
        }
        self.mark_dirty()?;
        let name = self.names.intern(path)?;
        let s = self.alloc_slot()?;
        self.init_slot(s, name, NONE, &d)?;
        self.roots.push(s);
        Ok(s)
    }

    /// Add `name` under directory `parent`. Contribution to ancestors is applied.
    pub fn add_child(&mut self, parent: u32, name: &[u8], mut d: EntryData) -> io::Result<u32> {
        if self.growth_blocked() {
            self.refused_adds += 1;
            return Err(io::Error::new(
                io::ErrorKind::StorageFull,
                "index footprint cap reached",
            ));
        }
        if d.kind == KIND_DIR && self.is_package(name) {
            d.flags |= FLAG_PACKAGE;
        }
        d.flags &= !FLAG_SECONDARY_LINK;
        d.private = d.private.min(d.alloc);
        self.mark_dirty()?;
        let nid = self.names.intern(name)?;
        let s = self.alloc_slot()?;
        self.init_slot(s, nid, parent, &d)?;
        // Link at the head of the parent's child list.
        let head = self.c_first.get(parent as usize);
        self.c_next.set(s as usize, head);
        if head != NONE {
            self.c_prev.set(head as usize, s);
        }
        self.c_first.set(parent as usize, s);
        self.attach(s);
        if self.log_changes {
            self.touched_log.push(s);
        }
        Ok(s)
    }

    fn unlink_from_parent(&mut self, s: u32) {
        let (p, n, pr) = (
            self.parent_of(s),
            self.c_next.get(s as usize),
            self.c_prev.get(s as usize),
        );
        if pr != NONE {
            self.c_next.set(pr as usize, n);
        } else if p != NONE {
            self.c_first.set(p as usize, n);
        }
        if n != NONE {
            self.c_prev.set(n as usize, pr);
        }
    }

    /// Replace the metadata of a live entry (same kind), keeping rollups exact.
    pub fn update(&mut self, s: u32, mut d: EntryData) {
        self.touch();
        let old = self.entry(s).d;
        d.flags = (d.flags & !(FLAG_PACKAGE | FLAG_SECONDARY_LINK)) | (old.flags & FLAG_PACKAGE);
        d.private = d.private.min(d.alloc);
        if d.kind == KIND_DIR {
            d.flags |= old.flags & FLAG_UNREADABLE;
            self.put_data(s, &d);
            return;
        }
        self.detach(s);
        self.put_data(s, &d);
        self.attach(s);
        if self.log_changes {
            self.touched_log.push(s);
        }
    }

    pub fn set_flag(&mut self, s: u32, flag: u8) {
        self.touch();
        let f = self.c_flags.get(s as usize);
        self.c_flags.set(s as usize, f | flag);
    }

    fn ancestors_apply(&mut self, s: u32, f: impl Fn(&mut Rollup)) {
        let mut p = self.parent_of(s);
        while p != NONE {
            if let Some(di) = self.dir_of(p) {
                let mut r = self.d_roll.get(di);
                f(&mut r);
                self.d_roll.set(di, r);
            }
            p = self.parent_of(p);
        }
    }

    fn unique_delta(&mut self, dir_slot: u32, delta: i64) {
        if let Some(di) = self.dir_of(dir_slot) {
            let mut r = self.d_roll.get(di);
            apply(&mut r.unique, delta);
            self.d_roll.set(di, r);
        }
    }

    /// Apply (sign = +1) or remove (-1) this entry's own contribution.
    fn contribute(&mut self, s: u32, sign: i64) {
        let d = self.entry(s).d;
        let (lg, al, pr) = (
            d.logical as i64 * sign,
            d.alloc as i64 * sign,
            d.private as i64 * sign,
        );
        self.ancestors_apply(s, |r| {
            apply(&mut r.logical, lg);
            apply(&mut r.alloc, al);
            apply(&mut r.unique, pr);
            apply(&mut r.files, sign);
        });
        let shared = d.alloc - d.private;
        if shared > 0 {
            if sign > 0 {
                self.clone_join(s, shared, true);
            } else {
                self.clone_leave(s);
            }
        }
    }

    /// Parent chain of `s`, nearest first (not including `s`).
    fn ancestors(&self, s: u32) -> Vec<u32> {
        let mut v = Vec::new();
        let mut p = self.parent_of(s);
        while p != NONE {
            v.push(p);
            p = self.parent_of(p);
        }
        v
    }

    /// Add `delta` to the `unique` rollup of every ancestor of `s`.
    fn add_unique_up(&mut self, s: u32, delta: i64) {
        let mut p = self.parent_of(s);
        while p != NONE {
            self.unique_delta(p, delta);
            p = self.parent_of(p);
        }
    }

    /// Shared bytes (allocated beyond private) `s` holds, read from its columns.
    fn shared_of(&self, s: u32) -> u64 {
        self.c_alloc
            .get(s as usize)
            .saturating_sub(self.c_private.get(s as usize))
    }

    /// `s` joins the clone group of its key. With `apply`, the rollups of its folders move by
    /// what the group adds (a first member: its shared bytes for every ancestor; later ones:
    /// the new maximum for folders already holding the group, the maximum for new folders).
    /// Without it only membership is recorded (rebuilding after open; rollups are on disk).
    /// A group is a table entry until a second member arrives, then a full `Group`.
    pub fn clone_rollups_need_rewrite(&self) -> bool {
        self.legacy_clone_keys
    }

    fn clone_key(&self, s: u32) -> CloneKey {
        let d = self.entry(s).d;
        if self.legacy_clone_keys {
            CloneKey {
                id: if d.cloneid == 0 {
                    (1 << 63) | d.fileid
                } else {
                    d.cloneid
                },
                dev: 0,
                fallback: false,
            }
        } else {
            CloneKey::of(&d)
        }
    }

    fn clone_join(&mut self, s: u32, shared: u64, apply: bool) {
        let key = self.clone_key(s);
        if !self.clone_multi.contains_key(&key) {
            match self.clone_solo.get(&key).copied() {
                None => {
                    self.clone_solo.insert(key, s);
                    if apply {
                        self.add_unique_up(s, shared as i64);
                    }
                    return;
                }
                Some(m) => {
                    // Second member: promote. The first one's contribution is already in the
                    // rollups; only its membership moves into the group.
                    self.clone_solo.remove(&key);
                    let m_shared = self.shared_of(m);
                    let mut g = Group {
                        members: vec![(m, m_shared)],
                        per_dir: HashMap::new(),
                        s: m_shared,
                    };
                    for a in self.ancestors(m) {
                        *g.per_dir.entry(a).or_insert(0) += 1;
                    }
                    self.clone_multi.insert(key, g);
                }
            }
        }
        let ancestors = self.ancestors(s);
        let Some(g) = self.clone_multi.get_mut(&key) else {
            return;
        };
        g.members.push((s, shared));
        let (old, new) = (g.s, g.s.max(shared));
        let mut deltas: Vec<(u32, i64)> = Vec::new();
        if new != old {
            deltas.extend(g.per_dir.keys().map(|&d| (d, (new - old) as i64)));
            g.s = new;
        }
        for a in ancestors {
            let c = g.per_dir.entry(a).or_insert(0);
            *c += 1;
            if *c == 1 {
                deltas.push((a, g.s as i64));
            }
        }
        if apply {
            for (d, v) in deltas {
                self.unique_delta(d, v);
            }
        }
    }

    /// `s` leaves its clone group (its columns still hold the values it joined with). The
    /// last member takes the group's table entry with it; a group left with one member goes
    /// back to a table entry.
    fn clone_leave(&mut self, s: u32) {
        let key = self.clone_key(s);
        if let Some(&m) = self.clone_solo.get(&key) {
            if m == s {
                self.clone_solo.remove(&key);
                let shared = self.shared_of(s);
                self.add_unique_up(s, -(shared as i64));
            }
            return;
        }
        let ancestors = self.ancestors(s);
        let Some(g) = self.clone_multi.get_mut(&key) else {
            return;
        };
        let mut deltas: Vec<(u32, i64)> = Vec::new();
        for a in ancestors {
            if let Some(c) = g.per_dir.get_mut(&a) {
                *c -= 1;
                if *c == 0 {
                    g.per_dir.remove(&a);
                    deltas.push((a, -(g.s as i64)));
                }
            }
        }
        g.members.retain(|m| m.0 != s);
        let new = g.members.iter().map(|m| m.1).max().unwrap_or(0);
        if new != g.s {
            deltas.extend(g.per_dir.keys().map(|&d| (d, -((g.s - new) as i64))));
            g.s = new;
        }
        if g.members.len() <= 1 {
            let survivor = g.members.first().map(|m| m.0);
            self.clone_multi.remove(&key);
            if let Some(m) = survivor {
                self.clone_solo.insert(key, m);
            }
        }
        for (d, v) in deltas {
            self.unique_delta(d, v);
        }
    }

    /// Join rollups (and the hard-link group) after the entry is placed.
    fn attach(&mut self, s: u32) {
        let d = self.entry(s).d;
        if d.kind == KIND_DIR {
            return;
        }
        if d.nlink > 1 && d.fileid != 0 {
            let v = self.links.entry((d.dev, d.fileid)).or_default();
            v.push(s);
            if v.len() > 1 {
                let f = self.c_flags.get(s as usize);
                self.c_flags.set(s as usize, f | FLAG_SECONDARY_LINK);
                return;
            }
        }
        self.contribute(s, 1);
    }

    /// Leave rollups (handing hard-link ownership on) before the entry changes or goes.
    fn detach(&mut self, s: u32) {
        let d = self.entry(s).d;
        if d.kind == KIND_DIR {
            return;
        }
        if d.nlink > 1 && d.fileid != 0 {
            let key = (d.dev, d.fileid);
            if let Some(v) = self.links.get_mut(&key) {
                if let Some(pos) = v.iter().position(|&x| x == s) {
                    v.remove(pos);
                    let next = v.first().copied();
                    if v.is_empty() {
                        self.links.remove(&key);
                    }
                    if pos == 0 {
                        self.contribute(s, -1);
                        if let Some(n) = next {
                            let f = self.c_flags.get(n as usize);
                            self.c_flags.set(n as usize, f & !FLAG_SECONDARY_LINK);
                            self.contribute(n, 1);
                        }
                    }
                    return;
                }
            }
        }
        self.contribute(s, -1);
    }

    /// Remove an entry and its whole subtree. Returns the removed count.
    pub fn remove_subtree(&mut self, s: u32) -> usize {
        self.touch();
        self.unlink_from_parent(s);
        self.remove_detached_subtree(s)
    }

    /// As `remove_subtree`, but the caller already unlinked `s` from its parent.
    pub fn remove_detached_subtree(&mut self, s: u32) -> usize {
        self.touch();
        let mut stack = vec![s];
        let mut all = Vec::new();
        while let Some(x) = stack.pop() {
            all.push(x);
            stack.extend(self.children(x));
        }
        // Detach in DFS order while every ancestor is still in place.
        for &x in &all {
            self.detach(x);
        }
        for &x in &all {
            if let Some(di) = self.dir_of(x) {
                self.d_owner.set(di, NONE);
                self.dir_free.push(di as u32);
            }
            self.release_name(self.c_name.get(x as usize));
            self.c_meta.set(x as usize, 0);
            self.c_dir.set(x as usize, NONE);
            self.free.push(x);
            if self.log_changes {
                self.removed_log.push(x);
            }
        }
        if let Some(i) = self.roots.iter().position(|&r| r == s) {
            self.roots.remove(i);
        }
        self.live -= all.len();
        all.len()
    }

    /// Drop children not in `keep`; returns the removed slots.
    pub fn prune_children(&mut self, dir: u32, keep: &HashSet<u32>) -> Vec<u32> {
        let gone: Vec<u32> = self
            .children(dir)
            .into_iter()
            .filter(|c| !keep.contains(c))
            .collect();
        for &g in &gone {
            self.unlink_from_parent(g);
            self.remove_detached_subtree(g);
        }
        gone
    }

    // ------------------------------------------------------------ mount boundaries

    /// Parent slot of `s` (`NONE` for a root).
    #[cfg(test)]
    pub fn parent_slot(&self, s: u32) -> u32 {
        self.c_parent.get(s as usize)
    }

    pub fn path_of(&self, s: u32) -> Vec<u8> {
        let mut parts = Vec::new();
        let mut x = s;
        while x != NONE {
            parts.push(x);
            x = self.parent_of(x);
        }
        let mut out = Vec::new();
        for (i, &p) in parts.iter().rev().enumerate() {
            let n = self.name_of(p);
            if i > 0 && out != b"/" {
                out.push(b'/');
            }
            out.extend_from_slice(n);
        }
        out
    }

    pub fn depth(&self, s: u32) -> usize {
        let mut n = 0;
        let mut x = self.parent_of(s);
        while x != NONE {
            n += 1;
            x = self.parent_of(x);
        }
        n
    }

    pub fn find_child(&self, dir: u32, name: &[u8]) -> Option<u32> {
        let mut c = self.c_first.get(dir as usize);
        while c != NONE {
            if self.name_of(c) == name {
                return Some(c);
            }
            c = self.c_next.get(c as usize);
        }
        None
    }

    /// Resolve an absolute path to a slot.
    pub fn lookup(&self, path: &[u8]) -> Option<u32> {
        let path = if path.len() > 1 {
            path.strip_suffix(b"/").unwrap_or(path)
        } else {
            path
        };
        for &r in &self.roots {
            let rp = self.name_of(r);
            if path == rp {
                return Some(r);
            }
            let rest = if rp == b"/" {
                path.strip_prefix(b"/")
            } else {
                path.strip_prefix(rp).and_then(|x| x.strip_prefix(b"/"))
            };
            if let Some(rest) = rest {
                let mut cur = r;
                for comp in rest.split(|&b| b == b'/').filter(|c| !c.is_empty()) {
                    cur = self.find_child(cur, comp)?;
                }
                return Some(cur);
            }
        }
        None
    }

    pub fn root_containing(&self, path: &[u8]) -> Option<u32> {
        self.roots.iter().copied().find(|&r| {
            let rp = self.name_of(r);
            rp == b"/" || path == rp || (path.starts_with(rp) && path.get(rp.len()) == Some(&b'/'))
        })
    }

    pub fn live_slots(&self) -> impl Iterator<Item = u32> + '_ {
        (0..self.nslots)
            .filter(|&i| self.c_meta.get(i) & LIVE != 0)
            .map(|i| i as u32)
    }

    /// Name id of a slot (for per-name score caches).
    pub fn name_id(&self, s: u32) -> u32 {
        self.c_name.get(s as usize)
    }

    pub fn take_logs(&mut self) -> (Vec<u32>, Vec<u32>) {
        (
            std::mem::take(&mut self.removed_log),
            std::mem::take(&mut self.touched_log),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file() -> EntryData {
        EntryData {
            kind: KIND_FILE,
            nlink: 1,
            logical: 10,
            alloc: 4096,
            private: 4096,
            ..EntryData::default()
        }
    }

    fn store_with(dir: &Path, n: usize) -> (Store, u32) {
        let mut s = Store::create(Some(dir), Vec::new(), 7).unwrap();
        let root = s.add_root(b"/r", EntryData::default()).unwrap();
        for i in 0..n {
            s.add_child(root, format!("f{i}").as_bytes(), file())
                .unwrap();
        }
        (s, root)
    }

    #[test]
    fn failed_commit_keeps_successful_flush_accounting() {
        use crate::limits::{WriteBudget, WRITE_WINDOW};
        use std::time::{Duration, Instant};
        for failure in ["c_parent", "header"] {
            let dir = tempfile::tempdir().unwrap();
            let (mut store, _) = store_with(dir.path(), 10);
            // Creating the store wrote its dirty header once; that is not part of the commit.
            store.take_flushed_bytes();
            let expected = store.pending_write_bytes();
            assert!(store
                .commit_with(1, |column| {
                    if column == failure {
                        Err(io::Error::other("injected flush failure"))
                    } else {
                        Ok(())
                    }
                })
                .is_err());
            let flushed = store.take_flushed_bytes();
            assert!(flushed > 0 && flushed < expected);
            let now = Instant::now();
            let mut budget = WriteBudget::new(Some(expected * 2));
            budget.record(now, flushed);
            store.commit(1).unwrap();
            let remaining = store.take_flushed_bytes();
            assert_eq!(flushed + remaining, expected);
            budget.record(now + Duration::from_secs(2), remaining);
            assert_eq!(budget.bytes_in_window(now + WRITE_WINDOW), remaining);
            assert_eq!(
                budget.bytes_in_window(now + WRITE_WINDOW + Duration::from_secs(2)),
                0
            );
        }
    }

    #[test]
    fn a_commit_reports_the_pages_it_wrote_and_an_idle_one_only_the_header() {
        let dir = tempfile::tempdir().unwrap();
        let page = crate::col::page_size() as u64;
        let (mut s, root) = store_with(dir.path(), 50_000);
        assert!(s.pending_write_bytes() > 10 * page);
        s.commit(1).unwrap();
        let first = s.take_flushed_bytes();
        assert!(first > 10 * page, "first={first}");
        assert_eq!(s.take_flushed_bytes(), 0, "taking resets the counter");
        s.commit(2).unwrap();
        assert_eq!(
            s.take_flushed_bytes(),
            page,
            "nothing dirty: the header page"
        );
        s.add_child(root, b"one-more", file()).unwrap();
        let pending = s.pending_write_bytes();
        s.commit(3).unwrap();
        let small = s.take_flushed_bytes();
        assert!(small >= 2 * page, "small={small}");
        assert!(small < first, "one change must cost less than the crawl");
        assert!(
            small >= pending,
            "the commit writes at least what was pending"
        );
    }

    #[test]
    fn the_footprint_cap_refuses_adds_and_keeps_what_is_stored() {
        let dir = tempfile::tempdir().unwrap();
        let (mut s, root) = store_with(dir.path(), 10);
        let before = s.len();
        s.set_max_footprint(Some(1));
        assert_eq!(
            s.add_child(root, b"already-full", file())
                .unwrap_err()
                .kind(),
            io::ErrorKind::StorageFull
        );
        assert_eq!(s.len(), before);
        let mut refused = 1;
        for i in 0..2 * FOOTPRINT_CHECK_EVERY as usize {
            match s.add_child(root, format!("g{i}").as_bytes(), file()) {
                Ok(_) => {}
                Err(e) => {
                    assert_eq!(e.kind(), io::ErrorKind::StorageFull);
                    refused += 1;
                }
            }
        }
        assert!(refused > 0);
        assert_eq!(s.refused_adds(), refused);
        assert!(s.len() >= before, "nothing already stored was dropped");
        assert!(s.len() < before + 2 * FOOTPRINT_CHECK_EVERY as usize);
        s.set_max_footprint(None);
        assert!(s.add_child(root, b"allowed-again", file()).is_ok());
    }

    #[test]
    fn the_footprint_grows_with_the_entries() {
        let dir = tempfile::tempdir().unwrap();
        let (small, _) = store_with(dir.path(), 10);
        let dir2 = tempfile::tempdir().unwrap();
        let (big, _) = store_with(dir2.path(), 1000);
        assert!(big.footprint_bytes() > small.footprint_bytes());
        assert!(big.footprint_bytes() >= 1000 * SLOT_BYTES);
    }
}

/// Test-only read of the index's own state file (store.rs is the sanctioned opener).
#[cfg(test)]
fn read_state_file(path: &std::path::Path) -> Vec<u8> {
    std::fs::read(path).unwrap()
}

#[cfg(test)]
mod p1_tests;
