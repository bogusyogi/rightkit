//! Interned name store on memory-mapped files.
//!
//! Names are appended into fixed-size segments and deduplicated through an
//! on-disk chained hash (bucket heads and next links are mapped columns), so
//! only touched pages are resident. A name id is stable for the life of the
//! store; garbage is dropped when the store is rebuilt (compaction).

use crate::col::Col;
use memmap2::MmapMut;
use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

/// Reads at most `max` bytes from the start of a regular file (never follows a final symlink
/// check beyond `symlink_metadata`). Lives here because this is a module the policy test in
/// tests/index.rs allows to open files; used only for the bounded `CACHEDIR.TAG` read.
pub fn read_file_head(path: &Path, max: usize) -> Option<Vec<u8>> {
    use std::io::Read;
    match std::fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_file() => {}
        _ => return None,
    }
    let f = std::fs::File::open(path).ok()?;
    let mut buf = Vec::with_capacity(max);
    f.take(max as u64).read_to_end(&mut buf).ok()?;
    Some(buf)
}

const SEG_SHIFT: u32 = 24;
const SEG: usize = 1 << SEG_SHIFT;
pub const MAX_SEGS: usize = 256;

/// One name segment: a mapping of at most `SEG` bytes. The last segment is sized to need and
/// grows (0.5.0); a full one keeps the length it had when the next segment started.
struct Seg {
    map: MmapMut,
    file: Option<File>,
}

/// Capacity (bytes) a segment of `cur` bytes grows to so that `need` bytes fit: doubling up to
/// 1 MiB, then `need` plus 12.5% rounded to 256 KiB, never above one segment.
fn seg_cap(cur: usize, need: usize) -> usize {
    const MIN: usize = 64 << 10;
    const ROUND: usize = 256 << 10;
    let want = if need <= (1 << 20) {
        need.max(cur.saturating_mul(2)).max(MIN).next_power_of_two()
    } else {
        (need + need / 8).div_ceil(ROUND) * ROUND
    };
    want.min(SEG).max(need)
}

pub struct NameStore {
    dir: Option<PathBuf>,
    segs: Vec<Seg>,
    pub seg_used: Vec<u32>,
    /// `seg_used` as of the last flush: bytes below it are already written out.
    seg_flushed: Vec<u32>,
    off: Col<u32>,
    len: Col<u16>,
    next: Col<u32>,
    heads: Col<u32>,
    pub count: usize,
}

fn hash(b: &[u8]) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for &x in b {
        h ^= x as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h ^ (h >> 29)
}

impl NameStore {
    #[cfg(test)]
    pub fn create(dir: Option<&Path>) -> io::Result<NameStore> {
        Self::create_sized(dir, 0, 0)
    }

    /// As `create`, with the per-name columns and the hash heads sized for `names` names (and
    /// `_name_bytes` bytes of text, which the segments grow into on demand).
    pub fn create_sized(
        dir: Option<&Path>,
        names: usize,
        _name_bytes: u64,
    ) -> io::Result<NameStore> {
        if let Some(d) = dir {
            for e in std::fs::read_dir(d)?.flatten() {
                if e.file_name().to_string_lossy().starts_with("names-") {
                    let _ = std::fs::remove_file(e.path());
                }
            }
        }
        let per_name = (names + names / 16).max(1024);
        // The heads are a power of two (the bucket is `hash & mask`, as in existing stores)
        // with at least one bucket more than names, so the first intern does not rebuild them.
        let buckets = (names + 1).next_power_of_two().max(1024);
        Ok(NameStore {
            dir: dir.map(|d| d.to_path_buf()),
            segs: Vec::new(),
            seg_used: Vec::new(),
            seg_flushed: Vec::new(),
            off: Col::create(dir, "names-off.col", per_name)?,
            len: Col::create(dir, "names-len.col", per_name)?,
            next: Col::create(dir, "names-next.col", per_name)?,
            heads: Col::create(dir, "names-heads.col", buckets)?,
            count: 0,
        })
    }

