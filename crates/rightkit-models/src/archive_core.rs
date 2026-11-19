//! Internal archive layer shared by the root stack ([`crate::archive`]) and the
//! suite store ([`crate::suite::archive`]). Not public API.
//!
//! One copy of the tar opener, the hardened `--strip-components=1` member
//! check, the Apple-metadata filter, and the wrapped unpack loop. Both callers
//! keep their own error type and their own install policy (single-file
//! activation in the root stack, staged directory swap in the suite store).

use std::cell::Cell;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};
use std::rc::Rc;

/// Bounds on one extraction. Every field is a hard stop: the first member or byte past a bound
/// fails the whole unpack (the caller's staging directory is then discarded).
///
/// `max_entries` counts every tar member that is read, directories, skipped Apple metadata and
/// refused types included. `max_entry_bytes` bounds the declared size of one regular file and
/// `max_total_bytes` the summed declared size of all regular files. The decompressed stream
/// itself is also capped (the total plus a fixed allowance per entry for headers and padding), so
/// a member that lies about its size, or extension headers that never end, cannot expand past
/// the bound either.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExtractLimits {
    pub max_entries: usize,
    pub max_entry_bytes: u64,
    pub max_total_bytes: u64,
}

impl ExtractLimits {
    /// What a caller that knows nothing about the archive gets: finite, and far beyond any model
    /// this suite ships. A caller that knows the archive (a pinned size, a pinned file list)
    /// passes its own, much smaller, limits.
    pub const DEFAULT: Self = Self {
        max_entries: 200_000,
        max_entry_bytes: 64 << 30,
        max_total_bytes: 256 << 30,
    };

    /// Header, padding and extension-header allowance charged to the decompressed stream for each
    /// permitted entry.
    const PER_ENTRY_STREAM_ALLOWANCE: u64 = 4096;

    /// The most decompressed bytes the tar stream may yield.
    fn stream_cap(&self) -> u64 {
        self.max_total_bytes
            .saturating_add(
                (self.max_entries as u64).saturating_mul(Self::PER_ENTRY_STREAM_ALLOWANCE),
            )
            .saturating_add(1024)
    }
}

impl Default for ExtractLimits {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// A reader that fails, rather than yields, a byte past `remaining`.
struct CappedRead<R> {
    inner: R,
    remaining: u64,
}

impl<R: Read> Read for CappedRead<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        if self.remaining == 0 {
            // At the cap: a clean end of stream is fine, one more byte is not.
            let mut probe = [0u8; 1];
            return match self.inner.read(&mut probe)? {
                0 => Ok(0),
                _ => Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "archive expands past its size limit",
                )),
            };
        }
        let room = usize::try_from(self.remaining).unwrap_or(usize::MAX);
        let take = room.min(buf.len());
        let read = self.inner.read(&mut buf[..take])?;
        self.remaining -= read as u64;
        Ok(read)
    }
}

/// Observe physical headers while `tar` still owns GNU/PAX interpretation. Extension headers
/// consumed internally by `Entries::next` must count too. After an ordinary entry is yielded,
/// its effective (possibly PAX-overridden) size supplies the next header boundary.
struct CountedRead<R> {
    inner: R,
    next_header: Rc<Cell<Option<u64>>>,
    position: u64,
    header: [u8; 512],
    header_used: usize,
    remaining_entries: usize,
}

impl<R: Read> Read for CountedRead<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let Some(start) = self.next_header.get() else {
            let count = self.inner.read(buf)?;
            self.position = self.position.checked_add(count as u64).ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "archive size overflow")
            })?;
            return Ok(count);
        };
        let take = if self.position < start {
            usize::try_from(start - self.position)
                .unwrap_or(usize::MAX)
                .min(buf.len())
        } else {
            (512 - self.header_used).min(buf.len())
        };
        let count = self.inner.read(&mut buf[..take])?;
        if self.position >= start {
            self.header[self.header_used..self.header_used + count].copy_from_slice(&buf[..count]);
            self.header_used += count;
            if self.header_used == 512 {
                self.header_used = 0;
                self.next_header.set(None);
                if self.header.iter().any(|byte| *byte != 0) {
                    self.remaining_entries =
                        self.remaining_entries.checked_sub(1).ok_or_else(|| {
                            io::Error::new(
                                io::ErrorKind::InvalidData,
                                "archive has more entries than allowed",
                            )
                        })?;
                    let header = tar::Header::from_byte_slice(&self.header);
                    let kind = header.entry_type();
                    // Match tar's preprocessing exactly; global PAX headers are yielded normally.
                    if (header.as_gnu().is_some() || header.as_ustar().is_some())
                        && (kind.is_gnu_longname()
                            || kind.is_gnu_longlink()
                            || kind.is_pax_local_extensions())
                    {
                        let next = header
                            .entry_size()?
                            .checked_add(511)
                            .and_then(|size| start.checked_add(512)?.checked_add(size & !511))
                            .ok_or_else(|| {
                                io::Error::new(io::ErrorKind::InvalidData, "archive size overflow")
                            })?;
                        self.next_header.set(Some(next));
                    }
                }
            }
        }
        self.position = self
            .position
            .checked_add(count as u64)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "archive size overflow"))?;
        Ok(count)
    }
}

