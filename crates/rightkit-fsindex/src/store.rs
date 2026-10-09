//! The tree on memory-mapped fixed-width columns, with incrementally
//! maintained folder rollups, hard-link ownership and APFS clone groups.
//!
//! Per-slot columns (names, parent, first-child/sibling links, sizes, flags)
//! and per-folder rollup columns live in mapped files, so only touched pages are
//! resident. Updates are in-place column writes plus slot appends; slots freed
//! by deletions are reused and `Index::compact` rebuilds the store.
//!
//! Crash safety: a header holds a `dirty` flag and a generation. The flag is
//! made durable (msync + fsync) before the first column write after a commit;
//! `commit` flushes every column, then writes the header clean with the next
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

const LIVE: u8 = 0x80;
/// Hard-linked file or one with shared (clone) extents: the only entries `rebuild_aux` must inspect.
const SPECIAL: u8 = 0x40;
const MAGIC: &[u8; 8] = b"RKFSIX02";
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

pub fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
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
    clones: HashMap<u64, Group>,
    package_exts: Vec<Vec<u8>>,
    live: usize,
    fingerprint: u64,
    generation: u64,
    dirty: bool,
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
    /// Fresh store. `dir` must exist for a file-backed store; `None` is anonymous (no persistence).
    pub fn create(dir: Option<&Path>, package_exts: Vec<Vec<u8>>, fingerprint: u64) -> io::Result<Store> {
        let hdr = match dir {
            Some(d) => {
                let f = OpenOptions::new().read(true).write(true).create(true).truncate(true).open(d.join("header.bin"))?;
                f.set_len(HDR_LEN.max(4096) as u64)?;
                Some(unsafe { MmapMut::map_mut(&f)? })
            }
            None => None,
        };
        let names = NameStore::create(dir)?;
        let n = 1024;
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
            Col::<u32>::create(dir, "d-owner.col", 256)?,
            Col::<Rollup>::create(dir, "d-roll.col", 256)?,
        );
        let mut s = Store::assemble(dir, hdr, names, (a, b, c, d, e, f, g, h, i, j, k, l, m, n2, o, p, q, r), package_exts, fingerprint);
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
            clones: HashMap::new(),
            package_exts,
            live: 0,
            fingerprint,
            generation: 0,
            dirty: false,
            log_changes: false,
            removed_log: Vec::new(),
            touched_log: Vec::new(),
        }
    }

    /// Reopen a committed store. `Ok(None)` when absent, dirty (torn), or for a different configuration.
    pub fn open_existing(dir: &Path, package_exts: Vec<Vec<u8>>, fingerprint: u64) -> io::Result<Option<(Store, OpenInfo)>> {
        let Ok(f) = OpenOptions::new().read(true).write(true).open(dir.join("header.bin")) else { return Ok(None) };
        if f.metadata()?.len() < HDR_LEN as u64 {
            return Ok(None);
        }
        let hdr = unsafe { MmapMut::map_mut(&f)? };
        if &hdr[0..8] != MAGIC || rd_u32(&hdr, H_DIRTY) != 0 || rd_u64(&hdr, H_FP) != fingerprint {
            return Ok(None);
        }
        let (nslots, ndirs) = (rd_u64(&hdr, H_NSLOTS) as usize, rd_u64(&hdr, H_NDIRS) as usize);
        let (ncount, buckets) = (rd_u64(&hdr, H_NAMES) as usize, rd_u64(&hdr, H_BUCKETS) as usize);
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
        s.hdr = Some(hdr);
        s.rebuild_aux();
        Ok(Some((s, OpenInfo { event_id, saved_at })))
    }

    /// Rebuild resident helpers (free lists, hard-link and clone groups) from the columns. Rollups are not touched.
    fn rebuild_aux(&mut self) {
        self.live = 0;
        self.free.clear();
        for s in (0..self.nslots).rev() {
            if self.c_meta.get(s) & LIVE != 0 {
                self.live += 1;
            } else {
                self.free.push(s as u32);
            }
        }
        self.dir_free = (0..self.ndirs).rev().filter(|&i| self.d_owner.get(i) == NONE).map(|i| i as u32).collect();
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
        for s in shared {
            let d = self.entry(s).d;
            self.group_join_membership(s, d.alloc - d.private);
        }
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
            h[0..8].copy_from_slice(MAGIC);
            h[H_DIRTY..H_DIRTY + 4].copy_from_slice(&1u32.to_le_bytes());
            h[H_FP..H_FP + 8].copy_from_slice(&self.fingerprint.to_le_bytes());
            h.flush()?;
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

    /// Flush every column, then publish a clean header with the next generation.
    pub fn commit(&mut self, event_id: u64) -> io::Result<()> {
        if self.hdr.is_none() {
            return Ok(());
        }
        self.names.flush()?;
        for r in [
            self.c_name.flush(),
            self.c_parent.flush(),
            self.c_first.flush(),
            self.c_next.flush(),
            self.c_prev.flush(),
            self.c_dir.flush(),
            self.c_meta.flush(),
            self.c_flags.flush(),
            self.c_nlink.flush(),
            self.c_mtime.flush(),
            self.c_dev.flush(),
            self.c_fileid.flush(),
            self.c_logical.flush(),
            self.c_alloc.flush(),
            self.c_private.flush(),
            self.c_clone.flush(),
            self.d_owner.flush(),
            self.d_roll.flush(),
        ] {
            r?;
        }
        self.generation += 1;
        let (gen, ns, nd, nn, nb) = (self.generation, self.nslots as u64, self.ndirs as u64, self.names.count as u64, self.names.buckets() as u64);
        let roots = self.roots.clone();
        let segs = self.names.seg_used.clone();
        let h = self.hdr.as_mut().unwrap();
        h[0..8].copy_from_slice(MAGIC);
        h[8..12].copy_from_slice(&2u32.to_le_bytes());
        h[H_FP..H_FP + 8].copy_from_slice(&self.fingerprint.to_le_bytes());
        h[H_GEN..H_GEN + 8].copy_from_slice(&gen.to_le_bytes());
        h[H_EVENT..H_EVENT + 8].copy_from_slice(&event_id.to_le_bytes());
        h[H_SAVED..H_SAVED + 8].copy_from_slice(&now_secs().to_le_bytes());
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
        h.flush()?;
        self.dirty = false;
        Ok(())
    }

    /// Drop clean mapped pages from this process's resident set (they refault on demand).
    pub fn trim(&self) {
        self.names.trim();
        for c in [&self.c_name, &self.c_parent, &self.c_first, &self.c_next, &self.c_prev, &self.c_dir, &self.c_mtime, &self.c_dev] {
            c.trim();
        }
        for c in [&self.c_meta, &self.c_flags] {
            c.trim();
        }
        self.c_nlink.trim();
        for c in [&self.c_fileid, &self.c_logical, &self.c_alloc, &self.c_private, &self.c_clone] {
            c.trim();
        }
        self.d_owner.trim();
        self.d_roll.trim();
    }

    /// The store directory moved (compaction swap).
    pub fn relocate(&mut self, dir: &Path) {
        self.dir = Some(dir.to_path_buf());
        self.names.relocate(dir);
    }

    /// Slots on the free list (reusable) as a fraction signal for compaction.
    pub fn free_slots(&self) -> usize {
        self.free.len()
    }

    // ---------------------------------------------------------------- accessors

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
        self.c_meta.set(i, LIVE | (d.kind & 3) | if special { SPECIAL } else { 0 });
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
        (self.c_meta.get(s as usize) & 3, self.c_flags.get(s as usize))
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
        self.dir_of(s).map_or(Rollup::default(), |d| self.d_roll.get(d))
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
                self.package_exts.iter().any(|e| e.len() == ext.len() && e.iter().zip(ext).all(|(a, b)| *a == b.to_ascii_lowercase()))
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

    fn init_slot(&mut self, s: u32, name: u32, parent: u32, d: &EntryData) -> io::Result<()> {
        let i = s as usize;
        self.c_name.set(i, name);
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
        let name = self.names.intern(path)?;
        let s = self.alloc_slot()?;
        self.init_slot(s, name, NONE, &d)?;
        self.roots.push(s);
        Ok(s)
    }

    /// Add `name` under directory `parent`. Contribution to ancestors is applied.
    pub fn add_child(&mut self, parent: u32, name: &[u8], mut d: EntryData) -> io::Result<u32> {
        if d.kind == KIND_DIR && self.is_package(name) {
            d.flags |= FLAG_PACKAGE;
        }
        d.flags &= !FLAG_SECONDARY_LINK;
        d.private = d.private.min(d.alloc);
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
        let (p, n, pr) = (self.parent_of(s), self.c_next.get(s as usize), self.c_prev.get(s as usize));
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

    fn clone_key(d: &EntryData) -> u64 {
        if d.cloneid != 0 {
            d.cloneid
        } else {
            (1u64 << 63) | d.fileid
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
        let (lg, al, pr) = (d.logical as i64 * sign, d.alloc as i64 * sign, d.private as i64 * sign);
        self.ancestors_apply(s, |r| {
            apply(&mut r.logical, lg);
            apply(&mut r.alloc, al);
            apply(&mut r.unique, pr);
            apply(&mut r.files, sign);
        });
        let shared = d.alloc - d.private;
        if shared > 0 {
            if sign > 0 {
                self.group_join(s, shared);
            } else {
                self.group_leave(s);
            }
        }
    }

    /// Group membership and per-folder counts only (no rollup change); used when reopening.
    fn group_join_membership(&mut self, s: u32, shared: u64) {
        let key = Self::clone_key(&self.entry(s).d);
        let mut dirs = Vec::new();
        let mut p = self.parent_of(s);
        while p != NONE {
            dirs.push(p);
            p = self.parent_of(p);
        }
        let g = self.clones.entry(key).or_default();
        g.members.push((s, shared));
        g.s = g.s.max(shared);
        for d in dirs {
            *g.per_dir.entry(d).or_insert(0) += 1;
        }
    }

    fn group_join(&mut self, s: u32, shared: u64) {
        let key = Self::clone_key(&self.entry(s).d);
        let mut ancestors = Vec::new();
        let mut p = self.parent_of(s);
        while p != NONE {
            ancestors.push(p);
            p = self.parent_of(p);
        }
        let g = self.clones.entry(key).or_default();
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
        for (d, v) in deltas {
            self.unique_delta(d, v);
        }
    }

    fn group_leave(&mut self, s: u32) {
        let key = Self::clone_key(&self.entry(s).d);
        let mut ancestors = Vec::new();
        let mut p = self.parent_of(s);
        while p != NONE {
            ancestors.push(p);
            p = self.parent_of(p);
        }
        let Some(g) = self.clones.get_mut(&key) else { return };
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
        if g.members.is_empty() {
            self.clones.remove(&key);
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
        let gone: Vec<u32> = self.children(dir).into_iter().filter(|c| !keep.contains(c)).collect();
        for &g in &gone {
            self.unlink_from_parent(g);
            self.remove_detached_subtree(g);
        }
        gone
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
        let path = if path.len() > 1 { path.strip_suffix(b"/").unwrap_or(path) } else { path };
        for &r in &self.roots {
            let rp = self.name_of(r);
            if path == rp {
                return Some(r);
            }
            let rest = if rp == b"/" { path.strip_prefix(b"/") } else { path.strip_prefix(rp).and_then(|x| x.strip_prefix(b"/")) };
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
        (0..self.nslots).filter(|&i| self.c_meta.get(i) & LIVE != 0).map(|i| i as u32)
    }

    /// Name id of a slot (for per-name score caches).
    pub fn name_id(&self, s: u32) -> u32 {
        self.c_name.get(s as usize)
    }

    pub fn take_logs(&mut self) -> (Vec<u32>, Vec<u32>) {
        (std::mem::take(&mut self.removed_log), std::mem::take(&mut self.touched_log))
    }
}
