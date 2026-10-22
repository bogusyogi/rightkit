//! Fixed-width memory-mapped column. File-backed (sparse, zero-filled growth)
//! when a directory is given, anonymous otherwise.
//!
//! Growth is bounded (0.5.0): up to `GROW_FLOOR` elements a column doubles, above it a column
//! grows to 12.5% more than the element that did not fit, so the unused tail of a settled
//! column stays under one eighth instead of up to one half.

use memmap2::MmapMut;
use std::fs::{File, OpenOptions};
use std::io;
use std::marker::PhantomData;
use std::path::Path;
use std::sync::OnceLock;

pub struct Col<T: Copy> {
    file: Option<File>,
    map: MmapMut,
    cap: usize,
    /// One bit per memory page of a file-backed mapping, set by `set` and cleared by `flush`:
    /// the pages the next flush has to write. Empty for anonymous columns.
    dirty: Vec<u64>,
    /// log2 of the page size the bitmap is kept in.
    shift: u32,
    /// The file was extended since the last flush (its length is not yet published).
    grown: bool,
    partial_flush_bytes: u64,
    _t: PhantomData<T>,
}

/// Elements up to which a column still doubles when it is full.
pub(crate) const GROW_FLOOR: usize = 1 << 16;

/// Granule the capacity of a grown column is rounded up to (elements).
const GROW_ROUND: usize = 1 << 12;

#[cfg(test)]
thread_local! {
    /// Test hook: grow by doubling, as 0.5.0 did, so a test can measure before and after.
    pub(crate) static LEGACY_GROWTH: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Capacity a full column of `cap` elements grows to so that `need` (> `cap`) elements fit.
/// Doubles up to `GROW_FLOOR`; above it, `need` plus 12.5%, rounded up to `GROW_ROUND`
/// elements, so unused capacity is at most about one eighth of what is in use.
pub(crate) fn grown_cap(cap: usize, need: usize) -> usize {
    #[cfg(test)]
    {
        if LEGACY_GROWTH.with(|l| l.get()) {
            return need.max(cap.saturating_mul(2));
        }
    }
    if need <= GROW_FLOOR {
        return need.max(cap.saturating_mul(2)).min(GROW_FLOOR).max(need);
    }
    let want = need.saturating_add(need / 8);
    let rounded = want.div_ceil(GROW_ROUND).saturating_mul(GROW_ROUND);
    rounded.max(need)
}

/// Memory page size the dirty-page accounting works in (the system page size on unix, 4 KiB
/// elsewhere). Always a power of two.
pub(crate) fn page_size() -> usize {
    static PAGE: OnceLock<usize> = OnceLock::new();
    *PAGE.get_or_init(|| {
        #[cfg(unix)]
        {
            let p = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
            if p > 0 && (p as usize).is_power_of_two() {
                return p as usize;
            }
        }
        4096
    })
}

/// 64-page words needed to track `bytes` of mapping.
fn dirty_words(bytes: usize, shift: u32) -> usize {
    bytes.div_ceil(1usize << shift).div_ceil(64)
}

/// Bytes of the mapping at `ptr..ptr+len` that are resident in memory, by page (`mincore`).
/// Zero when the query fails or the platform has no `mincore`.
pub(crate) fn resident_bytes(ptr: *const u8, len: usize) -> usize {
    #[cfg(unix)]
    {
        if len == 0 {
            return 0;
        }
        let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) }.max(1) as usize;
        let mut vec = vec![0u8; len.div_ceil(page)];
        let rc = unsafe { libc::mincore(ptr as *mut libc::c_void, len, vec.as_mut_ptr().cast()) };
        if rc != 0 {
            return 0;
        }
        vec.iter().filter(|&&b| b & 1 != 0).count() * page
    }
    #[cfg(not(unix))]
    {
        let _ = (ptr, len);
        0
    }
}

/// More dirty runs than this are flushed with one whole-mapping `msync` instead of one call
/// per run.
const MAX_FLUSH_RUNS: usize = 64;

impl<T: Copy> Col<T> {
    const SZ: usize = std::mem::size_of::<T>();