/// How the archive's outer compression is determined.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Compression {
    /// Always gzip. A plain tar fails to read.
    Gzip,
    /// gzip when the magic bytes say so, otherwise a plain tar.
    Sniff,
}

/// Open `archive` as a tar stream, decompressing per `compression`.
pub(crate) fn open_tar(
    archive: &Path,
    compression: Compression,
) -> Result<tar::Archive<Box<dyn Read>>, String> {
    open_tar_capped(archive, compression, None, None)
}

/// [`open_tar`] whose decoded stream fails past `decoded_cap` bytes (`None`: no cap).
fn open_tar_capped(
    archive: &Path,
    compression: Compression,
    decoded_cap: Option<u64>,
    entry_limit: Option<(usize, Rc<Cell<Option<u64>>>)>,
) -> Result<tar::Archive<Box<dyn Read>>, String> {
    let mut file =
        fs::File::open(archive).map_err(|err| format!("open {}: {err}", archive.display()))?;
    let gzip = match compression {
        Compression::Gzip => true,
        Compression::Sniff => {
            let mut magic = [0u8; 2];
            let n = file
                .read(&mut magic)
                .map_err(|err| format!("read {}: {err}", archive.display()))?;
            n == 2 && magic == [0x1f, 0x8b]
        }
    };
    let file =
        fs::File::open(archive).map_err(|err| format!("open {}: {err}", archive.display()))?;
    let reader: Box<dyn Read> = if gzip {
        Box::new(flate2::read::GzDecoder::new(file))
    } else {
        Box::new(file)
    };
    let reader: Box<dyn Read> = match decoded_cap {
        Some(remaining) => Box::new(CappedRead {
            inner: reader,
            remaining,
        }),
        None => reader,
    };
    let reader: Box<dyn Read> = match entry_limit {
        Some((remaining_entries, next_header)) => Box::new(CountedRead {
            inner: reader,
            next_header,
            position: 0,
            header: [0; 512],
            header_used: 0,
            remaining_entries,
        }),
        None => reader,
    };
    let mut tar = tar::Archive::new(reader);
    tar.set_preserve_ownerships(false);
    tar.set_preserve_permissions(false);
    tar.set_unpack_xattrs(false);
    Ok(tar)
}

/// Unpack `archive` into `dest_dir` with its single wrapper directory stripped.
/// Every member must sit under the same wrapper, no member may escape
/// `dest_dir`, Apple metadata is skipped, and only regular files and
/// directories are accepted. Creates `dest_dir` if needed.
///
/// When `expected_wrapper` is `Some`, the wrapper must be exactly that name;
/// any other top-level name (including a second top-level entry) fails.
///
/// Bounded by [`ExtractLimits::DEFAULT`]; [`unpack_wrapped_limited`] takes the caller's own.
pub(crate) fn unpack_wrapped(
    archive: &Path,
    compression: Compression,
    dest_dir: &Path,
    expected_wrapper: Option<&str>,
) -> Result<(), String> {
    unpack_wrapped_limited(
        archive,
        compression,
        dest_dir,
        expected_wrapper,
        &ExtractLimits::DEFAULT,
    )
}

