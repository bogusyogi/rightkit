//! Interned name store on memory-mapped files.
//!
//! Names are appended into fixed-size segments and deduplicated through an
//! on-disk chained hash (bucket heads and next links are mapped columns), so
//! only touched pages are resident. A name id is stable for the life of the
//! store; garbage is dropped when the store is rebuilt (compaction).

use crate::col::Col;
use memmap2::MmapMut;
use std::fs::OpenOptions;
use std::io;
use std::path::{Path, PathBuf};

const SEG_SHIFT: u32 = 24;
const SEG: usize = 1 << SEG_SHIFT;
pub const MAX_SEGS: usize = 256;

pub struct NameStore {
    dir: Option<PathBuf>,
    segs: Vec<MmapMut>,
    pub seg_used: Vec<u32>,
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
    pub fn create(dir: Option<&Path>) -> io::Result<NameStore> {
        if let Some(d) = dir {
            for e in std::fs::read_dir(d)?.flatten() {
                if e.file_name().to_string_lossy().starts_with("names-") {
                    let _ = std::fs::remove_file(e.path());
                }
            }
        }
        Ok(NameStore {
            dir: dir.map(|d| d.to_path_buf()),
            segs: Vec::new(),
            seg_used: Vec::new(),
            off: Col::create(dir, "names-off.col", 1024)?,
            len: Col::create(dir, "names-len.col", 1024)?,
            next: Col::create(dir, "names-next.col", 1024)?,
            heads: Col::create(dir, "names-heads.col", 1024)?,
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
        for i in 0..seg_used.len() {
            let f = OpenOptions::new()
                .read(true)
                .write(true)
                .open(dir.join(format!("names-{i}.seg")))?;
            if f.metadata()?.len() < SEG as u64 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "short name segment",
                ));
            }
            segs.push(unsafe { MmapMut::map_mut(&f)? });
        }
        Ok(NameStore {
            dir: Some(dir.to_path_buf()),
            segs,
            seg_used,
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

    pub fn relocate(&mut self, dir: &Path) {
        if self.dir.is_some() {
            self.dir = Some(dir.to_path_buf());
        }
    }

    pub fn trim(&mut self) {
        if let Some(d) = &self.dir {
            for (i, s) in self.segs.iter_mut().enumerate() {
                if let Ok(f) = OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(d.join(format!("names-{i}.seg")))
                {
                    if let Ok(m) = unsafe { MmapMut::map_mut(&f) } {
                        *s = m;
                    }
                }
            }
        }
        self.off.trim();
        self.len.trim();
        self.next.trim();
        self.heads.trim();
    }

    pub fn flush(&self) -> io::Result<()> {
        for s in &self.segs {
            s.flush()?;
        }
        self.off.flush()?;
        self.len.flush()?;
        self.next.flush()?;
        self.heads.flush()
    }

    fn new_seg(&mut self) -> io::Result<()> {
        if self.segs.len() >= MAX_SEGS {
            return Err(io::Error::other("name store full (4 GiB)"));
        }
        let m = match &self.dir {
            Some(d) => {
                let f = OpenOptions::new()
                    .read(true)
                    .write(true)
                    .create(true)
                    .truncate(true)
                    .open(d.join(format!("names-{}.seg", self.segs.len())))?;
                f.set_len(SEG as u64)?;
                unsafe { MmapMut::map_mut(&f)? }
            }
            None => MmapMut::map_anon(SEG)?,
        };
        self.segs.push(m);
        self.seg_used.push(0);
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.count
    }

    pub fn get(&self, id: u32) -> &[u8] {
        let off = self.off.get(id as usize);
        let len = self.len.get(id as usize) as usize;
        let seg = (off >> SEG_SHIFT) as usize;
        let at = (off as usize) & (SEG - 1);
        &self.segs[seg][at..at + len]
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
            self.new_seg()?;
        }
        let s = self.segs.len() - 1;
        let at = self.seg_used[s] as usize;
        self.segs[s][at..at + name.len()].copy_from_slice(name);
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