    /// Create (truncating) a column with room for `cap` elements.
    pub fn create(dir: Option<&Path>, name: &str, cap: usize) -> io::Result<Col<T>> {
        let cap = cap.max(16);
        match dir {
            Some(d) => {
                let f = OpenOptions::new()
                    .read(true)
                    .write(true)
                    .create(true)
                    .truncate(true)
                    .open(d.join(name))?;
                f.set_len((cap * Self::SZ) as u64)?;
                let map = unsafe { MmapMut::map_mut(&f)? };
                let shift = page_size().trailing_zeros();
                Ok(Col {
                    file: Some(f),
                    map,
                    cap,
                    dirty: vec![0; dirty_words(cap * Self::SZ, shift)],
                    shift,
                    grown: false,
                    partial_flush_bytes: 0,
                    _t: PhantomData,
                })
            }
            None => Ok(Col {
                file: None,
                map: MmapMut::map_anon(cap * Self::SZ)?,
                cap,
                dirty: Vec::new(),
                shift: page_size().trailing_zeros(),
                grown: false,
                partial_flush_bytes: 0,
                _t: PhantomData,
            }),
        }
    }

    /// Open an existing column file; it must hold at least `min_len` elements.
    pub fn open(dir: &Path, name: &str, min_len: usize) -> io::Result<Col<T>> {
        let f = OpenOptions::new()
            .read(true)
            .write(true)
            .open(dir.join(name))?;
        let len = f.metadata()?.len() as usize;
        if len < min_len * Self::SZ || len < 16 * Self::SZ {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "column shorter than header says",
            ));
        }
        let map = unsafe { MmapMut::map_mut(&f)? };
        let shift = page_size().trailing_zeros();
        Ok(Col {
            file: Some(f),
            map,
            cap: len / Self::SZ,
            dirty: vec![0; dirty_words(len, shift)],
            shift,
            grown: false,
            partial_flush_bytes: 0,
            _t: PhantomData,
        })
    }

    pub fn cap(&self) -> usize {
        self.cap
    }

    /// Bytes the mapping (and, when file-backed, the file) spans.
    pub fn capacity_bytes(&self) -> u64 {
        (self.cap * Self::SZ) as u64
    }

    #[inline]
    pub fn get(&self, i: usize) -> T {
        assert!(i < self.cap, "column index out of range");
        unsafe { (self.map.as_ptr() as *const T).add(i).read() }
    }

    #[inline]
    pub fn set(&mut self, i: usize, v: T) {
        assert!(i < self.cap, "column index out of range");
        let pg = (i * Self::SZ) >> self.shift;
        if let Some(w) = self.dirty.get_mut(pg >> 6) {
            *w |= 1u64 << (pg & 63);
        }
        unsafe { (self.map.as_mut_ptr() as *mut T).add(i).write(v) }
    }

    /// Grow so that `n` elements fit, by `grown_cap`. New space reads as zero.
    pub fn ensure(&mut self, n: usize) -> io::Result<()> {
        if n <= self.cap {
            return Ok(());
        }
        let newcap = grown_cap(self.cap, n);
        match &self.file {
            Some(f) => {
                release_map(&mut self.map)?;
                let grown = f.set_len((newcap * Self::SZ) as u64);
                let remap = unsafe { MmapMut::map_mut(f) };
                grown?;
                self.map = remap?;
                self.dirty
                    .resize(dirty_words(newcap * Self::SZ, self.shift), 0);
                self.grown = true;
            }
            None => {
                let mut m = MmapMut::map_anon(newcap * Self::SZ)?;
                m[..self.cap * Self::SZ].copy_from_slice(&self.map[..self.cap * Self::SZ]);
                self.map = m;
            }
        }
        self.cap = newcap;
        Ok(())
    }

    /// Drop this mapping's resident pages by remapping the file (data stays in the page cache and the file;
    /// madvise(DONTNEED) does not shrink the resident set on macOS, a fresh mapping does).
    pub fn trim(&mut self) {
        if let Some(f) = &self.file {
            if let Ok(m) = unsafe { MmapMut::map_mut(f) } {
                self.map = m;
            }
        }
    }

    /// Resident bytes of this column's mapping.
    pub fn resident_bytes(&self) -> usize {
        resident_bytes(self.map.as_ptr(), self.map.len())
    }

    /// Bytes the next `flush` writes: the pages written since the last flush (whole pages, so
    /// an upper bound for sparse writes). Zero for an anonymous column.
    pub fn dirty_bytes(&self) -> u64 {
        let pages: u64 = self.dirty.iter().map(|w| u64::from(w.count_ones())).sum();
        pages << self.shift
    }

    /// Whether `flush` has nothing to do: no dirty page and no unpublished growth. A commit
    /// skips such a column entirely.
    pub fn is_clean(&self) -> bool {
        self.file.is_none() || (!self.grown && self.dirty.iter().all(|&w| w == 0))
    }

    /// Dirty pages as `(byte offset, byte length)` runs of adjacent pages.
    fn dirty_runs(&self) -> Vec<(usize, usize)> {
        let page = 1usize << self.shift;
        let total = self.map.len().div_ceil(page);
        let bit = |pg: usize| (self.dirty.get(pg >> 6).copied().unwrap_or(0) >> (pg & 63)) & 1 == 1;
        let mut runs = Vec::new();
        let mut pg = 0usize;
        while pg < total {
            if self.dirty.get(pg >> 6).copied().unwrap_or(0) == 0 {
                pg = (pg | 63) + 1;
                continue;
            }
            if !bit(pg) {
                pg += 1;
                continue;
            }
            let start = pg;
            while pg < total && bit(pg) {
                pg += 1;
            }
            let off = start * page;
            let len = ((pg - start) * page).min(self.map.len() - off);
            runs.push((off, len));
        }
        runs
    }

    /// Dirty-page coverage from successful runs preceding a failed flush.
    pub fn take_partial_flush_bytes(&mut self) -> u64 {
        std::mem::take(&mut self.partial_flush_bytes)
    }

    /// Flush dirty runs. The result counts logical dirty-page coverage, not physical device
    /// writes (writeback, filesystem metadata and journal amplification are not observable).
    pub fn flush(&mut self) -> io::Result<u64> {
        self.flush_with(|_, _| Ok(()))
    }

    fn flush_with(
        &mut self,
        mut before: impl FnMut(usize, usize) -> io::Result<()>,
    ) -> io::Result<u64> {
        if self.file.is_none() {
            return Ok(0);
        }
        let bytes = self.dirty_bytes();
        if bytes == 0 && !self.grown {
            return Ok(0);
        }
        let runs = self.dirty_runs();
        if runs.len() > MAX_FLUSH_RUNS || runs.is_empty() {
            before(0, self.map.len())?;
            self.map.flush()?;
            self.dirty.fill(0);
        } else {
            let page = 1usize << self.shift;
            let mut done = 0;
            for (off, len) in runs {
                if let Err(e) = before(off, len).and_then(|()| self.map.flush_range(off, len)) {
                    self.partial_flush_bytes += done;
                    return Err(e);
                }
                let first = off / page;
                let end = (off + len).div_ceil(page);
                for pg in first..end {
                    self.dirty[pg >> 6] &= !(1u64 << (pg & 63));
                }
                done += ((end - first) * page) as u64;
            }
        }
        self.grown = false;
        Ok(bytes)
    }
}

