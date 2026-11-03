//! Public bulk directory listing: one `getattrlistbulk` pass over an open
//! directory descriptor ([`list_dir_fd`]).
//!
//! Only metadata is requested. File contents are never opened, and a dataless
//! file (its bytes are not on this disk) is never hydrated. Sizes here are the
//! data fork only, so `logical` equals `st_size`; the crawl that feeds the index
//! keeps its own size rule (data plus resource fork) in the private path.

use crate::error::{Error, Result};
use crate::index::EntryKind;
use std::cell::RefCell;
use std::ffi::{c_void, OsString};
use std::io;
use std::os::fd::{AsRawFd, BorrowedFd};
use std::os::unix::ffi::OsStringExt;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const ATTR_CMN_ERROR: u32 = 0x2000_0000;
const VREG: u32 = 1;
const VDIR: u32 = 2;
const VLNK: u32 = 5;
/// `SF_DATALESS`: the file's bytes are not on this disk (File Provider or cloud placeholder).
const SF_DATALESS: u32 = 0x4000_0000;
/// Record length word plus the five returned-attribute sets.
const RECORD_HEADER: usize = 24;
const BUFFER_SIZE: usize = 64 * 1024;

thread_local! {
    static DIR_BUF: RefCell<Vec<u8>> = RefCell::new(vec![0u8; BUFFER_SIZE]);
}

/// Options for [`list_dir_fd`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ListOptions {
    /// Stop after this many entries. `None` lists the whole directory.
    pub limit: Option<usize>,
    /// Also return each entry's creation time in [`Entry::created`]. Off by default.
    pub want_creation_time: bool,
    /// Report dataless files as [`EntrySize::Dataless`] instead of a size.
    pub refuse_dataless: bool,
}

/// The size of one entry, as far as the platform reports it.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntrySize {
    /// A non-directory object. `logical` is the data-fork length and equals `st_size`
    /// (the resource fork is excluded). `allocated` covers every fork and equals
    /// `st_blocks * 512` for ordinary files. For a dataless file `allocated` is zero
    /// because nothing is on disk; check [`Entry::dataless`].
    Bytes { logical: u64, allocated: u64 },
    /// A directory, which has no size of its own, or another kind with no size.
    NoSize,
    /// A dataless file whose size is refused because [`ListOptions::refuse_dataless`] is set.
    Dataless,
}

/// One child of a listed directory. Names are lossless bytes as the kernel returned them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub name: OsString,
    /// The object's kind. Symbolic links are not followed.
    pub kind: EntryKind,
    pub size: EntrySize,
    /// True when the file's bytes are not on this disk. Set whether or not the size was refused.
    pub dataless: bool,
    /// Device number of the entry (`st_dev`).
    pub dev: u64,
    /// Inode number of the entry (`st_ino`).
    pub ino: u64,
    /// Modification time (`st_mtimespec`).
    pub modified: SystemTime,
    /// Creation time (`st_birthtimespec`). `None` unless [`ListOptions::want_creation_time`] is set.
    pub created: Option<SystemTime>,
}

/// A directory listing. `truncated` is true only when at least one more entry existed
/// beyond `limit`; a directory with exactly `limit` entries is not truncated.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Listing {
    pub entries: Vec<Entry>,
    pub truncated: bool,
}

/// Lists the directory open as `fd`, reading from its current position.
///
/// The descriptor's read position advances with each call and is not restored. Pass
/// a freshly opened descriptor to list a directory from its first entry. `.` and `..`
/// are never returned. An entry the kernel reports an error for is omitted.
///
/// Only directories are accepted; anything else fails with `Error::Io`.
pub fn list_dir_fd(fd: BorrowedFd<'_>, opts: ListOptions) -> Result<Listing> {
    let mut request = request_attributes(opts.want_creation_time);
    let mut entries = Vec::new();
    let mut truncated = false;
    DIR_BUF.with_borrow_mut(|buf: &mut Vec<u8>| -> Result<()> {
        loop {
            let n = unsafe {
                libc::getattrlistbulk(
                    fd.as_raw_fd(),
                    &mut request as *mut libc::attrlist as *mut c_void,
                    buf.as_mut_ptr().cast(),
                    buf.len(),
                    0,
                )
            };
            if n < 0 {
                return Err(Error::Io(io::Error::last_os_error()));
            }
            if n == 0 {
                return Ok(());
            }
            let mut at = 0usize;
            for _ in 0..n {
                let length = buf
                    .get(at..at + 4)
                    .map(|b| u32::from_ne_bytes(b.try_into().unwrap()) as usize)
                    .ok_or_else(malformed)?;
                let end = at
                    .checked_add(length)
                    .filter(|&end| length >= RECORD_HEADER && end <= buf.len())
                    .ok_or_else(malformed)?;
                let record = &buf[at..end];
                at = end;
                let Some(entry) = parse_record(record, opts.refuse_dataless)? else {
                    continue;
                };
                if opts.limit.is_some_and(|limit| entries.len() >= limit) {
                    truncated = true;
                    return Ok(());
                }
                entries.push(entry);
            }
        }
    })?;
    Ok(Listing { entries, truncated })
}

fn request_attributes(want_creation_time: bool) -> libc::attrlist {
    let mut attrs: libc::attrlist = unsafe { std::mem::zeroed() };
    attrs.bitmapcount = libc::ATTR_BIT_MAP_COUNT;
    attrs.commonattr = libc::ATTR_CMN_RETURNED_ATTRS
        | ATTR_CMN_ERROR
        | libc::ATTR_CMN_NAME
        | libc::ATTR_CMN_DEVID
        | libc::ATTR_CMN_OBJTYPE
        | libc::ATTR_CMN_MODTIME
        | libc::ATTR_CMN_FLAGS
        | libc::ATTR_CMN_FILEID
        | if want_creation_time {
            libc::ATTR_CMN_CRTIME
        } else {
            0
        };
    attrs.fileattr = libc::ATTR_FILE_ALLOCSIZE | libc::ATTR_FILE_DATALENGTH;
    attrs
}