/// [`unpack_wrapped`] within `limits`: the first entry, file or byte past a limit fails the
/// unpack. The limits are checked against the declared size before a member is written, and the
/// decompressed stream is capped on top of that (see [`ExtractLimits`]).
pub(crate) fn unpack_wrapped_limited(
    archive: &Path,
    compression: Compression,
    dest_dir: &Path,
    expected_wrapper: Option<&str>,
    limits: &ExtractLimits,
) -> Result<(), String> {
    fs::create_dir_all(dest_dir).map_err(|err| format!("create {}: {err}", dest_dir.display()))?;
    let next_header = Rc::new(Cell::new(Some(0)));
    let mut tar = open_tar_capped(
        archive,
        compression,
        Some(limits.stream_cap()),
        Some((limits.max_entries, Rc::clone(&next_header))),
    )?;

    let mut total_bytes = 0u64;
    let mut saw_member = false;
    let mut wrapper: Option<OsString> = expected_wrapper.map(OsString::from);
    let entries = tar
        .entries()
        .map_err(|err| format!("read archive: {err}"))?;
    for entry in entries {
        let mut entry = entry.map_err(|err| format!("read archive entry: {err}"))?;
        // Sparse members have extra internal headers and are unsupported; refuse before skipping
        // Apple metadata so their logical size can never be mistaken for a physical boundary.
        if entry.header().entry_type().is_gnu_sparse() {
            return Err("unsupported sparse archive member".to_string());
        }
        let next = entry
            .size()
            .checked_add(511)
            .and_then(|size| entry.raw_file_position().checked_add(size & !511))
            .ok_or_else(|| "archive size overflow".to_string())?;
        next_header.set(Some(next));
        let kind = entry.header().entry_type();
        if kind.is_file() {
            let size = entry.size();
            if size > limits.max_entry_bytes {
                return Err("archive member is larger than allowed".to_string());
            }
            total_bytes = total_bytes
                .checked_add(size)
                .filter(|total| *total <= limits.max_total_bytes)
                .ok_or_else(|| "archive expands to more than the allowed size".to_string())?;
        }
        let member = entry
            .path()
            .map_err(|err| format!("read archive path: {err}"))?
            .into_owned();
        if is_ignorable_apple_metadata(&member) {
            continue;
        }
        if let (Some(expected), Some(Component::Normal(found))) =
            (expected_wrapper, member.components().next())
        {
            if found != OsStr::new(expected) {
                return Err(format!(
                    "archive top-level entry `{}` does not match expected wrapper `{expected}`",
                    found.to_string_lossy()
                ));
            }
        }
        let Some(rel) = checked_member_path(&member, &mut wrapper)? else {
            // The wrapper dir entry itself (`<wrapper>/`): nothing to unpack.
            continue;
        };
        if kind.is_dir() {
            fs::create_dir_all(dest_dir.join(&rel))
                .map_err(|err| format!("create extracted dir: {err}"))?;
        } else if kind.is_file() {
            let out = dest_dir.join(&rel);
            if let Some(parent) = out.parent() {
                fs::create_dir_all(parent).map_err(|err| format!("create extracted dir: {err}"))?;
            }
            entry
                .unpack(&out)
                .map_err(|err| format!("extract {}: {err}", member.display()))?;
        } else {
            return Err(format!(
                "unsupported archive member type at {}",
                member.display()
            ));
        }
        saw_member = true;
    }
    if !saw_member {
        return Err("archive was empty".to_string());
    }
    Ok(())
}

/// `--strip-components=1` with hardening: every member must be `<wrapper>/<path…>`
/// where `<wrapper>` is one consistent normal dir name and `<path…>` contains
/// only normal components — `..`, roots, prefixes, and alternate wrappers all
/// reject. The wrapper entry itself maps to `None` (skipped).
pub(crate) fn checked_member_path(
    path: &Path,
    wrapper: &mut Option<OsString>,
) -> Result<Option<PathBuf>, String> {
    let mut components = path.components();
    let first = match components.next() {
        Some(Component::Normal(part)) => part,
        _ => {
            return Err(format!(
                "archive member escapes model dir: {}",
                path.display()
            ))
        }
    };
    match wrapper {
        Some(w) if w != first => {
            return Err(format!(
                "archive member escapes model dir: {}",
                path.display()
            ))
        }
        None => *wrapper = Some(first.to_os_string()),
        _ => {}
    }
    let mut out = PathBuf::new();
    for component in components {
        match component {
            Component::Normal(part) => out.push(part),
            Component::CurDir => {}
            _ => {
                return Err(format!(
                    "archive member escapes model dir: {}",
                    path.display()
                ))
            }
        }
    }
    if out.as_os_str().is_empty() {
        return Ok(None);
    }
    Ok(Some(out))
}

/// `__MACOSX/` and `._*` AppleDouble entries carry no model data.
pub(crate) fn is_ignorable_apple_metadata(path: &Path) -> bool {
    for component in path.components() {
        match component {
            Component::Normal(part) => {
                let name = part.to_string_lossy();
                if name == "__MACOSX" || name.starts_with("._") {
                    return true;
                }
            }
            Component::CurDir => {}
            _ => return false,
        }
    }
    false
}