/// Windows refuses to resize a file while a view of it is mapped (error 1224), so a growing
/// column or segment unmaps first: the dirty pages are flushed, then the view is replaced by a
/// one-page anonymous placeholder that the caller overwrites with the new mapping. Elsewhere
/// the old view stays valid across `set_len` and this does nothing.
pub(crate) fn release_map(map: &mut MmapMut) -> io::Result<()> {
    #[cfg(windows)]
    {
        map.flush()?;
        *map = MmapMut::map_anon(1)?;
    }
    #[cfg(not(windows))]
    let _ = map;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flush_reports_the_dirty_pages_once() {
        let dir = tempfile::tempdir().unwrap();
        let page = page_size() as u64;
        let mut c = Col::<u64>::create(Some(dir.path()), "t.col", 20_000).unwrap();
        assert_eq!(c.dirty_bytes(), 0);
        assert!(c.is_clean());
        c.set(0, 1);
        c.set(1, 2); // same page as element 0
        assert_eq!(c.dirty_bytes(), page);
        assert!(!c.is_clean());
        c.set(15_000, 3); // 120000 bytes in: another page
        assert_eq!(c.flush().unwrap(), 2 * page);
        assert_eq!(c.flush().unwrap(), 0, "nothing is dirty after a flush");
        assert!(c.is_clean());
        assert_eq!(c.get(15_000), 3);
    }

    #[test]
    fn growth_keeps_tracking_and_an_anonymous_column_reports_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let page = page_size() as u64;
        let mut c = Col::<u32>::create(Some(dir.path()), "g.col", 16).unwrap();
        c.ensure(1_000_000).unwrap();
        c.set(999_999, 7);
        assert_eq!(c.flush().unwrap(), page);
        let mut a = Col::<u32>::create(None, "a.col", 16).unwrap();
        a.set(3, 1);
        assert_eq!(a.dirty_bytes(), 0);
        assert_eq!(a.flush().unwrap(), 0);
        assert!(a.is_clean());
    }

    #[test]
    fn scattered_dirty_pages_flush_as_runs_and_count_exactly() {
        let dir = tempfile::tempdir().unwrap();
        let page = page_size() as u64;
        let per_page = (page as usize) / 8;
        let mut c = Col::<u64>::create(Some(dir.path()), "r.col", per_page * 40).unwrap();
        for pg in [0usize, 1, 2, 7, 20, 21, 39] {
            c.set(pg * per_page, pg as u64 + 1);
        }
        let runs = c.dirty_runs();
        assert_eq!(
            runs,
            vec![
                (0, 3 * page as usize),
                (7 * page as usize, page as usize),
                (20 * page as usize, 2 * page as usize),
                (39 * page as usize, page as usize),
            ]
        );
        assert_eq!(c.flush().unwrap(), 7 * page);
        assert!(c.is_clean());
        assert_eq!(c.get(39 * per_page), 40);
    }

    #[test]
    fn a_grown_column_without_writes_still_flushes_once_and_reports_zero_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let mut c = Col::<u32>::create(Some(dir.path()), "z.col", 16).unwrap();
        c.ensure(100_000).unwrap();
        assert!(!c.is_clean(), "unpublished growth");
        assert_eq!(c.flush().unwrap(), 0);
        assert!(c.is_clean());
    }

    #[test]
    fn growth_is_bounded_above_the_floor_and_doubles_below_it() {
        // Below the floor: doubling, capped at the floor.
        assert_eq!(grown_cap(16, 17), 32);
        assert_eq!(grown_cap(40_000, 40_001), GROW_FLOOR);
        // Above it: need plus an eighth, never more than a rounding granule beyond that.
        for need in [65_537usize, 100_000, 1_000_001, 4_660_396, 20_000_000] {
            let cap = grown_cap(need - 1, need);
            assert!(cap >= need);
            let unused = cap - need;
            assert!(
                unused <= need / 8 + GROW_ROUND,
                "need {need}: unused {unused} of cap {cap}"
            );
        }
        // Filling a column one element at a time never leaves more than 12.5% (plus the
        // granule) unused once it passes the floor.
        let mut cap = 16usize;
        for n in 1..=1_000_000usize {
            if n > cap {
                cap = grown_cap(cap, n);
            }
            if n > 2 * GROW_FLOOR {
                assert!(cap - n <= n / 8 + GROW_ROUND, "n {n} cap {cap}");
            }
        }
    }

    #[test]
    fn legacy_growth_hook_restores_doubling() {
        LEGACY_GROWTH.with(|l| l.set(true));
        assert_eq!(grown_cap(300_000, 300_001), 600_000);
        LEGACY_GROWTH.with(|l| l.set(false));
        assert!(grown_cap(300_000, 300_001) < 400_000);
    }
    #[test]
    fn a_partial_run_failure_accounts_success_and_retries_only_remaining_pages() {
        let dir = tempfile::tempdir().unwrap();
        let page = page_size();
        let mut c = Col::<u8>::create(Some(dir.path()), "partial.col", page * 5).unwrap();
        c.set(0, 1);
        c.set(page * 4, 2);
        assert!(c
            .flush_with(|off, _| {
                if off > 0 {
                    Err(io::Error::other("injected run failure"))
                } else {
                    Ok(())
                }
            })
            .is_err());
        assert_eq!(c.take_partial_flush_bytes(), page as u64);
        assert_eq!(c.dirty_bytes(), page as u64);
        assert_eq!(c.flush().unwrap(), page as u64);
        assert_eq!(c.take_partial_flush_bytes(), 0);
        assert!(c.is_clean());
    }

    #[test]
    fn short_columns_count_one_logical_page() {
        let dir = tempfile::tempdir().unwrap();
        let mut c = Col::<u8>::create(Some(dir.path()), "short.col", 16).unwrap();
        c.set(15, 1);
        assert_eq!(c.dirty_bytes(), page_size() as u64);
        assert_eq!(c.flush().unwrap(), page_size() as u64);
    }
}
