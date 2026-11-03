//! Signature-independent content digest: the identity a catalog row pins.
//!
//! Code signing rewrites a binary: ad-hoc signing and Developer ID signing add a
//! `LC_CODE_SIGNATURE` blob to every Mach-O slice and grow `__LINKEDIT`; Authenticode adds a
//! certificate table and a checksum to a PE. A whole-file SHA-256 of the shipped bytes therefore
//! never equals the digest of the unsigned build that was reviewed. This module hashes the part
//! of the file the signature does not touch, so unsigned, ad-hoc-signed and Developer-ID-signed
//! copies of one build share one digest. Every parse is bounds-checked and any structure that
//! does not add up is an error ([`MediaError::MalformedBinary`]), never a fallback to a
//! whole-file hash.
//!
//! # Algorithm
//!
//! **Thin Mach-O** (64-bit little-endian, `MH_MAGIC_64`). SHA-256 of the *canonical image*:
//!
//! 1. Walk the load commands. Their sizes must add up to `sizeofcmds` exactly. There may be at
//!    most one `LC_CODE_SIGNATURE` and exactly one `LC_SEGMENT_64 __LINKEDIT`.
//! 2. The content end is the signature `dataoff` when a signature is present, otherwise the file
//!    length. The signature must sit inside `__LINKEDIT` and run to the end of both `__LINKEDIT`
//!    and the file; without a signature `__LINKEDIT` must run to the end of the file. Bytes that
//!    nothing accounts for are an error, so nothing can hide outside the hash.
//! 3. Trailing zero bytes are dropped from the content end (never below `__LINKEDIT`'s file
//!    offset). This removes the alignment padding codesign inserts before the signature.
//! 4. The load-command area is rewritten: the `LC_CODE_SIGNATURE` command is removed (later
//!    commands shift up), `ncmds` falls by one and `sizeofcmds` by 16, and the bytes freed at the
//!    end of the area are zeroed. `__LINKEDIT.filesize` becomes `content_end - fileoff` and
//!    `__LINKEDIT.vmsize` becomes that size rounded up to the page size (16 KiB for arm64,
//!    4 KiB otherwise).
//! 5. The digest covers the rewritten load-command area (header included) followed by the file
//!    bytes from the end of the original load-command area up to the content end.
//!
//! This matches the bytes `codesign --remove-signature` leaves behind. For an unsigned file the
//! same rules apply with step 4 removing nothing, so an unsigned build and its signed copies
//! agree. A binary that already carries a linker-applied ad-hoc signature hashes the same as
//! the same binary with a Developer ID signature.
//!
//! **Universal (fat) Mach-O** (`FAT_MAGIC` / `FAT_MAGIC_64`). The header is parsed (slices must
//! be in bounds, non-empty, with distinct CPU types) and each slice is digested as a thin
//! Mach-O whose CPU type must equal the one the fat header declares. The digest of a universal
//! file is the list of its slice digests, one per architecture; padding between slices and the
//! fat header itself are not hashed (the loader ignores them).
//!
//! **PE** (`MZ`). The Authenticode hash rule over SHA-256: everything except the optional
//! header `CheckSum`, the certificate-table data-directory entry, and the attribute certificate
//! table. Sections are hashed in file order, then any trailing bytes before the certificate
//! table. The certificate table, when present, must be the last thing in the file. Overlapping
//! or out-of-bounds sections are errors.
//!
//! **Anything else** is hashed whole: plain SHA-256.

use std::{fs, io, io::Read, path::Path};

use crate::MediaError;

const MH_MAGIC_64: u32 = 0xFEED_FACF;
const LC_SEGMENT_64: u32 = 0x19;
const LC_CODE_SIGNATURE: u32 = 0x1D;
const FAT_MAGIC: u32 = 0xCAFE_BABE;
const FAT_MAGIC_64: u32 = 0xCAFE_BABF;
pub(crate) const CPU_TYPE_X86_64: u32 = 0x0100_0007;
pub(crate) const CPU_TYPE_ARM64: u32 = 0x0100_000C;
const MACH_HEADER_64: usize = 32;
const SEGMENT_64_MIN: usize = 72;
const CODE_SIGNATURE_CMD: usize = 16;
/// A fat header with more entries than this is not a real universal binary (and `CAFEBABE`
/// also opens Java class files, whose count field is large).
const MAX_FAT_ARCHS: u32 = 16;

