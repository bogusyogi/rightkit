//! Interned name store backed by memory-mapped segments.
//!
//! Names are appended into fixed-size segments (file-backed in the state
//! directory, anonymous otherwise) and deduplicated. A name id is stable for
//! the life of the store; the arena is rebuilt (compacted) on every open.

use memmap2::MmapMut;
use std::collections::HashMap;
use std::path::PathBuf;

const SEG_SHIFT: u32 = 24;
const SEG: usize = 1 << SEG_SHIFT;

#[derive(Clone, Copy)]
struct NameRef {
    off: u32,
    len: u16,
}

pub struct NameStore {
    dir: Option<PathBuf>,
    segs: Vec<MmapMut>,
    used: Vec<usize>,
    refs: Vec<NameRef>,
    heads: HashMap<u64, u32>,
    next: Vec<u32>,
}

fn hash(b: &[u8]) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for &x in b {
        h ^= x as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

impl NameStore {
    pub fn new(dir: Option<PathBuf>) -> std::io::Result<NameStore> {
        if let Some(d) = &dir {
            std::fs::create_dir_all(d)?;
            for e in std::fs::read_dir(d)?.flatten() {
                if e.file_name().to_string_lossy().starts_with("names-") {
                    let _ = std::fs::remove_file(e.path());
                }
            }
        }
        Ok(NameStore {
            dir,
            segs: Vec::new(),
            used: Vec::new(),
            refs: Vec::new(),
            heads: HashMap::new(),
            next: Vec::new(),
        })
    }

    fn new_seg(&mut self) -> std::io::Result<()> {
        if self.segs.len() >= 256 {
            return Err(std::io::Error::other("name store full (4 GiB)"));
        }
        let m = match &self.dir {
            Some(d) => {
                let f = std::fs::OpenOptions::new()
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
        self.used.push(0);
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.refs.len()
    }

    pub fn get(&self, id: u32) -> &[u8] {
        let r = self.refs[id as usize];
        let seg = (r.off >> SEG_SHIFT) as usize;
        let at = (r.off as usize) & (SEG - 1);
        &self.segs[seg][at..at + r.len as usize]
    }

    pub fn intern(&mut self, name: &[u8]) -> std::io::Result<u32> {
        let name = &name[..name.len().min(u16::MAX as usize)];
        let h = hash(name);
        let mut cur = self.heads.get(&h).copied();
        while let Some(id) = cur {
            if self.get(id) == name {
                return Ok(id);
            }
            let n = self.next[id as usize];
            cur = (n != u32::MAX).then_some(n);
        }
        if self.segs.is_empty() || self.used[self.segs.len() - 1] + name.len() > SEG {
            self.new_seg()?;
        }
        let s = self.segs.len() - 1;
        let at = self.used[s];
        self.segs[s][at..at + name.len()].copy_from_slice(name);
        self.used[s] += name.len();
        let id = self.refs.len() as u32;
        self.refs.push(NameRef {
            off: ((s as u32) << SEG_SHIFT) | at as u32,
            len: name.len() as u16,
        });
        self.next.push(self.heads.insert(h, id).unwrap_or(u32::MAX));
        Ok(id)
    }
}
