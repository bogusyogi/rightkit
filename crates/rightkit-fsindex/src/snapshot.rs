//! Snapshot of the tree for event-ID resume. Our own state file (never user data).

use crate::backend::*;
use crate::error::{Error, Result};
use crate::store::Store;
use std::io::Write;
use std::path::Path;

const MAGIC: &[u8; 8] = b"RKFSIX01";

pub struct Loaded {
    pub event_id: u64,
    pub saved_at: u64,
}

pub fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

pub fn save(store: &Store, path: &Path, fingerprint: u64, event_id: u64) -> Result<()> {
    let tmp = path.with_extension("tmp");
    let mut w = std::io::BufWriter::new(std::fs::File::create(&tmp)?);
    w.write_all(MAGIC)?;
    w.write_all(&fingerprint.to_le_bytes())?;
    w.write_all(&event_id.to_le_bytes())?;
    w.write_all(&now_secs().to_le_bytes())?;
    w.write_all(&(store.len() as u64).to_le_bytes())?;
    let mut seq = vec![NONE; store.slots()];
    let mut n = 0u32;
    let mut stack: Vec<u32> = store.roots.iter().rev().copied().collect();
    while let Some(s) = stack.pop() {
        seq[s as usize] = n;
        n += 1;
        let e = store.entry(s);
        let parent = if e.parent == NONE {
            NONE
        } else {
            seq[e.parent as usize]
        };
        let name = store.name_of(s);
        w.write_all(&parent.to_le_bytes())?;
        w.write_all(&(name.len() as u16).to_le_bytes())?;
        w.write_all(name)?;
        let d = &e.d;
        w.write_all(&[d.kind, d.flags])?;
        w.write_all(&d.nlink.to_le_bytes())?;
        w.write_all(&d.mtime.to_le_bytes())?;
        w.write_all(&d.dev.to_le_bytes())?;
        for v in [d.fileid, d.logical, d.alloc, d.private, d.cloneid] {
            w.write_all(&v.to_le_bytes())?;
        }
        stack.extend(store.children(s).iter().rev());
    }
    w.flush()?;
    drop(w);
    std::fs::rename(&tmp, path)?;
    Ok(())
}

struct Cur<'a>(&'a [u8], usize);

impl<'a> Cur<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let s = self
            .0
            .get(self.1..self.1 + n)
            .ok_or_else(|| Error::State("snapshot truncated".into()))?;
        self.1 += n;
        Ok(s)
    }
    fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }
    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
}

pub fn load(store: &mut Store, path: &Path, fingerprint: u64) -> Result<Loaded> {
    let bytes = std::fs::read(path)?;
    let mut c = Cur(&bytes, 0);
    if c.take(8)? != MAGIC {
        return Err(Error::State("bad snapshot magic".into()));
    }
    if c.u64()? != fingerprint {
        return Err(Error::State("snapshot config fingerprint differs".into()));
    }
    let event_id = c.u64()?;
    let saved_at = c.u64()?;
    let count = c.u64()?;
    let mut slots: Vec<u32> = Vec::with_capacity(count.min(1 << 26) as usize);
    for _ in 0..count {
        let parent = c.u32()?;
        let nl = c.u16()? as usize;
        let name = c.take(nl)?;
        let kb = c.take(2)?;
        let d = EntryData {
            kind: kb[0],
            flags: kb[1],
            nlink: c.u32()?,
            mtime: c.u64()? as i64,
            dev: c.u32()?,
            fileid: c.u64()?,
            logical: c.u64()?,
            alloc: c.u64()?,
            private: c.u64()?,
            cloneid: c.u64()?,
        };
        let slot = if parent == NONE {
            store.add_root(name, d)?
        } else {
            let p = *slots
                .get(parent as usize)
                .ok_or_else(|| Error::State("bad parent".into()))?;
            store.add_child(p, name, d)?
        };
        slots.push(slot);
    }
    store.take_logs();
    Ok(Loaded { event_id, saved_at })
}