/// What kind of file a digest was computed from.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContentFormat {
    /// One 64-bit little-endian Mach-O.
    MachO,
    /// A universal binary; one digest per slice.
    MachOUniversal,
    /// A Windows PE image (Authenticode rule).
    Pe,
    /// Any other file, hashed whole.
    Plain,
}

/// The content digest of one architecture slice (or of the whole file).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SliceDigest {
    /// `aarch64` or `x86_64`; `None` for a plain file.
    pub arch: Option<&'static str>,
    /// Lower-case hex SHA-256.
    pub sha256: String,
}

/// The signature-independent identity of a file.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContentIdentity {
    pub format: ContentFormat,
    /// One entry, except for a universal Mach-O (one per slice, in fat-header order).
    pub slices: Vec<SliceDigest>,
}

impl ContentIdentity {
    /// The digest for `arch`, if the file has such a slice.
    pub fn digest_for(&self, arch: &str) -> Option<&str> {
        self.slices
            .iter()
            .find(|slice| slice.arch.is_some_and(|a| a.eq_ignore_ascii_case(arch)))
            .map(|slice| slice.sha256.as_str())
    }
}

/// Content identity of the file at `path`.
pub fn content_identity_file(path: &Path) -> Result<ContentIdentity, MediaError> {
    let bytes = fs::read(path)?;
    content_identity(&bytes)
}

/// Content identity of `bytes`; see the module docs for the algorithm.
pub fn content_identity(bytes: &[u8]) -> Result<ContentIdentity, MediaError> {
    let magic_be = be32(bytes, 0);
    let magic_le = le32(bytes, 0);
    match (magic_be, magic_le) {
        (Some(FAT_MAGIC | FAT_MAGIC_64), _) => fat_identity(bytes),
        (_, Some(MH_MAGIC_64)) => {
            let (arch, sha256) = macho_slice_digest(bytes)?;
            Ok(ContentIdentity {
                format: ContentFormat::MachO,
                slices: vec![SliceDigest {
                    arch: Some(arch),
                    sha256,
                }],
            })
        }
        // 32-bit, big-endian and swapped-fat Mach-O magics: not supported, never plain-hashed.
        (
            Some(0xFEED_FACE | 0xFEED_FACF | 0xCEFA_EDFE | 0xCFFA_EDFE | 0xBEBA_FECA | 0xBFBA_FECA),
            _,
        ) => malformed("unsupported Mach-O flavour (only 64-bit little-endian is catalogued)"),
        _ if bytes.starts_with(b"MZ") => {
            let (arch, sha256) = pe_digest(bytes)?;
            Ok(ContentIdentity {
                format: ContentFormat::Pe,
                slices: vec![SliceDigest {
                    arch: Some(arch),
                    sha256,
                }],
            })
        }
        _ => Ok(ContentIdentity {
            format: ContentFormat::Plain,
            slices: vec![SliceDigest {
                arch: None,
                sha256: rightkit_fs::digest::sha256_bytes(bytes),
            }],
        }),
    }
}

fn malformed<T>(why: &str) -> Result<T, MediaError> {
    Err(MediaError::MalformedBinary(why.to_owned()))
}

fn le32(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        bytes.get(at..at.checked_add(4)?)?.try_into().ok()?,
    ))
}

fn le64(bytes: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(
        bytes.get(at..at.checked_add(8)?)?.try_into().ok()?,
    ))
}

fn le16(bytes: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(
        bytes.get(at..at.checked_add(2)?)?.try_into().ok()?,
    ))
}

fn be32(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(
        bytes.get(at..at.checked_add(4)?)?.try_into().ok()?,
    ))
}

fn be64(bytes: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_be_bytes(
        bytes.get(at..at.checked_add(8)?)?.try_into().ok()?,
    ))
}

fn arch_name(cputype: u32) -> Result<&'static str, MediaError> {
    match cputype {
        CPU_TYPE_ARM64 => Ok("aarch64"),
        CPU_TYPE_X86_64 => Ok("x86_64"),
        _ => malformed("unsupported CPU type"),
    }
}

/// Reads a list of byte ranges as one stream, so a patched header and the file body hash
/// without copying the body.
struct Parts<'a> {
    parts: Vec<&'a [u8]>,
    index: usize,
}

impl Read for Parts<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        while let Some(part) = self.parts.get_mut(self.index) {
            if part.is_empty() {
                self.index += 1;
                continue;
            }
            let n = part.len().min(buf.len());
            buf[..n].copy_from_slice(&part[..n]);
            *part = &part[n..];
            return Ok(n);
        }
        Ok(0)
    }
}