    pub fn open(
        dir: &Path,
        count: usize,
        seg_used: Vec<u32>,
        buckets: usize,
    ) -> io::Result<NameStore> {
        let mut segs = Vec::new();
        for (i, &used) in seg_used.iter().enumerate() {
            let f = OpenOptions::new()
                .read(true)
                .write(true)
                .open(dir.join(format!("names-{i}.seg")))?;
            // Existing stores have full-size segments; 0.5.0 sizes the last one to need. Either
            // opens as long as the file holds the bytes the header says are used.
            if f.metadata()?.len() < u64::from(used).max(1) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "short name segment",
                ));
            }
            let map = unsafe { MmapMut::map_mut(&f)? };
            segs.push(Seg { map, file: Some(f) });
        }
        let seg_flushed = seg_used.clone();
        Ok(NameStore {
            dir: Some(dir.to_path_buf()),
            segs,
            seg_used,
            seg_flushed,
            off: Col::open(dir, "names-off.col", count)?,
            len: Col::open(dir, "names-len.col", count)?,
            next: Col::open(dir, "names-next.col", count)?,
            heads: Col::open(dir, "names-heads.col", buckets)?,
            count,
        })
    }

    pub fn buckets(&self) -> usize {
        self.heads.cap()
    }

    /// Resident bytes of the name segments and the hash columns.
    pub fn resident_bytes(&self) -> usize {
        self.segs
            .iter()
            .map(|s| crate::col::resident_bytes(s.map.as_ptr(), s.map.len()))
            .sum::<usize>()
            + self.off.resident_bytes()
            + self.len.resident_bytes()
            + self.next.resident_bytes()
            + self.heads.resident_bytes()
    }

    pub fn trim(&mut self) {
        for s in self.segs.iter_mut() {
            if let Some(f) = &s.file {
                if let Ok(m) = unsafe { MmapMut::map_mut(f) } {
                    s.map = m;
                }
            }
        }
        self.off.trim();
        self.len.trim();
        self.next.trim();
        self.heads.trim();
    }

    /// Drop the resident pages of the hash chains (`next`, `heads`), keeping the segments and
    /// the per-name offset and length columns: what a search reads.
    #[allow(dead_code)]
    pub fn trim_cold(&mut self) {
        self.next.trim();
        self.heads.trim();
    }

    /// Bytes the name files span on disk: segment mappings, per-name columns, hash heads.
    pub fn capacity_bytes(&self) -> u64 {
        self.segs.iter().map(|s| s.map.len() as u64).sum::<u64>()
            + self.off.capacity_bytes()
            + self.len.capacity_bytes()
            + self.next.capacity_bytes()
            + self.heads.capacity_bytes()
    }

    /// Pages of the name segments written since the last flush: the partly filled page the
    /// last flush ended in is written again, then every page the appended names reach.
    fn seg_dirty_bytes(&self) -> u64 {
        let page = crate::col::page_size() as u64;
        self.seg_used
            .iter()
            .zip(&self.seg_flushed)
            .map(|(&used, &done)| {
                let (used, done) = (u64::from(used), u64::from(done));
                if used > done {
                    (used.div_ceil(page) - done / page) * page
                } else {
                    0
                }
            })
            .sum()
    }

    /// Bytes the next `flush` writes (segments plus the hash columns).
    pub fn dirty_bytes(&self) -> u64 {
        self.seg_dirty_bytes()
            + self.off.dirty_bytes()
            + self.len.dirty_bytes()
            + self.next.dirty_bytes()
            + self.heads.dirty_bytes()
    }

    /// Whether `flush` has nothing to write.
    pub fn is_clean(&self) -> bool {
        self.seg_dirty_bytes() == 0
            && self.off.is_clean()
            && self.len.is_clean()
            && self.next.is_clean()
            && self.heads.is_clean()
    }

    /// Bytes the names occupy: appended name bytes plus the three per-name columns and the
    /// hash heads (their used part, not the doubled capacity).
    pub fn footprint_bytes(&self) -> u64 {
        let per_name = std::mem::size_of::<u32>() * 2 + std::mem::size_of::<u16>();
        self.seg_used.iter().map(|&u| u64::from(u)).sum::<u64>()
            + (self.count * per_name) as u64
            + (self.heads.cap() * std::mem::size_of::<u32>()) as u64
    }

    /// Conservative growth bound: every incoming entry contributes a distinct name.
    pub fn additional_footprint(&self, entries: usize, name_bytes: u64) -> u64 {
        let count = self.count.saturating_add(entries);
        let heads = count
            .checked_next_power_of_two()
            .unwrap_or(usize::MAX)
            .max(self.heads.cap());
        name_bytes
            .saturating_add((entries as u64).saturating_mul(10))
            .saturating_add((heads.saturating_sub(self.heads.cap()) as u64).saturating_mul(4))
    }

    /// Account each successful flush immediately, even if a later mapping fails. Only the
    /// pages written since the last flush are synced; a segment nothing was appended to is
    /// not touched.
    pub fn flush(&mut self, bytes: &mut u64) -> io::Result<()> {
        let page = crate::col::page_size() as u64;
        for ((seg, &used32), flushed) in self
            .segs
            .iter()
            .zip(&self.seg_used)
            .zip(self.seg_flushed.iter_mut())
        {
            let (used, done) = (u64::from(used32), u64::from(*flushed));
            if used > done {
                let first = (done / page) * page;
                let end = (used.div_ceil(page) * page).min(seg.map.len() as u64);
                seg.map
                    .flush_range(first as usize, (end - first) as usize)?;
                *bytes += (used.div_ceil(page) - done / page) * page;
                *flushed = used32;
            }
        }
        let result = self.off.flush();
        *bytes += self.off.take_partial_flush_bytes();
        *bytes += result?;
        let result = self.len.flush();
        *bytes += self.len.take_partial_flush_bytes();
        *bytes += result?;
        let result = self.next.flush();
        *bytes += self.next.take_partial_flush_bytes();
        *bytes += result?;
        let result = self.heads.flush();
        *bytes += self.heads.take_partial_flush_bytes();
        *bytes += result?;
        Ok(())
    }

    /// Start a segment sized for a first name of `need` bytes.
    fn new_seg(&mut self, need: usize) -> io::Result<()> {
        if self.segs.len() >= MAX_SEGS {
            return Err(io::Error::other("name store full (4 GiB)"));
        }
        let size = seg_cap(0, need);
        let seg = match &self.dir {
            Some(d) => {
                let f = OpenOptions::new()
                    .read(true)
                    .write(true)
                    .create(true)
                    .truncate(true)
                    .open(d.join(format!("names-{}.seg", self.segs.len())))?;
                f.set_len(size as u64)?;
                let map = unsafe { MmapMut::map_mut(&f)? };
                Seg { map, file: Some(f) }
            }
            None => Seg {
                map: MmapMut::map_anon(size)?,
                file: None,
            },
        };
        self.segs.push(seg);
        self.seg_used.push(0);
        self.seg_flushed.push(0);
        Ok(())
    }

    /// Grow segment `i` so that `need` bytes fit (the offsets of what is stored do not move).
    fn grow_seg(&mut self, i: usize, need: usize) -> io::Result<()> {
        let size = seg_cap(self.segs[i].map.len(), need);
        let seg = &mut self.segs[i];
        match &seg.file {
            Some(f) => {
                f.set_len(size as u64)?;
                seg.map = unsafe { MmapMut::map_mut(f)? };
            }
            None => {
                let mut m = MmapMut::map_anon(size)?;
                let used = self.seg_used[i] as usize;
                m[..used].copy_from_slice(&seg.map[..used]);
                seg.map = m;
            }
        }
        Ok(())
    }

    pub fn relocate(&mut self, dir: &Path) {
        if self.dir.is_some() {
            self.dir = Some(dir.to_path_buf());
        }
    }

    pub fn len(&self) -> usize {
        self.count
    }

    pub fn get(&self, id: u32) -> &[u8] {
        let off = self.off.get(id as usize);
        let len = self.len.get(id as usize) as usize;
        let seg = (off >> SEG_SHIFT) as usize;
        let at = (off as usize) & (SEG - 1);
        &self.segs[seg].map[at..at + len]
    }

    fn rebuild_heads(&mut self, buckets: usize) -> io::Result<()> {
        self.heads = Col::create(self.dir.as_deref(), "names-heads.col", buckets)?;
        let mask = self.heads.cap() as u64 - 1;
        for id in 0..self.count {
            let b = (hash(self.get(id as u32)) & mask) as usize;
            let h = self.heads.get(b);
            self.next.set(id, h);
            self.heads.set(b, id as u32 + 1);
        }
        Ok(())
    }

    /// The id of `name` if it is already interned. Read-only twin of the lookup in `intern`.
    #[cfg(test)]
    pub fn find(&self, name: &[u8]) -> Option<u32> {
        let name = &name[..name.len().min(u16::MAX as usize)];
        let h = hash(name);
        let mask = self.heads.cap() as u64 - 1;
        let mut cur = self.heads.get((h & mask) as usize);
        while cur != 0 {
            let id = cur - 1;
            if self.get(id) == name {
                return Some(id);
            }
            cur = self.next.get(id as usize);
        }
        None
    }

    pub fn intern(&mut self, name: &[u8]) -> io::Result<u32> {
        let name = &name[..name.len().min(u16::MAX as usize)];
        let h = hash(name);
        let mask = self.heads.cap() as u64 - 1;
        let mut cur = self.heads.get((h & mask) as usize);
        while cur != 0 {
            let id = cur - 1;
            if self.get(id) == name {
                return Ok(id);
            }
            cur = self.next.get(id as usize);
        }
        if self.count >= self.heads.cap() {
            self.next.ensure(self.count + 1)?;
            self.rebuild_heads(self.heads.cap() * 2)?;
        }
        if self.segs.is_empty() || self.seg_used[self.segs.len() - 1] as usize + name.len() > SEG {
            self.new_seg(name.len())?;
        } else {
            let last = self.segs.len() - 1;
            let want = self.seg_used[last] as usize + name.len();
            if want > self.segs[last].map.len() {
                self.grow_seg(last, want)?;
            }
        }
        let s = self.segs.len() - 1;
        let at = self.seg_used[s] as usize;
        self.segs[s].map[at..at + name.len()].copy_from_slice(name);
        self.seg_used[s] += name.len() as u32;
        let id = self.count;
        self.off.ensure(id + 1)?;
        self.len.ensure(id + 1)?;
        self.next.ensure(id + 1)?;
        self.off.set(id, ((s as u32) << SEG_SHIFT) | at as u32);
        self.len.set(id, name.len() as u16);
        let b = (hash(name) & (self.heads.cap() as u64 - 1)) as usize;
        let head = self.heads.get(b);
        self.next.set(id, head);
        self.heads.set(b, id as u32 + 1);
        self.count += 1;
        Ok(id as u32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segment_capacity_follows_need_and_never_exceeds_a_segment() {
        assert_eq!(seg_cap(0, 10), 64 << 10);
        assert_eq!(seg_cap(64 << 10, (64 << 10) + 1), 128 << 10);
        let five = 5usize << 20;
        assert_eq!(
            seg_cap(0, five),
            (five + five / 8).div_ceil(256 << 10) * (256 << 10)
        );
        assert!(seg_cap(15 << 20, (16 << 20) - 1) <= SEG);
        assert!(seg_cap(15 << 20, (16 << 20) - 1) >= (16 << 20) - 1);
        for need in [1usize, 4_096, 70_000, 1 << 20, 5 << 20, 12 << 20, SEG] {
            let cap = seg_cap(need / 2, need);
            assert!(cap >= need && cap <= SEG, "need {need} cap {cap}");
        }
    }

    #[test]
    fn a_new_store_has_a_small_segment_that_grows_with_the_names_and_reopens() {
        let dir = tempfile::tempdir().unwrap();
        let mut n = NameStore::create(Some(dir.path())).unwrap();
        for i in 0..2000 {
            n.intern(format!("name-{i}").as_bytes()).unwrap();
        }
        let seg0 = std::fs::metadata(dir.path().join("names-0.seg"))
            .unwrap()
            .len();
        assert!(
            seg0 < (1 << 20),
            "sized to need, not a whole segment: {seg0}"
        );
        // Grow past the first size.
        let long = vec![b'x'; 60_000];
        for i in 0..40 {
            let mut name = long.clone();
            name.extend_from_slice(format!("{i}").as_bytes());
            n.intern(&name).unwrap();
        }
        let grown = std::fs::metadata(dir.path().join("names-0.seg"))
            .unwrap()
            .len();
        assert!(grown > seg0 && grown < SEG as u64);
        let mut bytes = 0;
        n.flush(&mut bytes).unwrap();
        assert!(bytes > 0);
        let mut again = 0;
        n.flush(&mut again).unwrap();
        assert_eq!(again, 0, "a second flush writes nothing");
        let (count, used, buckets) = (n.count, n.seg_used.clone(), n.buckets());
        drop(n);
        let mut n = NameStore::open(dir.path(), count, used, buckets).unwrap();
        assert_eq!(n.get(7), b"name-7");
        assert_eq!(n.find(b"name-1999"), Some(1999));
        // Appending after a reopen keeps growing the same segment.
        for i in 0..40 {
            let mut name = long.clone();
            name.extend_from_slice(format!("later{i}").as_bytes());
            n.intern(&name).unwrap();
        }
        assert_eq!(n.get(7), b"name-7");
    }

    #[test]
    fn a_full_size_segment_from_an_older_store_still_opens() {
        let dir = tempfile::tempdir().unwrap();
        let mut n = NameStore::create(Some(dir.path())).unwrap();
        for i in 0..100 {
            n.intern(format!("n{i}").as_bytes()).unwrap();
        }
        let mut bytes = 0;
        n.flush(&mut bytes).unwrap();
        let (count, used, buckets) = (n.count, n.seg_used.clone(), n.buckets());
        drop(n);
        let f = OpenOptions::new()
            .read(true)
            .write(true)
            .open(dir.path().join("names-0.seg"))
            .unwrap();
        f.set_len(SEG as u64).unwrap();
        drop(f);
        let n = NameStore::open(dir.path(), count, used, buckets).unwrap();
        assert_eq!(n.get(42), b"n42");
        assert!(n.capacity_bytes() >= SEG as u64);
    }

    #[test]
    fn a_presized_store_needs_no_head_rebuild_for_its_names() {
        let dir = tempfile::tempdir().unwrap();
        let mut n = NameStore::create_sized(Some(dir.path()), 5000, 100_000).unwrap();
        let b0 = n.buckets();
        assert!(b0 > 5000 && b0.is_power_of_two());
        for i in 0..5000 {
            n.intern(format!("p{i}").as_bytes()).unwrap();
        }
        assert_eq!(n.buckets(), b0);
    }
}
