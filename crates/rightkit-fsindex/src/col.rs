//! Fixed-width memory-mapped column. File-backed (sparse, zero-filled growth)
//! when a directory is given, anonymous otherwise.

use memmap2::MmapMut;
use std::fs::{File, OpenOptions};
use std::io;
use std::marker::PhantomData;
use std::path::Path;

pub struct Col<T: Copy> {
    file: Option<File>,
    map: MmapMut,
    cap: usize,
    _t: PhantomData<T>,
}

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
                Ok(Col {
                    file: Some(f),
                    map,
                    cap,
                    _t: PhantomData,
                })
            }
            None => Ok(Col {
                file: None,
                map: MmapMut::map_anon(cap * Self::SZ)?,
                cap,
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
        Ok(Col {
            file: Some(f),
            map,
            cap: len / Self::SZ,
            _t: PhantomData,
        })
    }

    pub fn cap(&self) -> usize {
        self.cap
    }

    #[inline]
    pub fn get(&self, i: usize) -> T {
        assert!(i < self.cap, "column index out of range");
        unsafe { (self.map.as_ptr() as *const T).add(i).read() }
    }

    #[inline]
    pub fn set(&mut self, i: usize, v: T) {
        assert!(i < self.cap, "column index out of range");
        unsafe { (self.map.as_mut_ptr() as *mut T).add(i).write(v) }
    }

    /// Grow (doubling) so that `n` elements fit. New space reads as zero.
    pub fn ensure(&mut self, n: usize) -> io::Result<()> {
        if n <= self.cap {
            return Ok(());
        }
        let newcap = n.max(self.cap * 2);
        match &self.file {
            Some(f) => {
                f.set_len((newcap * Self::SZ) as u64)?;
                self.map = unsafe { MmapMut::map_mut(f)? };
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

    /// Ask the kernel to drop this mapping's resident pages (file-backed only; data stays in the file).
    pub fn trim(&self) {
        if self.file.is_some() {
            // Safe for MAP_SHARED file mappings: dirty pages are written back, nothing is discarded.
            let _ = unsafe {
                self.map
                    .unchecked_advise(memmap2::UncheckedAdvice::DontNeed)
            };
        }
    }

    /// msync (blocking) when file-backed.
    pub fn flush(&self) -> io::Result<()> {
        if self.file.is_some() {
            self.map.flush()?;
        }
        Ok(())
    }
}