fn digest_parts(parts: Vec<&[u8]>) -> Result<String, MediaError> {
    let mut reader = Parts { parts, index: 0 };
    Ok(rightkit_fs::digest::sha256_reader(&mut reader)?)
}

fn round_up(value: u64, to: u64) -> Option<u64> {
    value.checked_add(to - 1).map(|v| v / to * to)
}

/// Digest of one thin Mach-O, with the architecture its header declares.
fn macho_slice_digest(slice: &[u8]) -> Result<(&'static str, String), MediaError> {
    let bad = |why: &str| MediaError::MalformedBinary(format!("Mach-O: {why}"));
    if le32(slice, 0) != Some(MH_MAGIC_64) {
        return Err(bad("not a 64-bit little-endian Mach-O"));
    }
    let cputype = le32(slice, 4).ok_or_else(|| bad("truncated header"))?;
    let arch = arch_name(cputype)?;
    let ncmds = le32(slice, 16).ok_or_else(|| bad("truncated header"))? as usize;
    let sizeofcmds = le32(slice, 20).ok_or_else(|| bad("truncated header"))? as usize;
    let cmds_end = MACH_HEADER_64
        .checked_add(sizeofcmds)
        .filter(|end| *end <= slice.len())
        .ok_or_else(|| bad("load commands run past the end of the file"))?;

    // (offset, size) of every command, validated and exactly tiling the command area.
    let mut commands = Vec::with_capacity(ncmds.min(4096));
    let mut offset = MACH_HEADER_64;
    for _ in 0..ncmds {
        let cmd = le32(slice, offset).ok_or_else(|| bad("truncated load command"))?;
        let size = le32(slice, offset + 4).ok_or_else(|| bad("truncated load command"))? as usize;
        if size < 8
            || !size.is_multiple_of(8)
            || offset.checked_add(size).is_none_or(|e| e > cmds_end)
        {
            return Err(bad("load command size is invalid"));
        }
        commands.push((offset, size, cmd));
        offset += size;
    }
    if offset != cmds_end {
        return Err(bad("load commands do not fill sizeofcmds"));
    }

    let mut signature: Option<usize> = None;
    let mut linkedit: Option<usize> = None;
    for (index, &(offset, size, cmd)) in commands.iter().enumerate() {
        match cmd {
            LC_CODE_SIGNATURE => {
                if size != CODE_SIGNATURE_CMD || signature.replace(index).is_some() {
                    return Err(bad("invalid or repeated LC_CODE_SIGNATURE"));
                }
            }
            LC_SEGMENT_64 => {
                if size < SEGMENT_64_MIN {
                    return Err(bad("short LC_SEGMENT_64"));
                }
                let name = &slice[offset + 8..offset + 24];
                let end = name.iter().position(|b| *b == 0).unwrap_or(name.len());
                if &name[..end] == b"__LINKEDIT" && linkedit.replace(index).is_some() {
                    return Err(bad("repeated __LINKEDIT"));
                }
            }
            _ => {}
        }
    }
    let linkedit_index = linkedit.ok_or_else(|| bad("no __LINKEDIT segment"))?;
    let (le_off, _, _) = commands[linkedit_index];
    let vmsize = le64(slice, le_off + 32).ok_or_else(|| bad("truncated __LINKEDIT"))?;
    let fileoff = le64(slice, le_off + 40).ok_or_else(|| bad("truncated __LINKEDIT"))?;
    let filesize = le64(slice, le_off + 48).ok_or_else(|| bad("truncated __LINKEDIT"))?;
    let linkedit_end = fileoff
        .checked_add(filesize)
        .ok_or_else(|| bad("__LINKEDIT overflows"))?;
    let len = slice.len() as u64;
    if fileoff < cmds_end as u64 || linkedit_end != len || vmsize < filesize || vmsize % 0x1000 != 0
    {
        return Err(bad("__LINKEDIT does not run to the end of the file"));
    }

    let mut content_end = len;
    if let Some(index) = signature {
        let (off, _, _) = commands[index];
        let dataoff = u64::from(le32(slice, off + 8).ok_or_else(|| bad("truncated signature"))?);
        let datasize = u64::from(le32(slice, off + 12).ok_or_else(|| bad("truncated signature"))?);
        if dataoff < fileoff || dataoff.checked_add(datasize) != Some(len) {
            return Err(bad("code signature is not the tail of __LINKEDIT"));
        }
        content_end = dataoff;
    }
    // Drop codesign's alignment padding (and nothing above __LINKEDIT's start).
    let mut end = content_end as usize;
    while end as u64 > fileoff && slice[end - 1] == 0 {
        end -= 1;
    }
    let canonical_filesize = end as u64 - fileoff;
    let page = if cputype == CPU_TYPE_ARM64 {
        0x4000
    } else {
        0x1000
    };
    let canonical_vmsize =
        round_up(canonical_filesize, page).ok_or_else(|| bad("__LINKEDIT overflows"))?;

    // Rebuild the load-command area without the signature command.
    let mut head = Vec::with_capacity(cmds_end);
    head.extend_from_slice(&slice[..MACH_HEADER_64]);
    let mut new_linkedit_at = 0usize;
    let mut removed = 0usize;
    for (index, &(offset, size, _)) in commands.iter().enumerate() {
        if Some(index) == signature {
            removed = size;
            continue;
        }
        if index == linkedit_index {
            new_linkedit_at = head.len();
        }
        head.extend_from_slice(&slice[offset..offset + size]);
    }
    head.resize(cmds_end, 0);
    head[new_linkedit_at + 32..new_linkedit_at + 40]
        .copy_from_slice(&canonical_vmsize.to_le_bytes());
    head[new_linkedit_at + 48..new_linkedit_at + 56]
        .copy_from_slice(&canonical_filesize.to_le_bytes());
    let new_ncmds = (ncmds - usize::from(signature.is_some())) as u32;
    let new_sizeofcmds = (sizeofcmds - removed) as u32;
    head[16..20].copy_from_slice(&new_ncmds.to_le_bytes());
    head[20..24].copy_from_slice(&new_sizeofcmds.to_le_bytes());

    let sha256 = digest_parts(vec![&head, &slice[cmds_end..end]])?;
    Ok((arch, sha256))
}