fn malformed() -> Error {
    Error::Io(io::Error::new(
        io::ErrorKind::InvalidData,
        "malformed bulk directory record",
    ))
}

/// Bounds-checked reader over one record. Every read fails rather than running past it.
struct Cursor<'a> {
    buf: &'a [u8],
    at: usize,
}

impl Cursor<'_> {
    fn take<const N: usize>(&mut self) -> Result<[u8; N]> {
        let end = self
            .at
            .checked_add(N)
            .filter(|&end| end <= self.buf.len())
            .ok_or_else(malformed)?;
        let mut out = [0u8; N];
        out.copy_from_slice(&self.buf[self.at..end]);
        self.at = end;
        Ok(out)
    }

    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_ne_bytes(self.take()?))
    }

    fn i32(&mut self) -> Result<i32> {
        Ok(i32::from_ne_bytes(self.take()?))
    }

    fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_ne_bytes(self.take()?))
    }

    fn i64(&mut self) -> Result<i64> {
        Ok(i64::from_ne_bytes(self.take()?))
    }

    /// A `timespec`: seconds then nanoseconds, both 64-bit on macOS.
    fn time(&mut self) -> Result<SystemTime> {
        let secs = self.i64()?;
        let nanos = self.i64()?;
        timespec(secs, nanos)
    }
}

fn timespec(secs: i64, nanos: i64) -> Result<SystemTime> {
    if !(0..1_000_000_000).contains(&nanos) {
        return Err(malformed());
    }
    let base = if secs >= 0 {
        UNIX_EPOCH.checked_add(Duration::from_secs(secs.unsigned_abs()))
    } else {
        UNIX_EPOCH.checked_sub(Duration::from_secs(secs.unsigned_abs()))
    };
    base.and_then(|t| t.checked_add(Duration::from_nanos(nanos as u64)))
        .ok_or_else(malformed)
}

/// Parses one record in the order the kernel lays fields out: the returned-attribute
/// sets, the optional per-entry error, then the common attributes in bit order (name,
/// device, object type, creation time, modification time, flags, file id), then the
/// file attributes (allocation, then data length). Returns `Ok(None)` for entries that
/// are omitted (kernel error, `.`, `..`, empty name).
fn parse_record(record: &[u8], refuse_dataless: bool) -> Result<Option<Entry>> {
    let mut c = Cursor { buf: record, at: 4 };
    let common = c.u32()?;
    let _volume = c.u32()?;
    let _directory = c.u32()?;
    let file = c.u32()?;
    let _fork = c.u32()?;
    if common & ATTR_CMN_ERROR != 0 && c.u32()? != 0 {
        return Ok(None);
    }
    if common & libc::ATTR_CMN_NAME == 0 {
        return Ok(None);
    }

    // The name is an attrreference: an offset from the reference itself, and a length
    // that includes the trailing NUL.
    let reference_at = c.at;
    let data_offset = c.i32()?;
    let name_len = c.u32()? as usize;
    let start =
        usize::try_from(reference_at as i64 + i64::from(data_offset)).map_err(|_| malformed())?;
    let end = start
        .checked_add(name_len.checked_sub(1).ok_or_else(malformed)?)
        .ok_or_else(malformed)?;
    let name = record.get(start..end).ok_or_else(malformed)?;
    if name.is_empty() || name == b"." || name == b".." {
        return Ok(None);
    }

    let dev = if common & libc::ATTR_CMN_DEVID != 0 {
        c.i32()? as u32 as u64
    } else {
        0
    };
    let kind_code = if common & libc::ATTR_CMN_OBJTYPE != 0 {
        c.u32()?
    } else {
        0
    };
    let created = if common & libc::ATTR_CMN_CRTIME != 0 {
        Some(c.time()?)
    } else {
        None
    };
    if common & libc::ATTR_CMN_MODTIME == 0 {
        return Err(malformed());
    }
    let modified = c.time()?;
    let flags = if common & libc::ATTR_CMN_FLAGS != 0 {
        c.u32()?
    } else {
        0
    };
    let ino = if common & libc::ATTR_CMN_FILEID != 0 {
        c.u64()?
    } else {
        0
    };
    let has_allocated = file & libc::ATTR_FILE_ALLOCSIZE != 0;
    let has_logical = file & libc::ATTR_FILE_DATALENGTH != 0;
    let allocated = if has_allocated { c.i64()? } else { 0 };
    let logical = if has_logical { c.i64()? } else { 0 };

    let kind = match kind_code {
        VREG => EntryKind::File,
        VDIR => EntryKind::Dir,
        VLNK => EntryKind::Symlink,
        _ => EntryKind::Other,
    };
    let dataless = !matches!(kind, EntryKind::Dir) && flags & SF_DATALESS != 0;
    let size = if matches!(kind, EntryKind::Dir) {
        EntrySize::NoSize
    } else if dataless && refuse_dataless {
        EntrySize::Dataless
    } else if has_allocated && has_logical {
        if logical < 0 || allocated < 0 {
            return Err(malformed());
        }
        EntrySize::Bytes {
            logical: logical as u64,
            allocated: allocated as u64,
        }
    } else if matches!(kind, EntryKind::File) {
        // A regular file must carry its sizes; without them the record is unusable.
        return Err(malformed());
    } else {
        EntrySize::NoSize
    };

    Ok(Some(Entry {
        name: OsString::from_vec(name.to_vec()),
        kind,
        size,
        dataless,
        dev,
        ino,
        modified,
        created,
    }))
}