fn fat_identity(bytes: &[u8]) -> Result<ContentIdentity, MediaError> {
    let bad = |why: &str| MediaError::MalformedBinary(format!("universal Mach-O: {why}"));
    let wide = be32(bytes, 0) == Some(FAT_MAGIC_64);
    let count = be32(bytes, 4).ok_or_else(|| bad("truncated header"))?;
    if count == 0 || count > MAX_FAT_ARCHS {
        return Err(bad("implausible architecture count"));
    }
    let entry_size = if wide { 32 } else { 20 };
    let table_end = 8 + entry_size * count as usize;
    if table_end > bytes.len() {
        return Err(bad("architecture table runs past the end of the file"));
    }
    let mut slices = Vec::new();
    let mut seen = Vec::new();
    let mut ranges: Vec<(u64, u64)> = Vec::new();
    for index in 0..count as usize {
        let at = 8 + index * entry_size;
        let cputype = be32(bytes, at).ok_or_else(|| bad("truncated entry"))?;
        let (offset, size) = if wide {
            (
                be64(bytes, at + 8).ok_or_else(|| bad("truncated entry"))?,
                be64(bytes, at + 16).ok_or_else(|| bad("truncated entry"))?,
            )
        } else {
            (
                u64::from(be32(bytes, at + 8).ok_or_else(|| bad("truncated entry"))?),
                u64::from(be32(bytes, at + 12).ok_or_else(|| bad("truncated entry"))?),
            )
        };
        let end = offset
            .checked_add(size)
            .ok_or_else(|| bad("slice overflows"))?;
        if size == 0 || offset < table_end as u64 || end > bytes.len() as u64 {
            return Err(bad("slice is empty or out of bounds"));
        }
        if seen.contains(&cputype) {
            return Err(bad("duplicate architecture"));
        }
        if ranges.iter().any(|&(s, e)| offset < e && s < end) {
            return Err(bad("slices overlap"));
        }
        seen.push(cputype);
        ranges.push((offset, end));
        let slice = &bytes[offset as usize..end as usize];
        let (arch, sha256) = macho_slice_digest(slice)?;
        if arch != arch_name(cputype)? {
            return Err(bad("slice architecture differs from the fat header"));
        }
        slices.push(SliceDigest {
            arch: Some(arch),
            sha256,
        });
    }
    Ok(ContentIdentity {
        format: ContentFormat::MachOUniversal,
        slices,
    })
}

/// Authenticode SHA-256 of a PE image.
fn pe_digest(bytes: &[u8]) -> Result<(&'static str, String), MediaError> {
    let bad = |why: &str| MediaError::MalformedBinary(format!("PE: {why}"));
    let len = bytes.len();
    let pe = le32(bytes, 0x3C).ok_or_else(|| bad("no PE header offset"))? as usize;
    if bytes.get(pe..pe.saturating_add(4)) != Some(b"PE\0\0") {
        return Err(bad("missing PE signature"));
    }
    let machine = le16(bytes, pe + 4).ok_or_else(|| bad("truncated COFF header"))?;
    let arch = match machine {
        0x8664 => "x86_64",
        0xAA64 => "aarch64",
        _ => return Err(bad("unsupported machine type")),
    };
    let nsections = usize::from(le16(bytes, pe + 6).ok_or_else(|| bad("truncated COFF header"))?);
    let opt_size = usize::from(le16(bytes, pe + 20).ok_or_else(|| bad("truncated COFF header"))?);
    let opt = pe + 24;
    let sections_at = opt
        .checked_add(opt_size)
        .filter(|end| *end <= len)
        .ok_or_else(|| bad("optional header runs past the end of the file"))?;
    let dirs = match le16(bytes, opt) {
        Some(0x10B) => 96,
        Some(0x20B) => 112,
        _ => return Err(bad("unknown optional header magic")),
    };
    if opt_size < dirs {
        return Err(bad("optional header too short"));
    }
    let checksum_at = opt + 64;
    let size_of_headers =
        le32(bytes, opt + 60).ok_or_else(|| bad("truncated optional header"))? as usize;
    let dir_count = le32(bytes, opt + dirs - 4).ok_or_else(|| bad("truncated optional header"))?;
    let cert_entry = (dir_count > 4 && opt_size >= dirs + 5 * 8).then_some(opt + dirs + 4 * 8);
    let table_end = sections_at
        .checked_add(
            nsections
                .checked_mul(40)
                .ok_or_else(|| bad("section count overflows"))?,
        )
        .filter(|end| *end <= len)
        .ok_or_else(|| bad("section table runs past the end of the file"))?;
    let mut floor = checksum_at + 4;
    if let Some(entry) = cert_entry {
        floor = floor.max(entry + 8);
    }
    if size_of_headers > len || size_of_headers < floor.max(table_end) {
        return Err(bad("SizeOfHeaders is inconsistent"));
    }

    let (cert_offset, cert_size) = match cert_entry {
        Some(entry) => {
            let offset = le32(bytes, entry).ok_or_else(|| bad("truncated directory"))? as usize;
            let size = le32(bytes, entry + 4).ok_or_else(|| bad("truncated directory"))? as usize;
            (offset, size)
        }
        None => (0, 0),
    };
    if cert_size > 0
        && (cert_offset < size_of_headers || cert_offset.checked_add(cert_size) != Some(len))
    {
        return Err(bad("certificate table is not the tail of the file"));
    }

    let mut sections = Vec::with_capacity(nsections);
    for index in 0..nsections {
        let at = sections_at + index * 40;
        let size = le32(bytes, at + 16).ok_or_else(|| bad("truncated section"))? as usize;
        let offset = le32(bytes, at + 20).ok_or_else(|| bad("truncated section"))? as usize;
        if size > 0 {
            sections.push((offset, size));
        }
    }
    sections.sort_unstable();

    let mut parts: Vec<&[u8]> = vec![&bytes[..checksum_at]];
    match cert_entry {
        Some(entry) => {
            parts.push(&bytes[checksum_at + 4..entry]);
            parts.push(&bytes[entry + 8..size_of_headers]);
        }
        None => parts.push(&bytes[checksum_at + 4..size_of_headers]),
    }
    let mut hashed = size_of_headers;
    let mut previous_end = size_of_headers;
    let body_end = len - cert_size;
    for (offset, size) in sections {
        let end = offset
            .checked_add(size)
            .ok_or_else(|| bad("section overflows"))?;
        if offset < previous_end || end > body_end {
            return Err(bad("section overlaps the headers or the certificate table"));
        }
        parts.push(&bytes[offset..end]);
        hashed += size;
        previous_end = end;
    }
    // Authenticode hashes the trailing bytes starting at the count already hashed.
    if hashed > body_end {
        return Err(bad("hashed bytes exceed the file"));
    }
    parts.push(&bytes[hashed..body_end]);
    Ok((arch, digest_parts(parts)?))
}
